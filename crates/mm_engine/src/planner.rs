//! Planner orchestration (§5).
//!
//! Stages 1–11: scan, classify, parse, group, probe, resolve, regroup,
//! associate, route, validate, reconcile. Only planning — no writes.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use mm_core::classify::{FileClass, MediaKind};
use mm_core::config::Config;
use mm_core::error::{Diagnostic, Severity};
use mm_core::fs::FileSystem;
use mm_core::identity::{MovieId, Norm};
use mm_core::plan::{Action, DirRemoval, FieldName, ItemId, Plan, PlanItem, Readiness};
use mm_core::volume::VolumeSemantics;
use mm_parse::parse_movie;

use crate::classify::classify;
use crate::group::{MovieGroup, group_movies};
use crate::probe_stage::{could_become_move, open_cache, probe_and_merge};
use crate::reconcile::reconcile;
use crate::resolve::{ResolvedMovie, resolve_movie};
use crate::route::{RouteContext, route};
use crate::scan::{ScannedFile, scan};
use crate::validate::{Validation, validate_destination};

/// Internal planner item, mutated across stages.
#[derive(Debug, Clone)]
pub struct PlanItemInternal {
    pub id: ItemId,
    pub source: PathBuf,
    pub relative: PathBuf,
    pub class: FileClass,
    pub parsed: Option<mm_parse::ParsedMovie>,
    pub resolved: Option<ResolvedMovie>,
    pub movie_id: MovieId,
    pub destination: Option<PathBuf>,
    pub destination_relative: Option<PathBuf>,
    pub action: Action,
    pub readiness: Readiness,
}

/// Planning options.
#[derive(Debug, Clone, Copy, Default)]
pub struct PlanOptions {
    pub dry_run: bool,
}

/// The planner.
pub struct Planner<'a, F: FileSystem> {
    pub fs: &'a F,
    pub root: &'a Path,
    pub kind: MediaKind,
    pub cfg: &'a Config,
    pub volume: VolumeSemantics,
}

impl<'a, F: FileSystem> Planner<'a, F> {
    pub fn new(
        fs: &'a F,
        root: &'a Path,
        kind: MediaKind,
        cfg: &'a Config,
    ) -> Result<Self, std::io::Error> {
        let volume = fs
            .volume_semantics(root)
            .unwrap_or_else(|_| VolumeSemantics::conservative());
        Ok(Planner {
            fs,
            root,
            kind,
            cfg,
            volume,
        })
    }

    /// Run the full planning pipeline.
    pub fn plan(&self, _opts: PlanOptions) -> Result<Plan, std::io::Error> {
        let run_id = mm_core::plan::ItemId::new(0); // placeholder; real run_id comes from CLI
        let _ = run_id;
        let run_uuid = uuid::Uuid::new_v4();
        let mut plan = Plan::new(
            run_uuid,
            self.root.to_path_buf(),
            self.kind,
            self.cfg.digest(),
            self.volume,
        );

        // 1. Scan
        let mut scanned = scan(self.fs, self.root, self.cfg)?;
        if scanned.is_empty() {
            plan.diagnostics
                .push(Diagnostic::warning("scan", "no files found"));
            return Ok(plan);
        }

        // 2. Classify
        classify(&mut scanned, self.cfg);

        // 3. Parse (filename only) and a first-pass resolve.
        let mut items = self.parse_and_resolve(scanned);

        // 4. Provisional group on mandatory discriminators (title).
        let groups = self.group_items(&items);

        // 5. Probe only grouped videos that could become a Move.
        self.probe_grouped(&mut items, &groups, &mut plan);

        // 6+7. Re-resolve readiness after container fields, then regroup
        // with canonical year written back into every member.
        self.reresolve_readiness(&mut items);
        let groups = self.regroup_items(&mut items);

        // 8. Associate sidecars to videos in the same source directory.
        self.associate_sidecars(&mut items, &groups);

        // 9. Route
        self.route_items(&mut items, &groups);

        // 10. Validate
        self.validate_items(&mut items, &mut plan);

        // 11. Reconcile
        reconcile(self.fs, &mut items, &self.volume, self.cfg)?;

        // Build final plan.
        self.finalise_plan(items, &mut plan);
        Ok(plan)
    }

