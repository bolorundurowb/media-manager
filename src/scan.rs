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
    /// True when this scan unit is a loose video file rather than a
    /// directory. Loose files are parsed from their own filename and their
    /// parent (the library root) must never be treated as removable.
    pub loose: bool,
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
    if path.is_file() {
        let is_media = is_video_path(path)
            && path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| !is_extra_filename(n))
                .unwrap_or(true);
        return Ok(if is_media {
            vec![MediaFolder {
                path: path.to_path_buf(),
                videos: vec![path.to_path_buf()],
                loose: true,
            }]
        } else {
            Vec::new()
        });
    }

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
                loose: false,
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

        match has_direct_video(&path) {
            Ok(true) => {
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
                out.push(MediaFolder {
                    path,
                    videos,
                    loose: false,
                });
            }
            Ok(false) => discover_media_folders(&path, depth + 1, out)?,
            Err(err) => {
                tracing::warn!(path = %path.display(), error = %err, "cannot inspect directory");
            }
        }
    }
    Ok(())
}

/// True when `dir` directly contains at least one non-extra video file.
fn has_direct_video(dir: &Path) -> io::Result<bool> {
    let entries = fs::read_dir(dir)?;
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
    /// Dest language suffix including the leading dot (`""`, `.en`, `.en.forced`).
    pub suffix: String,
    /// RARBG-style track index (`2` in `2_English.srt`), used only to
    /// disambiguate when two files of the same language would collide.
    pub track: Option<u32>,
}

/// Subtitle files associated with `video`: adjacent, then `subs/` /
/// `subtitles/` (and one extra directory under those).
///
/// A nested directory whose name matches the video stem (the RARBG
/// `subs/<release-name>/` layout) associates every subtitle in it whose
/// filename maps to a known language (`2_English.srt` → `.en`), not just
/// files that repeat the stem.
pub fn associated_subtitles(video: &Path) -> Vec<AssociatedSub> {
    let Some(parent) = video.parent() else {
        return Vec::new();
    };
    let Some(stem) = video.file_stem().and_then(|s| s.to_str()) else {
        return Vec::new();
    };

    let mut found = Vec::new();
    let mut seen = std::collections::HashSet::new();

    collect_subs_in_dir(parent, stem, false, false, &mut found, &mut seen);
    for name in ["subs", "subtitles"] {
        let nested_root = parent.join(name);
        if !nested_root.is_dir() {
            continue;
        }
        collect_subs_in_dir(&nested_root, stem, true, false, &mut found, &mut seen);
        // One extra nested level (e.g. subs/en/ or subs/<video-stem>/).
        if let Ok(entries) = fs::read_dir(&nested_root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let stem_folder = dir_matches_stem(&path, stem);
                    collect_subs_in_dir(&path, stem, true, stem_folder, &mut found, &mut seen);
                }
            }
        }
    }
    found.sort_by(|a, b| a.path.cmp(&b.path));
    found
}

fn dir_matches_stem(dir: &Path, stem: &str) -> bool {
    dir.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.eq_ignore_ascii_case(stem))
        .unwrap_or(false)
}

fn collect_subs_in_dir(
    dir: &Path,
    stem: &str,
    nested: bool,
    stem_folder: bool,
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
        let (suffix, track) = if let Some(s) = subtitle_suffix(name, stem) {
            (s.to_string(), None)
        } else if stem_folder {
            match language_subtitle_suffix(name) {
                Some(parts) => parts,
                None => continue,
            }
        } else {
            continue;
        };
        if seen.insert(path.clone()) {
            out.push(AssociatedSub {
                path,
                nested,
                suffix,
                track,
            });
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
    if without_ext.eq_ignore_ascii_case(video_stem) {
        return Some("");
    }
    if without_ext.len() > video_stem.len() {
        if let Some(prefix) = without_ext.get(..video_stem.len()) {
            if prefix.eq_ignore_ascii_case(video_stem) {
                let remainder = &without_ext[video_stem.len()..];
                if remainder.starts_with('.') {
                    return Some(remainder);
                }
            }
        }
    }
    None
}

/// Map a RARBG-style sidecar name (`2_English.srt`, `3_English.forced.srt`)
/// to a Jellyfin language suffix and optional track number.
///
/// Unknown names return `None` rather than inventing a language.
pub(crate) fn language_subtitle_suffix(filename: &str) -> Option<(String, Option<u32>)> {
    let dot_ext = filename.rfind('.')?;
    if dot_ext == 0 {
        return None;
    }
    let without_ext = &filename[..dot_ext];
    let (track, rest) = split_track_prefix(without_ext);
    let mut tokens = rest
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty());
    let first = tokens.next()?;
    let lang = language_code(first)?;
    let mut flags = Vec::new();
    for token in tokens {
        if let Some(flag) = subtitle_flag(token) {
            if !flags.contains(&flag) {
                flags.push(flag);
            }
        }
    }
    let mut suffix = format!(".{lang}");
    for flag in flags {
        suffix.push('.');
        suffix.push_str(flag);
    }
    Some((suffix, track))
}

fn split_track_prefix(name: &str) -> (Option<u32>, &str) {
    let digits = name.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 || digits >= name.len() {
        return (None, name);
    }
    let sep = name.as_bytes()[digits];
    if sep != b'_' && sep != b'-' {
        return (None, name);
    }
    match name[..digits].parse() {
        Ok(n) => (Some(n), &name[digits + 1..]),
        Err(_) => (None, name),
    }
}

