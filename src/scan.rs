//! Discover one level of media folders under the library root.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::parse::{is_extra_filename, is_sub_path, is_video_path};

const MAX_DEPTH: u8 = 3;

#[derive(Debug, Clone)]
pub struct MediaFolder {
    pub path: PathBuf,
    pub videos: Vec<PathBuf>,
}

pub fn scan_root(root: &Path) -> io::Result<Vec<MediaFolder>> {
    let mut folders = Vec::new();
    let entries = match fs::read_dir(root) {
        Ok(e) => e,
        Err(err) => {
            tracing::error!(path = %root.display(), error = %err, "cannot read library root");
            return Err(err);
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                tracing::warn!(error = %err, "skipping unreadable directory entry");
                continue;
            }
        };
        let path = entry.path();
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(err) => {
                tracing::warn!(path = %path.display(), error = %err, "skipping inaccessible path");
                continue;
            }
        };
        if !meta.is_dir() {
            continue;
        }
        let mut videos = Vec::new();
        if let Err(err) = collect_videos(&path, 0, &mut videos) {
            tracing::warn!(path = %path.display(), error = %err, "error while scanning folder");
        }
        videos.retain(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| !is_extra_filename(n))
                .unwrap_or(true)
        });
        if videos.is_empty() {
            tracing::debug!(path = %path.display(), "no video files; skipping");
            continue;
        }
        folders.push(MediaFolder { path, videos });
    }

    folders.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(folders)
}

fn collect_videos(dir: &Path, depth: u8, out: &mut Vec<PathBuf>) -> io::Result<()> {
    if depth > MAX_DEPTH {
        return Ok(());
    }
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(err) => {
            tracing::warn!(path = %dir.display(), error = %err, "cannot read directory");
            return Ok(());
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                tracing::warn!(error = %err, "skipping unreadable entry");
                continue;
            }
        };
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(err) => {
                tracing::warn!(path = %path.display(), error = %err, "skipping");
                continue;
            }
        };
        if file_type.is_dir() {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if name == "subs" || name == "subtitles" {
                continue;
            }
            collect_videos(&path, depth + 1, out)?;
        } else if file_type.is_file() && is_video_path(&path) {
            out.push(path);
        }
    }
    Ok(())
}

/// Subtitle files in the same directory as `video` that share its stem.
pub fn adjacent_subtitles(video: &Path) -> Vec<PathBuf> {
    let Some(parent) = video.parent() else {
        return Vec::new();
    };
    let Some(stem) = video.file_stem().and_then(|s| s.to_str()) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    let entries = match fs::read_dir(parent) {
        Ok(e) => e,
        Err(err) => {
            tracing::warn!(path = %parent.display(), error = %err, "cannot list subtitles");
            return found;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !is_sub_path(&path) {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if subtitle_suffix(name, stem).is_some() {
            found.push(path);
        }
    }
    found.sort();
    found
}

/// If `filename` is a subtitle associated with `video_stem`, return the suffix
/// to preserve (e.g. `""`, `.en`, `.en.forced`).
pub fn subtitle_suffix<'a>(filename: &'a str, video_stem: &str) -> Option<&'a str> {
    let dot_ext = filename.rfind('.')?;
    if dot_ext == 0 {
        return None;
    }
    let without_ext = &filename[..dot_ext];
    if without_ext == video_stem {
        return Some("");
    }
    let remainder = without_ext.strip_prefix(video_stem)?;
    if remainder.starts_with('.') {
        Some(remainder)
    } else {
        None
    }
}
