//! Parsed filename model.
//!
//! Phase 2 covers movies; the model is structured so TV/music can be added in
//! later phases without changing the movie path.

use mm_core::{Confidence, Field, Source};

/// Options controlling movie parsing.
#[derive(Debug, Clone, Copy)]
pub struct ParseOptions {
    pub min_year: u16,
    pub max_year: u16,
}

impl Default for ParseOptions {
    fn default() -> Self {
        ParseOptions {
            min_year: 1888,
            max_year: 2030,
        }
    }
}

/// Fields extracted from a movie filename.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedMovie {
    pub title: Field<String>,
    pub year: Field<u16>,
    pub edition: Field<String>,
    pub resolution: Field<String>,
    pub source: Field<String>,
    pub video_codec: Field<String>,
    pub audio_format: Field<String>,
    pub hdr: Field<String>,
    pub language: Field<String>,
    pub release_group: Field<String>,
    /// True when the filename looks like an episode marker (e.g. `1x01`).
    /// Always false for Phase 2; populated by the TV parser in Phase 5.
    pub ambiguous_episode_like: bool,
}

impl ParsedMovie {
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` if every field is unknown.
    pub fn is_empty(&self) -> bool {
        !self.title.is_known()
            && !self.year.is_known()
            && !self.edition.is_known()
            && !self.resolution.is_known()
            && !self.source.is_known()
            && !self.video_codec.is_known()
            && !self.audio_format.is_known()
            && !self.hdr.is_known()
            && !self.language.is_known()
            && !self.release_group.is_known()
    }
}

/// Convenience constructors for known fields.
pub fn known<T>(value: T, source: Source, confidence: Confidence) -> Field<T> {
    Field::known(value, source, confidence)
}

pub fn unknown<T>() -> Field<T> {
    Field::unknown(vec![])
}

/// A media parse result. Movies for now; extended in Phase 5/6.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaParse {
    Movie(ParsedMovie),
    Unknown,
}
