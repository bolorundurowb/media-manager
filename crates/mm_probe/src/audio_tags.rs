//! Audio tag probing via `lofty` (Phase 6, §4, §8.4).
//!
//! Read-only: this crate's `clippy.toml` bans `TaggedFileExt::save_to`, and
//! nothing in this module calls any other mutating/write API either — only
//! `lofty::read_from_path` plus the read-side `Accessor`/`Tag` accessors.
//!
//! A corrupt or non-audio file must never hang or panic the caller. The
//! actual `lofty` parse runs under the same [`crate::run_with_timeout`]
//! watchdog `mp4.rs`/`mkv.rs` use for their parsers (§4 non-panic/non-hang
//! guarantee extended to audio) — `lofty`'s pure-Rust decoders are not known
//! to be immune to the same class of malformed-size-field hangs that were
//! observed in `re_mp4`, and there is no cheaper way to be sure than bounding
//! the call.

use std::path::Path;

use lofty::file::TaggedFileExt;
use lofty::tag::{Accessor, ItemKey, Tag};

use crate::Prober;
use crate::error::ProbeError;
use crate::probe::{AudioTags, Probe};

/// Probes embedded audio tags via `lofty`.
///
/// Extensions match `mm_core::config::Extensions::audio`'s default set
/// (kept in sync manually since `mm_probe` does not depend on `mm_core`'s
/// config for its own extension list — see `mp4.rs`/`mkv.rs` for the same
/// pattern of a small hardcoded `supports` list).
#[derive(Debug, Clone, Default)]
pub struct AudioProber;

impl Prober for AudioProber {
    fn supports(&self, ext: &str) -> bool {
        matches!(
            ext,
            "mp3" | "flac" | "m4a" | "aac" | "ogg" | "opus" | "wav" | "wma" | "alac"
        )
    }

    fn probe(&self, p: &Path) -> Result<Probe, ProbeError> {
        let path = p.to_path_buf();
        let path_for_thread = path.clone();
        // `lofty::error::LoftyError` is `Send + Sync`, so it can cross the
        // watchdog thread boundary directly — no need to stringify inside the
        // closure the way a non-`Send` error would require.
        let outcome = crate::run_with_timeout(move || {
            lofty::read_from_path(&path_for_thread).map(|tagged| tags_from_tagged_file(&tagged))
        });
        match outcome {
            Some(Ok(tags)) => Ok(Probe {
                video: None,
                audio: Some(tags),
                duration: None,
                subtitle_tracks: vec![],
            }),
            Some(Err(lofty_err)) => {
                let detail = lofty_err.to_string();
                if let lofty::error::ErrorKind::Io(io_err) = lofty_err.kind() {
                    // Preserve the underlying `io::ErrorKind` (e.g. `NotFound`)
                    // rather than collapsing every failure into `Parse` —
                    // mirrors `mp4.rs`'s split between `ProbeError::Io` (the
                    // file could not be read at all) and `ProbeError::Parse`
                    // (it was read, but isn't a valid/recognised container).
                    Err(ProbeError::io(path, std::io::Error::new(io_err.kind(), detail)))
                } else {
                    Err(ProbeError::parse(path, detail))
                }
            }
            None => Err(ProbeError::Timeout {
                path,
                timeout_secs: crate::PROBE_TIMEOUT.as_secs(),
            }),
        }
    }
}

/// Probe `p` for audio tags, mapping "no prober for this extension" to
/// [`crate::ProbeOutcome::Unsupported`] the same way [`crate::probe_path`]
/// does for containers.
pub fn probe_audio_path(p: &Path) -> crate::ProbeOutcome {
    let ext = crate::extension_of(p);
    let prober = AudioProber;
    if !prober.supports(&ext) {
        return crate::ProbeOutcome::Unsupported {
            ext,
            reason: crate::CONTAINER_PROBING_UNAVAILABLE,
        };
    }
    match prober.probe(p) {
        Ok(probe) => crate::ProbeOutcome::Probed(probe),
        Err(ProbeError::Unsupported { ext }) => crate::ProbeOutcome::Unsupported {
            ext,
            reason: crate::CONTAINER_PROBING_UNAVAILABLE,
        },
        Err(error) => crate::ProbeOutcome::Failed { error },
    }
}

