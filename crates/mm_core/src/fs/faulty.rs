//! `FaultyFs<F>` — fault injection (§22.5).
//!
//! Wraps any [`FileSystem`] and injects `PermissionDenied`,
//! `ReadOnlyFilesystem`, `CrossesDevices`, or a hard failure at the *n*th call.
//! This is what makes spec §22.5 testable in CI: assertions that no source
//! file is ever lost, and that only §14's `Fatal` set aborts a run.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use crate::fs::FileSystem;
use crate::fs::{CancelToken, FileId, FileMeta, Hash, ReadDirIter};
use crate::volume::VolumeSemantics;

/// Which method to inject a fault into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method {
    Metadata,
    SymlinkMetadata,
    FileId,
    ReadDir,
    IsDirEmpty,
    CreateDirAll,
    RenameNoReplace,
    CreateNew,
    CopyInto,
    SyncDir,
    SetMtime,
    RemoveFile,
    RemoveDir,
    Hash,
}

/// The kind of error to inject.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InjectErr {
    PermissionDenied,
    ReadOnlyFilesystem,
    CrossesDevices,
    NotFound,
    AlreadyExists,
    StorageFull,
    Other,
}

impl InjectErr {
    fn into_io(self) -> io::Error {
        match self {
            InjectErr::PermissionDenied => io::Error::from(io::ErrorKind::PermissionDenied),
            InjectErr::ReadOnlyFilesystem => io::Error::from(io::ErrorKind::ReadOnlyFilesystem),
            InjectErr::CrossesDevices => io::Error::from(io::ErrorKind::CrossesDevices),
            InjectErr::NotFound => io::Error::from(io::ErrorKind::NotFound),
            InjectErr::AlreadyExists => io::Error::from(io::ErrorKind::AlreadyExists),
            InjectErr::StorageFull => io::Error::from(io::ErrorKind::StorageFull),
            InjectErr::Other => io::Error::from(io::ErrorKind::Other),
        }
    }
}

/// One injected fault: the `call_index`th call to `method` returns `err`.
#[derive(Debug, Clone, Copy)]
pub struct Fault {
    pub call_index: u64,
    pub method: Method,
    pub err: InjectErr,
}

/// A wrapper that injects faults into an underlying filesystem.
pub struct FaultyFs<F: FileSystem> {
    inner: F,
    faults: Mutex<Vec<Fault>>,
    counter: AtomicU64,
    /// Fail the *n*th operation unconditionally (hard failure at op *n*).
    hard_fail_at: Mutex<Option<u64>>,
}

impl<F: FileSystem> FaultyFs<F> {
    pub fn new(inner: F) -> Self {
        FaultyFs {
            inner,
            faults: Mutex::new(Vec::new()),
            counter: AtomicU64::new(0),
            hard_fail_at: Mutex::new(None),
        }
    }

    pub fn with_faults(inner: F, faults: Vec<Fault>) -> Self {
        FaultyFs {
            inner,
            faults: Mutex::new(faults),
            counter: AtomicU64::new(0),
            hard_fail_at: Mutex::new(None),
        }
    }

    /// Make the *n*th operation (any method) fail hard.
    pub fn fail_at_nth(self, n: u64) -> Self {
        *self.hard_fail_at.lock().unwrap() = Some(n);
        self
    }

    fn tick(&self) -> u64 {
        self.counter.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// `Err(e)` if a fault fires for `method` at the current call index.
    fn check(&self, method: Method) -> Option<io::Error> {
        let idx = self.tick();
        if let Some(n) = *self.hard_fail_at.lock().unwrap() {
            if idx == n {
                return Some(io::Error::other("hard fail at op n"));
            }
        }
        let faults = self.faults.lock().unwrap();
        for f in faults.iter() {
            if f.call_index == idx && f.method == method {
                return Some(f.err.into_io());
            }
        }
        None
    }
}

impl<F: FileSystem> FileSystem for FaultyFs<F> {
    type Handle = F::Handle;

