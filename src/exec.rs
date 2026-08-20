//! Apply a plan: create directories, rename files, remove emptied source folders.
//!
//! Every step goes through a `FileSystem` (real disk for the CLI, an
//! in-memory + fault-injecting backend in tests) and is checked against a
//! `CancelToken` before it starts, so a Ctrl+C (or a future GUI Stop button)
//! never interrupts a rename that has already begun — it only stops the next
//! one from starting. Nothing already moved is ever rolled back. Every
//! attempt is also appended to the run's `Journal` for crash diagnosis.

use std::collections::HashSet;
use std::path::Path;

use crate::cancel::CancelToken;
use crate::journal::Journal;
use crate::plan::{MoveOp, Plan, Skip};
use crate::vfs::FileSystem;

#[derive(Debug, Default)]
pub struct ExecReport {
    pub created_dirs: usize,
    pub moved: usize,
    pub failed: Vec<Skip>,
    pub removed_dirs: usize,
    /// Set when cancellation was requested before the plan finished. Items
    /// not yet started are simply absent from `moved`/`failed`.
    pub cancelled: bool,
}

pub fn execute(
    plan: &Plan,
    fs: &dyn FileSystem,
    cancel: &CancelToken,
    journal: &mut Journal,
) -> ExecReport {
    let mut report = ExecReport::default();

    let mut failed_dirs: HashSet<std::path::PathBuf> = HashSet::new();

    for dir in &plan.dirs {
        if cancel.is_cancelled() {
            tracing::warn!("cancelled before all directories were created");
            journal.record("CANCELLED before directory creation finished");
            report.cancelled = true;
            return report;
        }
        journal.record(&format!("CREATE_DIR START {}", dir.display()));
        match fs.create_dir_all(dir) {
            Ok(()) => {
                tracing::debug!(path = %dir.display(), "create dir");
                journal.record(&format!("CREATE_DIR OK {}", dir.display()));
                report.created_dirs += 1;
            }
            Err(err) => {
                tracing::error!(path = %dir.display(), error = %err, "failed to create directory");
                journal.record(&format!("CREATE_DIR FAIL {} ({err})", dir.display()));
                failed_dirs.insert(dir.clone());
                report.failed.push(Skip {
                    path: dir.clone(),
                    reason: format!("create dir failed: {err}"),
                });
            }
        }
    }

    let mut moves_started = 0usize;
    for mv in &plan.moves {
        if cancel.is_cancelled() {
            let remaining = plan.moves.len().saturating_sub(moves_started);
            tracing::warn!(remaining, "cancelled; not starting further moves");
            journal.record("CANCELLED before all moves finished");
            report.cancelled = true;
            return report;
        }
        if failed_dirs.iter().any(|d| mv.to.starts_with(d)) {
            tracing::warn!(
                from = %mv.from.display(),
                to = %mv.to.display(),
                "skipping move; destination directory was not created"
            );
            journal.record(&format!(
                "MOVE SKIP {} -> {} (dest dir was not created)",
                mv.from.display(),
                mv.to.display()
            ));
            report.failed.push(Skip {
                path: mv.from.clone(),
                reason: format!(
                    "destination directory was not created: {}",
                    mv.to.display()
                ),
            });
            moves_started += 1;
            continue;
        }
        journal.record(&format!(
            "MOVE START {} -> {}",
            mv.from.display(),
            mv.to.display()
        ));
        match move_no_overwrite(fs, mv) {
            Ok(()) => {
                tracing::info!(from = %mv.from.display(), to = %mv.to.display(), "moved");
                journal.record(&format!(
                    "MOVE OK {} -> {}",
                    mv.from.display(),
                    mv.to.display()
                ));
                report.moved += 1;
            }
            Err(err) => {
                tracing::error!(
                    from = %mv.from.display(),
                    to = %mv.to.display(),
                    error = %err,
                    "move failed"
                );
                journal.record(&format!(
                    "MOVE FAIL {} -> {} ({err})",
                    mv.from.display(),
                    mv.to.display()
                ));
                report.failed.push(Skip {
                    path: mv.from.clone(),
                    reason: format!("move failed: {err}"),
                });
            }
        }
        moves_started += 1;
    }

    for folder in &plan.source_folders {
        if cancel.is_cancelled() {
            tracing::warn!("cancelled before all emptied source folders were removed");
            journal.record("CANCELLED before source-folder cleanup finished");
            report.cancelled = true;
            return report;
        }
        match remove_if_empty(fs, folder) {
            Ok(true) => {
                tracing::debug!(path = %folder.display(), "removed empty source folder");
                journal.record(&format!("REMOVE_DIR OK {}", folder.display()));
                report.removed_dirs += 1;
            }
            Ok(false) => {}
            Err(err) => {
                tracing::warn!(path = %folder.display(), error = %err, "could not remove source folder");
                journal.record(&format!("REMOVE_DIR FAIL {} ({err})", folder.display()));
            }
        }
    }

    report
}

