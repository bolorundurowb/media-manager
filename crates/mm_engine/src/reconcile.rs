//! Reconcile stage (§5.6, §5.7).
//!
//! Detect intra-plan collisions, existing-file conflicts, and duplicates.
//! Existing-file conflicts honour [`mm_core::config::ConflictPolicy`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use mm_core::config::{CompareField, Config, ConflictPolicy};
use mm_core::fs::{CancelToken, FileSystem, Hash};
use mm_core::identity::MovieId;
use mm_core::plan::{Action, ExistingInfo, SkipReason};
use mm_core::volume::VolumeSemantics;
use mm_parse::split_copy_suffix;

use crate::planner::PlanItemInternal;

/// What to do with a destination that is already occupied.
#[derive(Debug, Clone, PartialEq)]
pub enum OccupiedDecision {
    Move {
        to: PathBuf,
    },
    Skip {
        reason: SkipReason,
    },
    Conflict {
        existing: ExistingInfo,
    },
    /// Reserve a sibling, copy, replacing-rename. Execute only.
    Replace,
}

/// Reconcile planned items. Mutates `items` in place, converting `Move` actions
/// to `Conflict` / `Skip` / `Duplicate` (or a RenameNew dest) where appropriate.
pub fn reconcile<F: FileSystem>(
    fs: &F,
    items: &mut [PlanItemInternal],
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
    //    within the same MovieId. Sidecars are never duplicates of videos even
    //    if their bytes match.
    let mut hash_buckets: HashMap<(MovieId, Hash), Vec<usize>> = HashMap::new();
    for (i, item) in items.iter().enumerate() {
        if !matches!(item.action, Action::Move { .. }) {
            continue;
        }
        if item.class != mm_core::classify::FileClass::Video {
            continue;
        }
        let hash = fs.hash(&item.source, &CancelToken::new())?;
        let key = (item.movie_id.clone(), hash);
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

/// Decide how to treat an occupied destination (§5.6).
///
/// Under `SkipIfIdentical` this compares before any reservation. `RenameNew`
/// picks `stem (N).ext` that does not currently exist. After RenameNew, if the
/// chosen dest is the source itself, the caller should treat that as NoOp.
pub fn decide_occupied<F: FileSystem>(
    fs: &F,
    cfg: &Config,
    volume: &VolumeSemantics,
    from: &Path,
    to: &Path,
    cancel: &CancelToken,
) -> OccupiedDecision {
    let Ok(meta) = fs.metadata(to) else {
        return OccupiedDecision::Move {
            to: to.to_path_buf(),
        };
    };
    let existing = ExistingInfo {
        path: to.to_path_buf(),
        len: meta.len,
        blake3: None,
    };

    match cfg.conflict.policy {
        ConflictPolicy::Report => OccupiedDecision::Conflict { existing },
        ConflictPolicy::Skip => OccupiedDecision::Skip {
            reason: SkipReason::Conflict,
        },
        ConflictPolicy::SkipIfIdentical => {
            if meta.is_dir {
                return OccupiedDecision::Conflict { existing };
            }
            if files_identical(fs, cfg, from, to, &meta, cancel) {
                OccupiedDecision::Skip {
                    reason: SkipReason::Identical,
                }
            } else {
                OccupiedDecision::Conflict { existing }
            }
        }
        ConflictPolicy::RenameNew => {
            let next = next_free_copy_path(fs, volume, from, to);
            OccupiedDecision::Move { to: next }
        }
        ConflictPolicy::Replace => {
            if meta.is_dir {
                OccupiedDecision::Conflict { existing }
            } else {
                OccupiedDecision::Replace
            }
        }
    }
}

fn files_identical<F: FileSystem>(
    fs: &F,
    cfg: &Config,
    from: &Path,
    to: &Path,
    dest_meta: &mm_core::fs::FileMeta,
    cancel: &CancelToken,
) -> bool {
    let Ok(src_meta) = fs.metadata(from) else {
        return false;
    };
    let compare = &cfg.conflict.compare;
    let want_size = compare.is_empty() || compare.contains(&CompareField::Size);
    let want_hash = compare.contains(&CompareField::Blake3);
    if want_size && src_meta.len != dest_meta.len {
        return false;
    }
    if want_hash {
        let Ok(a) = fs.hash(from, cancel) else {
            return false;
        };
        let Ok(b) = fs.hash(to, cancel) else {
            return false;
        };
        return a == b;
    }
    want_size && src_meta.len == dest_meta.len
}

/// Next free `stem (N).ext` that is not occupied and is not `from` itself
/// under volume collision semantics.
pub fn next_free_copy_path<F: FileSystem>(
    fs: &F,
    volume: &VolumeSemantics,
    from: &Path,
    dest: &Path,
) -> PathBuf {
    let parent = dest.parent().unwrap_or_else(|| Path::new(""));
    let ext = dest.extension().and_then(|e| e.to_str()).unwrap_or("");
    let stem_raw = dest.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let (stem, _) = split_copy_suffix(stem_raw);
    for n in 2..=9999u16 {
        let name = if ext.is_empty() {
            format!("{stem} ({n})")
        } else {
            format!("{stem} ({n}).{ext}")
        };
        let candidate = parent.join(&name);
        if same_path(from, &candidate, volume) {
            return candidate;
        }
        if fs.metadata(&candidate).is_err() {
            return candidate;
        }
    }
    dest.to_path_buf()
}

pub fn same_path(a: &Path, b: &Path, volume: &VolumeSemantics) -> bool {
    let key = |p: &Path| {
        p.components()
            .map(|c| volume.collision_key(c.as_os_str().to_string_lossy().as_ref()))
            .collect::<Vec<_>>()
            .join("\u{1f}")
    };
    key(a) == key(b)
}
