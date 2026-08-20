//! Music planning pipeline (Phase 6, §3.3, §5.3, §5.5, PLAN §4 probing).
//!
//! Mirrors `planner::Planner::plan_movies`'s shape (scan → classify → parse
//! → group → probe → resolve → regroup → associate → route → validate →
//! reconcile) but keyed on `mm_core::identity::AlbumId` instead of `MovieId`,
//! for the same reason `tv_plan` is its own pipeline rather than a
//! generalisation of the movie one (see `planner::Planner::plan`).
//!
//! **Where this deliberately departs from the movie pipeline, and why:**
//!
//! - **The probe gate is "is this file classified `Audio`", not "could this
//!   become a `Move`".** Movies gate stage-5 probing on `title.is_known()`
//!   from the filename, because a movie filename usually *does* carry the
//!   title. A music filename usually does **not** carry `album`/`album_artist`
//!   at all (`01 - Title.mp3` tells you nothing about the album) — per §3.3
//!   and §8.4, that information is expected to live in tags. Gating on a
//!   filename-derived signal that is routinely absent would skip probing
//!   almost the entire library, which is the opposite of what Phase 6 exists
//!   to do. So every `Audio`-classified file is probed.
//! - **The two-pass `AlbumId` grouping (§2.2, §5.3) is still real, just with
//!   a different balance between the passes.** Pass 1 (`music_group::group_albums`
//!   called pre-probe) groups whatever filename-only `album_artist`/`album`
//!   signal exists — usually little to none, so it groups few items. Its
//!   only job here is structural parity with the documented pipeline shape;
//!   the *meaningful* grouping is pass 2 (`group_albums` called again
//!   post-probe/post-resolve), once tags have supplied `album_artist`/`album`
//!   for nearly every file. This is a direct, documented consequence of tags
//!   being the primary source for music (§3.3) rather than a shortcut.
//! - **Compilation detection is per-track, not whole-group.** §5.3 also
//!   allows "≥ N distinct track artists in one album key" as a compilation
//!   signal; that needs whole-group context (count distinct artists *within*
//!   a provisional group) that this per-track resolve step does not have.
//!   Only the `TCMP`/`FlagCompilation` flag and an explicit "Various
//!   Artists"-shaped album-artist tag are used (`music_resolve::ResolvedTrack::merge_tags`).
//!   A **known limitation** follows directly: if a compilation's tracks are
//!   *inconsistently* tagged — some carry the compilation flag or an
//!   explicit "Various Artists" album-artist tag, others carry neither and
//!   only a per-track `artist` tag — the untagged tracks fall back to their
//!   own individual artist as `album_artist` (see the "not a compilation"
//!   branch of `merge_tags`) and end up keyed into their own one-track album
//!   rather than merging with the rest. This is a real gap, not swept under
//!   a generic caveat — it is called out here because fixing it needs a
//!   whole-group-aware pass this phase does not implement.
//! - **No directory-rename detection.** `planner::detect_case_rename` is
//!   private to that module (movies' file) and Phase 6's exit criteria do
//!   not call for it, so `dir_renames` is left empty for music.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use mm_core::classify::FileClass;
use mm_core::config::Config;
use mm_core::error::{Diagnostic, Severity};
use mm_core::fs::{CancelToken, FileSystem, Hash};
use mm_core::identity::{AlbumId, Norm};
use mm_core::plan::{
    Action, DirRemoval, DirRemovalId, ExistingInfo, FieldName, ItemId, Plan, PlanItem, Readiness,
};
use mm_core::volume::VolumeSemantics;
use mm_parse::{ParseOptions, ParsedTrack, parse_track_filename};
use mm_probe::{CacheKey, ProbeCache, ProbeOutcome};

use crate::classify::classify;
use crate::music_group::{AlbumGroup, group_albums, has_partial_disc_info, is_multi_disc};
use crate::music_resolve::ResolvedTrack;
use crate::music_route::{MusicRouteContext, route};
use crate::planner::{PlanOptions, Planner};
use crate::probe_stage::open_cache;
use crate::reconcile::{OccupiedDecision, decide_occupied, same_path};
use crate::scan::{ScannedFile, scan};
use crate::validate::{Validation, validate_destination};