    fn metadata(&self, p: &Path) -> io::Result<FileMeta> {
        if let Some(e) = self.check(Method::Metadata) {
            return Err(e);
        }
        self.inner.metadata(p)
    }
    fn symlink_metadata(&self, p: &Path) -> io::Result<FileMeta> {
        if let Some(e) = self.check(Method::SymlinkMetadata) {
            return Err(e);
        }
        self.inner.symlink_metadata(p)
    }
    fn read_link(&self, p: &Path) -> io::Result<PathBuf> {
        self.inner.read_link(p)
    }
    fn file_id(&self, p: &Path) -> io::Result<FileId> {
        if let Some(e) = self.check(Method::FileId) {
            return Err(e);
        }
        self.inner.file_id(p)
    }
    fn read_dir(&self, p: &Path) -> io::Result<ReadDirIter> {
        if let Some(e) = self.check(Method::ReadDir) {
            return Err(e);
        }
        self.inner.read_dir(p)
    }
    fn is_dir_empty(&self, p: &Path) -> io::Result<bool> {
        if let Some(e) = self.check(Method::IsDirEmpty) {
            return Err(e);
        }
        self.inner.is_dir_empty(p)
    }
    fn volume_semantics(&self, p: &Path) -> io::Result<VolumeSemantics> {
        self.inner.volume_semantics(p)
    }
    fn create_dir_all(&self, p: &Path) -> io::Result<()> {
        if let Some(e) = self.check(Method::CreateDirAll) {
            return Err(e);
        }
        self.inner.create_dir_all(p)
    }
    fn rename_no_replace(&self, from: &Path, to: &Path) -> io::Result<()> {
        if let Some(e) = self.check(Method::RenameNoReplace) {
            return Err(e);
        }
        self.inner.rename_no_replace(from, to)
    }
    fn create_new(&self, p: &Path) -> io::Result<Self::Handle> {
        if let Some(e) = self.check(Method::CreateNew) {
            return Err(e);
        }
        self.inner.create_new(p)
    }
    fn copy_into(
        &self,
        from: &Path,
        handle: &mut Self::Handle,
        cancel: &CancelToken,
    ) -> io::Result<u64> {
        if let Some(e) = self.check(Method::CopyInto) {
            return Err(e);
        }
        self.inner.copy_into(from, handle, cancel)
    }
    fn sync_dir(&self, p: &Path) -> io::Result<()> {
        if let Some(e) = self.check(Method::SyncDir) {
            return Err(e);
        }
        self.inner.sync_dir(p)
    }
    fn set_mtime(&self, p: &Path, t: SystemTime) -> io::Result<()> {
        if let Some(e) = self.check(Method::SetMtime) {
            return Err(e);
        }
        self.inner.set_mtime(p, t)
    }
    fn remove_file(&self, p: &Path) -> io::Result<()> {
        if let Some(e) = self.check(Method::RemoveFile) {
            return Err(e);
        }
        self.inner.remove_file(p)
    }
    fn remove_dir(&self, p: &Path) -> io::Result<()> {
        if let Some(e) = self.check(Method::RemoveDir) {
            return Err(e);
        }
        self.inner.remove_dir(p)
    }
    fn hash(&self, p: &Path, cancel: &CancelToken) -> io::Result<Hash> {
        if let Some(e) = self.check(Method::Hash) {
            return Err(e);
        }
        self.inner.hash(p, cancel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::real::RealFs;
    use tempfile::TempDir;

    #[test]
    fn injects_permission_denied_on_nth_call() {
        let tmp = TempDir::new().unwrap();
        let real = RealFs::new();
        let fs = FaultyFs::with_faults(
            real,
            vec![Fault {
                call_index: 1,
                method: Method::RemoveFile,
                err: InjectErr::PermissionDenied,
            }],
        );
        let p = tmp.path().join("x");
        std::fs::write(&p, b"hi").unwrap();
        let err = fs.remove_file(&p).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        // second call succeeds
        fs.remove_file(&p).unwrap();
    }
}
