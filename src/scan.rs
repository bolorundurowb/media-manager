//! Discover media folders at any depth under the library root.
//!
//! A directory is a *media folder* when it directly contains at least one
//! video file. Directories without direct videos are treated as containers and
//! are searched recursively, so libraries nested under e.g. `Movies/` are still
//! found. Once a media folder is identified, videos are gathered from it
//! (including nested sub-folders, but never from `subs`/`subtitles`).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::parse::{is_extra_filename, is_sub_path, is_video_path};

/// Maximum depth for discovering nested media folders under the root.
const MAX_DISCOVERY_DEPTH: u8 = 8;
/// Maximum depth for collecting videos inside a single media folder.
const MAX_VIDEO_DEPTH: u8 = 3;

#[derive(Debug, Clone)]
pub struct MediaFolder {
    pub path: PathBuf,
    pub videos: Vec<PathBuf>,
}

pub fn scan_root(root: &Path) -> io::Result<Vec<MediaFolder>> {
    let mut folders = Vec::new();
    if let Err(err) = discover_media_folders(root, 0, &mut folders) {
        tracing::error!(path = %root.display(), error = %err, "cannot read library root");
        return Err(err);
    }
    folders.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(folders)
}

/// Scan a single, explicitly chosen child (used by the multi-selection
/// engine in `crate::multi`, Phase 7): unlike `scan_root`, `path` itself is
/// allowed to directly be a media folder, not only a container of one. If it
/// directly holds a qualifying video it is treated as one `MediaFolder`;
/// otherwise it is searched the same way `scan_root` searches the library
/// root, so a container the user assigned (e.g. a whole "Movies/" folder)
/// still works.
pub(crate) fn scan_child(path: &Path) -> io::Result<Vec<MediaFolder>> {
    let mut out = Vec::new();
    if has_direct_video(path)? {
        let mut videos = Vec::new();
        collect_videos(path, 0, &mut videos)?;
        videos.retain(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| !is_extra_filename(n))
                .unwrap_or(true)
        });
        if !videos.is_empty() {
            out.push(MediaFolder {
                path: path.to_path_buf(),
                videos,
            });
        }
    } else {
        discover_media_folders(path, 0, &mut out)?;
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn discover_media_folders(dir: &Path, depth: u8, out: &mut Vec<MediaFolder>) -> io::Result<()> {
    if depth > MAX_DISCOVERY_DEPTH {
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
                tracing::warn!(error = %err, "skipping unreadable directory entry");
                continue;
            }
        };
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(err) => {
                tracing::warn!(path = %path.display(), error = %err, "skipping inaccessible path");
                continue;
            }
        };
        if !file_type.is_dir() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if name == "subs" || name == "subtitles" {
            continue;
        }

        if has_direct_video(&path)? {
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
            out.push(MediaFolder { path, videos });
        } else {
            discover_media_folders(&path, depth + 1, out)?;
        }
    }
    Ok(())
}

/// True when `dir` directly contains at least one non-extra video file.
fn has_direct_video(dir: &Path) -> io::Result<bool> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(false),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_file = entry.file_type().map(|t| t.is_file()).unwrap_or(false);
        if !is_file || !is_video_path(&path) {
            continue;
        }
        let ok = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| !is_extra_filename(n))
            .unwrap_or(true);
        if ok {
            return Ok(true);
        }
    }
    Ok(false)
}

fn collect_videos(dir: &Path, depth: u8, out: &mut Vec<PathBuf>) -> io::Result<()> {
    if depth > MAX_VIDEO_DEPTH {
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

/// A subtitle associated with a video, and whether it came from a
/// `subs`/`subtitles` tree (one extra nested level is allowed).
#[derive(Debug, Clone)]
pub struct AssociatedSub {
    pub path: PathBuf,
    pub nested: bool,
}

/// Subtitle files associated with `video`: adjacent, then `subs/` /
/// `subtitles/` (and one extra directory under those).
pub fn associated_subtitles(video: &Path) -> Vec<AssociatedSub> {
    let Some(parent) = video.parent() else {
        return Vec::new();
    };
    let Some(stem) = video.file_stem().and_then(|s| s.to_str()) else {
        return Vec::new();
    };

    let mut found = Vec::new();
    let mut seen = std::collections::HashSet::new();

    collect_subs_in_dir(parent, stem, false, &mut found, &mut seen);
    for name in ["subs", "subtitles"] {
        let nested_root = parent.join(name);
        if !nested_root.is_dir() {
            continue;
        }
        collect_subs_in_dir(&nested_root, stem, true, &mut found, &mut seen);
        // One extra nested level (e.g. subs/en/).
        if let Ok(entries) = fs::read_dir(&nested_root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    collect_subs_in_dir(&path, stem, true, &mut found, &mut seen);
                }
            }
        }
    }
    found.sort_by(|a, b| a.path.cmp(&b.path));
    found
}

fn collect_subs_in_dir(
    dir: &Path,
    stem: &str,
    nested: bool,
    out: &mut Vec<AssociatedSub>,
    seen: &mut std::collections::HashSet<PathBuf>,
) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(err) => {
            tracing::warn!(path = %dir.display(), error = %err, "cannot list subtitles");
            return;
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
        if subtitle_suffix(name, stem).is_none() {
            continue;
        }
        if seen.insert(path.clone()) {
            out.push(AssociatedSub { path, nested });
        }
    }
}

/// Every subtitle file under a media folder (adjacent + one-level `subs` tree).
/// Used to log unassociated files that we will not move.
pub fn list_subtitle_files(media_folder: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_all_subs_in_dir(media_folder, &mut out);
    for name in ["subs", "subtitles"] {
        let nested_root = media_folder.join(name);
        if !nested_root.is_dir() {
            continue;
        }
        collect_all_subs_in_dir(&nested_root, &mut out);
        if let Ok(entries) = fs::read_dir(&nested_root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    collect_all_subs_in_dir(&path, &mut out);
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

fn collect_all_subs_in_dir(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if is_sub_path(&path) {
            out.push(path);
        }
    }
}

/// Non-video, non-subtitle files sitting in `dir` (nfo, artwork, xml, unknown).
pub fn extra_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_file = entry.file_type().map(|t| t.is_file()).unwrap_or(false);
        if !is_file {
            continue;
        }
        if is_video_path(&path) || is_sub_path(&path) {
            continue;
        }
        out.push(path);
    }
    out.sort();
    out
}

pub fn is_subs_dir_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n == "subs" || n == "subtitles"
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
