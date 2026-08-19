//! `RealFs` — the production filesystem implementation.
//!
//! Contains the platform shims for `rename_no_replace` (§2.5) and best-effort
//! `volume_semantics` detection (§2.4). Where detection fails, the conservative
//! case (insensitive + normalisation-insensitive) is assumed.

// The platform shims require inline `unsafe` for syscalls. `unsafe` is confined
// to this module (the workspace lint `unsafe_code = warn` is overridden here).
#![allow(unsafe_code)]

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::fs::FileSystem;
use crate::fs::{CancelToken, DirEntry, FileId, FileMeta, Hash, ReadDirIter};
#[cfg(unix)]
use crate::volume::ComponentLimit;
use crate::volume::{NoReplaceStrategy, VolumeSemantics};

/// The production filesystem.
#[derive(Debug, Default, Clone)]
pub struct RealFs {
    /// Override the auto-detected strategy (§7 `moves.no_replace_strategy`).
    pub strategy_override: Option<NoReplaceStrategy>,
}

impl RealFs {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_strategy(mut self, s: NoReplaceStrategy) -> Self {
        self.strategy_override = Some(s);
        self
    }
}

fn meta_of(m: &fs::Metadata, symlink: bool) -> FileMeta {
    FileMeta {
        is_dir: m.is_dir(),
        is_symlink: symlink,
        len: m.len(),
        modified: m.modified().ok(),
        read_only: m.permissions().readonly(),
    }
}

impl FileSystem for RealFs {
    type Handle = File;

    fn metadata(&self, p: &Path) -> io::Result<FileMeta> {
        let m = fs::metadata(p)?;
        Ok(meta_of(&m, false))
    }

    fn symlink_metadata(&self, p: &Path) -> io::Result<FileMeta> {
        let m = fs::symlink_metadata(p)?;
        Ok(meta_of(&m, m.file_type().is_symlink()))
    }

    fn read_link(&self, p: &Path) -> io::Result<PathBuf> {
        fs::read_link(p)
    }

    fn file_id(&self, p: &Path) -> io::Result<FileId> {
        file_id_of(p)
    }

    fn read_dir(&self, p: &Path) -> io::Result<ReadDirIter> {
        let rd = fs::read_dir(p)?;
        Ok(Box::new(rd.map(|res| {
            res.and_then(|de| {
                let ft = de.file_type()?;
                let meta = de.metadata().ok();
                Ok(DirEntry {
                    path: de.path(),
                    file_name: de.file_name(),
                    is_dir: ft.is_dir(),
                    is_symlink: ft.is_symlink(),
                    len: meta.as_ref().map(|m| m.len()).unwrap_or(0),
                })
            })
        })))
    }

    fn is_dir_empty(&self, p: &Path) -> io::Result<bool> {
        Ok(fs::read_dir(p)?.next().is_none())
    }

    fn volume_semantics(&self, p: &Path) -> io::Result<VolumeSemantics> {
        Ok(detect_volume_semantics(p))
    }

    fn create_dir_all(&self, p: &Path) -> io::Result<()> {
        fs::create_dir_all(p)
    }

    fn rename_no_replace(&self, from: &Path, to: &Path) -> io::Result<()> {
        rename_no_replace(from, to)
    }

    fn rename_replace(&self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }

    fn create_new(&self, p: &Path) -> io::Result<Self::Handle> {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .truncate(false)
            .open(p)
    }

    fn copy_into(
        &self,
        from: &Path,
        handle: &mut Self::Handle,
        cancel: &CancelToken,
    ) -> io::Result<u64> {
        let mut src = File::open(from)?;
        let mut buf = [0u8; 64 * 1024];
        let mut total = 0u64;
        loop {
            if cancel.is_cancelled() {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
            }
            let n = src.read(&mut buf)?;
            if n == 0 {
                break;
            }
            handle.write_all(&buf[..n])?;
            total += n as u64;
        }
        handle.sync_all()?;
        Ok(total)
    }

    fn sync_dir(&self, p: &Path) -> io::Result<()> {
        sync_dir(p)
    }

    fn set_mtime(&self, p: &Path, t: SystemTime) -> io::Result<()> {
        let file = OpenOptions::new().write(true).open(p)?;
        let times = std::fs::FileTimes::new().set_modified(t);
        file.set_times(times)
    }

    fn remove_file(&self, p: &Path) -> io::Result<()> {
        fs::remove_file(p)
    }