fn language_code(token: &str) -> Option<&'static str> {
    let key = token.to_ascii_lowercase();
    LANGUAGES
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, code)| *code)
}

fn subtitle_flag(token: &str) -> Option<&'static str> {
    match token.to_ascii_lowercase().as_str() {
        "forced" | "foreign" => Some("forced"),
        "sdh" => Some("sdh"),
        "hi" => Some("hi"),
        "cc" => Some("cc"),
        "default" => Some("default"),
        _ => None,
    }
}

/// Names and ISO 639-1 codes. Matched as whole tokens; unknown names are
/// left unassociated rather than guessed.
const LANGUAGES: &[(&str, &str)] = &[
    ("english", "en"),
    ("eng", "en"),
    ("en", "en"),
    ("spanish", "es"),
    ("castilian", "es"),
    ("spa", "es"),
    ("es", "es"),
    ("french", "fr"),
    ("fre", "fr"),
    ("fra", "fr"),
    ("fr", "fr"),
    ("german", "de"),
    ("ger", "de"),
    ("deu", "de"),
    ("de", "de"),
    ("italian", "it"),
    ("ita", "it"),
    ("it", "it"),
    ("portuguese", "pt"),
    ("brazilian", "pt"),
    ("por", "pt"),
    ("pt", "pt"),
    ("dutch", "nl"),
    ("dut", "nl"),
    ("nld", "nl"),
    ("nl", "nl"),
    ("polish", "pl"),
    ("pol", "pl"),
    ("pl", "pl"),
    ("russian", "ru"),
    ("rus", "ru"),
    ("ru", "ru"),
    ("japanese", "ja"),
    ("jpn", "ja"),
    ("jp", "ja"),
    ("ja", "ja"),
    ("chinese", "zh"),
    ("mandarin", "zh"),
    ("cantonese", "zh"),
    ("chi", "zh"),
    ("zho", "zh"),
    ("zh", "zh"),
    ("korean", "ko"),
    ("kor", "ko"),
    ("ko", "ko"),
    ("arabic", "ar"),
    ("ara", "ar"),
    ("ar", "ar"),
    ("hindi", "hi"),
    ("hin", "hi"),
    ("swedish", "sv"),
    ("swe", "sv"),
    ("sv", "sv"),
    ("norwegian", "no"),
    ("nor", "no"),
    ("no", "no"),
    ("danish", "da"),
    ("dan", "da"),
    ("da", "da"),
    ("finnish", "fi"),
    ("fin", "fi"),
    ("fi", "fi"),
    ("greek", "el"),
    ("gre", "el"),
    ("ell", "el"),
    ("el", "el"),
    ("turkish", "tr"),
    ("tur", "tr"),
    ("tr", "tr"),
    ("hebrew", "he"),
    ("heb", "he"),
    ("he", "he"),
    ("czech", "cs"),
    ("cze", "cs"),
    ("ces", "cs"),
    ("cs", "cs"),
    ("hungarian", "hu"),
    ("hun", "hu"),
    ("hu", "hu"),
    ("romanian", "ro"),
    ("rum", "ro"),
    ("ron", "ro"),
    ("ro", "ro"),
    ("thai", "th"),
    ("tha", "th"),
    ("th", "th"),
    ("vietnamese", "vi"),
    ("vie", "vi"),
    ("vi", "vi"),
    ("indonesian", "id"),
    ("ind", "id"),
    ("id", "id"),
    ("ukrainian", "uk"),
    ("ukr", "uk"),
    ("uk", "uk"),
    ("bulgarian", "bg"),
    ("bul", "bg"),
    ("bg", "bg"),
    ("croatian", "hr"),
    ("hrv", "hr"),
    ("hr", "hr"),
    ("serbian", "sr"),
    ("srp", "sr"),
    ("sr", "sr"),
    ("slovak", "sk"),
    ("slo", "sk"),
    ("slk", "sk"),
    ("sk", "sk"),
    ("slovenian", "sl"),
    ("slv", "sl"),
    ("sl", "sl"),
    ("catalan", "ca"),
    ("cat", "ca"),
    ("ca", "ca"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stem_prefix_suffix_is_preserved() {
        assert_eq!(
            subtitle_suffix("Show.S01E01.en.srt", "Show.S01E01"),
            Some(".en")
        );
        assert_eq!(subtitle_suffix("Show.S01E01.srt", "Show.S01E01"), Some(""));
        assert_eq!(subtitle_suffix("3_English.srt", "Show.S01E01"), None);
    }

    #[test]
    fn numbered_language_files_map_to_iso_suffix() {
        assert_eq!(
            language_subtitle_suffix("2_English.srt"),
            Some((".en".into(), Some(2)))
        );
        assert_eq!(
            language_subtitle_suffix("3_English.forced.srt"),
            Some((".en.forced".into(), Some(3)))
        );
        assert_eq!(
            language_subtitle_suffix("4_Spanish.srt"),
            Some((".es".into(), Some(4)))
        );
        assert_eq!(
            language_subtitle_suffix("English.srt"),
            Some((".en".into(), None))
        );
        assert_eq!(language_subtitle_suffix("unrelated.srt"), None);
        assert_eq!(language_subtitle_suffix("3_UnknownTongue.srt"), None);
    }
}
