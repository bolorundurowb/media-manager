//! Provenance and confidence (§2.1).
//!
//! Every parsed field carries its source and confidence. Nothing gets renamed
//! on a guess: a `Source::Fallback`-sourced field can only ever produce
//! [`crate::error::Severity::Warning`] outcomes, never a move.

use serde::{Deserialize, Serialize};

/// Where a field value came from.
///
/// Source preference is a *per-field* table (see [`crate::config::source_rank`]),
/// not enum declaration order. A single global ranking cannot be right:
/// `ContainerHeader` must beat `Nfo` for pixel dimensions, while `Provider` must
/// beat `Filename` for episode titles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// ID3, Vorbis comment, MP4 ilst, Matroska tag.
    EmbeddedTag,
    /// Pixel dimensions from tkhd / TrackEntry.
    ContainerHeader,
    /// Local `.nfo` (Kodi-style).
    Nfo,
    /// TMDB / MusicBrainz (opt-in, default off).
    Provider,
    /// The filename itself.
    Filename,
    /// A parent directory name.
    ParentDir,
    /// A configured default — never justifies a rename.
    Fallback,
}

/// How trustworthy a value is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

impl Confidence {
    /// `true` if this confidence meets or exceeds `minimum`.
    pub fn meets(self, minimum: Confidence) -> bool {
        self >= minimum
    }
}

impl PartialOrd for Confidence {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Confidence {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // declaration order: Low < Medium < High
        (*self as u8).cmp(&(*other as u8))
    }
}

/// A field is either known with provenance, or unknown with a record of what was
/// tried. `Unknown` carries the attempted sources so the "explain what could not
/// be determined" requirement (spec §24 point 3) has material to render.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "data")]
pub enum Field<T> {
    Known {
        value: T,
        source: Source,
        confidence: Confidence,
    },
    Unknown {
        attempted: Vec<Source>,
    },
}

impl<T> Default for Field<T> {
    fn default() -> Self {
        Field::Unknown { attempted: vec![] }
    }
}

impl<T> Field<T> {
    pub fn known(value: T, source: Source, confidence: Confidence) -> Self {
        Field::Known {
            value,
            source,
            confidence,
        }
    }

    pub fn unknown(attempted: Vec<Source>) -> Self {
        Field::Unknown { attempted }
    }

    /// `true` if this field is known.
    pub fn is_known(&self) -> bool {
        matches!(self, Field::Known { .. })
    }

    /// Borrow the value if known.
    pub fn as_value(&self) -> Option<&T> {
        match self {
            Field::Known { value, .. } => Some(value),
            Field::Unknown { .. } => None,
        }
    }

    /// The source if known, else `None`.
    pub fn source(&self) -> Option<Source> {
        match self {
            Field::Known { source, .. } => Some(*source),
            Field::Unknown { .. } => None,
        }
    }

    /// The confidence if known, else `None`.
    pub fn confidence(&self) -> Option<Confidence> {
        match self {
            Field::Known { confidence, .. } => Some(*confidence),
            Field::Unknown { .. } => None,
        }
    }

    /// `true` if this field comes from [`Source::Fallback`].
    ///
    /// Per §2.1 a fallback-sourced field can only produce
    /// [`crate::error::Severity::Warning`] outcomes, never a move.
    pub fn is_fallback(&self) -> bool {
        matches!(self, Field::Known { source: Source::Fallback, .. })
    }

    /// Transform the inner value of a `Known` field, preserving provenance.
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Field<U> {
        match self {
            Field::Known {
                value,
                source,
                confidence,
            } => Field::Known {
                value: f(value),
                source,
                confidence,
            },
            Field::Unknown { attempted } => Field::Unknown { attempted },
        }
    }

    /// Convert to an `Option<T>` (known value, else `None`).
    pub fn ok(self) -> Option<T> {
        match self {
            Field::Known { value, .. } => Some(value),
            Field::Unknown { .. } => None,
        }
    }

    /// Map over the known value by reference.
    pub fn as_ref(&self) -> Field<&T> {
        match self {
            Field::Known {
                value,
                source,
                confidence,
            } => Field::Known {
                value,
                source: *source,
                confidence: *confidence,
            },
            Field::Unknown { attempted } => Field::Unknown {
                attempted: attempted.clone(),
            },
        }
    }
}

/// Minimum-source gate used by the router (§2.1). A field below the minimum
/// rank produces [`crate::Readiness::NeedsReview`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MinSource {
    Filename,
    ParentDir,
    Nfo,
    Provider,
    EmbeddedTag,
    ContainerHeader,
}

impl Default for MinSource {
    fn default() -> Self {
        // §7 config: min_confidence = "medium"; the source floor is "at least
        // the filename told us something".
        MinSource::Filename
    }
}
