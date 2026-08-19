//! File classification (§2.3).
//!
//! By extension, plus filename heuristics for artwork and metadata. `Unknown`
//! files are never moved and always reported (spec §3.4).

use serde::{Deserialize, Serialize};

/// What kind of media file this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileClass {
    Video,
    Audio,
    Subtitle,
    Artwork,
    Metadata,
    Unknown,
}

impl FileClass {
    pub fn as_str(self) -> &'static str {
        match self {
            FileClass::Video => "video",
            FileClass::Audio => "audio",
            FileClass::Subtitle => "subtitle",
            FileClass::Artwork => "artwork",
            FileClass::Metadata => "metadata",
            FileClass::Unknown => "unknown",
        }
    }
}

/// Top-level media kind selected by the user for a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Movies,
    Tv,
    Music,
}

impl MediaKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MediaKind::Movies => "movies",
            MediaKind::Tv => "tv",
            MediaKind::Music => "music",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "movies" | "movie" => Some(MediaKind::Movies),
            "tv" | "show" | "shows" => Some(MediaKind::Tv),
            "music" => Some(MediaKind::Music),
            _ => None,
        }
    }
}

/// Filename heuristics for artwork (§2.3).
///
/// Matched against the file stem, case-insensitively, after stripping
/// resolution/quality suffixes. The set is configurable via
/// [`crate::config::Config`].
pub fn is_artwork_stem(stem: &str) -> bool {
    let lower = stem.to_ascii_lowercase();
    // exact, or starts-with-when-followed-by-separator
    const NAMES: &[&str] = &[
        "cover",
        "folder",
        "poster",
        "fanart",
        "backdrop",
        "banner",
        "album",
        "front",
        "thumb",
        "landscape",
        "clearart",
        "disc",
        "cd",
    ];
    if NAMES.contains(&lower.as_str()) {
        return true;
    }
    // "poster-movie" / "fanart_01" style
    for n in NAMES {
        if lower.starts_with(n)
            && (lower.len() == n.len()
                || matches!(lower.as_bytes()[n.len()], b'-' | b'_' | b'.' | b' '))
        {
            return true;
        }
    }
    false
}

/// Filename heuristics for metadata sidecars (§2.3).
pub fn is_metadata_ext(ext_lower: &str) -> bool {
    matches!(ext_lower, "nfo" | "xml" | "json" | "cue" | "m3u" | "m3u8")
}
