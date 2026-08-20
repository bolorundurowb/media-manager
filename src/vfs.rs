//! A small filesystem seam so `exec` can be tested without touching real
//! disk: real IO in production, an in-memory backend plus a fault-injecting
//! wrapper in tests (dest collisions, mid-rename failures, inaccessible
//! paths, etc.).

use std::cell::RefCell;
use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

/// Everything `exec` needs from a filesystem: list children, check existence
/// and kind, create a directory (and its ancestors), rename without ever
/// clobbering an existing destination, and remove a directory that must
/// already be empty.
pub trait FileSystem {
    fn read_dir(&self, dir: &Path) -> io::Result<Vec<PathBuf>>;
    fn exists(&self, path: &Path) -> bool;
    fn is_dir(&self, path: &Path) -> bool;
    fn create_dir_all(&self, path: &Path) -> io::Result<()>;
    fn rename_no_replace(&self, from: &Path, to: &Path) -> io::Result<()>;
    fn remove_empty_dir(&self, path: &Path) -> io::Result<()>;
}

/// The real filesystem, used by the CLI.
pub struct RealFileSystem;

impl FileSystem for RealFileSystem {
    fn read_dir(&self, dir: &Path) -> io::Result<Vec<PathBuf>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            out.push(entry?.path());
        }
        Ok(out)
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn is_dir(&self, path: &Path) -> bool {
        path.is_dir()
    }

    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        std::fs::create_dir_all(path)
    }

    fn rename_no_replace(&self, from: &Path, to: &Path) -> io::Result<()> {
        crate::os_rename::rename_no_replace(from, to)
    }

    fn remove_empty_dir(&self, path: &Path) -> io::Result<()> {
        std::fs::remove_dir(path)
    }
}

#[derive(Default)]
struct InMemoryState {
    dirs: HashSet<PathBuf>,
    files: HashSet<PathBuf>,
}

/// An in-memory filesystem for fast, deterministic tests. Paths are
/// arbitrary `PathBuf`s (they never need to point at anything real);
/// `with_dir` / `with_file` seed initial state, including ancestor
/// directories.
#[derive(Default)]
pub struct InMemoryFileSystem {
    state: RefCell<InMemoryState>,
}

impl InMemoryFileSystem {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_dir(self, path: impl Into<PathBuf>) -> Self {
        self.ensure_dir_and_ancestors(&path.into());
        self
    }

    pub fn with_file(self, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        if let Some(parent) = path.parent() {
            self.ensure_dir_and_ancestors(parent);
        }
        self.state.borrow_mut().files.insert(path);
        self
    }

    fn ensure_dir_and_ancestors(&self, path: &Path) {
        let mut chain = Vec::new();
        let mut cur = Some(path);
        while let Some(p) = cur {
            chain.push(p.to_path_buf());
            cur = p.parent();
        }
        let mut state = self.state.borrow_mut();
        for p in chain {
            state.dirs.insert(p);
        }
    }
}

impl FileSystem for InMemoryFileSystem {
    fn read_dir(&self, dir: &Path) -> io::Result<Vec<PathBuf>> {
        let state = self.state.borrow();
        if !state.dirs.contains(dir) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("no such directory: {}", dir.display()),
            ));
        }
        let mut out: Vec<PathBuf> = state
            .dirs
            .iter()
            .chain(state.files.iter())
            .filter(|p| p.parent() == Some(dir))
            .cloned()
            .collect();
        out.sort();
        out.dedup();
        Ok(out)
    }

    fn exists(&self, path: &Path) -> bool {
        let state = self.state.borrow();
        state.dirs.contains(path) || state.files.contains(path)
    }

    fn is_dir(&self, path: &Path) -> bool {
        self.state.borrow().dirs.contains(path)
    }

    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        self.ensure_dir_and_ancestors(path);
        Ok(())
    }

    fn rename_no_replace(&self, from: &Path, to: &Path) -> io::Result<()> {
        {
            let state = self.state.borrow();
            if state.dirs.contains(to) || state.files.contains(to) {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("destination exists: {}", to.display()),
                ));
            }
        }
        if let Some(parent) = to.parent() {
            self.ensure_dir_and_ancestors(parent);
        }
        let mut state = self.state.borrow_mut();
        if state.files.remove(from) {
            state.files.insert(to.to_path_buf());
            Ok(())
        } else if state.dirs.remove(from) {
            state.dirs.insert(to.to_path_buf());
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("source missing: {}", from.display()),
            ))
        }
    }

    fn remove_empty_dir(&self, path: &Path) -> io::Result<()> {
        let mut state = self.state.borrow_mut();
        if !state.dirs.contains(path) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("not a directory: {}", path.display()),
            ));
        }
        let has_children = state.dirs.iter().any(|p| p.parent() == Some(path))
            || state.files.iter().any(|p| p.parent() == Some(path));
        if has_children {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("directory not empty: {}", path.display()),
            ));
        }
        state.dirs.remove(path);
        Ok(())
    }
}

