//! Multi-selection entry point used by the GUI (Phase 7): run several
//! independently-typed source children against one shared (optionally
//! different) destination root, streaming progress as it happens.
//!
//! This runs the same scan → parse → group → plan → validate → execute
//! pipeline as the CLI's [`crate::run`], just once per assigned kind, and
//! adds one thing the CLI never needs: a check that a "movies" assignment
//! and a "tv" assignment never end up wanting the same destination path.
//!
//! Everything here runs sequentially today. Phase 6 is expected to
//! parallelize scan/parse and independent-destination execute jobs; this
//! function's signature (`run_items` / `WorkItem`) is the seam that work is
//! meant to land behind, so the GUI does not need to change when it does.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use crate::cancel::CancelToken;
use crate::group::{self, GroupOutcome};
use crate::journal::Journal;
use crate::parse::LibraryKind;
use crate::plan::{self, Plan, Skip};
use crate::vfs::RealFileSystem;
use crate::{scan, Error, Summary};

/// One source child (a folder, today — a loose top-level video file is a
/// Phase 6 scan capability this does not depend on yet) and the library
/// kind the user assigned it.
#[derive(Debug, Clone)]
pub struct WorkItem {
    pub path: PathBuf,
    pub kind: LibraryKind,
}

/// A single unit of progress, suitable for streaming to a UI as it happens.
#[derive(Debug)]
pub enum LogEvent {
    Scanning,
    CreateDir(PathBuf),
    Moved { from: PathBuf, to: PathBuf },
    Skipped { path: PathBuf, reason: String },
    Failed { path: PathBuf, reason: String },
    Finished {
        moved: usize,
        merged: usize,
        skipped: usize,
        failed: usize,
        cancelled: bool,
    },
}

/// Run a specific set of source children, each independently typed, against
/// an optional destination root. `dest == None` means in-place, matching the
/// CLI. Progress is streamed to `sink` as each stage completes; the return
/// value is the same aggregate a caller would get from [`crate::run`].
pub fn run_items(
    root: &Path,
    dest: Option<&Path>,
    items: Vec<WorkItem>,
    apply: bool,
    cancel: &CancelToken,
    sink: Sender<LogEvent>,
) -> Result<Summary, Error> {
    let root = match absolute(root) {
        Ok(r) => r,
        Err(err) => return Err(bail(&sink, root.to_path_buf(), err)),
    };
    match std::fs::metadata(&root) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            return Err(bail(&sink, root.clone(), "not a directory"));
        }
        Err(err) => return Err(bail(&sink, root.clone(), err)),
    }

    let dest_root = match dest {
        Some(d) => {
            let d = match absolute(d) {
                Ok(d) => d,
                Err(err) => return Err(bail(&sink, d.to_path_buf(), err)),
            };
            if let Err(err) = std::fs::create_dir_all(&d) {
                return Err(bail(&sink, d.clone(), err));
            }
            d
        }
        None => root.clone(),
    };

    let _ = sink.send(LogEvent::Scanning);

    let mut movie_folders = Vec::new();
    let mut tv_folders = Vec::new();
    for item in &items {
        match scan::scan_child(&item.path) {
            Ok(found) => match item.kind {
                LibraryKind::Movies => movie_folders.extend(found),
                LibraryKind::Tv => tv_folders.extend(found),
            },
            Err(err) => {
                let _ = sink.send(LogEvent::Failed {
                    path: item.path.clone(),
                    reason: format!("could not scan: {err}"),
                });
            }
        }
    }

    let mut summary = Summary::default();
    let mut plans: Vec<Plan> = Vec::new();

    for (kind, folders) in [
        (LibraryKind::Movies, movie_folders),
        (LibraryKind::Tv, tv_folders),
    ] {
        if folders.is_empty() {
            continue;
        }
        let (outcome, prior_skips) = group::group_folders(kind, folders);
        summary.merged += count_merged(&outcome);
        plans.push(plan::build_plan(&dest_root, outcome, prior_skips));
    }

    resolve_cross_plan_collisions(&mut plans, &sink);

    for plan in &plans {
        summary.planned_moves += plan.moves.len();
        summary.skipped += plan.skips.len();
        for skip in &plan.skips {
            let _ = sink.send(LogEvent::Skipped {
                path: skip.path.clone(),
                reason: skip.reason.clone(),
            });
        }
        for dir in &plan.dirs {
            let _ = sink.send(LogEvent::CreateDir(dir.clone()));
        }
    }

    if apply && !plans.is_empty() {
        let mut journal = Journal::open(&dest_root);
        for plan in &plans {
            if cancel.is_cancelled() {
                summary.cancelled = true;
                break;
            }
            journal.record(&format!(
                "RUN START dest={} moves={}",
                dest_root.display(),
                plan.moves.len()
            ));
            let exec = crate::exec::execute(plan, &RealFileSystem, cancel, &mut journal);
            journal.record(&format!(
                "RUN END moved={} failed={} cancelled={}",
                exec.moved,
                exec.failed.len(),
                exec.cancelled
            ));

            let failed_from: HashSet<&PathBuf> = exec.failed.iter().map(|f| &f.path).collect();
            for mv in &plan.moves {
                if !failed_from.contains(&mv.from) {
                    let _ = sink.send(LogEvent::Moved {
                        from: mv.from.clone(),
                        to: mv.to.clone(),
                    });
                }
            }
            for f in &exec.failed {
                let _ = sink.send(LogEvent::Failed {
                    path: f.path.clone(),
                    reason: f.reason.clone(),
                });
            }

            summary.moved += exec.moved;
            summary.failed += exec.failed.len();
            if exec.cancelled {
                summary.cancelled = true;
                break;
            }
        }
    }

    let _ = sink.send(LogEvent::Finished {
        moved: summary.moved,
        merged: summary.merged,
        skipped: summary.skipped,
        failed: summary.failed,
        cancelled: summary.cancelled,
    });

    Ok(summary)
}