    fn parse_and_resolve(&self, scanned: Vec<ScannedFile>) -> Vec<PlanItemInternal> {
        scanned
            .into_iter()
            .enumerate()
            .map(|(i, f)| {
                let (resolved, parsed, movie_id, readiness) = if f.class == FileClass::Video {
                    match parse_movie(
                        f.absolute
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .as_ref(),
                    ) {
                        mm_parse::MediaParse::Movie(p) => {
                            let r = resolve_movie(&p, self.cfg);
                            let title_norm = r
                                .title
                                .as_value()
                                .map(|t| Norm::from_display(t))
                                .unwrap_or_else(Norm::empty);
                            let id = MovieId::new(title_norm);
                            let readiness = r.readiness(self.cfg);
                            (Some(r), Some(p), id, readiness)
                        }
                        mm_parse::MediaParse::Unknown => {
                            let id = MovieId::new(Norm::empty());
                            (
                                None,
                                None,
                                id,
                                Readiness::NeedsReview {
                                    missing: vec![FieldName::Title],
                                    reasons: vec!["could not parse filename".into()],
                                },
                            )
                        }
                    }
                } else {
                    let id = MovieId::new(Norm::empty());
                    (None, None, id, Readiness::Ready)
                };

                PlanItemInternal {
                    id: ItemId::new(i as u64),
                    source: f.absolute,
                    relative: f.relative,
                    class: f.class,
                    parsed,
                    resolved,
                    movie_id,
                    destination: None,
                    destination_relative: None,
                    action: Action::NoOp,
                    readiness,
                }
            })
            .collect()
    }

    fn probe_grouped(
        &self,
        items: &mut [PlanItemInternal],
        groups: &[MovieGroup],
        plan: &mut Plan,
    ) {
        let grouped: HashSet<usize> = groups
            .iter()
            .flat_map(|g| g.items.iter().copied())
            .collect();
        let cache = open_cache();
        for (idx, item) in items.iter_mut().enumerate() {
            if !grouped.contains(&idx) {
                continue;
            }
            if !could_become_move(item.class, item.resolved.as_ref()) {
                continue;
            }
            let source = item.source.clone();
            let id = item.id;
            let Some(resolved) = item.resolved.as_mut() else {
                continue;
            };
            probe_and_merge(self.fs, &source, id, resolved, &cache, self.cfg, plan);
        }
    }

    fn group_items(&self, items: &[PlanItemInternal]) -> Vec<MovieGroup> {
        let resolved: Vec<(usize, &ResolvedMovie)> = items
            .iter()
            .enumerate()
            .filter_map(|(i, it)| it.resolved.as_ref().map(|r| (i, r)))
            .collect();
        group_movies(&resolved)
    }

    fn reresolve_readiness(&self, items: &mut [PlanItemInternal]) {
        for item in items.iter_mut() {
            if let Some(resolved) = &item.resolved {
                if item.class == FileClass::Video {
                    item.readiness = resolved.readiness(self.cfg);
                }
            }
        }
    }

    /// Pass 2 of grouping: write the canonical year/id back into every member.
    fn regroup_items(&self, items: &mut [PlanItemInternal]) -> Vec<MovieGroup> {
        let groups = self.group_items(items);
        for g in &groups {
            for &idx in &g.items {
                let Some(item) = items.get_mut(idx) else {
                    continue;
                };
                item.movie_id = g.id.clone();
                if let (Some(year), Some(resolved)) = (g.id.year, item.resolved.as_mut()) {
                    if !resolved.year.is_known() {
                        resolved.year = mm_core::Field::known(
                            year,
                            mm_core::Source::Filename,
                            mm_core::Confidence::Medium,
                        );
                    }
                }
            }
        }
        groups
    }

