//! Rename without replacing an existing destination.
//!
//! `std::fs::rename` overwrites files on Windows and macOS, so the real
//! filesystem backend uses the platform no-replace primitive instead of a
//! racy exists-then-rename check.

use std::io;
use std::path::Path;

pub fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    os_rename_no_replace(from, to)
}

#[cfg(windows)]
fn os_rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_COPY_ALLOWED};

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    // MOVEFILE_COPY_ALLOWED: allow a copy+delete across volumes (what
    // std::fs::rename does). Do not set MOVEFILE_REPLACE_EXISTING, so an
    // existing destination is left untouched.
    let from_w = wide(from);
    let to_w = wide(to);
    let ok = unsafe { MoveFileExW(from_w.as_ptr(), to_w.as_ptr(), MOVEFILE_COPY_ALLOWED) };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn os_rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    fn cstr(path: &Path) -> io::Result<CString> {
        CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains interior NUL"))
    }

    let old = cstr(from)?;
    let new = cstr(to)?;
    let rc = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            old.as_ptr(),
            libc::AT_FDCWD,
            new.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "macos")]
fn os_rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    fn cstr(path: &Path) -> io::Result<CString> {
        CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains interior NUL"))
    }

    let old = cstr(from)?;
    let new = cstr(to)?;
    // RENAME_EXCL: fail if the destination exists.
    let rc = unsafe { libc::renamex_np(old.as_ptr(), new.as_ptr(), libc::RENAME_EXCL) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn os_rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    if to.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("destination exists: {}", to.display()),
        ));
    }
    std::fs::rename(from, to)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "media-manager-rename-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn real_fs_rename_moves_when_dest_is_free() {
        let dir = temp_dir();
        let from = dir.join("a.mkv");
        let to = dir.join("b.mkv");
        fs::write(&from, b"aaa").unwrap();
        rename_no_replace(&from, &to).unwrap();
        assert!(!from.exists());
        assert_eq!(fs::read(&to).unwrap(), b"aaa");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn real_fs_rename_refuses_to_overwrite() {
        let dir = temp_dir();
        let from = dir.join("a.mkv");
        let to = dir.join("b.mkv");
        fs::write(&from, b"aaa").unwrap();
        fs::write(&to, b"bbb").unwrap();
        let err = rename_no_replace(&from, &to).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert!(from.exists(), "source must remain after a refused overwrite");
        assert_eq!(fs::read(&to).unwrap(), b"bbb", "dest must not be clobbered");
        let _ = fs::remove_dir_all(&dir);
    }
}
