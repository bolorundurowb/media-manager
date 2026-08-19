//! `MemFs` — a fast in-memory filesystem for unit tests.
//!
//! Not a benchmark target; correct behaviour is what matters. Implements the
//! full [`FileSystem`] contract so planning/execution logic is testable without
//! touching disk.

use std::collections::{BTreeSet, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use crate::fs::{CancelToken, DirEntry, FileId, FileMeta, Hash, ReadDirIter};
use crate::fs::FileSystem;
use crate::volume::VolumeSemantics;

#[derive(Debug, Clone)]
enum Node {
    File {
        data: Vec<u8>,
        mtime: SystemTime,
        read_only: bool,
    },
    Dir {
        entries: BTreeSet<String>,
    },
    // Not yet constructed by any `MemFs` method — reserved for the §5.1
    // symlink-policy tests (`skip` / `follow` / `treat_as_file`), which are
    // not yet exercised against `MemFs`.
    #[allow(dead_code)]
    Symlink {
        target: PathBuf,
    },
}

/// Handle returned by [`MemFs::create_new`]: the path to write into.
#[derive(Debug, Clone)]
pub struct MemHandle {
    path: PathBuf,
}

/// An in-memory filesystem.
#[derive(Debug, Default)]
pub struct MemFs {
    nodes: Mutex<HashMap<PathBuf, Node>>,
    next_id: Mutex<u64>,
    volume: VolumeSemantics,
}

impl MemFs {
    pub fn new() -> Self {
        Self {
            nodes: Mutex::new(HashMap::new()),
            next_id: Mutex::new(1),
            volume: VolumeSemantics::conservative(),
        }
    }

    pub fn with_volume(volume: VolumeSemantics) -> Self {
        let me = Self::new();
        // SAFETY: we hold the only reference after construction.
        let mut me = me;
        me.volume = volume;
        me
    }

    /// Seed a file with the given contents at `path`.
    pub fn seed_file(&self, path: impl Into<PathBuf>, data: impl Into<Vec<u8>>) {
        let path = path.into();
        self.ensure_parent(&path);
        let mut nodes = self.nodes.lock().unwrap();
        // register in parent's entry list
        if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
            let parent = path.parent().unwrap_or_else(|| Path::new("")).to_path_buf();
            if let Some(Node::Dir { entries }) = nodes.get_mut(&parent) {
                entries.insert(name.to_string());
            }
        }
        nodes.insert(
            path.clone(),
            Node::File {
                data: data.into(),
                mtime: SystemTime::now(),
                read_only: false,
            },
        );
    }

    /// Seed an empty directory.
    pub fn seed_dir(&self, path: impl Into<PathBuf>) {
        let path = path.into();
        self.ensure_parent(&path);
        let mut nodes = self.nodes.lock().unwrap();
        if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
            let parent = path.parent().unwrap_or_else(|| Path::new("")).to_path_buf();
            if let Some(Node::Dir { entries }) = nodes.get_mut(&parent) {
                entries.insert(name.to_string());
            }
        }
        nodes
            .entry(path)
            .or_insert_with(|| Node::Dir {
                entries: BTreeSet::new(),
            });
    }

    fn ensure_parent(&self, path: &Path) {
        let mut nodes = self.nodes.lock().unwrap();
        let mut acc = PathBuf::new();
        for comp in path.parent().and_then(|p| p.iter().next()).into_iter().flat_map(|_| {
            // iterate over all ancestor components
            path.parent().unwrap_or_else(|| Path::new("")).iter()
        }) {
            acc.push(comp);
            nodes
                .entry(acc.clone())
                .or_insert_with(|| Node::Dir {
                    entries: BTreeSet::new(),
                });
        }
    }

    fn alloc_id(&self) -> u64 {
        let mut id = self.next_id.lock().unwrap();
        let v = *id;
        *id += 1;
        v
    }
}

impl FileSystem for MemFs {
    type Handle = MemHandle;

    fn metadata(&self, p: &Path) -> io::Result<FileMeta> {
        let nodes = self.nodes.lock().unwrap();
        match nodes.get(p) {
            Some(Node::File { data, mtime, read_only }) => Ok(FileMeta {
                is_dir: false,
                is_symlink: false,
                len: data.len() as u64,
                modified: Some(*mtime),
                read_only: *read_only,
            }),
            Some(Node::Dir { .. }) => Ok(FileMeta {
                is_dir: true,
                is_symlink: false,
                len: 0,
                modified: Some(SystemTime::now()),
                read_only: false,
            }),
            Some(Node::Symlink { .. }) => Ok(FileMeta {
                is_dir: false,
                is_symlink: true,
                len: 0,
                modified: Some(SystemTime::now()),
                read_only: false,
            }),
            None => Err(io::Error::from(io::ErrorKind::NotFound)),
        }
    }