    fn remove_dir(&self, p: &Path) -> io::Result<()> {
        fs::remove_dir(p)
    }

    fn hash(&self, p: &Path, cancel: &CancelToken) -> io::Result<Hash> {
        let mut f = File::open(p)?;
        let mut hasher = blake3::Hasher::new();
        let mut buf = [0u8; 64 * 1024];
        loop {
            if cancel.is_cancelled() {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
            }
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Ok(Hash(hasher.finalize().to_hex().to_string()))
    }

    fn no_replace_strategy(&self, p: &Path) -> NoReplaceStrategy {
        if let Some(s) = self.strategy_override {
            return s;
        }
        default_no_replace_strategy(p)
    }
}

// ---------------------------------------------------------------------------
// file_id
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn file_id_of(p: &Path) -> io::Result<FileId> {
    use std::os::unix::fs::MetadataExt;
    let m = fs::metadata(p)?;
    Ok(FileId {
        device: m.dev(),
        inode: m.ino(),
    })
}

#[cfg(windows)]
fn file_id_of(p: &Path) -> io::Result<FileId> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    let f = fs::File::open(p)?;
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    let ok = unsafe { GetFileInformationByHandle(f.as_raw_handle() as *mut _, &mut info) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(FileId {
        device: info.dwVolumeSerialNumber as u64,
        inode: ((info.nFileIndexHigh as u64) << 32) | (info.nFileIndexLow as u64),
    })
}

#[cfg(not(any(unix, windows)))]
fn file_id_of(p: &Path) -> io::Result<FileId> {
    let m = fs::metadata(p)?;
    Ok(FileId {
        device: 0,
        inode: m.len(),
    })
}

// ---------------------------------------------------------------------------
// sync_dir
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn sync_dir(p: &Path) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let f = fs::File::open(p)?;
    let ret = unsafe { libc::fsync(f.as_raw_fd()) };
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn sync_dir(p: &Path) -> io::Result<()> {
    // Open the directory with FILE_FLAG_BACKUP_SEMANTICS (required to open a
    // directory handle on Windows) and flush it. `File::sync_all` calls
    // `FlushFileBuffers`, which is the durability primitive we need (§6.2).
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;
    let f = OpenOptions::new()
        .read(true)
        .attributes(FILE_FLAG_BACKUP_SEMANTICS)
        .open(p)?;
    f.sync_all()
}

#[cfg(not(any(unix, windows)))]
fn sync_dir(_p: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(windows)]
fn wide_path(p: &Path) -> io::Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;
    let mut wide: Vec<u16> = p.as_os_str().encode_wide().collect();
    wide.push(0);
    Ok(wide)
}

#[cfg(windows)]
fn detect_windows(p: &Path) -> io::Result<VolumeSemantics> {
    // §2.4: the *per-directory* `FILE_CASE_SENSITIVE_INFORMATION` flag
    // (Win10 1803+), explicitly not `GetVolumeInformationW`'s
    // `FILE_CASE_SENSITIVE_SEARCH` — that bit means the volume *supports*
    // case-sensitive names, and NTFS reports it while Win32 path resolution
    // is case-insensitive by default. Using it would classify every NTFS
    // volume as case-sensitive and silently corrupt the §5.6 collision key.
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Wdk::Storage::FileSystem::FILE_CASE_SENSITIVE_INFORMATION;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FileCaseSensitiveInfo, GetFileInformationByHandleEx,
    };
    use windows_sys::Win32::System::SystemServices::FILE_CS_FLAG_CASE_SENSITIVE_DIR;

    // `FILE_FLAG_BACKUP_SEMANTICS` is required to open a directory handle at
    // all on Windows (same trick `sync_dir` uses below).
    let f = OpenOptions::new()
        .read(true)
        .attributes(FILE_FLAG_BACKUP_SEMANTICS)
        .open(p)?;

    let mut info: FILE_CASE_SENSITIVE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is sized exactly for `FILE_CASE_SENSITIVE_INFORMATION`
    // and the handle is open for the duration of the call.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            f.as_raw_handle() as *mut _,
            FileCaseSensitiveInfo,
            std::ptr::addr_of_mut!(info).cast(),
            std::mem::size_of::<FILE_CASE_SENSITIVE_INFORMATION>() as u32,
        )
    };
    if ok == 0 {
        // Pre-1803 Windows, or a filesystem that doesn't implement this info
        // class: not an error, just undetermined — conservative NTFS shape.
        return Ok(VolumeSemantics::ntfs());
    }
    let case_sensitive = info.Flags & FILE_CS_FLAG_CASE_SENSITIVE_DIR != 0;
    Ok(VolumeSemantics {
        case_sensitive,
        normalisation_sensitive: false,
        ..VolumeSemantics::ntfs()
    })
}

