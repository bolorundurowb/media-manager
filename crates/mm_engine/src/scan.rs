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
pub fn scan<F: FileSystem>(
    fs: &F,
    root: &Path,
    cfg: &Config,
) -> Result<Vec<ScannedFile>, std::io::Error> {
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
    absolute
        .strip_prefix(root)
        .unwrap_or(absolute)
        .to_path_buf()
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
    // Tiny glob: * matches any sequence, ? matches one char. Backtracking is
    // required for correctness: `*` greedily trying only the *first*
    // occurrence of the next literal char (with no way to retry further
    // along) fails on inputs with more than one occurrence of that char, e.g.
    // `*.tmp` against `Show.Name.S01E01.mkv.tmp` — exactly the multi-dot
    // filenames download clients produce, which these ignore patterns exist
    // to catch. This uses the standard two-pointer wildcard algorithm: on a
    // literal/`?` mismatch, retry from just after the most recent `*`,
    // consuming one more character of `s` than last time.
    let p: Vec<char> = pat.chars().collect();
    let s: Vec<char> = s.chars().collect();
    let (mut pi, mut si) = (0usize, 0usize);
    let mut star: Option<usize> = None; // index in `p` just after the last '*' seen
    let mut star_si = 0usize; // index in `s` to resume matching from after that '*'

    while si < s.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi + 1);
            star_si = si;
            pi += 1;
        } else if let Some(resume_p) = star {
            pi = resume_p;
            star_si += 1;
            si = star_si;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
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

    #[test]
    fn glob_matching_multi_dot_filenames() {
        // Download-client artefacts commonly have more than one dot before
        // the ignored extension — the matcher must backtrack past the first
        // occurrence of '.' to find one that lets the rest of the pattern
        // match, not give up after trying only the first.
        assert!(glob_match("*.part", "movie.name.part"));
        assert!(glob_match(
            "*.tmp",
            "Show.Name.S01E01.1080p.mkv.tmp"
        ));
        assert!(glob_match("*.!qB", "movie.name.mkv.!qB"));
        assert!(!glob_match("*.tmp", "Show.Name.S01E01.1080p.mkv"));
    }
}
