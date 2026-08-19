//! Container/tag probing (§4).
//!
//! Phase 4 extracts video pixel/display dimensions from Matroska (`matroska`)
//! and ISO-BMFF (`re_mp4`). Audio tag reading via `lofty` is Phase 6 — this
//! crate must never call `lofty::TaggedFileExt::save_to` (banned in
//! `clippy.toml`). `Probe.audio` is always `None` here.
//!
//! HDR cannot come from the container with these crates (`matroska` has no
//! Colour/MasteringMetadata; `re_mp4` has no `colr`/`mdcv`). Do not invent HDR
//! from the container; it remains filename-only.

mod cache;
mod error;
mod mkv;
mod mp4;
mod probe;
mod resolution;

#[cfg(test)]
mod container_gen;

use std::path::Path;

pub use cache::{CacheKey, ProbeCache};
pub use error::ProbeError;
pub use mkv::MatroskaProber;
pub use mp4::Mp4Prober;
pub use probe::{AudioTags, Probe, SubtitleTrackInfo, VideoInfo};
pub use resolution::{ResolutionBands, corrected_dims, label_resolution, label_resolution_with};

/// Stable reason string for [`ProbeOutcome::Unsupported`].
///
/// The engine should emit a Warning diagnostic with this text (§4) and fall
/// back to filename-derived resolution (`Source::Filename`).
pub const CONTAINER_PROBING_UNAVAILABLE: &str =
    "resolution undetectable / container-based detection unavailable";

/// Reads container headers without writing to the file.
pub trait Prober: Send + Sync {
    /// `ext` is the lowercase extension **without** a leading dot (`mkv`, `mp4`).
    fn supports(&self, ext: &str) -> bool;
    /// Probe `p`. Must not mutate the file.
    fn probe(&self, p: &Path) -> Result<Probe, ProbeError>;
}

/// Dispatches by lowercase extension to the Matroska or ISO-BMFF prober.
#[derive(Debug, Clone, Default)]
pub struct DefaultProber {
    mkv: MatroskaProber,
    mp4: Mp4Prober,
}

impl DefaultProber {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Prober for DefaultProber {
    fn supports(&self, ext: &str) -> bool {
        self.mkv.supports(ext) || self.mp4.supports(ext)
    }