    fn symlink_metadata(&self, p: &Path) -> io::Result<FileMeta> {
        self.metadata(p)
    }

    fn read_link(&self, p: &Path) -> io::Result<PathBuf> {
        let nodes = self.nodes.lock().unwrap();
        match nodes.get(p) {
            Some(Node::Symlink { target }) => Ok(target.clone()),
            _ => Err(io::Error::from(io::ErrorKind::InvalidInput)),
        }
    }

    fn file_id(&self, p: &Path) -> io::Result<FileId> {
        let _ = self.metadata(p)?;
        Ok(FileId {
            device: 0,
            inode: self.alloc_id(),
        })
    }

    fn read_dir(&self, p: &Path) -> io::Result<ReadDirIter> {
        let nodes = self.nodes.lock().unwrap();
        let mut out: Vec<DirEntry> = Vec::new();
        match nodes.get(p) {
            Some(Node::Dir { entries }) => {
                for name in entries {
                    let child = p.join(name);
                    let (is_dir, len) = match nodes.get(&child) {
                        Some(Node::File { data, .. }) => (false, data.len() as u64),
                        Some(Node::Dir { .. }) => (true, 0u64),
                        Some(Node::Symlink { .. }) => (false, 0u64),
                        None => (false, 0u64),
                    };
                    out.push(DirEntry {
                        path: child,
                        file_name: std::ffi::OsString::from(name),
                        is_dir,
                        is_symlink: false,
                        len,
                    });
                }
            }
            _ => return Err(io::Error::from(io::ErrorKind::NotFound)),
        }
        Ok(Box::new(out.into_iter().map(Ok)))
    }

    fn is_dir_empty(&self, p: &Path) -> io::Result<bool> {
        let nodes = self.nodes.lock().unwrap();
        match nodes.get(p) {
            Some(Node::Dir { entries }) => Ok(entries.is_empty()),
            _ => Err(io::Error::from(io::ErrorKind::NotFound)),
        }
    }

    fn volume_semantics(&self, _p: &Path) -> io::Result<VolumeSemantics> {
        Ok(self.volume)
    }

    fn create_dir_all(&self, p: &Path) -> io::Result<()> {
        let mut nodes = self.nodes.lock().unwrap();
        let mut acc = PathBuf::new();
        for comp in p.iter() {
            acc.push(comp);
            nodes
                .entry(acc.clone())
                .or_insert_with(|| Node::Dir {
                    entries: BTreeSet::new(),
                });
        }
        Ok(())
    }

    fn rename_no_replace(&self, from: &Path, to: &Path) -> io::Result<()> {
        let mut nodes = self.nodes.lock().unwrap();
        if nodes.contains_key(to) {
            return Err(io::Error::from(io::ErrorKind::AlreadyExists));
        }
        let node = nodes
            .remove(from)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        // remove from old parent, add to new parent
        if let (Some(old_name), Some(old_parent)) = (from.file_name(), from.parent()) {
            if let Some(Node::Dir { entries }) = nodes.get_mut(old_parent) {
                if let Some(s) = old_name.to_str() {
                    entries.remove(s);
                }
            }
        }
        if let (Some(new_name), Some(new_parent)) = (to.file_name(), to.parent()) {
            if let Some(Node::Dir { entries }) = nodes.get_mut(new_parent) {
                if let Some(s) = new_name.to_str() {
                    entries.insert(s.to_string());
                }
            }
        }
        nodes.insert(to.to_path_buf(), node);
        Ok(())
    }

