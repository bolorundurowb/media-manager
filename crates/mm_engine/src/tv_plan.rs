//! TV planning pipeline (Phase 5, §3.2, §5.3, §5.4, §5.5, §6.5).
//!
//! Mirrors `planner::Planner::plan_movies` in shape (scan → classify → parse
//! → group (with canonical show-year write-back) → associate → route →
//! validate → reconcile) but keyed on `mm_core::identity::{ShowId,
//! EpisodeId}` instead of `MovieId` — see `crate::planner::Planner::plan` for
//! why this is a separate pipeline rather than a generalisation of the movie
//! one.
//!
//! ## Explicit follow-up: no probe stage
//!
//! The movie pipeline's stage list is scan → classify → parse → group →
//! **probe** → resolve → regroup → associate → route → validate → reconcile,
//! with grouping split in two around probing because probing is expensive
//! and should only run on files that survived provisional grouping (§5,
//! §5.3). This pass does not wire TV into `crate::probe_stage` at all — it is
//! filename-only, the same starting point the movie pipeline itself had
//! before Phase 4. Because there is no probe stage, there is also no need to
//! split grouping in two: nothing changes `ResolvedEpisode`'s fields between
//! an initial group and a second one, so `regroup_items` below does the
//! two-pass §2.2 title/year resolution in one call. If/when TV gains
//! container probing, this is the seam where a `probe_grouped` stage and a
//! second grouping pass would be reintroduced, exactly as movies have it.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use mm_core::classify::FileClass;
use mm_core::config::Config;
use mm_core::error::{Diagnostic, Severity};
use mm_core::fs::{CancelToken, FileSystem, Hash};
use mm_core::identity::{EpisodeId, Norm, ShowId};
use mm_core::plan::{
    Action, DirRemoval, DirRemovalId, DirRename, DirRenameId, ExistingInfo, FieldName, ItemId,
    Plan, PlanItem, Readiness,
};
use mm_core::volume::VolumeSemantics;

use crate::classify::classify;
use crate::planner::{PlanOptions, Planner};
use crate::reconcile::{OccupiedDecision, decide_occupied, same_path};
use crate::scan::{ScannedFile, scan};
use crate::tv_group::{ShowGroup, group_shows};
use crate::tv_resolve::{ResolvedEpisode, resolve_episode};
use crate::tv_route::{self, RouteContext};
use crate::validate::{Validation, validate_destination};

/// Internal planner item for the TV pipeline, mutated across stages. Mirrors
/// `planner::PlanItemInternal` but keyed on `ShowId`/`EpisodeId` — see the
/// module docs on `crate::tv_plan` and `Planner::plan` for why this isn't
/// `PlanItemInternal` itself.
#[derive(Debug, Clone)]
struct PlanItemInternalTv {
    id: ItemId,
    source: PathBuf,
    relative: PathBuf,
    class: FileClass,
    resolved: Option<ResolvedEpisode>,
    show_id: ShowId,
    /// `Some` once season+episodes are known (either from this file's own
    /// filename, or — for sidecars — adopted from an associated video).
    episode_id: Option<EpisodeId>,
    destination: Option<PathBuf>,
    destination_relative: Option<PathBuf>,
    action: Action,
    readiness: Readiness,
    /// Subtitle-only: detected language/flags (§5.4). Meaningless for other
    /// classes.
    language: String,
    flags: Option<String>,
}

/// Run the TV planning pipeline.
pub fn plan_tv<F: FileSystem>(
    planner: &Planner<'_, F>,
    _opts: PlanOptions,
) -> Result<Plan, std::io::Error> {
    let run_uuid = uuid::Uuid::new_v4();
    let mut plan = Plan::new(
        run_uuid,
        planner.root.to_path_buf(),
        planner.kind,
        planner.cfg.digest(),
        planner.volume,
    );

    // 1. Scan
    let mut scanned = scan(planner.fs, planner.root, planner.cfg)?;
    if scanned.is_empty() {
        plan.diagnostics
            .push(Diagnostic::warning("scan", "no files found"));
        return Ok(plan);
    }

    // 2. Classify
    classify(&mut scanned, planner.cfg);

    // 3. Parse (filename only) + resolve.
    let mut items = parse_and_resolve(scanned, planner.cfg);

    // 4/7 (collapsed — see module docs). Two-pass §2.2 group: pass 1 keys on
    // normalised show title, pass 2 resolves canonical show year and writes
    // it back into every member; each item's own season/episodes (always
    // present in its own filename, unlike a show's year) combine with the
    // resolved `ShowId` into that item's `EpisodeId`.
    regroup_items(&mut items);

    // 8. Associate sidecars (subtitles/artwork/metadata) to episodes.
    associate_sidecars(planner.root, &mut items);

    // 9. Route
    route_items(planner, &mut items);

    // 10. Validate
    validate_items(planner, &mut items, &mut plan);

    // 11. Reconcile
    reconcile_tv(planner.fs, &mut items, &planner.volume, planner.cfg)?;

    // Build final plan.
    finalise_plan(planner, items, &mut plan);
    Ok(plan)
}

