//! Append-only, best-effort journal written during `--apply` runs so a crash
//! or kill mid-run can be diagnosed afterwards.
//!
//! The journal is plain text, one line per event, timestamped with Unix
//! seconds. It is diagnostic only: nothing ever reads it back to decide what
//! to do, and a failure to write to it never aborts the run.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct Journal {
    file: Mutex<Option<File>>,
    path: PathBuf,
}

impl Journal {
    /// Open (creating/appending) the journal file for a run rooted at
    /// `root`. If the file cannot be opened, the journal silently becomes a
    /// no-op rather than failing the run.
    pub fn open(root: &Path) -> Journal {
        let path = root.join(".media-manager-journal.log");
        let file = match OpenOptions::new().create(true).append(true).open(&path) {
            Ok(f) => Some(f),
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "could not open journal file; continuing without a journal"
                );
                None
            }
        };
        Journal {
            file: Mutex::new(file),
            path,
        }
    }

    /// A journal that never writes anywhere; used for pure in-memory tests.
    #[cfg(test)]
    pub fn disabled() -> Journal {
        Journal {
            file: Mutex::new(None),
            path: PathBuf::new(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn record(&self, line: &str) {
        let mut guard = match self.file.lock() {
            Ok(guard) => guard,
            Err(_) => {
                tracing::warn!("journal lock poisoned; continuing without a journal");
                return;
            }
        };
        let Some(file) = guard.as_mut() else {
            return;
        };
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if let Err(err) = writeln!(file, "{ts} {line}") {
            tracing::warn!(error = %err, "failed to write journal entry");
            return;
        }
        let _ = file.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "media-manager-journal-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn writes_lines_to_disk() {
        let root = temp_dir();
        let journal = Journal::open(&root);
        journal.record("RUN START");
        journal.record("RUN END");
        let contents = fs::read_to_string(journal.path()).unwrap();
        assert!(contents.contains("RUN START"));
        assert!(contents.contains("RUN END"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn disabled_journal_is_a_no_op() {
        let journal = Journal::disabled();
        journal.record("should not panic or write anywhere");
        assert_eq!(journal.path(), Path::new(""));
    }
}
