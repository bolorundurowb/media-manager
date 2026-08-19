//! Probe stage (§5 stage 5, PLAN §4).
//!
//! Runs after provisional grouping and before field resolution is finalised.
//! Only videos that already have a title (so they could become a `Move`) are
//! probed. The cache is keyed on `(file_id, size, mtime)`, never on path.
//!
//! Unsupported containers degrade to filename fields plus a Warning diagnostic.
//! HDR is never taken from the container.

use std::path::Path;

use mm_core::classify::FileClass;
use mm_core::config::Config;
use mm_core::error::{Diagnostic, Severity};
use mm_core::fs::FileSystem;
use mm_core::plan::{ItemId, Plan};
use mm_core::provenance::{Confidence, Field, Source};
use mm_probe::{
    CONTAINER_PROBING_UNAVAILABLE, CacheKey, ProbeCache, ProbeOutcome, label_resolution, probe_path,
};

use crate::resolve::{ResolvedMovie, merge_field};

/// `true` if this video is in a group that could become a `Move`.
pub fn could_become_move(class: FileClass, resolved: Option<&ResolvedMovie>) -> bool {
    class == FileClass::Video && resolved.is_some_and(|r| r.title.is_known())
}

/// On-disk cache under the user cache dir, or in-memory if that dir is unknown.
pub fn open_cache() -> ProbeCache {
    match mm_core::config::cache_dir() {
        Some(dir) => ProbeCache::new(dir.join("probe")),
        None => ProbeCache::in_memory(),
    }
}

/// Probe `source` and merge container resolution/codec into `resolved`.
pub fn probe_and_merge<F: FileSystem>(
    fs: &F,
    source: &Path,
    id: ItemId,
    resolved: &mut ResolvedMovie,
    cache: &ProbeCache,
    cfg: &Config,
    plan: &mut Plan,
) {
    let probe = match cached_or_probe(fs, source, id, cache, plan) {
        Some(p) => p,
        None => return,
    };
    let Some(video) = probe.video.as_ref() else {
        plan.diagnostics.push(item_warning(
            id,
            "container has no video track; falling back to filename",
        ));
        return;
    };

    let prefs = &cfg.source_preference;
    if let Some(label) = label_resolution(video) {
        let candidate = Field::known(label, Source::ContainerHeader, Confidence::High);
        resolved.resolution = merge_field(&resolved.resolution, candidate, prefs);
    } else if !resolved.resolution.is_known() {
        plan.diagnostics
            .push(item_warning(id, CONTAINER_PROBING_UNAVAILABLE));
    }

    if let Some(codec) = &video.codec {
        let candidate = Field::known(codec.clone(), Source::ContainerHeader, Confidence::High);
        resolved.video_codec = merge_field(&resolved.video_codec, candidate, prefs);
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

    match probe_path(source) {
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
                format!("container probe failed: {error}; falling back to filename"),
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