fn parse_and_resolve(scanned: Vec<ScannedFile>, cfg: &Config) -> Vec<PlanItemInternalTv> {
    let opts = mm_parse::ParseOptions::default();
    scanned
        .into_iter()
        .enumerate()
        .map(|(i, f)| {
            let (resolved, readiness) = if f.class == FileClass::Video {
                let filename = f
                    .absolute
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                let parsed = mm_parse::parse_episode_filename(&filename, &opts);
                let resolved = resolve_episode(&parsed, cfg);
                let readiness = resolved.readiness(cfg);
                (Some(resolved), readiness)
            } else {
                // Sidecars start `Ready` and are resolved by association
                // (§5.4) — mirrors `planner::parse_and_resolve`'s treatment
                // of non-video files.
                (None, Readiness::Ready)
            };

            PlanItemInternalTv {
                id: ItemId::new(i as u64),
                source: f.absolute,
                relative: f.relative,
                class: f.class,
                resolved,
                show_id: ShowId::new(Norm::empty()),
                episode_id: None,
                destination: None,
                destination_relative: None,
                action: Action::NoOp,
                readiness,
                language: "und".to_string(),
                flags: None,
            }
        })
        .collect()
}

fn group_items(items: &[PlanItemInternalTv]) -> Vec<ShowGroup> {
    let resolved: Vec<(usize, &ResolvedEpisode)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, it)| it.resolved.as_ref().map(|r| (i, r)))
        .collect();
    group_shows(&resolved)
}

/// Two-pass §2.2 grouping: write the canonical show id (with resolved year)
/// back into every member, and build each item's full `EpisodeId` from its
/// own (already-known) season/episodes plus that canonical show id.
fn regroup_items(items: &mut [PlanItemInternalTv]) {
    let groups = group_items(items);
    for g in &groups {
        for &idx in &g.items {
            let Some(item) = items.get_mut(idx) else {
                continue;
            };
            item.show_id = g.id.clone();
            if let (Some(year), Some(resolved)) = (g.id.year, item.resolved.as_mut()) {
                if !resolved.year.is_known() {
                    resolved.year = mm_core::Field::known(
                        year,
                        mm_core::Source::Filename,
                        mm_core::Confidence::Medium,
                    );
                }
            }
            if let Some(resolved) = &item.resolved {
                if let (Some(season), Some(episodes)) = (
                    resolved.season.as_value().copied(),
                    resolved.episodes.as_value().cloned(),
                ) {
                    item.episode_id = Some(EpisodeId::new(g.id.clone(), season, episodes));
                }
            }
        }
    }
}

fn is_sidecar(class: FileClass) -> bool {
    matches!(
        class,
        FileClass::Subtitle | FileClass::Artwork | FileClass::Metadata
    )
}