    fn create_new(&self, p: &Path) -> io::Result<Self::Handle> {
        self.create_dir_all(p.parent().unwrap_or(Path::new(""))).ok();
        let mut nodes = self.nodes.lock().unwrap();
        if nodes.contains_key(p) {
            return Err(io::Error::from(io::ErrorKind::AlreadyExists));
        }
        if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
            let parent = p.parent().unwrap_or_else(|| Path::new("")).to_path_buf();
            if let Some(Node::Dir { entries }) = nodes.get_mut(&parent) {
                entries.insert(name.to_string());
            }
        }
        nodes.insert(
            p.to_path_buf(),
            Node::File {
                data: Vec::new(),
                mtime: SystemTime::now(),
                read_only: false,
            },
        );
        Ok(MemHandle {
            path: p.to_path_buf(),
        })
    }

    fn copy_into(
        &self,
        from: &Path,
        handle: &mut Self::Handle,
        cancel: &CancelToken,
    ) -> io::Result<u64> {
        let data = {
            let nodes = self.nodes.lock().unwrap();
            match nodes.get(from) {
                Some(Node::File { data, .. }) => data.clone(),
                _ => return Err(io::Error::from(io::ErrorKind::NotFound)),
            }
        };
        if cancel.is_cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        let n = data.len() as u64;
        let mut nodes = self.nodes.lock().unwrap();
        match nodes.get_mut(&handle.path) {
            Some(Node::File { data: dst, .. }) => {
                dst.clear();
                dst.extend_from_slice(&data);
            }
            _ => return Err(io::Error::from(io::ErrorKind::NotFound)),
        }
        Ok(n)
    }

    fn sync_dir(&self, _p: &Path) -> io::Result<()> {
        Ok(())
    }

    fn set_mtime(&self, p: &Path, t: SystemTime) -> io::Result<()> {
        let mut nodes = self.nodes.lock().unwrap();
        match nodes.get_mut(p) {
            Some(Node::File { mtime, .. }) => {
                *mtime = t;
                Ok(())
            }
            _ => Err(io::Error::from(io::ErrorKind::NotFound)),
        }
    }

    fn remove_file(&self, p: &Path) -> io::Result<()> {
        let mut nodes = self.nodes.lock().unwrap();
        let existed = nodes.remove(p).is_some();
        if existed {
            if let (Some(name), Some(parent)) = (p.file_name(), p.parent()) {
                if let Some(Node::Dir { entries }) = nodes.get_mut(parent) {
                    if let Some(s) = name.to_str() {
                        entries.remove(s);
                    }
                }
            }
            Ok(())
        } else {
            Err(io::Error::from(io::ErrorKind::NotFound))
        }
    }

    fn remove_dir(&self, p: &Path) -> io::Result<()> {
        let mut nodes = self.nodes.lock().unwrap();
        match nodes.get(p) {
            Some(Node::Dir { entries }) if entries.is_empty() => {
                nodes.remove(p);
                if let (Some(name), Some(parent)) = (p.file_name(), p.parent()) {
                    if let Some(Node::Dir { entries }) = nodes.get_mut(parent) {
                        if let Some(s) = name.to_str() {
                            entries.remove(s);
                        }
                    }
                }
                Ok(())
            }
            Some(Node::Dir { .. }) => Err(io::Error::from(io::ErrorKind::Other)),
            _ => Err(io::Error::from(io::ErrorKind::NotFound)),
        }
    }

    fn hash(&self, p: &Path, cancel: &CancelToken) -> io::Result<Hash> {
        let data = {
            let nodes = self.nodes.lock().unwrap();
            match nodes.get(p) {
                Some(Node::File { data, .. }) => data.clone(),
                _ => return Err(io::Error::from(io::ErrorKind::NotFound)),
            }
        };
        if cancel.is_cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        let h = blake3::hash(&data);
        Ok(Hash(h.to_hex().to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::FileSystem;
    use crate::volume::VolumeSemantics;

    #[test]
    fn memfs_rename_no_replace_is_atomic() {
        let fs = MemFs::with_volume(VolumeSemantics::sensitive_bytes());
        fs.seed_file("/lib/a.txt", b"hi");
        fs.seed_file("/lib/b.txt", b"yo");
        // rename a.txt -> b.txt fails (occupied)
        let err = fs
            .rename_no_replace(Path::new("/lib/a.txt"), Path::new("/lib/b.txt"))
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        // original still present
        assert!(fs.metadata(Path::new("/lib/a.txt")).is_ok());
    }

    #[test]
    fn memfs_copy_into_writes_bytes() {
        let fs = MemFs::new();
        fs.seed_file("/s.bin", vec![9u8; 64]);
        let mut h = fs.create_new(Path::new("/d.bin")).unwrap();
        let n = fs.copy_into(Path::new("/s.bin"), &mut h, &CancelToken::new()).unwrap();
        assert_eq!(n, 64);
        let m = fs.metadata(Path::new("/d.bin")).unwrap();
        assert_eq!(m.len, 64);
    }
}