    fn probe(&self, p: &Path) -> Result<Probe, ProbeError> {
        let ext = extension_of(p);
        if self.mkv.supports(&ext) {
            self.mkv.probe(p)
        } else if self.mp4.supports(&ext) {
            self.mp4.probe(p)
        } else {
            Err(ProbeError::Unsupported { ext })
        }
    }
}

/// Outcome of [`probe_path`]: probed, no prober, or hard failure.
///
/// Filename fallback lives in the engine. This crate makes the unavailable
/// case explicit so the engine can emit a Warning diagnostic.
#[derive(Debug)]
pub enum ProbeOutcome {
    Probed(Probe),
    /// Container has no pure-Rust prober; caller must fall back to filename
    /// and emit a diagnostic (§4).
    Unsupported {
        ext: String,
        reason: &'static str,
    },
    Failed {
        error: ProbeError,
    },
}

/// Probe `p` by extension, mapping unsupported containers to
/// [`ProbeOutcome::Unsupported`] rather than a parse error.
pub fn probe_path(p: &Path) -> ProbeOutcome {
    let ext = extension_of(p);
    let prober = DefaultProber::new();
    if !prober.supports(&ext) {
        return ProbeOutcome::Unsupported {
            ext,
            reason: CONTAINER_PROBING_UNAVAILABLE,
        };
    }
    match prober.probe(p) {
        Ok(probe) => ProbeOutcome::Probed(probe),
        Err(ProbeError::Unsupported { ext }) => ProbeOutcome::Unsupported {
            ext,
            reason: CONTAINER_PROBING_UNAVAILABLE,
        },
        Err(error) => ProbeOutcome::Failed { error },
    }
}

pub(crate) fn extension_of(p: &Path) -> String {
    p.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

/// Serde helper: `Option<Duration>` as optional nanoseconds.
pub(crate) mod duration_nanos {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;

    pub fn serialize<S>(d: &Option<Duration>, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        d.map(|d| d.as_nanos() as u64).serialize(s)
    }

    pub fn deserialize<'de, D>(d: D) -> Result<Option<Duration>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let n = Option::<u64>::deserialize(d)?;
        Ok(n.map(Duration::from_nanos))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::container_gen::{minimal_mkv, minimal_mp4};
    use std::fs;
    use std::path::PathBuf;

    fn testdata_media() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/media")
    }

    fn write_committed_fixtures() -> PathBuf {
        let dir = testdata_media();
        fs::create_dir_all(&dir).expect("testdata/media");
        fs::write(
            dir.join("scope_1080p.mp4"),
            minimal_mp4(1920, 800, 1920, 800),
        )
        .expect("scope_1080p.mp4");
        fs::write(
            dir.join("uhd_scope.mp4"),
            minimal_mp4(3840, 1600, 3840, 1600),
        )
        .expect("uhd_scope.mp4");
        fs::write(dir.join("dvd_dar.mp4"), minimal_mp4(720, 576, 1024, 576)).expect("dvd_dar.mp4");
        fs::write(
            dir.join("dvd_dar.mkv"),
            minimal_mkv(720, 576, Some(1024), Some(576)),
        )
        .expect("dvd_dar.mkv");
        fs::write(
            dir.join("plain_1080p.mkv"),
            minimal_mkv(1920, 1080, None, None),
        )
        .expect("plain_1080p.mkv");
        dir
    }

    fn digest(path: &Path) -> Vec<u8> {
        fs::read(path).expect("read for digest")
    }

    fn assert_probed_label(path: &Path, expected: &str) -> Probe {
        let before = digest(path);
        let outcome = probe_path(path);
        let after = digest(path);
        assert_eq!(before, after, "probe must not mutate {}", path.display());
        let ProbeOutcome::Probed(probe) = outcome else {
            panic!("expected Probed for {}, got {outcome:?}", path.display());
        };
        let video = probe.video.as_ref().expect("video track");
        assert_eq!(
            label_resolution(video).as_deref(),
            Some(expected),
            "label for {}",
            path.display()
        );
        assert!(probe.audio.is_none(), "Phase 4 leaves audio unused");
        probe
    }

    #[test]
    fn default_prober_dispatches_by_extension() {
        let p = DefaultProber::new();
        assert!(p.supports("mkv"));
        assert!(p.supports("webm"));
        assert!(p.supports("mp4"));
        assert!(p.supports("m4v"));
        assert!(p.supports("mov"));
        assert!(!p.supports("avi"));
        assert!(!p.supports("wmv"));
        assert!(!p.supports("ts"));
        assert!(!p.supports("m2ts"));
        assert!(!p.supports("flac"));
    }

    #[test]
    fn unsupported_containers_are_explicit() {
        let dir = tempfile::tempdir().unwrap();
        for (name, ext) in [
            ("clip.avi", "avi"),
            ("clip.wmv", "wmv"),
            ("clip.ts", "ts"),
            ("clip.m2ts", "m2ts"),
        ] {
            let path = dir.path().join(name);
            fs::write(&path, b"not a real container").unwrap();
            match probe_path(&path) {
                ProbeOutcome::Unsupported { ext: got, reason } => {
                    assert_eq!(got, ext);
                    assert_eq!(reason, CONTAINER_PROBING_UNAVAILABLE);
                }
                other => panic!("expected Unsupported for .{ext}, got {other:?}"),
            }
        }
    }

    #[test]
    fn mp4_fixtures_parse_and_band() {
        let dir = write_committed_fixtures();
        let scope = assert_probed_label(&dir.join("scope_1080p.mp4"), "1080p");
        let v = scope.video.unwrap();
        assert_eq!((v.pixel_width, v.pixel_height), (1920, 800));
        assert_eq!(corrected_dims(&v), (1920, 800));

        let uhd = assert_probed_label(&dir.join("uhd_scope.mp4"), "2160p");
        let v = uhd.video.unwrap();
        assert_eq!((v.pixel_width, v.pixel_height), (3840, 1600));

        let dvd = assert_probed_label(&dir.join("dvd_dar.mp4"), "576p");
        let v = dvd.video.unwrap();
        assert_eq!((v.pixel_width, v.pixel_height), (720, 576));
        assert_eq!(v.display_width, Some(1024));
        assert_eq!(v.display_height, Some(576));
        assert_eq!(corrected_dims(&v), (1024, 576));
    }

    #[test]
    fn mkv_fixtures_parse_and_band() {
        let dir = write_committed_fixtures();
        let dvd = assert_probed_label(&dir.join("dvd_dar.mkv"), "576p");
        let v = dvd.video.unwrap();
        assert_eq!((v.pixel_width, v.pixel_height), (720, 576));
        assert_eq!(v.display_width, Some(1024));
        assert_eq!(v.display_height, Some(576));

        let plain = assert_probed_label(&dir.join("plain_1080p.mkv"), "1080p");
        let v = plain.video.unwrap();
        assert_eq!((v.pixel_width, v.pixel_height), (1920, 1080));
    }

    #[test]
    fn corrupt_supported_extension_is_failed_not_unsupported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.mkv");
        fs::write(&path, b"this is not ebml").unwrap();
        match probe_path(&path) {
            ProbeOutcome::Failed {
                error: ProbeError::Parse { .. },
            } => {}
            other => panic!("expected Failed parse, got {other:?}"),
        }
    }

    #[test]
    fn missing_file_is_io_failure() {
        let path = PathBuf::from("definitely-missing-media-manager-probe.mp4");
        match probe_path(&path) {
            ProbeOutcome::Failed {
                error: ProbeError::Io { .. },
            } => {}
            other => panic!("expected Failed io, got {other:?}"),
        }
    }
}