/// Internal planner item for the music pipeline, mutated across stages.
struct MusicItemInternal {
    id: ItemId,
    source: PathBuf,
    relative: PathBuf,
    class: FileClass,
    #[allow(dead_code)] // kept for parity/debuggability with the movie pipeline's shape
    parsed: Option<ParsedTrack>,
    resolved: Option<ResolvedTrack>,
    album_id: AlbumId,
    /// Whether this item's album spans more than one disc (§5.5), computed
    /// once per group in `apply_group_level_rules` and consumed at routing.
    multi_disc: bool,
    destination: Option<PathBuf>,
    destination_relative: Option<PathBuf>,
    action: Action,
    readiness: Readiness,
}

/// Run the music planning pipeline.
pub fn plan_music<F: FileSystem>(
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

    // 3. Parse (filename only) and a first-pass resolve.
    let mut items = parse_and_resolve(scanned, planner.cfg);

    // 4. Provisional group on mandatory discriminators (album_artist+album),
    //    from filename-only candidates — see module docs for why this pass
    //    groups little on its own for music.
    let _provisional_groups = group_items(&items);

    // 5. Probe every audio file for embedded tags (module docs: the movie
    //    "could become a Move" gate does not transfer to music).
    probe_audio_items(planner.fs, &mut items, &mut plan, planner.cfg);

    // 6+7. Re-resolve readiness with tag fields folded in, then regroup with
    //      the canonical year written back into every member.
    reresolve_readiness(&mut items, planner.cfg);
    let groups = regroup_items(&mut items);

    // Group-level rules: partial disc info -> NeedsReview (§5.5); otherwise
    // record whether the album is multi-disc for routing's `disc_dir` gate.
    apply_group_level_rules(&mut items, &groups);

    // 8. Associate artwork/metadata sidecars with the album owning their
    //    source directory.
    associate_sidecars(&mut items, planner.root);

    // 9. Route
    route_items(planner, &mut items);

    // 10. Validate
    validate_items(planner, &mut items, &mut plan);

    // 11. Reconcile
    reconcile_items(planner.fs, &mut items, &planner.volume, planner.cfg)?;

    // Build final plan.
    finalise_plan(planner, items, &mut plan);
    Ok(plan)
}

fn parse_and_resolve(scanned: Vec<ScannedFile>, cfg: &Config) -> Vec<MusicItemInternal> {
    scanned
        .into_iter()
        .enumerate()
        .map(|(i, f)| {
            let (resolved, parsed, album_id, readiness) = if f.class == FileClass::Audio {
                let filename = f
                    .absolute
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                let parsed = parse_track_filename(&filename, &ParseOptions::default());
                let resolved = ResolvedTrack::from_parsed(&parsed);
                let id = provisional_album_id(&resolved);
                let readiness = resolved.readiness(cfg);
                (Some(resolved), Some(parsed), id, readiness)
            } else {
                let id = AlbumId::new(Norm::empty(), Norm::empty());
                (None, None, id, Readiness::Ready)
            };

            MusicItemInternal {
                id: ItemId::new(i as u64),
                source: f.absolute,
                relative: f.relative,
                class: f.class,
                parsed,
                resolved,
                album_id,
                multi_disc: false,
                destination: None,
                destination_relative: None,
                action: Action::NoOp,
                readiness,
            }
        })
        .collect()
}

fn provisional_album_id(resolved: &ResolvedTrack) -> AlbumId {
    let artist = resolved
        .album_artist
        .as_value()
        .map(|s| Norm::from_display(s))
        .unwrap_or_else(Norm::empty);
    let album = resolved
        .album
        .as_value()
        .map(|s| Norm::from_display(s))
        .unwrap_or_else(Norm::empty);
    AlbumId::new(artist, album)
}

fn group_items(items: &[MusicItemInternal]) -> Vec<AlbumGroup> {
    let resolved: Vec<(usize, &ResolvedTrack)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, it)| it.resolved.as_ref().map(|r| (i, r)))
        .collect();
    group_albums(&resolved)
}

fn probe_audio_items<F: FileSystem>(
    fs: &F,
    items: &mut [MusicItemInternal],
    plan: &mut Plan,
    cfg: &Config,
) {
    let cache = open_cache();
    let prefs = &cfg.source_preference;
    for item in items.iter_mut() {
        if item.class != FileClass::Audio {
            continue;
        }
        let source = item.source.clone();
        let id = item.id;
        let Some(resolved) = item.resolved.as_mut() else {
            continue;
        };
        let Some(probe) = cached_or_probe(fs, &source, id, &cache, plan) else {
            continue;
        };
        match probe.audio {
            Some(tags) => resolved.merge_tags(&tags, prefs),
            None => {
                plan.diagnostics.push(item_warning(
                    id,
                    "audio file yielded no tags; falling back to filename",
                ));
            }
        }
    }
}