    fn associate_sidecars(&self, items: &mut [PlanItemInternal], groups: &[MovieGroup]) {
        // Build a map of source directory -> video indices.
        let mut dir_to_videos: HashMap<PathBuf, Vec<usize>> = HashMap::new();
        for (i, it) in items.iter().enumerate() {
            if it.class == FileClass::Video && matches!(it.readiness, Readiness::Ready) {
                let dir = it.source.parent().unwrap_or(self.root).to_path_buf();
                dir_to_videos.entry(dir).or_default().push(i);
            }
        }

        // For each sidecar, find a video in the same directory and adopt its
        // movie_id / resolved fields.
        for i in 0..items.len() {
            if !is_sidecar(items[i].class) {
                continue;
            }
            let dir = items[i].source.parent().unwrap_or(self.root).to_path_buf();
            let candidates = dir_to_videos.get(&dir).cloned().unwrap_or_default();
            let chosen = choose_sidecar_parent(&items[i], &candidates, items);
            if let Some(parent_idx) = chosen {
                items[i].movie_id = items[parent_idx].movie_id.clone();
                items[i].resolved = items[parent_idx].resolved.clone();
                items[i].readiness = Readiness::Ready;
            } else {
                items[i].readiness = Readiness::NeedsReview {
                    missing: vec![FieldName::Title],
                    reasons: vec!["orphan sidecar: no matching video".into()],
                };
            }
        }

        let _ = groups; // used for future multi-dir logic
    }

    fn route_items(&self, items: &mut [PlanItemInternal], _groups: &[MovieGroup]) {
        for item in items.iter_mut() {
            if !item.readiness.is_ready() {
                item.action = Action::NeedsReview {
                    path: item.source.clone(),
                    missing: vec![FieldName::Title],
                };
                continue;
            }

            let Some(resolved) = &item.resolved else {
                continue;
            };
            let ctx = RouteContext {
                root: self.root,
                id: &item.movie_id,
                resolved,
                volume: &self.volume,
                cfg: self.cfg,
            };
            if let Some((abs, rel)) = route(&ctx, item.class, &item.source) {
                item.destination = Some(abs.clone());
                item.destination_relative = Some(rel);
            }
        }
    }

    fn validate_items(&self, items: &mut [PlanItemInternal], plan: &mut Plan) {
        for item in items.iter_mut() {
            let Some(dest) = &item.destination else {
                continue;
            };
            let rel = item
                .destination_relative
                .clone()
                .unwrap_or_else(|| dest.clone());
            let (validation, mut diags) =
                validate_destination(self.root, &item.source, dest, &rel, &self.volume);
            plan.diagnostics.append(&mut diags);

            match validation {
                Validation::NoOp => {
                    item.action = Action::NoOp;
                }
                Validation::Ok => {
                    item.action = Action::Move {
                        from: item.source.clone(),
                        to: dest.clone(),
                    };
                }
                Validation::ContainmentBreach { path } => {
                    item.action = Action::NeedsReview {
                        path: item.source.clone(),
                        missing: vec![FieldName::Title],
                    };
                    plan.diagnostics.push(Diagnostic {
                        severity: Severity::Failure,
                        item: Some(item.id),
                        stage: "validate".into(),
                        message: format!("destination escapes root: {}", path.display()),
                        io_kind: None,
                    });
                }
                Validation::TooLong { .. } | Validation::EmptyName { .. } => {
                    item.action = Action::NeedsReview {
                        path: item.source.clone(),
                        missing: vec![FieldName::Title],
                    };
                }
            }
        }
    }

    fn finalise_plan(&self, items: Vec<PlanItemInternal>, plan: &mut Plan) {
        let mut next_dir_rename_id = 0u64;
        let mut next_dir_removal_id = 0u64;

        for item in items {
            // Collect directories that need to be created.
            if let Some(dest) = &item.destination {
                if let Some(parent) = dest.parent() {
                    plan.dir_creates.insert(parent.to_path_buf());
                }
            }

            let action = item.action;

            // Stats
            match &action {
                Action::NoOp => plan.stats.noop += 1,
                Action::Move { .. } => plan.stats.ready += 1,
                Action::Skip { .. } => plan.stats.skipped += 1,
                Action::Conflict { .. } => plan.stats.conflicts += 1,
                Action::Duplicate { .. } => plan.stats.duplicates += 1,
                Action::NeedsReview { .. } => plan.stats.needs_review += 1,
            }
            plan.stats.total += 1;

            // Convert to PlanItem.
            plan.items.push(PlanItem {
                id: item.id,
                source: item.source.clone(),
                relative: item.relative.clone(),
                class: item.class,
                readiness: item.readiness,
                action,
                destination: item.destination,
            });
        }

        // Directory renames: case/normalisation-only fixes. Phase 2 leaves this
        // empty; populated when a library already has `season 01` style names.
        for path in plan.dir_creates.iter() {
            if let Some(rename) =
                detect_case_rename(self.fs, path, &self.volume, &mut next_dir_rename_id)
            {
                plan.dir_renames.push(rename);
            }
        }

        // Directory removals: deepest-first empty source directories, but never root.
        let mut removals: Vec<PathBuf> = plan
            .items
            .iter()
            .filter_map(|it| match &it.action {
                Action::Move { from, .. } => from.parent().map(Path::to_path_buf),
                _ => None,
            })
            .filter(|p| p != self.root)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        removals.sort_by_key(|b| std::cmp::Reverse(b.as_os_str().len()));
        for path in removals {
            plan.dir_removals.push(DirRemoval {
                id: mm_core::plan::DirRemovalId(next_dir_removal_id),
                path,
            });
            next_dir_removal_id += 1;
        }
    }
}

