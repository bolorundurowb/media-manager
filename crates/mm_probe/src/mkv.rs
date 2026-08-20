//! Matroska / WebM probing via the `matroska` crate.
//!
//! HDR cannot come from the container: `matroska` exposes no `Colour` /
//! `MasteringMetadata`. HDR remains filename-only (`Source::Filename`).

use std::path::Path;

use matroska::{Settings, Track};

use crate::Prober;
use crate::error::ProbeError;
use crate::probe::{Probe, VideoInfo};

/// Probes `.mkv` / `.webm` via [`matroska::open`].
#[derive(Debug, Clone, Default)]
pub struct MatroskaProber;

impl Prober for MatroskaProber {
    fn supports(&self, ext: &str) -> bool {
        matches!(ext, "mkv" | "webm")
    }

    fn probe(&self, p: &Path) -> Result<Probe, ProbeError> {
        // `matroska` reports truncated files as I/O (`UnexpectedEof`). Open first
        // so missing/unreadable paths stay [`ProbeError::Io`], while a readable
        // but invalid container is [`ProbeError::Parse`].
        std::fs::File::open(p).map_err(|e| ProbeError::io(p, e))?;
        let path = p.to_path_buf();
        let path_for_parse = path.clone();
        // Same watchdog as the MP4 prober: a corrupted element-size field
        // should never be able to hang the pipeline forever, even if this
        // parser has not been observed doing so on the current fixtures.
        let outcome = crate::run_with_timeout(move || {
            matroska::open(&path_for_parse)
                .map(|mkv| {
                    let video = mkv.video_tracks().find_map(video_info_from_track);
                    Probe {
                        video,
                        audio: None,
                        duration: mkv.info.duration,
                        subtitle_tracks: vec![],
                    }
                })
                .map_err(|e| e.to_string())
        });
        match outcome {
            Some(Ok(probe)) => Ok(probe),
            Some(Err(detail)) => Err(ProbeError::parse(path, detail)),
            None => Err(ProbeError::Timeout {
                path,
                timeout_secs: crate::PROBE_TIMEOUT.as_secs(),
            }),
        }
    }
}

fn video_info_from_track(track: &Track) -> Option<VideoInfo> {
    let Settings::Video(v) = &track.settings else {
        return None;
    };
    let pixel_width = u32_dim(v.pixel_width);
    let pixel_height = u32_dim(v.pixel_height);
    if pixel_width == 0 || pixel_height == 0 {
        return None;
    }
    let codec = if track.codec_id.is_empty() {
        None
    } else {
        Some(track.codec_id.clone())
    };
    Some(VideoInfo {
        pixel_width,
        pixel_height,
        display_width: v.display_width.map(u32_dim).filter(|&w| w > 0),
        display_height: v.display_height.map(u32_dim).filter(|&h| h > 0),
        codec,
    })
}

fn u32_dim(v: u64) -> u32 {
    v.min(u64::from(u32::MAX)) as u32
}