// ---------------------------------------------------------------------------
// rename_no_replace shims (§2.5)
// ---------------------------------------------------------------------------

/// `rename_no_replace` — the platform shim. Never silently overwrites.
pub fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        match renameat2_noreplace(from, to) {
            Ok(()) => return Ok(()),
            Err(e) if crate::fs::is_cross_device(&e) => return Err(e),
            Err(e)
                if matches!(
                    e.raw_os_error(),
                    Some(libc::EINVAL) | Some(libc::EOPNOTSUPP) | Some(libc::ENOSYS)
                ) =>
            {
                // fall through to link+unlink fallback
            }
            Err(e) => return Err(e),
        }
        // Fallback: link + unlink. link returns EEXIST on an occupied target.
        match std::fs::hard_link(from, to) {
            Ok(()) => {
                std::fs::remove_file(from)?;
                Ok(())
            }
            Err(e) => Err(e),
        }
    }
    #[cfg(windows)]
    {
        rename_no_replace_win(from, to)
    }
    #[cfg(not(any(unix, windows)))]
    {
        // No platform primitive: check-then-rename (racy, last resort).
        if to.exists() {
            return Err(io::Error::from(io::ErrorKind::AlreadyExists));
        }
        std::fs::rename(from, to)
    }
}

#[cfg(unix)]
fn renameat2_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let from_c = CString::new(from.as_os_str().as_bytes())?;
    let to_c = CString::new(to.as_os_str().as_bytes())?;

    // RENAME_NOREPLACE = 1<<0 (Linux <linux/fs.h>)
    const RENAME_NOREPLACE: libc::c_uint = 1;

    // Try renameat2 via the raw syscall. glibc 2.28+ exposes renameat2; older
    // toolchains need the raw syscall. We attempt the libc binding first.
    let ret = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            from_c.as_ptr(),
            libc::AT_FDCWD,
            to_c.as_ptr(),
            RENAME_NOREPLACE,
        )
    };
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn rename_no_replace_win(from: &Path, to: &Path) -> io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::MoveFileExW;

    let from_w = wide_path(from)?;
    let to_w = wide_path(to)?;
    // No MOVEFILE_REPLACE_EXISTING (0x1). No MOVEFILE_COPY_ALLOWED so a
    // cross-volume move returns ERROR_NOT_SAME_DEVICE, feeding the copy path.
    let ok = unsafe { MoveFileExW(from_w.as_ptr(), to_w.as_ptr(), 0u32) };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// volume semantics detection (§2.4)
// ---------------------------------------------------------------------------

/// Default per-volume strategy selection (§2.5 `auto`).
pub fn default_no_replace_strategy(p: &Path) -> NoReplaceStrategy {
    let vol = detect_volume_semantics(p);
    if vol == VolumeSemantics::conservative() {
        // Unknown volume — be safe, use reservation.
        NoReplaceStrategy::Reserve
    } else {
        NoReplaceStrategy::Native
    }
}

pub fn detect_volume_semantics(p: &Path) -> VolumeSemantics {
    #[cfg(unix)]
    {
        if let Ok(v) = detect_unix(p) {
            return v;
        }
    }
    #[cfg(windows)]
    {
        if let Ok(v) = detect_windows(p) {
            return v;
        }
    }
    #[allow(unreachable_code)]
    VolumeSemantics::conservative()
}