fn tags_from_tagged_file(tagged: &lofty::file::TaggedFile) -> AudioTags {
    match tagged.primary_tag().or_else(|| tagged.first_tag()) {
        Some(tag) => tags_from_tag(tag),
        None => AudioTags::default(),
    }
}

fn tags_from_tag(tag: &Tag) -> AudioTags {
    AudioTags {
        artist: tag.artist().map(|c| c.into_owned()),
        album_artist: tag
            .get_string(&ItemKey::AlbumArtist)
            .map(str::to_string),
        album: tag.album().map(|c| c.into_owned()),
        year: tag.year().and_then(|y| u16::try_from(y).ok()),
        track: tag.track().and_then(|t| u16::try_from(t).ok()),
        disc: tag.disk().and_then(|d| u16::try_from(d).ok()),
        title: tag.title().map(|c| c.into_owned()),
        genre: tag.genre().map(|c| c.into_owned()),
        compilation: tag
            .get_string(&ItemKey::FlagCompilation)
            .and_then(parse_bool_flag),
    }
}

/// Parse a lofty `FlagCompilation` string item as a boolean. Taggers
/// typically store this as `"1"`/`"0"` (ID3 `TCMP`, MP4 `cpil`), but a couple
/// of textual spellings are accepted defensively since this is read from
/// arbitrary user files, not written by us.
fn parse_bool_flag(s: &str) -> Option<bool> {
    match s.trim() {
        "1" | "true" | "TRUE" | "True" => Some(true),
        "0" | "false" | "FALSE" | "False" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn default_prober_extensions() {
        let p = AudioProber;
        for ext in ["mp3", "flac", "m4a", "aac", "ogg", "opus", "wav", "wma", "alac"] {
            assert!(p.supports(ext), "expected support for .{ext}");
        }
        assert!(!p.supports("mkv"));
        assert!(!p.supports("mp4"));
        assert!(!p.supports("txt"));
    }

    /// A corrupt/non-audio file handed to the real prober must come back as
    /// an error, never panic — the concrete correctness risk this module's
    /// docs call out. `lofty::read_from_path` on garbage bytes is expected to
    /// return `Err` (unrecognised format), which we map to `ProbeError::Parse`;
    /// asserting only "does not panic and returns an error" keeps this test
    /// independent of `lofty`'s exact error text.
    #[test]
    fn corrupt_file_is_probe_failure_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.mp3");
        fs::write(&path, b"this is not an mp3 file at all, just text").unwrap();

        let outcome = probe_audio_path(&path);
        match outcome {
            crate::ProbeOutcome::Failed { .. } => {}
            other => panic!("expected Failed for corrupt input, got {other:?}"),
        }
    }

    #[test]
    fn empty_file_is_probe_failure_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.flac");
        fs::write(&path, b"").unwrap();

        let outcome = probe_audio_path(&path);
        match outcome {
            crate::ProbeOutcome::Failed { .. } => {}
            other => panic!("expected Failed for empty input, got {other:?}"),
        }
    }

    #[test]
    fn unsupported_extension_is_explicit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.avi");
        fs::write(&path, b"not audio").unwrap();
        match probe_audio_path(&path) {
            crate::ProbeOutcome::Unsupported { ext, reason } => {
                assert_eq!(ext, "avi");
                assert_eq!(reason, crate::CONTAINER_PROBING_UNAVAILABLE);
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn missing_file_is_io_failure() {
        let path = Path::new("definitely-missing-media-manager-audio-probe.mp3");
        match probe_audio_path(path) {
            crate::ProbeOutcome::Failed {
                error: ProbeError::Io { .. },
            } => {}
            other => panic!("expected Failed io, got {other:?}"),
        }
    }

    #[test]
    fn bool_flag_parsing() {
        assert_eq!(parse_bool_flag("1"), Some(true));
        assert_eq!(parse_bool_flag("0"), Some(false));
        assert_eq!(parse_bool_flag("true"), Some(true));
        assert_eq!(parse_bool_flag("false"), Some(false));
        assert_eq!(parse_bool_flag("banana"), None);
    }
}