/// A movie assignment and a TV assignment must never write the same
/// destination path. Plans are built independently (one per kind), so this
/// checks after the fact: any destination directory claimed by more than
/// one plan has every move under it turned into a skip in both, rather than
/// letting whichever plan executes second overwrite (or race with) the
/// other.
fn resolve_cross_plan_collisions(plans: &mut [Plan], sink: &Sender<LogEvent>) {
    if plans.len() < 2 {
        return;
    }

    let mut owner: HashMap<PathBuf, usize> = HashMap::new();
    let mut colliding: Vec<PathBuf> = Vec::new();
    for (i, plan) in plans.iter().enumerate() {
        for dir in &plan.dirs {
            let key = lower_path(dir);
            match owner.get(&key) {
                Some(&o) if o != i => {
                    if !colliding.contains(&key) {
                        colliding.push(key);
                    }
                }
                Some(_) => {}
                None => {
                    owner.insert(key, i);
                }
            }
        }
    }
    if colliding.is_empty() {
        return;
    }

    // Path-component-aware (not raw string) prefix check, so e.g. a
    // colliding "Foo (2020)" never wrongly swallows a sibling folder like
    // "Foo (2020) Extended" that merely shares a string prefix.
    let under_colliding = |p: &Path| {
        let key = lower_path(p);
        colliding.iter().any(|c| key.starts_with(c))
    };

    for plan in plans.iter_mut() {
        let mut kept = Vec::with_capacity(plan.moves.len());
        for mv in plan.moves.drain(..) {
            if under_colliding(&mv.to) {
                let reason = "destination claimed by both a movies and a TV assignment".to_string();
                let _ = sink.send(LogEvent::Skipped {
                    path: mv.from.clone(),
                    reason: reason.clone(),
                });
                plan.skips.push(Skip {
                    path: mv.from,
                    reason,
                });
            } else {
                kept.push(mv);
            }
        }
        plan.moves = kept;
        plan.dirs.retain(|d| !under_colliding(d));
    }
}

/// Case-folded `PathBuf` so path comparisons behave the same on
/// case-insensitive (Windows, macOS) and case-sensitive filesystems, while
/// still using `Path::starts_with`'s component-aware matching rather than a
/// raw string prefix.
fn lower_path(path: &Path) -> PathBuf {
    PathBuf::from(path.to_string_lossy().to_ascii_lowercase())
}

fn count_merged(outcome: &GroupOutcome) -> usize {
    match outcome {
        GroupOutcome::Movies(groups) => groups.iter().filter(|g| g.folders.len() > 1).count(),
        GroupOutcome::Tv(groups) => groups
            .iter()
            .filter(|g| g.seasons.values().map(|v| v.len()).sum::<usize>() > 1)
            .count(),
    }
}

