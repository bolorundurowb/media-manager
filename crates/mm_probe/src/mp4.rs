//! ISO-BMFF / QuickTime probing via `re_mp4`.
//!
//! ## Crate decision (PLAN.md §4 / §15)
//!
//! Keep **`re_mp4`** (MIT, maintained Rerun fork of `mp4`). `deny.toml` already
//! allows MPL-2.0, so `mp4parse` and `symphonia-format-isomp4` were acceptable
//! fallbacks, but `re_mp4` 0.4 exposes `tkhd` display dimensions
//! (`Track::{width,height}`) and sample-description coded dims (`Avc1Box`,
//! `HevcBox`, `Av01Box`, `Vp08Box`, `Vp09Box`). That is sufficient for
//! DAR-corrected banding. MPL is acceptable; switching crates solely to pick
//! it up is not justified.
//!
//! Limitation: `Mp4::read` requires an `ftyp` box
//! (`Error::BoxNotFound(FtypBox)`), so a `.mov` that lacks `ftyp` fails to
//! parse. That surfaces as [`crate::ProbeError::Parse`], not silent
//! mis-labelling; the engine still has filename fallback.
//!
//! HDR cannot be read from ISO-BMFF with this crate (`colr` / `mdcv` are not
//! exposed). HDR remains filename-only (`Source::Filename`).

use std::path::Path;
use std::time::Duration;

use re_mp4::{Mp4, StsdBoxContent, Track, TrackKind};

use crate::Prober;
use crate::error::ProbeError;
use crate::probe::{Probe, VideoInfo};

/// Probes `.mp4` / `.m4v` / `.mov` via [`re_mp4::Mp4::read_bytes`].
#[derive(Debug, Clone, Default)]
pub struct Mp4Prober;

impl Prober for Mp4Prober {
    fn supports(&self, ext: &str) -> bool {
        matches!(ext, "mp4" | "m4v" | "mov")
    }

    fn probe(&self, p: &Path) -> Result<Probe, ProbeError> {
        let bytes = std::fs::read(p).map_err(|e| ProbeError::io(p, e))?;
        let path = p.to_path_buf();
        // `re_mp4` has been observed spinning forever on a single corrupted
        // box-size byte (a truncated/hostile `stsc` box), so the actual
        // parse runs under a watchdog rather than directly here (§4
        // non-panic guarantee extended to non-hang).
        let outcome = crate::run_with_timeout(move || {
            Mp4::read_bytes(&bytes)
                .map(|mp4| {
                    let video_track = video_track_of(&mp4);
                    let video = video_track.map(|t| video_info(&mp4, t));
                    let duration = video_track
                        .and_then(track_duration)
                        .or_else(|| mvhd_duration(&mp4));
                    Probe {
                        video,
                        audio: None,
                        duration,
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

fn video_track_of(mp4: &Mp4) -> Option<&Track> {
    let tracks = mp4.tracks();
    tracks
        .values()
        .find(|t| t.kind == Some(TrackKind::Video))
        .or_else(|| tracks.values().find(|t| t.width > 0 && t.height > 0))
}

fn video_info(mp4: &Mp4, track: &Track) -> VideoInfo {
    let tkhd_w = u32::from(track.width);
    let tkhd_h = u32::from(track.height);
    let sample = sample_dims(&track.trak(mp4).mdia.minf.stbl.stsd.contents);
    let codec = track.codec_string(mp4);
    match sample {
        Some((sw, sh)) if sw > 0 && sh > 0 => VideoInfo {
            pixel_width: sw,
            pixel_height: sh,
            display_width: (tkhd_w > 0).then_some(tkhd_w),
            display_height: (tkhd_h > 0).then_some(tkhd_h),
            codec,
        },
        _ => VideoInfo {
            pixel_width: tkhd_w,
            pixel_height: tkhd_h,
            display_width: None,
            display_height: None,
            codec,
        },
    }
}

fn sample_dims(contents: &StsdBoxContent) -> Option<(u32, u32)> {
    match contents {
        StsdBoxContent::Avc1(b) => Some((u32::from(b.width), u32::from(b.height))),
        StsdBoxContent::Hvc1(b) | StsdBoxContent::Hev1(b) => {
            Some((u32::from(b.width), u32::from(b.height)))
        }
        StsdBoxContent::Av01(b) => Some((u32::from(b.width), u32::from(b.height))),
        StsdBoxContent::Vp08(b) => Some((u32::from(b.width), u32::from(b.height))),
        StsdBoxContent::Vp09(b) => Some((u32::from(b.width), u32::from(b.height))),
        StsdBoxContent::Mp4a(_) | StsdBoxContent::Tx3g(_) | StsdBoxContent::Unknown(_) => None,
    }
}

fn track_duration(track: &Track) -> Option<Duration> {
    if track.timescale == 0 || track.duration == 0 {
        return None;
    }
    Some(Duration::from_secs_f64(
        track.duration as f64 / track.timescale as f64,
    ))
}

fn mvhd_duration(mp4: &Mp4) -> Option<Duration> {
    let mvhd = &mp4.moov.mvhd;
    if mvhd.timescale == 0 || mvhd.duration == 0 {
        return None;
    }
    Some(Duration::from_secs_f64(
        mvhd.duration as f64 / f64::from(mvhd.timescale),
    ))
}