fn move_no_overwrite(fs: &dyn FileSystem, mv: &MoveOp) -> std::io::Result<()> {
    fs.rename_no_replace(&mv.from, &mv.to)
}

fn remove_if_empty(fs: &dyn FileSystem, dir: &Path) -> std::io::Result<bool> {
    if !fs.exists(dir) {
        return Ok(false);
    }
    remove_empty_tree(fs, dir)
}

fn remove_empty_tree(fs: &dyn FileSystem, dir: &Path) -> std::io::Result<bool> {
    if !fs.is_dir(dir) {
        return Ok(false);
    }
    let children = fs.read_dir(dir)?;
    for child in &children {
        if fs.is_dir(child) {
            remove_empty_tree(fs, child)?;
        }
    }
    let remaining = fs.read_dir(dir)?.is_empty();
    if remaining {
        fs.remove_empty_dir(dir)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::{FaultyFileSystem, InMemoryFileSystem};
    use std::path::PathBuf;

    fn plan_with(dirs: Vec<&str>, moves: Vec<(&str, &str)>, source_folders: Vec<&str>) -> Plan {
        Plan {
            dirs: dirs.into_iter().map(PathBuf::from).collect(),
            moves: moves
                .into_iter()
                .map(|(from, to)| MoveOp {
                    from: PathBuf::from(from),
                    to: PathBuf::from(to),
                })
                .collect(),
            skips: Vec::new(),
            source_folders: source_folders.into_iter().map(PathBuf::from).collect(),
        }
    }

    #[test]
    fn happy_path_moves_and_cleans_up() {
        let fs = InMemoryFileSystem::new().with_file("/root/src/a.mkv");
        let plan = plan_with(
            vec!["/root/dest"],
            vec![("/root/src/a.mkv", "/root/dest/a.mkv")],
            vec!["/root/src"],
        );
        let report = execute(&plan, &fs, &CancelToken::new(), &mut Journal::disabled());
        assert_eq!(report.moved, 1);
        assert!(report.failed.is_empty());
        assert_eq!(report.removed_dirs, 1);
        assert!(!report.cancelled);
        assert!(fs.exists(Path::new("/root/dest/a.mkv")));
        assert!(!fs.exists(Path::new("/root/src/a.mkv")));
    }

    #[test]
    fn destination_collision_is_reported_as_failed_not_overwritten() {
        let fs = InMemoryFileSystem::new()
            .with_file("/root/src/a.mkv")
            .with_file("/root/dest/a.mkv");
        let plan = plan_with(
            vec!["/root/dest"],
            vec![("/root/src/a.mkv", "/root/dest/a.mkv")],
            vec!["/root/src"],
        );
        let report = execute(&plan, &fs, &CancelToken::new(), &mut Journal::disabled());
        assert_eq!(report.moved, 0);
        assert_eq!(report.failed.len(), 1);
        // The original destination file must be untouched.
        assert!(fs.exists(Path::new("/root/dest/a.mkv")));
        assert!(fs.exists(Path::new("/root/src/a.mkv")));
    }

    #[test]
    fn inaccessible_destination_dir_skips_only_moves_into_it() {
        let fs = FaultyFileSystem::new(InMemoryFileSystem::new().with_file("/root/src/a.mkv"))
            .fail_create_dir("/root/dest");
        let plan = plan_with(
            vec!["/root/dest"],
            vec![("/root/src/a.mkv", "/root/dest/a.mkv")],
            vec!["/root/src"],
        );
        let report = execute(&plan, &fs, &CancelToken::new(), &mut Journal::disabled());
        assert_eq!(report.created_dirs, 0);
        assert_eq!(
            report.moved, 0,
            "moves into a dest dir that failed to create must not run"
        );
        assert!(report.failed.len() >= 2, "dir failure and skipped move");
        assert!(fs.exists(Path::new("/root/src/a.mkv")));
    }

    #[test]
    fn failed_dest_dir_does_not_block_unrelated_moves() {
        let fs = FaultyFileSystem::new(
            InMemoryFileSystem::new()
                .with_file("/root/src_a/a.mkv")
                .with_file("/root/src_b/b.mkv"),
        )
        .fail_create_dir("/root/dest_a");
        let plan = plan_with(
            vec!["/root/dest_a", "/root/dest_b"],
            vec![
                ("/root/src_a/a.mkv", "/root/dest_a/a.mkv"),
                ("/root/src_b/b.mkv", "/root/dest_b/b.mkv"),
            ],
            vec!["/root/src_a", "/root/src_b"],
        );
        let report = execute(&plan, &fs, &CancelToken::new(), &mut Journal::disabled());
        assert_eq!(report.moved, 1);
        assert!(fs.exists(Path::new("/root/dest_b/b.mkv")));
        assert!(fs.exists(Path::new("/root/src_a/a.mkv")));
        assert!(!fs.exists(Path::new("/root/src_b/b.mkv")));
    }

    #[test]
    fn mid_batch_rename_failure_does_not_stop_the_rest() {
        let fs = FaultyFileSystem::new(
            InMemoryFileSystem::new()
                .with_file("/root/src/a.mkv")
                .with_file("/root/src/b.mkv"),
        )
        .fail_rename_from("/root/src/a.mkv");
        let plan = plan_with(
            vec!["/root/dest"],
            vec![
                ("/root/src/a.mkv", "/root/dest/a.mkv"),
                ("/root/src/b.mkv", "/root/dest/b.mkv"),
            ],
            vec!["/root/src"],
        );
        let report = execute(&plan, &fs, &CancelToken::new(), &mut Journal::disabled());
        assert_eq!(report.moved, 1);
        assert_eq!(report.failed.len(), 1);
        assert!(fs.exists(Path::new("/root/dest/b.mkv")));
        assert!(fs.exists(Path::new("/root/src/a.mkv")));
    }

    #[test]
    fn cancellation_before_moves_starts_none_of_them() {
        let fs = InMemoryFileSystem::new()
            .with_file("/root/src/a.mkv")
            .with_file("/root/src/b.mkv");
        let plan = plan_with(
            vec![],
            vec![
                ("/root/src/a.mkv", "/root/dest/a.mkv"),
                ("/root/src/b.mkv", "/root/dest/b.mkv"),
            ],
            vec!["/root/src"],
        );
        let cancel = CancelToken::new();
        cancel.cancel();
        let report = execute(&plan, &fs, &cancel, &mut Journal::disabled());
        assert!(report.cancelled);
        assert_eq!(report.moved, 0);
        assert_eq!(report.failed.len(), 0);
        assert!(fs.exists(Path::new("/root/src/a.mkv")));
        assert!(fs.exists(Path::new("/root/src/b.mkv")));
    }

    #[test]
    fn never_removes_a_source_folder_that_still_has_a_skipped_file() {
        let fs = InMemoryFileSystem::new()
            .with_file("/root/src/a.mkv")
            .with_file("/root/src/leftover.nfo");
        let plan = plan_with(
            vec!["/root/dest"],
            vec![("/root/src/a.mkv", "/root/dest/a.mkv")],
            vec!["/root/src"],
        );
        let report = execute(&plan, &fs, &CancelToken::new(), &mut Journal::disabled());
        assert_eq!(report.moved, 1);
        assert_eq!(report.removed_dirs, 0);
        assert!(fs.exists(Path::new("/root/src/leftover.nfo")));
    }
}
