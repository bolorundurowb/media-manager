//! Probe cache keyed on `(file_id, size, mtime)`, **not** path.
//!
//! A path-keyed cache misses after the first organise run moves the file,
//! which would re-probe the entire library on every subsequent run.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use mm_core::fs::{FileId, FileMeta};

use crate::probe::Probe;

/// Cache lookup identity: filesystem id plus size and mtime.
///
/// Path is intentionally absent. After a file is moved, `(device, inode)` is
/// stable on the same volume while the path is not.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheKey {
    pub file_id: FileId,
    pub size: u64,
    pub mtime: Option<SystemTime>,
}

impl CacheKey {
    pub fn new(file_id: FileId, size: u64, mtime: Option<SystemTime>) -> Self {
        CacheKey {
            file_id,
            size,
            mtime,
        }
    }

    /// Build a key from [`FileId`] and [`FileMeta`].
    pub fn from_meta(file_id: FileId, meta: &FileMeta) -> Self {
        CacheKey {
            file_id,
            size: meta.len,
            mtime: meta.modified,
        }
    }
}

/// On-disk or in-memory probe cache.
///
/// Persist format is a JSON array of `{key, probe}` records under the
/// directory given to [`ProbeCache::new`].
#[derive(Clone)]
pub struct ProbeCache {
    inner: Arc<Mutex<HashMap<CacheKey, Probe>>>,
    persist_path: Option<PathBuf>,
}

impl ProbeCache {
    /// On-disk cache stored as `probe-cache.json` under `dir`.
    pub fn new(dir: impl AsRef<Path>) -> Self {
        let dir = dir.as_ref();
        if let Err(e) = fs::create_dir_all(dir) {
            tracing::warn!(error = %e, path = %dir.display(), "could not create probe cache directory");
        }
        let persist_path = dir.join("probe-cache.json");
        let map = load_map(&persist_path);
        ProbeCache {
            inner: Arc::new(Mutex::new(map)),
            persist_path: Some(persist_path),
        }
    }

    /// Process-local cache; nothing is written to disk. For tests.
    pub fn in_memory() -> Self {
        ProbeCache {
            inner: Arc::new(Mutex::new(HashMap::new())),
            persist_path: None,
        }
    }

    /// Cached probe for `key`, if present.
    pub fn get(&self, key: &CacheKey) -> Option<Probe> {
        self.lock().get(key).cloned()
    }

    /// Store `probe` under `key`, persisting if this cache is on-disk.
    pub fn insert(&self, key: CacheKey, probe: Probe) {
        {
            let mut map = self.lock();
            map.insert(key, probe);
        }
        self.persist();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<CacheKey, Probe>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn persist(&self) {
        let Some(path) = &self.persist_path else {
            return;
        };
        let snapshot: Vec<PersistedEntry> = self
            .lock()
            .iter()
            .map(|(k, v)| PersistedEntry {
                key: PersistedKey::from(k),
                probe: v.clone(),
            })
            .collect();
        match serde_json::to_vec_pretty(&snapshot) {
            Ok(bytes) => {
                if let Err(e) = fs::write(path, bytes) {
                    tracing::warn!(error = %e, path = %path.display(), "failed to persist probe cache");
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to serialise probe cache");
            }
        }
    }
}

fn load_map(path: &Path) -> HashMap<CacheKey, Probe> {
    let Ok(bytes) = fs::read(path) else {
        return HashMap::new();
    };
    let Ok(entries) = serde_json::from_slice::<Vec<PersistedEntry>>(&bytes) else {
        tracing::warn!(path = %path.display(), "probe cache unreadable; starting empty");
        return HashMap::new();
    };
    entries
        .into_iter()
        .map(|e| (e.key.into_cache_key(), e.probe))
        .collect()
}

#[derive(Serialize, Deserialize)]
struct PersistedEntry {
    key: PersistedKey,
    probe: Probe,
}

#[derive(Serialize, Deserialize)]
struct PersistedKey {
    device: u64,
    inode: u64,
    size: u64,
    /// Seconds since UNIX_EPOCH; `None` if mtime is missing or pre-epoch.
    mtime_secs: Option<u64>,
    mtime_nanos: Option<u32>,
}

impl From<&CacheKey> for PersistedKey {
    fn from(k: &CacheKey) -> Self {
        let (mtime_secs, mtime_nanos) =
            match k.mtime.and_then(|t| t.duration_since(UNIX_EPOCH).ok()) {
                Some(d) => (Some(d.as_secs()), Some(d.subsec_nanos())),
                None => (None, None),
            };
        PersistedKey {
            device: k.file_id.device,
            inode: k.file_id.inode,
            size: k.size,
            mtime_secs,
            mtime_nanos,
        }
    }
}

impl PersistedKey {
    fn into_cache_key(self) -> CacheKey {
        let mtime = match (self.mtime_secs, self.mtime_nanos) {
            (Some(secs), nanos) => {
                Some(UNIX_EPOCH + std::time::Duration::new(secs, nanos.unwrap_or(0)))
            }
            (None, _) => None,
        };
        CacheKey {
            file_id: FileId {
                device: self.device,
                inode: self.inode,
            },
            size: self.size,
            mtime,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::VideoInfo;
    use std::time::Duration;

    fn probe_1080() -> Probe {
        Probe {
            video: Some(VideoInfo::from_pixel(1920, 1080)),
            audio: None,
            duration: None,
            subtitle_tracks: vec![],
        }
    }

    fn key(size: u64, mtime: Option<SystemTime>) -> CacheKey {
        CacheKey::new(
            FileId {
                device: 1,
                inode: 42,
            },
            size,
            mtime,
        )
    }

    #[test]
    fn hit_on_same_file_id_size_mtime() {
        let cache = ProbeCache::in_memory();
        let t = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let k = key(1234, Some(t));
        cache.insert(k.clone(), probe_1080());
        assert_eq!(cache.get(&k).and_then(|p| p.video), probe_1080().video);
    }

    #[test]
    fn miss_when_size_changes() {
        let cache = ProbeCache::in_memory();
        let t = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        cache.insert(key(1234, Some(t)), probe_1080());
        assert!(cache.get(&key(9999, Some(t))).is_none());
    }

    #[test]
    fn miss_when_mtime_changes() {
        let cache = ProbeCache::in_memory();
        let t1 = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let t2 = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        cache.insert(key(1234, Some(t1)), probe_1080());
        assert!(cache.get(&key(1234, Some(t2))).is_none());
    }

    #[test]
    fn on_disk_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let k = key(50, Some(UNIX_EPOCH + Duration::from_secs(10)));
        {
            let cache = ProbeCache::new(dir.path());
            cache.insert(k.clone(), probe_1080());
        }
        let cache = ProbeCache::new(dir.path());
        let got = cache.get(&k).expect("persisted hit");
        assert_eq!(got.video.unwrap().pixel_width, 1920);
    }

    #[test]
    fn from_meta_uses_len_and_mtime() {
        let meta = FileMeta {
            is_dir: false,
            is_symlink: false,
            len: 77,
            modified: Some(UNIX_EPOCH),
            read_only: false,
        };
        let k = CacheKey::from_meta(
            FileId {
                device: 3,
                inode: 9,
            },
            &meta,
        );
        assert_eq!(k.size, 77);
        assert_eq!(k.mtime, Some(UNIX_EPOCH));
    }
}