#[cfg(unix)]
fn detect_unix(p: &Path) -> io::Result<VolumeSemantics> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = CString::new(p.as_os_str().as_bytes())?;

    // `_PC_NAME_MAX`/`_PC_PATH_MAX` are portable pathconf names (unlike
    // `_PC_CASE_SENSITIVE`, which — despite being defined for Apple targets in
    // the `libc` crate — the plan deliberately routes through `getattrlist`
    // instead; see `platform_case_sensitivity` below).
    let name_max = pathconf_signed(c.as_ptr(), libc::_PC_NAME_MAX)
        .filter(|v| *v > 0)
        .unwrap_or(255) as u32;
    let path_max = pathconf_signed(c.as_ptr(), libc::_PC_PATH_MAX)
        .filter(|v| *v > 0)
        .unwrap_or(4096) as u32;

    Ok(match platform_case_sensitivity(&c) {
        Some((case_sensitive, normalisation_sensitive)) => VolumeSemantics {
            case_sensitive,
            normalisation_sensitive,
            max_component: ComponentLimit::Bytes(name_max),
            max_total: path_max,
        },
        // Unrecognised fstype (network mount, FUSE, an fstype we don't have a
        // table entry for) or the detection syscall itself failed: the
        // conservative case (§2.4) — but keep the length caps we did
        // successfully measure rather than discarding them too.
        None => VolumeSemantics {
            max_component: ComponentLimit::Bytes(name_max),
            max_total: path_max,
            ..VolumeSemantics::conservative()
        },
    })
}

#[cfg(unix)]
fn pathconf_signed(path: *const libc::c_char, name: libc::c_int) -> Option<libc::c_long> {
    // SAFETY: `path` is a valid NUL-terminated C string for the duration of
    // this call; `pathconf` with a valid `name` writes a long or returns -1.
    let ret = unsafe { libc::pathconf(path, name) };
    if ret == -1 { None } else { Some(ret) }
}

/// `Some((case_sensitive, normalisation_sensitive))` when this platform could
/// determine it for the volume containing `c`; `None` when the fstype is
/// unrecognised or undeterminable, in which case the caller applies the
/// conservative default (§2.4).
#[cfg(target_os = "linux")]
fn platform_case_sensitivity(c: &std::ffi::CString) -> Option<(bool, bool)> {
    // Linux glibc does not define `_PC_CASE_SENSITIVE` at all (that pathconf
    // name is an Apple extension), so detection goes by `statfs` fstype magic
    // (linux/magic.h) instead — the "statfs fstype" half of §2.4's "Linux:
    // statfs fstype, plus pathconf(_PC_CASE_SENSITIVE) where available".
    let mut buf: libc::statfs = unsafe { std::mem::zeroed() };
    let ret = unsafe { libc::statfs(c.as_ptr(), &mut buf) };
    if ret != 0 {
        return None;
    }
    // `f_type` is a signed word; the kernel's magic constants are unsigned
    // 32-bit values, so compare bit patterns via `as u32` rather than value.
    let magic = buf.f_type as u32;
    match magic {
        // Local, case- and normalisation-sensitive filesystems.
        v if v == libc::EXT4_SUPER_MAGIC as u32
            || v == libc::BTRFS_SUPER_MAGIC as u32
            || v == libc::TMPFS_MAGIC as u32
            || v == XFS_SUPER_MAGIC =>
        {
            Some((true, true))
        }
        // FAT/exFAT: case-insensitive. `libc` has no `EXFAT_SUPER_MAGIC`
        // constant, so it's a local literal (linux/magic.h).
        v if v == libc::MSDOS_SUPER_MAGIC as u32 || v == EXFAT_SUPER_MAGIC => Some((false, false)),
        // Network / FUSE-backed mounts (NFS, CIFS, SMB2, generic FUSE
        // passthrough — sshfs, rclone, ntfs-3g, …): the far end could be
        // anything, and per §2.5 this is exactly where the conservative
        // default is the *correct* answer, not merely a fallback.
        v if v == libc::NFS_SUPER_MAGIC as u32
            || v == libc::SMB_SUPER_MAGIC as u32
            || v == libc::FUSE_SUPER_MAGIC as u32
            || v == CIFS_MAGIC_NUMBER
            || v == SMB2_MAGIC_NUMBER =>
        {
            None
        }
        _ => None,
    }
}

#[cfg(target_os = "linux")]
const XFS_SUPER_MAGIC: u32 = 0x5846_5342;
#[cfg(target_os = "linux")]
const EXFAT_SUPER_MAGIC: u32 = 0x2011_BAB0;
#[cfg(target_os = "linux")]
const CIFS_MAGIC_NUMBER: u32 = 0xFF53_4D42;
#[cfg(target_os = "linux")]
const SMB2_MAGIC_NUMBER: u32 = 0xFE53_4D42;