fn is_sidecar(class: FileClass) -> bool {
    matches!(
        class,
        FileClass::Subtitle | FileClass::Artwork | FileClass::Metadata
    )
}

fn choose_sidecar_parent(
    sidecar: &PlanItemInternal,
    candidates: &[usize],
    items: &[PlanItemInternal],
) -> Option<usize> {
    if candidates.is_empty() {
        return None;
    }
    let sidecar_stem = sidecar
        .source
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();

    // Gate: only a video whose stem is a prefix of the sidecar's stem is
    // name-evidence for association at all (§5.4 "name-evidence gate then
    // ranked score"). A single video in the directory is *not* an automatic
    // match — a differently-named orphan sidecar must fall through to
    // `NeedsReview`, not be silently attached to whatever video happens to be
    // there.
    let mut best: Option<(usize, usize)> = None;
    for &idx in candidates {
        let video_stem = items[idx]
            .source
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        if !video_stem.is_empty() && sidecar_stem.starts_with(&video_stem) {
            // Score: longest matching stem wins.
            let len = video_stem.len();
            if best.is_none_or(|(_, bl)| len > bl) {
                best = Some((idx, len));
            }
        }
    }
    best.map(|(idx, _)| idx)
}

fn detect_case_rename<F: FileSystem>(
    fs: &F,
    path: &Path,
    volume: &VolumeSemantics,
    next_id: &mut u64,
) -> Option<mm_core::plan::DirRename> {
    // Case- and normalisation-sensitive volumes have nothing to fix.
    if volume.case_sensitive && volume.normalisation_sensitive {
        return None;
    }
    let parent = path.parent()?;
    let desired = path.file_name()?.to_string_lossy().into_owned();
    let desired_key = volume.collision_key(&desired);
    let iter = fs.read_dir(parent).ok()?;
    let mut mismatch: Option<PathBuf> = None;
    for entry in iter.flatten() {
        if !entry.is_dir {
            continue;
        }
        let name = entry.file_name.to_string_lossy();
        if name.as_ref() == desired {
            return None;
        }
        if volume.collision_key(&name) == desired_key {
            mismatch = Some(entry.path);
        }
    }
    mismatch.map(|from| {
        let id = *next_id;
        *next_id += 1;
        mm_core::plan::DirRename {
            id: mm_core::plan::DirRenameId(id),
            from,
            to: path.to_path_buf(),
        }
    })
}

// Helper trait so Readiness can be queried uniformly.
trait ReadinessExt {
    fn is_ready(&self) -> bool;
}

impl ReadinessExt for Readiness {
    fn is_ready(&self) -> bool {
        matches!(self, Readiness::Ready)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: u64, file_name: &str) -> PlanItemInternal {
        PlanItemInternal {
            id: ItemId::new(id),
            source: PathBuf::from(format!("/movies/{file_name}")),
            relative: PathBuf::from(file_name),
            class: FileClass::Video,
            parsed: None,
            resolved: None,
            movie_id: MovieId::new(Norm::empty()),
            destination: None,
            destination_relative: None,
            action: Action::NoOp,
            readiness: Readiness::Ready,
        }
    }

    #[test]
    fn sidecar_matches_longest_stem_prefix() {
        let items = vec![
            item(0, "Inception.2010.1080p.mkv"),
            item(1, "Inception.2010.mkv"),
        ];
        let mut sidecar = item(2, "Inception.2010.1080p.en.srt");
        sidecar.class = FileClass::Subtitle;
        let chosen = choose_sidecar_parent(&sidecar, &[0, 1], &items);
        assert_eq!(chosen, Some(0));
    }

