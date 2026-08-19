//! Filesystem abstraction (§2.5).
//!
//! Behind a `FileSystem` trait so that permission failures, read-only mounts,
//! `EXDEV`, and mid-operation failures (§22.5) are testable without root and
//! real network shares.

pub mod faulty;
pub mod mem;
pub mod real;

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::volume::{NoReplaceStrategy, VolumeSemantics};

/// A cancellation token — `Arc<AtomicBool>`, checked between operations and
/// inside `copy_into`'s loop (§6.5).
#[derive(Debug, Clone)]
pub struct CancelToken {
    inner: Arc<AtomicBool>,
}

impl Default for CancelToken {
    fn default() -> Self {
        CancelToken {
            inner: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self) {
        self.inner.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.load(Ordering::Relaxed)
    }

    /// `Err(Interrupted)` if cancelled.
    pub fn check(&self) -> io::Result<()> {
        if self.is_cancelled() {
            Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))
        } else {
            Ok(())
        }
    }
}

/// File metadata we care about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileMeta {
    pub is_dir: bool,
    pub is_symlink: bool,
    pub len: u64,
    pub modified: Option<SystemTime>,
    pub read_only: bool,
}

/// A filesystem-unique identifier: `(device, inode)` on Unix,
/// `GetFileInformationByHandleEx` volume+index on Windows.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileId {
    pub device: u64,
    pub inode: u64,
}

/// A content hash (blake3 by default).
#[derive(Debug, Clone, PartialEq, Eq, std::hash::Hash, Serialize, Deserialize)]
pub struct Hash(pub String);

/// One directory entry, materialised lazily by [`FileSystem::read_dir`].
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub path: PathBuf,
    pub file_name: OsString,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub len: u64,
}

/// Streaming directory iterator (§2.5 — not a `Vec`, the benchmark case is a
/// flat 100 000-file directory).
pub type ReadDirIter = Box<dyn Iterator<Item = io::Result<DirEntry>> + Send>;

/// The filesystem trait.
///
/// Implementations: [`real::RealFs`], [`mem::MemFs`], [`faulty::FaultyFs`].
pub trait FileSystem: Send + Sync {
    type Handle: Send;

    fn metadata(&self, p: &Path) -> io::Result<FileMeta>;
    fn symlink_metadata(&self, p: &Path) -> io::Result<FileMeta>;
    fn read_link(&self, p: &Path) -> io::Result<PathBuf>;
    fn file_id(&self, p: &Path) -> io::Result<FileId>;
    fn read_dir(&self, p: &Path) -> io::Result<ReadDirIter>;
    fn is_dir_empty(&self, p: &Path) -> io::Result<bool>;
    fn volume_semantics(&self, p: &Path) -> io::Result<VolumeSemantics>;
    fn create_dir_all(&self, p: &Path) -> io::Result<()>;
    fn rename_no_replace(&self, from: &Path, to: &Path) -> io::Result<()>;
    /// Replace-semantics rename: `to` is overwritten if it is an existing file.
    /// Used only by [`crate::config::ConflictPolicy::Replace`].
    fn rename_replace(&self, from: &Path, to: &Path) -> io::Result<()>;
    fn create_new(&self, p: &Path) -> io::Result<Self::Handle>;
    fn copy_into(
        &self,
        from: &Path,
        handle: &mut Self::Handle,
        cancel: &CancelToken,
    ) -> io::Result<u64>;
    fn sync_dir(&self, p: &Path) -> io::Result<()>;
    fn set_mtime(&self, p: &Path, t: SystemTime) -> io::Result<()>;
    fn remove_file(&self, p: &Path) -> io::Result<()>;
    fn remove_dir(&self, p: &Path) -> io::Result<()>;
    fn hash(&self, p: &Path, cancel: &CancelToken) -> io::Result<Hash>;

    /// The configured/observed no-replace strategy for a path (§2.5).
    fn no_replace_strategy(&self, _p: &Path) -> NoReplaceStrategy {
        // default: native where a rename primitive exists; the implementation
        // overrides on detected volumes.
        NoReplaceStrategy::Native
    }
}

/// `true` if this error indicates a cross-device link/rename (EXDEV).
pub fn is_cross_device(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::CrossesDevices)
        || e.raw_os_error() == Some(18) // Unix EXDEV
        || (cfg!(windows) && e.raw_os_error() == Some(17)) // ERROR_NOT_SAME_DEVICE
}

/// Normalise an occupied-target error to a single [`io::Error`]. §2.5: on Unix
/// `O_EXCL` against a directory gives `EEXIST` → `AlreadyExists`; on Windows
/// `CREATE_NEW` against a directory gives `ERROR_ACCESS_DENIED`. Both must map
/// to a destination-occupied error, else the Windows case is misreported as a
/// permission failure and routed to `Failure` instead of `Conflict` (§14).
pub fn destination_occupied_error(e: &io::Error) -> bool {
    if is_cross_device(e) {
        return false;
    }
    matches!(e.kind(), io::ErrorKind::AlreadyExists)
        || e.raw_os_error() == Some(17) /* EEXIST */
        || e.raw_os_error() == Some(5) /* ERROR_ACCESS_DENIED */
        || e.raw_os_error() == Some(80) /* ERROR_FILE_EXISTS */
        || e.raw_os_error() == Some(183) /* ERROR_ALREADY_EXISTS */
}