#[cfg(target_os = "macos")]
fn platform_case_sensitivity(c: &std::ffi::CString) -> Option<(bool, bool)> {
    // §2.4: macOS via `getattrlist` → `VOL_CAP_FMT_CASE_SENSITIVE`. Deliberately
    // not `pathconf(_PC_CASE_SENSITIVE)` — that name exists in the `libc`
    // crate's Apple bindings but isn't the documented way to ask this
    // question, and not `GetVolumeInformationW` (that's Windows and has the
    // analogous "supports" vs "behaves" problem documented in §2.4).
    #[repr(C)]
    struct AttrListBuf {
        length: u32,
        caps: libc::vol_capabilities_attr_t,
    }

    let mut list: libc::attrlist = unsafe { std::mem::zeroed() };
    list.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
    list.volattr = libc::ATTR_VOL_INFO | libc::ATTR_VOL_CAPABILITIES;

    let mut reply: AttrListBuf = unsafe { std::mem::zeroed() };
    // SAFETY: `list` and `reply` are correctly sized/zeroed for `getattrlist`;
    // `c` is a valid NUL-terminated C string for the duration of the call.
    let ret = unsafe {
        libc::getattrlist(
            c.as_ptr(),
            std::ptr::addr_of_mut!(list).cast(),
            std::ptr::addr_of_mut!(reply).cast(),
            std::mem::size_of::<AttrListBuf>(),
            0,
        )
    };
    if ret != 0 {
        return None;
    }
    let fmt = reply.caps.capabilities[libc::VOL_CAPABILITIES_FORMAT];
    let valid = reply.caps.valid[libc::VOL_CAPABILITIES_FORMAT];
    if valid & libc::VOL_CAP_FMT_CASE_SENSITIVE == 0 {
        return None; // volume didn't report this capability as meaningful
    }
    let case_sensitive = fmt & libc::VOL_CAP_FMT_CASE_SENSITIVE != 0;
    // APFS (case-insensitive default) and HFS+ are both normalisation-
    // insensitive but normalisation-*preserving* (§2.4) — there's no separate
    // capability bit for this, and a case-sensitive APFS volume is still
    // normalisation-insensitive, so this doesn't follow `case_sensitive`.
    Some((case_sensitive, false))
}

#[cfg(all(unix, not(target_os = "linux"), not(target_os = "macos")))]
fn platform_case_sensitivity(_c: &std::ffi::CString) -> Option<(bool, bool)> {
    // No fstype table or getattrlist-equivalent wired up for this platform
    // yet; the caller's conservative default applies.
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn realfs_create_and_rename() {
        let tmp = TempDir::new().unwrap();
        let fs = RealFs::new();
        let root = tmp.path();
        let a = root.join("a.txt");
        std::fs::write(&a, b"hi").unwrap();
        let b = root.join("b.txt");
        fs.rename_no_replace(&a, &b).unwrap();
        assert!(b.exists());
        assert!(!a.exists());
        // renaming onto an existing target must fail
        let c = root.join("c.txt");
        std::fs::write(&c, b"yo").unwrap();
        assert!(fs.rename_no_replace(&b, &c).is_err());
    }

    #[test]
    fn realfs_create_new_is_exclusive() {
        let tmp = TempDir::new().unwrap();
        let fs = RealFs::new();
        let p = tmp.path().join("x.bin");
        let _h = fs.create_new(&p).unwrap();
        // second create_new must fail
        assert!(fs.create_new(&p).is_err());
    }

    #[test]
    fn realfs_copy_into_preserves_bytes() {
        let tmp = TempDir::new().unwrap();
        let fs = RealFs::new();
        let src = tmp.path().join("s.bin");
        let dst = tmp.path().join("d.bin");
        let data = vec![7u8; 100_000];
        std::fs::write(&src, &data).unwrap();
        let mut h = fs.create_new(&dst).unwrap();
        let n = fs.copy_into(&src, &mut h, &CancelToken::new()).unwrap();
        assert_eq!(n, 100_000);
        assert_eq!(std::fs::read(&dst).unwrap(), data);
    }

    #[test]
    fn realfs_hash_is_stable() {
        let tmp = TempDir::new().unwrap();
        let fs = RealFs::new();
        let p = tmp.path().join("h.bin");
        std::fs::write(&p, b"media-manager").unwrap();
        let a = fs.hash(&p, &CancelToken::new()).unwrap();
        let b = fs.hash(&p, &CancelToken::new()).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn realfs_volume_semantics_returns_something() {
        let tmp = TempDir::new().unwrap();
        let fs = RealFs::new();
        let _ = fs.volume_semantics(tmp.path()).unwrap();
    }
}