    #[test]
    fn orphan_sidecar_with_no_stem_match_is_not_associated() {
        // A single video in the directory is not itself evidence — the gate
        // must reject a sidecar whose name shares no prefix with it, rather
        // than falling back to "the only candidate".
        let items = vec![item(0, "Some Other Movie.2019.mkv")];
        let mut sidecar = item(1, "Unrelated.Subtitle.en.srt");
        sidecar.class = FileClass::Subtitle;
        let chosen = choose_sidecar_parent(&sidecar, &[0], &items);
        assert_eq!(chosen, None);
    }

    #[test]
    fn detect_case_rename_on_insensitive_volume() {
        use mm_core::fs::mem::MemFs;

        let fs = MemFs::with_volume(VolumeSemantics::conservative());
        fs.seed_dir(Path::new("/lib"));
        fs.seed_dir(Path::new("/lib/movie (2010)"));
        let mut next = 0u64;
        let rename = detect_case_rename(
            &fs,
            Path::new("/lib/Movie (2010)"),
            &VolumeSemantics::conservative(),
            &mut next,
        )
        .expect("case-only rename");
        assert_eq!(rename.from, PathBuf::from("/lib/movie (2010)"));
        assert_eq!(rename.to, PathBuf::from("/lib/Movie (2010)"));
    }

    #[test]
    fn detect_case_rename_skips_sensitive_volumes() {
        use mm_core::fs::mem::MemFs;

        let fs = MemFs::with_volume(VolumeSemantics::sensitive_bytes());
        fs.seed_dir(Path::new("/lib"));
        fs.seed_dir(Path::new("/lib/movie (2010)"));
        let mut next = 0u64;
        assert!(
            detect_case_rename(
                &fs,
                Path::new("/lib/Movie (2010)"),
                &VolumeSemantics::sensitive_bytes(),
                &mut next,
            )
            .is_none()
        );
    }

    fn testdata_media() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/media")
    }

    #[test]
    fn probe_scope_ratio_labels_1080p_not_720p() {
        use mm_core::fs::real::RealFs;
        use mm_core::plan::Action;

        let tmp = tempfile::TempDir::new().unwrap();
        let src_fixture = testdata_media().join("scope_1080p.mp4");
        let dest = tmp.path().join("Scope.Movie.2010.mp4");
        std::fs::copy(&src_fixture, &dest).expect("copy scope fixture");

        let cfg = Config::default();
        let fs = RealFs::new();
        let planner = Planner::new(&fs, tmp.path(), MediaKind::Movies, &cfg).unwrap();
        let plan = planner.plan(Default::default()).unwrap();
        let item = plan
            .items
            .iter()
            .find(|i| i.class == FileClass::Video)
            .expect("video item");
        match &item.action {
            Action::Move { to, .. } => {
                let name = to.file_name().unwrap().to_string_lossy();
                assert!(
                    name.contains("1080p"),
                    "scope 1920x800 must band as 1080p, got {name}"
                );
                assert!(
                    !name.contains("720p"),
                    "scope 1920x800 must not band as 720p, got {name}"
                );
            }
            other => panic!("expected Move, got {other:?}"),
        }
    }

    #[test]
    fn unsupported_avi_falls_back_to_filename_with_diagnostic() {
        use mm_core::fs::real::RealFs;
        use mm_core::plan::Action;

        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("Some.Movie.2010.1080p.avi"),
            b"not a real avi",
        )
        .unwrap();

        let cfg = Config::default();
        let fs = RealFs::new();
        let planner = Planner::new(&fs, tmp.path(), MediaKind::Movies, &cfg).unwrap();
        let plan = planner.plan(Default::default()).unwrap();
        let item = plan
            .items
            .iter()
            .find(|i| i.class == FileClass::Video)
            .expect("video item");
        match &item.action {
            Action::Move { to, .. } => {
                let name = to.file_name().unwrap().to_string_lossy();
                assert!(
                    name.contains("1080p"),
                    "filename resolution must survive unsupported container, got {name}"
                );
            }
            other => panic!("expected Move from filename fallback, got {other:?}"),
        }
        assert!(
            plan.diagnostics.iter().any(|d| {
                d.stage == "probe" && d.message.contains("container-based detection unavailable")
            }),
            "expected unsupported-container diagnostic, got {:?}",
            plan.diagnostics
        );
    }
}
