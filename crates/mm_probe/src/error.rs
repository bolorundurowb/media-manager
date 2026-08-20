//! Probe failures (§4).

use std::io;
use std::path::PathBuf;

use thiserror::Error;

/// An error while probing a media file.
///
/// Unsupported containers that have no pure-Rust prober are usually surfaced
/// as [`crate::ProbeOutcome::Unsupported`] rather than this error, so the
/// engine can emit a Warning and fall back to the filename.
#[derive(Debug, Error)]
pub enum ProbeError {
    /// Filesystem read failed.
    #[error("I/O error while probing {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// Extension is not handled by a Phase 4 prober.
    #[error("unsupported container '.{ext}'")]
    Unsupported { ext: String },
    /// The file looked like a supported container but could not be parsed.
    #[error("failed to parse container {path}: {detail}")]
    Parse { path: PathBuf, detail: String },
    /// Parsing did not finish within the wall-clock budget.
    ///
    /// A handful of malformed box/element size fields have been observed
    /// driving the underlying pure-Rust parsers into unbounded loops (a
    /// single corrupted byte in an ISO-BMFF `stsc` box size is enough to
    /// spin `re_mp4` forever). Probing must never block the pipeline
    /// indefinitely on one hostile or corrupt file, so the parse runs on a
    /// watchdog and this variant is returned if the budget is exceeded.
    #[error("probing {path} did not finish within {timeout_secs}s (corrupt or hostile container?)")]
    Timeout { path: PathBuf, timeout_secs: u64 },
}

impl ProbeError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        ProbeError::Io {
            path: path.into(),
            source,
        }
    }

    pub(crate) fn parse(path: impl Into<PathBuf>, detail: impl Into<String>) -> Self {
        ProbeError::Parse {
            path: path.into(),
            detail: detail.into(),
        }
    }
}