fn cached_or_probe<F: FileSystem>(
    fs: &F,
    source: &Path,
    id: ItemId,
    cache: &ProbeCache,
    plan: &mut Plan,
) -> Option<mm_probe::Probe> {
    let meta = match fs.metadata(source) {
        Ok(m) => m,
        Err(e) => {
            plan.diagnostics.push(item_warning(
                id,
                format!("probe skipped: metadata failed: {e}"),
            ));
            return None;
        }
    };
    let file_id = match fs.file_id(source) {
        Ok(fid) => fid,
        Err(e) => {
            plan.diagnostics.push(item_warning(
                id,
                format!("probe skipped: file id failed: {e}"),
            ));
            return None;
        }
    };
    let key = CacheKey::from_meta(file_id, &meta);
    if let Some(hit) = cache.get(&key) {
        return Some(hit);
    }

    match mm_probe::probe_audio_path(source) {
        ProbeOutcome::Probed(probe) => {
            cache.insert(key, probe.clone());
            Some(probe)
        }
        ProbeOutcome::Unsupported { reason, ext } => {
            plan.diagnostics
                .push(item_warning(id, format!("{reason} (.{ext})")));
            None
        }
        ProbeOutcome::Failed { error } => {
            plan.diagnostics.push(item_warning(
                id,
                format!("audio tag probe failed: {error}; falling back to filename"),
            ));
            None
        }
    }
}

fn item_warning(item: ItemId, message: impl Into<String>) -> Diagnostic {
    Diagnostic {
        severity: Severity::Warning,
        item: Some(item),
        stage: "probe".into(),
        message: message.into(),
        io_kind: None,
    }
}

fn reresolve_readiness(items: &mut [MusicItemInternal], cfg: &Config) {
    for item in items.iter_mut() {
        if let Some(resolved) = &item.resolved {
            if item.class == FileClass::Audio {
                item.readiness = resolved.readiness(cfg);
            }
        }
    }
}

