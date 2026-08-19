//! Probe result types.
//!
//! HDR cannot come from the container with the Phase 4 crates: `matroska`
//! exposes no `Colour` / `MasteringMetadata`, and `re_mp4` exposes no `colr` /
//! `mdcv`. HDR remains a filename-only signal (`Source::Filename` at best).

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::duration_nanos;

/// Container probe result: video dimensions, optional audio tags, duration.
///
/// `audio` is unused in Phase 4 (`lofty` tag reading is Phase 6) and should
/// remain [`None`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Probe {
    pub video: Option<VideoInfo>,
    /// Embedded audio tags. Always `None` in Phase 4.
    pub audio: Option<AudioTags>,
    #[serde(with = "duration_nanos")]
    pub duration: Option<Duration>,
    pub subtitle_tracks: Vec<SubtitleTrackInfo>,
}

/// Pixel and display dimensions from a container header, plus a codec id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoInfo {
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub display_width: Option<u32>,
    pub display_height: Option<u32>,
    pub codec: Option<String>,
}

impl VideoInfo {
    /// Construct from pixel dimensions; display dims default to absent.
    pub fn from_pixel(width: u32, height: u32) -> Self {
        VideoInfo {
            pixel_width: width,
            pixel_height: height,
            display_width: None,
            display_height: None,
            codec: None,
        }
    }
}

/// Embedded audio tags. Stub: populated in Phase 6 via `lofty`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioTags {
    pub artist: Option<String>,
    pub album_artist: Option<String>,
    pub album: Option<String>,
    pub year: Option<u16>,
    pub track: Option<u16>,
    pub disc: Option<u16>,
    pub title: Option<String>,
    pub genre: Option<String>,
    pub compilation: Option<bool>,
}

/// One subtitle track advertised by the container. Unused in Phase 4.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubtitleTrackInfo {
    pub codec: Option<String>,
    pub language: Option<String>,
}
