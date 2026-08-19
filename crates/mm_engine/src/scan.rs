//! Scan stage (§5.1).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use mm_core::classify::FileClass;
use mm_core::config::{Config, SymlinkPolicy};
use mm_core::fs::{FileId, FileSystem};

/// A file discovered during scanning.
#[derive(Debug, Clone)]
pub struct ScannedFile {
    pub absolute: PathBuf,
    pub relative: PathBuf,
    pub class: FileClass,
    pub meta: mm_core::fs::FileMeta,
}

/// Scan `root` recursively using the provided filesystem and config.
pub fn scan<F: FileSystem>(fs: &F, root: &Path, cfg: &Config) -> Result<Vec<ScannedFile>, std::io::Error> {
    let mut out = Vec::new();
    let mut visited = HashSet::new();
    scan_dir(fs, root, root, cfg, &mut visited, &mut out)?;
    Ok(out)
}

fn scan_dir<F: FileSystem>(
    fs: &F,
    root: &Path,
    dir: &Path,
    cfg: &Config,
    visited: &mut HashSet<FileId>,
    out: &mut Vec<ScannedFile>,
) -> Result<(), std::io::Error> {
    let iter = fs.read_dir(dir)?;
    for entry in iter {
        let entry = entry?;
        let rel = make_relative(root, &entry.path);
        if is_ignored(&rel, &entry.file_name, cfg) {
            continue;
        }

        if entry.is_symlink {
            match cfg.behaviour.symlinks {
                SymlinkPolicy::Skip => continue,
                SymlinkPolicy::TreatAsFile => {
                    // fall through to file handling below using symlink metadata
                }
                SymlinkPolicy::Follow => {
                    let target = fs.read_link(&entry.path)?;
                    let canonical = fs.file_id(&entry.path)?;
                    if !visited.insert(canonical) {
                        continue; // loop
                    }
                    let meta = fs.metadata(&target)?;
                    if meta.is_dir {
                        scan_dir(fs, root, &target, cfg, visited, out)?;
                    } else {
                        push_file(root, target, rel, FileClass::Unknown, meta, out);
                    }
                    continue;
                }
            }
        }

        if entry.is_dir {
            scan_dir(fs, root, &entry.path, cfg, visited, out)?;
        } else {
            let meta = fs.metadata(&entry.path)?;
            push_file(root, entry.path, rel, FileClass::Unknown, meta, out);
        }
    }
    Ok(())
}

fn push_file(
    _root: &Path,
    absolute: PathBuf,
    relative: PathBuf,
    class: FileClass,
    meta: mm_core::fs::FileMeta,
    out: &mut Vec<ScannedFile>,
) {
    out.push(ScannedFile {
        absolute,
        relative,
        class,
        meta,
    });
}

fn make_relative(root: &Path, absolute: &Path) -> PathBuf {
    absolute.strip_prefix(root).unwrap_or(absolute).to_path_buf()
}

fn is_ignored(relative: &Path, file_name: &std::ffi::OsStr, _cfg: &Config) -> bool {
    let name_lossy = file_name.to_string_lossy();
    let patterns = default_ignore_patterns();
    for pat in &patterns {
        if glob_match(pat, &name_lossy) || glob_match(pat, &relative.to_string_lossy()) {
            return true;
        }
    }
    false
}

fn default_ignore_patterns() -> Vec<&'static str> {
    vec![
        ".git",
        "@eaDir",
        "#recycle",
        ".Trash*",
        "lost+found",
        "*.part",
        "*.!qB",
        "*.tmp",
    ]
}

fn glob_match(pat: &str, s: &str) -> bool {
    // Tiny glob: * matches any sequence, ? matches one char.
    let mut pchars = pat.chars().peekable();
    let mut schars = s.chars().peekable();
    while let Some(pc) = pchars.next() {
        match pc {
            '*' => {
                let next = pchars.peek().copied();
                if next.is_none() {
                    return true;
                }
                while let Some(sc) = schars.peek() {
                    if *sc == next.unwrap() {
                        break;
                    }
                    schars.next();
                }
            }
            '?' => {
                schars.next();
            }
            c => {
                if schars.next() != Some(c) {
                    return false;
                }
            }
        }
    }
    schars.next().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matching() {
        assert!(glob_match("*.part", "movie.part"));
        assert!(glob_match(".Trash*", ".Trash-100"));
        assert!(!glob_match("*.part", "movie.mkv"));
    }
}