/// Pass 2 of grouping: write the canonical year back into every member and
/// key every member on the complete `AlbumId`.
fn regroup_items(items: &mut [MusicItemInternal]) -> Vec<AlbumGroup> {
    let groups = group_items(items);
    for g in &groups {
        for &idx in &g.items {
            let Some(item) = items.get_mut(idx) else {
                continue;
            };
            item.album_id = g.id.clone();
            if let (Some(year), Some(resolved)) = (g.id.year, item.resolved.as_mut()) {
                if !resolved.year.is_known() {
                    // Mirrors `planner::Planner::regroup_items`'s convention
                    // for a majority-resolved value with no single clean
                    // provenance of its own.
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

fn apply_group_level_rules(items: &mut [MusicItemInternal], groups: &[AlbumGroup]) {
    for g in groups {
        let (partial, multi) = {
            let tracks: Vec<(usize, &ResolvedTrack)> = g
                .items
                .iter()
                .filter_map(|&i| items[i].resolved.as_ref().map(|r| (i, r)))
                .collect();
            (
                has_partial_disc_info(&g.items, &tracks),
                is_multi_disc(&g.items, &tracks),
            )
        };
        for &idx in &g.items {
            if partial {
                items[idx].readiness = Readiness::NeedsReview {
                    missing: vec![FieldName::Disc],
                    reasons: vec![
                        "some tracks in this album have a disc number and others do not (§5.5)"
                            .into(),
                    ],
                };
            } else {
                items[idx].multi_disc = multi;
            }
        }
    }
}

fn associate_sidecars(items: &mut [MusicItemInternal], root: &Path) {
    let mut dir_to_tracks: HashMap<PathBuf, Vec<usize>> = HashMap::new();
    for (i, it) in items.iter().enumerate() {
        if it.class == FileClass::Audio && matches!(it.readiness, Readiness::Ready) {
            let dir = it.source.parent().unwrap_or(root).to_path_buf();
            dir_to_tracks.entry(dir).or_default().push(i);
        }
    }

    for i in 0..items.len() {
        if !matches!(items[i].class, FileClass::Artwork | FileClass::Metadata) {
            continue;
        }
        let dir = items[i].source.parent().unwrap_or(root).to_path_buf();
        let candidates = dir_to_tracks.get(&dir).cloned().unwrap_or_default();
        if candidates.is_empty() {
            items[i].readiness = Readiness::NeedsReview {
                missing: vec![],
                reasons: vec!["orphan artwork/metadata: no ready track in the same directory".into()],
            };
            continue;
        }
        // Adopt the identity of a ready track sharing this directory. Unlike
        // §5.4's subtitle gate (name-evidence, then ranked score, keyed to a
        // *specific* video), artwork/metadata belong to the whole album
        // directory, so "any ready track here" is the natural analogue — a
        // directory mixing tracks from more than one album is not
        // disambiguated further.
        let chosen = candidates[0];
        items[i].album_id = items[chosen].album_id.clone();
        items[i].resolved = items[chosen].resolved.clone();
        items[i].multi_disc = items[chosen].multi_disc;
        items[i].readiness = Readiness::Ready;
    }
}

fn route_items<F: FileSystem>(planner: &Planner<'_, F>, items: &mut [MusicItemInternal]) {
    for item in items.iter_mut() {
        if !matches!(item.readiness, Readiness::Ready) {
            item.action = Action::NeedsReview {
                path: item.source.clone(),
                missing: readiness_missing(&item.readiness),
            };
            continue;
        }
        let Some(resolved) = &item.resolved else {
            continue;
        };
        let ctx = MusicRouteContext {
            root: planner.root,
            id: &item.album_id,
            resolved,
            multi_disc: item.multi_disc,
            volume: &planner.volume,
            cfg: planner.cfg,
        };
        if let Some((abs, rel)) = route(&ctx, item.class, &item.source) {
            item.destination = Some(abs);
            item.destination_relative = Some(rel);
        }
    }
}

fn readiness_missing(r: &Readiness) -> Vec<FieldName> {
    match r {
        Readiness::NeedsReview { missing, .. } => missing.clone(),
        _ => vec![],
    }
}

fn validate_items<F: FileSystem>(
    planner: &Planner<'_, F>,
    items: &mut [MusicItemInternal],
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
                    missing: vec![],
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
                    missing: vec![],
                };
            }
        }
    }
}

fn reconcile_items<F: FileSystem>(
    fs: &F,
    items: &mut [MusicItemInternal],
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

    // 3. Duplicates: hash Move *audio* sources and detect identical bytes
    //    within the same AlbumId. Sidecars are never duplicates of tracks.
    let mut hash_buckets: HashMap<(AlbumId, Hash), Vec<usize>> = HashMap::new();
    for (i, item) in items.iter().enumerate() {
        if !matches!(item.action, Action::Move { .. }) {
            continue;
        }
        if item.class != FileClass::Audio {
            continue;
        }
        let hash = fs.hash(&item.source, &CancelToken::new())?;
        let key = (item.album_id.clone(), hash);
        hash_buckets.entry(key).or_default().push(i);
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
    items: Vec<MusicItemInternal>,
    plan: &mut Plan,
) {
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

    // Directory removals: deepest-first empty source directories, never root.
    // No directory-rename detection for music — see module docs.
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
    let mut next_dir_removal_id = 0u64;
    for path in removals {
        plan.dir_removals.push(DirRemoval {
            id: DirRemovalId(next_dir_removal_id),
            path,
        });
        next_dir_removal_id += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mm_core::config::Config;
    use mm_core::fs::mem::MemFs;

    fn write_flac_like(fs: &MemFs, path: &Path, content: &[u8]) {
        fs.seed_file(path, content);
    }

    /// End-to-end smoke test using tagless files (no real audio bytes are
    /// exercised here — `MemFs` never calls into `lofty`, only `mm_probe`'s
    /// `RealFs`-backed tests do that). This exercises scan → classify →
    /// parse → group → route → validate → reconcile with pure filename
    /// signal, asserting the pipeline does not panic and produces a
    /// `NeedsReview` for a file whose filename alone can't supply
    /// `album`/`album_artist` — the expected, common case for music.
    #[test]
    fn tagless_track_without_album_info_needs_review() {
        let fs = MemFs::new();
        fs.seed_dir(Path::new("/music"));
        write_flac_like(&fs, Path::new("/music/01 - Some Song.mp3"), b"not real audio");

        let cfg = Config::default();
        let planner = Planner::new(&fs, Path::new("/music"), mm_core::classify::MediaKind::Music, &cfg)
            .unwrap();
        let plan = plan_music(&planner, PlanOptions::default()).unwrap();

        assert_eq!(plan.items.len(), 1);
        match &plan.items[0].action {
            Action::NeedsReview { .. } => {}
            other => panic!("expected NeedsReview without album/artist tags, got {other:?}"),
        }
    }
}
