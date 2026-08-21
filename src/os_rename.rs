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
    match os_rename_no_replace(from, to) {
        #[cfg(unix)]
        Err(err) if err.raw_os_error() == Some(libc::EXDEV) => {
            copy_across_volumes_no_replace(from, to)
        }
        #[cfg(windows)]
        Err(err)
            if err.raw_os_error()
                == Some(windows_sys::Win32::Foundation::ERROR_NOT_SAME_DEVICE as i32) =>
        {
            copy_across_volumes_no_replace(from, to)
        }
        result => result,
    }
}

fn copy_across_volumes_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    use std::fs::OpenOptions;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COPY_SEQ: AtomicU64 = AtomicU64::new(0);

    let parent = to
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "destination has no parent"))?;
    let mut temp = None;
    for _ in 0..100 {
        let candidate = parent.join(format!(
            ".media-manager-copy-{}-{}.tmp",
            std::process::id(),
            COPY_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => {
                temp = Some((candidate, file));
                break;
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    let Some((temp_path, mut output)) = temp else {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique cross-volume temporary file",
        ));
    };

    let copied = (|| {
        let mut input = std::fs::File::open(from)?;
        io::copy(&mut input, &mut output)?;
        output.sync_all()?;
        if let Ok(metadata) = input.metadata() {
            std::fs::set_permissions(&temp_path, metadata.permissions())?;
        }
        drop(output);
        // The temp file is on the destination volume, so this final publish
        // is atomic and still refuses to replace a racing destination.
        os_rename_no_replace(&temp_path, to)?;
        if let Err(err) = std::fs::remove_file(from) {
            return Err(io::Error::new(
                err.kind(),
                format!(
                    "copied to {} but could not remove source {}: {err}",
                    to.display(),
                    from.display()
                ),
            ));
        }
        Ok(())
    })();

    if copied.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    } else {
        tracing::info!(
            from = %from.display(),
            to = %to.display(),
            "moved across volumes using verified copy and source removal"
        );
    }
    copied
}

