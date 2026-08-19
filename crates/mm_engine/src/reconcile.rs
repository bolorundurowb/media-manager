//! Reconcile stage (§5.6, §5.7).
//!
//! Detect intra-plan collisions, existing-file conflicts, and duplicates.

use std::collections::HashMap;

use mm_core::fs::{FileSystem, Hash};
use mm_core::identity::MovieId;
use mm_core::plan::{Action, ExistingInfo};
use mm_core::volume::VolumeSemantics;

use crate::planner::PlanItemInternal;

/// Reconcile planned items. Mutates `items` in place, converting `Move` actions
/// to `Conflict` or `Duplicate` where appropriate.
pub fn reconcile<F: FileSystem>(
    fs: &F,
    items: &mut [PlanItemInternal],
    volume: &VolumeSemantics,
) -> Result<(), std::io::Error> {
    // 1. Intra-plan collisions.
    let mut dest_buckets: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, item) in items.iter().enumerate() {
        if let Some(dest) = &item.destination {
            let key = volume.collision_key(&dest.to_string_lossy());
            dest_buckets.entry(key).or_default().push(i);
        }
    }
    for (_key, indices) in dest_buckets {
        if indices.len() > 1 {
            // Mark all but the first as conflicts.
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
    for item in items.iter_mut() {
        if !matches!(item.action, Action::Move { .. }) {
            continue;
        }
        let Some(dest) = &item.destination else { continue };
        if let Ok(meta) = fs.metadata(dest) {
            if !meta.is_dir {
                item.action = Action::Conflict {
                    from: item.source.clone(),
                    to: dest.clone(),
                    existing: ExistingInfo {
                        path: dest.clone(),
                        len: meta.len,
                        blake3: None,
                    },
                };
            }
        }
    }

    // 3. Duplicates: hash all Move sources and detect identical bytes within
    //    the same MovieId.
    let mut hash_buckets: HashMap<(MovieId, Hash), Vec<usize>> = HashMap::new();
    for (i, item) in items.iter().enumerate() {
        if !matches!(item.action, Action::Move { .. }) {
            continue;
        }
        let hash = fs.hash(&item.source, &mm_core::fs::CancelToken::new())?;
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
