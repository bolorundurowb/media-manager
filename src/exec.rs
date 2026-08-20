//! Apply a plan: create directories, rename files, remove emptied source folders.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::plan::{MoveOp, Plan, Skip};

#[derive(Debug, Default)]
pub struct ExecReport {
    pub created_dirs: usize,
    pub moved: usize,
    pub failed: Vec<Skip>,
    pub removed_dirs: usize,
}

pub fn execute(plan: &Plan) -> ExecReport {
    let mut report = ExecReport::default();

    for dir in &plan.dirs {
        match fs::create_dir_all(dir) {
            Ok(()) => {
                tracing::debug!(path = %dir.display(), "create dir");
                report.created_dirs += 1;
            }
            Err(err) => {
                tracing::error!(path = %dir.display(), error = %err, "failed to create directory");
                report.failed.push(Skip {
                    path: dir.clone(),
                    reason: format!("create dir failed: {err}"),
                });
            }
        }
    }

    if !report.failed.is_empty() {
        tracing::error!("aborting file moves because directory creation failed");
        return report;
    }

    for mv in &plan.moves {
        match move_no_overwrite(mv) {
            Ok(()) => {
                tracing::info!(from = %mv.from.display(), to = %mv.to.display(), "moved");
                report.moved += 1;
            }
            Err(err) => {
                tracing::error!(
                    from = %mv.from.display(),
                    to = %mv.to.display(),
                    error = %err,
                    "move failed"
                );
                report.failed.push(Skip {
                    path: mv.from.clone(),
                    reason: format!("move failed: {err}"),
                });
            }
        }
    }

    for folder in &plan.source_folders {
        match remove_if_empty(folder) {
            Ok(true) => {
                tracing::debug!(path = %folder.display(), "removed empty source folder");
                report.removed_dirs += 1;
            }
            Ok(false) => {}
            Err(err) => {
                tracing::warn!(path = %folder.display(), error = %err, "could not remove source folder");
            }
        }
    }

    report
}

fn move_no_overwrite(mv: &MoveOp) -> io::Result<()> {
    if mv.to.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("destination exists: {}", mv.to.display()),
        ));
    }
    if let Some(parent) = mv.to.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::rename(&mv.from, &mv.to)
}

fn remove_if_empty(dir: &Path) -> io::Result<bool> {
    if !dir.exists() {
        return Ok(false);
    }
    remove_empty_tree(dir)
}

fn remove_empty_tree(dir: &Path) -> io::Result<bool> {
    if !dir.is_dir() {
        return Ok(false);
    }
    let mut children: Vec<PathBuf> = Vec::new();
    for entry in fs::read_dir(dir)? {
        children.push(entry?.path());
    }
    for child in &children {
        if child.is_dir() {
            remove_empty_tree(child)?;
        }
    }
    let remaining = fs::read_dir(dir)?.next().is_none();
    if remaining {
        fs::remove_dir(dir)?;
        Ok(true)
    } else {
        Ok(false)
    }
}