#[cfg(windows)]
fn os_rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::MoveFileExW;

    fn wide(path: &Path) -> Vec<u16> {
        // `std::fs` transparently prefixes well-formed absolute paths with
        // the extended-length `\\?\` marker so ordinary calls (`rename`,
        // `create_dir_all`, `read_dir`, ...) aren't limited to `MAX_PATH`
        // (260 chars). Scene-release folder/file names routinely produce
        // source paths past that once nested under a season-pack container,
        // so this raw `MoveFileExW` FFI call — which bypasses `std::fs`
        // entirely to get no-replace semantics — has to do the same
        // conversion itself, or long moves fail with `ERROR_PATH_NOT_FOUND`
        // (os error 3) even though the file plainly exists.
        to_verbatim(path)
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    // Flags 0: same-volume atomic rename, with no replacement. A
    // cross-volume error is handled by the verified temp-copy path above.
    let from_w = wide(from);
    let to_w = wide(to);
    let ok = unsafe { MoveFileExW(from_w.as_ptr(), to_w.as_ptr(), 0) };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Prefix an absolute Windows path with the extended-length (`\\?\`) marker,
/// enabling paths well past `MAX_PATH` (260 chars) for raw Win32 calls that
/// don't already go through `std::fs`'s own long-path handling.
///
/// Left unchanged (and simply passed through to the OS, which will reject
/// it if too long) when the path is already verbatim, isn't drive-absolute
/// or UNC, or contains `.`/`..` components — verbatim paths are used
/// exactly as given with no normalization, so only paths `std::fs` would
/// already consider "safe to verbatim-ify" are converted here.
#[cfg(windows)]
fn to_verbatim(path: &Path) -> std::path::PathBuf {
    use std::path::{Component, Prefix};

    let text = path.to_string_lossy();
    if text.starts_with(r"\\?\") {
        return path.to_path_buf();
    }
    if path
        .components()
        .any(|c| matches!(c, Component::CurDir | Component::ParentDir))
    {
        return path.to_path_buf();
    }
    match path.components().next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(_) => std::path::PathBuf::from(format!(r"\\?\{text}")),
            // `\\server\share\...` -> `\\?\UNC\server\share\...`. Built by
            // string surgery rather than recombining `Component`s: pushing a
            // rooted-but-unprefixed fragment onto a `PathBuf` replaces the
            // whole thing instead of appending, per `PathBuf::push`'s rules.
            Prefix::UNC(..) => match text.strip_prefix(r"\\") {
                Some(rest) => std::path::PathBuf::from(format!(r"\\?\UNC\{rest}")),
                None => path.to_path_buf(),
            },
            _ => path.to_path_buf(),
        },
        _ => path.to_path_buf(),
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
        assert!(
            from.exists(),
            "source must remain after a refused overwrite"
        );
        assert_eq!(fs::read(&to).unwrap(), b"bbb", "dest must not be clobbered");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cross_volume_fallback_publishes_complete_file_then_removes_source() {
        let dir = temp_dir();
        let from = dir.join("a.mkv");
        let to = dir.join("b.mkv");
        fs::write(&from, b"complete media bytes").unwrap();
        copy_across_volumes_no_replace(&from, &to).unwrap();
        assert!(!from.exists());
        assert_eq!(fs::read(&to).unwrap(), b"complete media bytes");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cross_volume_fallback_never_replaces_existing_destination() {
        let dir = temp_dir();
        let from = dir.join("a.mkv");
        let to = dir.join("b.mkv");
        fs::write(&from, b"source").unwrap();
        fs::write(&to, b"destination").unwrap();
        let err = copy_across_volumes_no_replace(&from, &to).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&from).unwrap(), b"source");
        assert_eq!(fs::read(&to).unwrap(), b"destination");
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .contains("media-manager-copy")
            })
            .collect();
        assert!(leftovers.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn to_verbatim_prefixes_drive_absolute_paths() {
        let p = to_verbatim(Path::new(r"C:\Users\a\b.mkv"));
        assert_eq!(p, std::path::PathBuf::from(r"\\?\C:\Users\a\b.mkv"));
    }

    #[cfg(windows)]
    #[test]
    fn to_verbatim_prefixes_unc_paths() {
        let p = to_verbatim(Path::new(r"\\server\share\folder\file.mkv"));
        assert_eq!(
            p,
            std::path::PathBuf::from(r"\\?\UNC\server\share\folder\file.mkv")
        );
    }

    #[cfg(windows)]
    #[test]
    fn to_verbatim_leaves_already_verbatim_paths_alone() {
        let p = to_verbatim(Path::new(r"\\?\C:\already\verbatim.mkv"));
        assert_eq!(p, std::path::PathBuf::from(r"\\?\C:\already\verbatim.mkv"));
    }

    #[cfg(windows)]
    #[test]
    fn to_verbatim_skips_paths_needing_normalization() {
        // `.`/`..` components must be resolved before a path can safely be
        // used verbatim; leave those to the OS instead of guessing.
        let p = to_verbatim(Path::new(r"C:\Users\a\..\b.mkv"));
        assert_eq!(p, std::path::PathBuf::from(r"C:\Users\a\..\b.mkv"));
    }

    #[cfg(windows)]
    #[test]
    fn real_fs_rename_succeeds_past_max_path() {
        // Regression test for the exact failure reported in production:
        // scene-release folder names routinely produce source paths well
        // past Windows' 260-char `MAX_PATH`, and the raw `MoveFileExW` FFI
        // call used for no-replace semantics fails with
        // `ERROR_PATH_NOT_FOUND` unless it verbatim-prefixes long paths the
        // same way `std::fs` already does for everything else.
        let dir = temp_dir();
        let long_component = "x".repeat(120);
        let nested = dir.join(&long_component).join(&long_component);
        fs::create_dir_all(&nested).unwrap();
        let from = nested.join("a.mkv");
        fs::write(&from, b"aaa").unwrap();
        assert!(
            from.as_os_str().len() > 260,
            "test setup must exceed MAX_PATH"
        );

        let to = nested.join("b.mkv");
        rename_no_replace(&from, &to).unwrap();
        assert!(!from.exists());
        assert_eq!(fs::read(&to).unwrap(), b"aaa");
        let _ = fs::remove_dir_all(&dir);
    }
}
