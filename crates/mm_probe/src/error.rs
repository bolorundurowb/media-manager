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