/// Associate subtitles/artwork/metadata to the episode they belong with
/// (§5.4, §7 "episode id is the strongest gate signal" for TV subtitles).
fn associate_sidecars(root: &Path, items: &mut [PlanItemInternalTv]) {
    // Directory -> ready video indices, exactly mirroring
    // `planner::associate_sidecars`'s map (only `Ready` videos are viable
    // association parents — an item that itself needs review is not a safe
    // anchor for anything else).
    let mut dir_to_videos: HashMap<PathBuf, Vec<usize>> = HashMap::new();
    for (i, it) in items.iter().enumerate() {
        if it.class == FileClass::Video && matches!(it.readiness, Readiness::Ready) {
            let dir = it.source.parent().unwrap_or(root).to_path_buf();
            dir_to_videos.entry(dir).or_default().push(i);
        }
    }

    for i in 0..items.len() {
        if !is_sidecar(items[i].class) {
            continue;
        }
        let dir = items[i].source.parent().unwrap_or(root).to_path_buf();
        let candidates = dir_to_videos.get(&dir).cloned().unwrap_or_default();
        let chosen = choose_sidecar_parent(&items[i], &candidates, items);
        if let Some(parent_idx) = chosen {
            items[i].show_id = items[parent_idx].show_id.clone();
            items[i].episode_id = items[parent_idx].episode_id.clone();
            items[i].resolved = items[parent_idx].resolved.clone();
            items[i].readiness = Readiness::Ready;
            if items[i].class == FileClass::Subtitle {
                let stem = items[i]
                    .source
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();
                let (lang, flags) = tv_route::detect_language_and_flags(&stem);
                items[i].language = lang;
                items[i].flags = flags;
            }
        } else {
            items[i].readiness = Readiness::NeedsReview {
                missing: vec![FieldName::Title],
                reasons: vec!["orphan sidecar: no matching episode".into()],
            };
        }
    }
}

/// Gate on real evidence, never on absence of alternatives (the exact bug
/// class `planner::choose_sidecar_parent`'s current, fixed form guards
/// against — see its two regression tests). Three gates, in descending
/// strength:
///
/// 1. (subtitles only, §5.4/§7 "episode id is the strongest gate signal")
///    the sidecar's own filename parses to a season+episode list that
///    exactly matches a candidate video's `EpisodeId`.
/// 2. Stem-prefix match against a candidate video's stem, longest wins.
/// 3. (artwork/metadata only) every video candidate in the directory agrees
///    on the same show — real corroborating evidence from *every*
///    alternative concurring, which is a different thing from "the only
///    alternative" and is never applied to subtitles.
///
/// A tie or no match at any gate is an orphan — untouched and reported, not
/// silently attached to whichever video happens to be present.
fn choose_sidecar_parent(
    sidecar: &PlanItemInternalTv,
    candidates: &[usize],
    items: &[PlanItemInternalTv],
) -> Option<usize> {
    if candidates.is_empty() {
        return None;
    }

    if sidecar.class == FileClass::Subtitle {
        if let Some(sidecar_name) = sidecar.source.file_name().and_then(|s| s.to_str()) {
            let parsed =
                mm_parse::parse_episode_filename(sidecar_name, &mm_parse::ParseOptions::default());
            if let (Some(season), Some(episodes)) =
                (parsed.season.as_value().copied(), parsed.episodes.as_value())
            {
                for &idx in candidates {
                    if let Some(eid) = &items[idx].episode_id {
                        if eid.season == season && &eid.episodes == episodes {
                            return Some(idx);
                        }
                    }
                }
            }
        }
    }

    let sidecar_stem = sidecar
        .source
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let mut best: Option<(usize, usize)> = None;
    for &idx in candidates {
        let video_stem = items[idx]
            .source
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        if !video_stem.is_empty() && sidecar_stem.starts_with(&video_stem) {
            let len = video_stem.len();
            if best.is_none_or(|(_, bl)| len > bl) {
                best = Some((idx, len));
            }
        }
    }
    if let Some((idx, _)) = best {
        return Some(idx);
    }

    if matches!(sidecar.class, FileClass::Artwork | FileClass::Metadata) {
        let mut shows = candidates.iter().map(|&idx| &items[idx].show_id);
        if let Some(first) = shows.next() {
            if shows.all(|s| s == first) {
                return Some(candidates[0]);
            }
        }
    }

    None
}

fn route_items<F: FileSystem>(planner: &Planner<'_, F>, items: &mut [PlanItemInternalTv]) {
    for item in items.iter_mut() {
        if !matches!(item.readiness, Readiness::Ready) {
            item.action = Action::NeedsReview {
                path: item.source.clone(),
                missing: missing_fields_for(&item.readiness),
            };
            continue;
        }
        let (Some(resolved), Some(episode_id)) = (&item.resolved, &item.episode_id) else {
            continue;
        };
        let ctx = RouteContext {
            root: planner.root,
            id: episode_id,
            resolved,
            volume: &planner.volume,
            cfg: planner.cfg,
            language: item.language.clone(),
            flags: item.flags.clone(),
        };
        if let Some((abs, rel)) = tv_route::route(&ctx, item.class, &item.source) {
            item.destination = Some(abs);
            item.destination_relative = Some(rel);
        }
    }
}