/// Which operation a fault applies to, keyed by the path passed to it (for
/// `rename_no_replace` this matches against `from`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum FaultOp {
    ReadDir,
    CreateDir,
    Rename,
    RemoveDir,
}

/// Wraps another `FileSystem` and fails specific operations on specific
/// paths, so tests can simulate permission errors, inaccessible
/// directories, or a rename that fails partway through a batch.
pub struct FaultyFileSystem<F> {
    inner: F,
    faults: HashSet<(FaultOp, PathBuf)>,
}

impl<F: FileSystem> FaultyFileSystem<F> {
    pub fn new(inner: F) -> Self {
        FaultyFileSystem {
            inner,
            faults: HashSet::new(),
        }
    }

    pub fn fail_read_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.faults.insert((FaultOp::ReadDir, path.into()));
        self
    }

    pub fn fail_create_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.faults.insert((FaultOp::CreateDir, path.into()));
        self
    }

    /// Fail a rename whose *source* is `path`.
    pub fn fail_rename_from(mut self, path: impl Into<PathBuf>) -> Self {
        self.faults.insert((FaultOp::Rename, path.into()));
        self
    }

    pub fn fail_remove_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.faults.insert((FaultOp::RemoveDir, path.into()));
        self
    }

    fn faulted(&self, op: FaultOp, path: &Path) -> bool {
        self.faults.contains(&(op, path.to_path_buf()))
    }
}

fn injected_fault(op: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("injected fault: {op}"),
    )
}

impl<F: FileSystem> FileSystem for FaultyFileSystem<F> {
    fn read_dir(&self, dir: &Path) -> io::Result<Vec<PathBuf>> {
        if self.faulted(FaultOp::ReadDir, dir) {
            return Err(injected_fault("read_dir"));
        }
        self.inner.read_dir(dir)
    }

    fn exists(&self, path: &Path) -> bool {
        self.inner.exists(path)
    }

    fn is_dir(&self, path: &Path) -> bool {
        self.inner.is_dir(path)
    }

    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        if self.faulted(FaultOp::CreateDir, path) {
            return Err(injected_fault("create_dir_all"));
        }
        self.inner.create_dir_all(path)
    }

    fn rename_no_replace(&self, from: &Path, to: &Path) -> io::Result<()> {
        if self.faulted(FaultOp::Rename, from) {
            return Err(injected_fault("rename"));
        }
        self.inner.rename_no_replace(from, to)
    }

    fn remove_empty_dir(&self, path: &Path) -> io::Result<()> {
        if self.faulted(FaultOp::RemoveDir, path) {
            return Err(injected_fault("remove_empty_dir"));
        }
        self.inner.remove_empty_dir(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_rename_moves_file_and_updates_listing() {
        let fs = InMemoryFileSystem::new().with_file("/root/a/video.mkv");
        assert!(fs.exists(Path::new("/root/a/video.mkv")));
        fs.rename_no_replace(Path::new("/root/a/video.mkv"), Path::new("/root/b/video.mkv"))
            .unwrap();
        assert!(!fs.exists(Path::new("/root/a/video.mkv")));
        assert!(fs.exists(Path::new("/root/b/video.mkv")));
        let listing = fs.read_dir(Path::new("/root/b")).unwrap();
        assert_eq!(listing, vec![PathBuf::from("/root/b/video.mkv")]);
    }

    #[test]
    fn in_memory_rename_refuses_to_overwrite() {
        let fs = InMemoryFileSystem::new()
            .with_file("/root/a.mkv")
            .with_file("/root/b.mkv");
        let err = fs
            .rename_no_replace(Path::new("/root/a.mkv"), Path::new("/root/b.mkv"))
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn in_memory_remove_empty_dir_requires_empty() {
        let fs = InMemoryFileSystem::new().with_file("/root/a/video.mkv");
        assert!(fs.remove_empty_dir(Path::new("/root/a")).is_err());
        fs.rename_no_replace(Path::new("/root/a/video.mkv"), Path::new("/root/video.mkv"))
            .unwrap();
        assert!(fs.remove_empty_dir(Path::new("/root/a")).is_ok());
    }

    #[test]
    fn faulty_filesystem_injects_requested_failure_only() {
        let fs = FaultyFileSystem::new(InMemoryFileSystem::new().with_file("/root/a.mkv"))
            .fail_rename_from("/root/a.mkv");
        let err = fs
            .rename_no_replace(Path::new("/root/a.mkv"), Path::new("/root/b.mkv"))
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        // Unrelated operations still work.
        assert!(fs.create_dir_all(Path::new("/root/c")).is_ok());
    }
}