fn absolute(path: &Path) -> std::io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn bail(sink: &Sender<LogEvent>, path: PathBuf, err: impl std::fmt::Display) -> Error {
    let reason = err.to_string();
    let _ = sink.send(LogEvent::Failed {
        path: path.clone(),
        reason: reason.clone(),
    });
    let _ = sink.send(LogEvent::Finished {
        moved: 0,
        merged: 0,
        skipped: 0,
        failed: 1,
        cancelled: false,
    });
    Error::Io(std::io::Error::other(reason))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc;

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_lib() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "media-manager-multi-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, b"").unwrap();
    }

    #[test]
    fn assigns_different_children_to_different_kinds_in_one_run() {
        let root = temp_lib();
        touch(&root.join("300 (2006) [1080p]/300.1080p.mkv"));
        touch(&root.join("The.Wire.S01.1080p.BluRay.x265-RARBG/the.wire.s01e01.mkv"));

        let (tx, rx) = mpsc::channel();
        let items = vec![
            WorkItem {
                path: root.join("300 (2006) [1080p]"),
                kind: LibraryKind::Movies,
            },
            WorkItem {
                path: root.join("The.Wire.S01.1080p.BluRay.x265-RARBG"),
                kind: LibraryKind::Tv,
            },
        ];
        let summary = run_items(&root, None, items, true, &CancelToken::new(), tx).unwrap();
        assert_eq!(summary.failed, 0);
        assert!(root.join("300 (2006)/300 (2006) - 1080p.mkv").is_file());
        assert!(root
            .join("The Wire/Season 01/The Wire S01E01.mkv")
            .is_file());
        // At least a Scanning event and a final Finished event were sent.
        let events: Vec<_> = rx.try_iter().collect();
        assert!(matches!(events.first(), Some(LogEvent::Scanning)));
        assert!(matches!(events.last(), Some(LogEvent::Finished { .. })));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn unassigned_children_are_left_untouched() {
        let root = temp_lib();
        touch(&root.join("300 (2006) [1080p]/300.1080p.mkv"));
        touch(&root.join("Untouched (2020)/video.mkv"));

        let (tx, _rx) = mpsc::channel();
        let items = vec![WorkItem {
            path: root.join("300 (2006) [1080p]"),
            kind: LibraryKind::Movies,
        }];
        run_items(&root, None, items, true, &CancelToken::new(), tx).unwrap();

        assert!(root.join("300 (2006)/300 (2006) - 1080p.mkv").is_file());
        assert!(root.join("Untouched (2020)/video.mkv").is_file());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn optional_dest_places_output_outside_the_source_root() {
        let root = temp_lib();
        let dest = temp_lib();
        touch(&root.join("300 (2006) [1080p]/300.1080p.mkv"));

        let (tx, _rx) = mpsc::channel();
        let items = vec![WorkItem {
            path: root.join("300 (2006) [1080p]"),
            kind: LibraryKind::Movies,
        }];
        run_items(&root, Some(&dest), items, true, &CancelToken::new(), tx).unwrap();

        assert!(dest.join("300 (2006)/300 (2006) - 1080p.mkv").is_file());
        assert!(!root.join("300 (2006) [1080p]").exists());
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&dest);
    }

    #[test]
    fn movie_and_tv_assignments_never_write_the_same_destination() {
        let root = temp_lib();
        // Same title/year, assigned as both a movie and a show: both would
        // want to create "Ambiguous (2020)/" at the dest root.
        touch(&root.join("Ambiguous (2020) [1080p]/movie.mkv"));
        touch(&root.join("Ambiguous.2020.S01/Ambiguous.S01E01.mkv"));

        let (tx, _rx) = mpsc::channel();
        let items = vec![
            WorkItem {
                path: root.join("Ambiguous (2020) [1080p]"),
                kind: LibraryKind::Movies,
            },
            WorkItem {
                path: root.join("Ambiguous.2020.S01"),
                kind: LibraryKind::Tv,
            },
        ];
        let summary = run_items(&root, None, items, true, &CancelToken::new(), tx).unwrap();

        assert!(summary.skipped >= 2);
        // Neither source moved: the shared dest folder name was dropped
        // from both plans rather than being handed to whichever ran first.
        assert!(root.join("Ambiguous (2020) [1080p]/movie.mkv").is_file());
        assert!(root
            .join("Ambiguous.2020.S01/Ambiguous.S01E01.mkv")
            .is_file());
        let _ = fs::remove_dir_all(&root);
    }
}