fn missing_fields_for(readiness: &Readiness) -> Vec<FieldName> {
    match readiness {
        Readiness::NeedsReview { missing, .. } => missing.clone(),
        _ => vec![FieldName::Title],
    }
}

fn validate_items<F: FileSystem>(
    planner: &Planner<'_, F>,
    items: &mut [PlanItemInternalTv],
    plan: &mut Plan,
) {
    for item in items.iter_mut() {
        let Some(dest) = &item.destination else {
            continue;
        };
        let rel = item
            .destination_relative
            .clone()
            .unwrap_or_else(|| dest.clone());
        let (validation, mut diags) =
            validate_destination(planner.root, &item.source, dest, &rel, &planner.volume);
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

/// Reconcile stage (§5.6, §5.7), mirroring `crate::reconcile::reconcile`'s
/// three sub-stages exactly, but keyed on `EpisodeId` instead of `MovieId`
/// for duplicate detection. Reuses `decide_occupied`/`same_path` directly —
/// both are already generic over plain paths/`&Config`/`&VolumeSemantics`,
/// not `PlanItemInternal`, so there is nothing movie-specific to work around.
fn reconcile_tv<F: FileSystem>(
    fs: &F,
    items: &mut [PlanItemInternalTv],
    volume: &VolumeSemantics,
    cfg: &Config,
) -> Result<(), std::io::Error> {
    // 1. Intra-plan collisions.
    let mut dest_buckets: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, item) in items.iter().enumerate() {
        if !matches!(item.action, Action::Move { .. }) {
            continue;
        }
        if let Some(dest) = &item.destination {
            let key = volume.collision_key(&dest.to_string_lossy());
            dest_buckets.entry(key).or_default().push(i);
        }
    }
    for (_key, indices) in dest_buckets {
        if indices.len() > 1 {
            for &idx in &indices[1..] {
                if let Some(item) = items.get_mut(idx) {
                    if let Some(dest) = item.destination.clone() {
                        item.action = Action::Conflict {
                            from: item.source.clone(),
                            to: dest.clone(),
                            existing: ExistingInfo {
                                path: dest,
                                len: 0,
                                blake3: None,
                            },
                        };
                    }
                }
            }
        }
    }

    // 2. Existing-file conflicts (only for items still marked Move).
    let cancel = CancelToken::new();
    for item in items.iter_mut() {
        if !matches!(item.action, Action::Move { .. }) {
            continue;
        }
        let Some(dest) = item.destination.clone() else {
            continue;
        };
        if fs.metadata(&dest).is_err() {
            continue;
        }
        match decide_occupied(fs, cfg, volume, &item.source, &dest, &cancel) {
            OccupiedDecision::Move { to } => {
                if same_path(&item.source, &to, volume) {
                    item.destination = Some(to);
                    item.action = Action::NoOp;
                } else {
                    item.destination = Some(to.clone());
                    item.action = Action::Move {
                        from: item.source.clone(),
                        to,
                    };
                }
            }
            OccupiedDecision::Skip { reason } => {
                item.action = Action::Skip { reason };
            }
            OccupiedDecision::Conflict { existing } => {
                item.action = Action::Conflict {
                    from: item.source.clone(),
                    to: dest,
                    existing,
                };
            }
            OccupiedDecision::Replace => {
                // Keep Move; execute performs the replacing copy.
            }
        }
    }

    // 3. Duplicates: hash Move *video* sources and detect identical bytes
    //    within the same `EpisodeId`. Sidecars are never duplicates of
    //    videos even if their bytes happen to match.
    let mut hash_buckets: HashMap<(EpisodeId, Hash), Vec<usize>> = HashMap::new();
    for (i, item) in items.iter().enumerate() {
        if !matches!(item.action, Action::Move { .. }) {
            continue;
        }
        if item.class != FileClass::Video {
            continue;
        }
        let Some(eid) = &item.episode_id else {
            continue;
        };
        let hash = fs.hash(&item.source, &CancelToken::new())?;
        hash_buckets.entry((eid.clone(), hash)).or_default().push(i);
    }
    for (_key, indices) in hash_buckets {
        if indices.len() > 1 {
            let first = indices[0];
            let first_dest = items[first].destination.clone();
            for &idx in &indices[1..] {
                if let Some(item) = items.get_mut(idx) {
                    item.action = Action::Duplicate {
                        from: item.source.clone(),
                        identical_to: first_dest.clone().unwrap_or_else(|| item.source.clone()),
                    };
                }
            }
        }
    }

    Ok(())
}

fn finalise_plan<F: FileSystem>(
    planner: &Planner<'_, F>,
    items: Vec<PlanItemInternalTv>,
    plan: &mut Plan,
) {
    let mut next_dir_rename_id = 0u64;
    let mut next_dir_removal_id = 0u64;

    for item in items {
        if let Some(dest) = &item.destination {
            if let Some(parent) = dest.parent() {
                plan.dir_creates.insert(parent.to_path_buf());
            }
        }

        let action = item.action;

        match &action {
            Action::NoOp => plan.stats.noop += 1,
            Action::Move { .. } => plan.stats.ready += 1,
            Action::Skip { .. } => plan.stats.skipped += 1,
            Action::Conflict { .. } => plan.stats.conflicts += 1,
            Action::Duplicate { .. } => plan.stats.duplicates += 1,
            Action::NeedsReview { .. } => plan.stats.needs_review += 1,
        }
        plan.stats.total += 1;

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

    for path in plan.dir_creates.iter() {
        if let Some(rename) =
            detect_case_rename(planner.fs, path, &planner.volume, &mut next_dir_rename_id)
        {
            plan.dir_renames.push(rename);
        }
    }

    let mut removals: Vec<PathBuf> = plan
        .items
        .iter()
        .filter_map(|it| match &it.action {
            Action::Move { from, .. } => from.parent().map(Path::to_path_buf),
            _ => None,
        })
        .filter(|p| p != planner.root)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    removals.sort_by_key(|b| std::cmp::Reverse(b.as_os_str().len()));
    for path in removals {
        plan.dir_removals.push(DirRemoval {
            id: DirRemovalId(next_dir_removal_id),
            path,
        });
        next_dir_removal_id += 1;
    }
}

/// Detect a case/normalisation-only directory rename candidate (§5.6/§5.7).
/// Duplicated from `planner::detect_case_rename` (private there) rather than
/// shared — identical logic, generic over `FileSystem`/`VolumeSemantics`.
fn detect_case_rename<F: FileSystem>(
    fs: &F,
    path: &Path,
    volume: &VolumeSemantics,
    next_id: &mut u64,
) -> Option<DirRename> {
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
        DirRename {
            id: DirRenameId(id),
            from,
            to: path.to_path_buf(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mm_core::classify::MediaKind;
    use mm_core::fs::mem::MemFs;

    fn cfg() -> Config {
        Config::default()
    }

    #[test]
    fn plans_single_episode_end_to_end() {
        let fs = MemFs::new();
        fs.seed_file(
            "/lib/Show.Name.S01E01.1080p.WEB.x264.mkv",
            vec![0u8; 16],
        );
        let cfg = cfg();
        let planner = Planner::new(&fs, Path::new("/lib"), MediaKind::Tv, &cfg).unwrap();
        let plan = planner.plan(PlanOptions::default()).unwrap();
        let item = plan
            .items
            .iter()
            .find(|i| i.class == FileClass::Video)
            .expect("video item");
        match &item.action {
            Action::Move { to, .. } => {
                let s = to.to_string_lossy();
                assert!(s.contains("Show Name"), "got {s}");
                assert!(s.contains("Season 01"), "got {s}");
                assert!(s.contains("S01E01"), "got {s}");
            }
            other => panic!("expected Move, got {other:?}"),
        }
    }

    #[test]
    fn specials_route_under_specials_dir_not_season_00() {
        let fs = MemFs::new();
        fs.seed_file("/lib/Show.Name.S00E01.mkv", vec![0u8; 16]);
        let cfg = cfg();
        let planner = Planner::new(&fs, Path::new("/lib"), MediaKind::Tv, &cfg).unwrap();
        let plan = planner.plan(PlanOptions::default()).unwrap();
        let item = plan
            .items
            .iter()
            .find(|i| i.class == FileClass::Video)
            .expect("video item");
        match &item.action {
            Action::Move { to, .. } => {
                let s = to.to_string_lossy();
                assert!(s.contains("Specials"), "got {s}");
                assert!(!s.contains("Season 00"), "got {s}");
            }
            other => panic!("expected Move, got {other:?}"),
        }
    }

    #[test]
    fn multi_episode_file_is_not_split() {
        let fs = MemFs::new();
        fs.seed_file("/lib/Show.Name.S01E01-E02.mkv", vec![0u8; 16]);
        let cfg = cfg();
        let planner = Planner::new(&fs, Path::new("/lib"), MediaKind::Tv, &cfg).unwrap();
        let plan = planner.plan(PlanOptions::default()).unwrap();
        let item = plan
            .items
            .iter()
            .find(|i| i.class == FileClass::Video)
            .expect("video item");
        match &item.action {
            Action::Move { to, .. } => {
                let s = to.to_string_lossy();
                assert!(s.contains("S01E01E02"), "multi-episode must not split, got {s}");
            }
            other => panic!("expected Move, got {other:?}"),
        }
    }

    #[test]
    fn subtitle_is_associated_by_episode_id_not_sole_video() {
        // Two videos in one directory — an orphan subtitle whose own name
        // carries a *different* episode id must not be attached to either
        // one just because association ran, the exact class of bug the
        // movie sidecar-association code was fixed for.
        let fs = MemFs::new();
        fs.seed_file("/lib/Show.Name.S01E01.mkv", vec![0u8; 16]);
        fs.seed_file("/lib/Show.Name.S01E02.mkv", vec![0u8; 16]);
        fs.seed_file("/lib/Show.Name.S01E02.en.srt", vec![0u8; 16]);
        let cfg = cfg();
        let planner = Planner::new(&fs, Path::new("/lib"), MediaKind::Tv, &cfg).unwrap();
        let plan = planner.plan(PlanOptions::default()).unwrap();
        let sub = plan
            .items
            .iter()
            .find(|i| i.class == FileClass::Subtitle)
            .expect("subtitle item");
        match &sub.action {
            Action::Move { to, .. } => {
                let s = to.to_string_lossy();
                assert!(s.contains("S01E02"), "subtitle must follow its own episode id, got {s}");
            }
            other => panic!("expected Move, got {other:?}"),
        }
    }

    #[test]
    fn orphan_subtitle_with_no_evidence_is_not_associated() {
        let fs = MemFs::new();
        fs.seed_file("/lib/Show.Name.S01E01.mkv", vec![0u8; 16]);
        fs.seed_file("/lib/completely.unrelated.en.srt", vec![0u8; 16]);
        let cfg = cfg();
        let planner = Planner::new(&fs, Path::new("/lib"), MediaKind::Tv, &cfg).unwrap();
        let plan = planner.plan(PlanOptions::default()).unwrap();
        let sub = plan
            .items
            .iter()
            .find(|i| i.class == FileClass::Subtitle)
            .expect("subtitle item");
        assert!(
            matches!(sub.readiness, Readiness::NeedsReview { .. }),
            "orphan subtitle must be reported, not silently attached: {:?}",
            sub.readiness
        );
    }

    #[test]
    fn absolute_numbering_needs_review_not_a_guess() {
        let fs = MemFs::new();
        fs.seed_file("/lib/Show Name - 137.mkv", vec![0u8; 16]);
        let cfg = cfg();
        let planner = Planner::new(&fs, Path::new("/lib"), MediaKind::Tv, &cfg).unwrap();
        let plan = planner.plan(PlanOptions::default()).unwrap();
        let item = plan
            .items
            .iter()
            .find(|i| i.class == FileClass::Video)
            .expect("video item");
        assert!(
            matches!(item.readiness, Readiness::Ambiguous { .. }),
            "ambiguous absolute numbering must not silently become a Move: {:?}",
            item.readiness
        );
        assert!(matches!(item.action, Action::NeedsReview { .. }));
    }
}
