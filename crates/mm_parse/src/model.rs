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
    /// Copy-number suffix ` (N)` from `RenameNew` (`Movie (2010) (2).mkv`).
    /// Optional; unknown when the name has no such suffix.
    pub copy: Field<u16>,
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

/// Fields extracted from a TV episode filename (Phase 5, §3.2).
///
/// Owned/filled in by the TV pipeline (`crate::tv`); movies and music must not
/// depend on its fields. Placeholder shape only — the TV pipeline is free to
/// add fields here as needed (e.g. multi-episode ranges, absolute numbering).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedEpisode {
    /// Show title.
    pub title: Field<String>,
    /// Show year, if disambiguating two shows with the same title.
    pub year: Field<u16>,
    pub season: Field<u16>,
    /// One or more episode numbers in this file (§6.5 multi-episode files).
    pub episodes: Field<Vec<u16>>,
    pub episode_title: Field<String>,
    pub resolution: Field<String>,
    pub source: Field<String>,
    pub video_codec: Field<String>,
    pub audio_format: Field<String>,
    pub hdr: Field<String>,
    pub language: Field<String>,
    pub release_group: Field<String>,
    /// Copy-number suffix ` (N)` from `RenameNew` (`Show S01E01 (2).mkv`).
    /// Optional; unknown when the name has no such suffix. Mirrors
    /// `ParsedMovie::copy` — the §5.6 re-parseability requirement is generic
    /// across media kinds, not movie-specific.
    pub copy: Field<u16>,
    /// True when the season/episode marker itself was ambiguous (e.g. could
    /// also be read as a year), so downstream can flag for review instead of
    /// guessing.
    pub ambiguous: bool,
}

impl ParsedEpisode {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Fields extracted from a music track (Phase 6, §3.3).
///
/// Owned/filled in by the music pipeline (`crate::music`); the track/artist
/// decision is meant to come from embedded tags, not the filename (§3.3) —
/// this struct is deliberately source-agnostic (`Field<T>` already carries
/// provenance) so it can be filled from either. Placeholder shape only — the
/// music pipeline is free to add fields here as needed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedTrack {
    pub album_artist: Field<String>,
    pub album: Field<String>,
    pub year: Field<u16>,
    pub disc: Field<u16>,
    pub track: Field<u16>,
    pub title: Field<String>,
    /// `true` when the filename looked like `N - A - B` (track number, then
    /// two dash-separated runs) with tags absent — the parser cannot tell
    /// artist from title in that shape (§3.3), so `title` is left `Unknown`
    /// rather than guessed, and this flag lets the engine explain why in a
    /// diagnostic instead of silently rendering wrong fields.
    pub ambiguous_artist_title: bool,
}

impl ParsedTrack {
    pub fn new() -> Self {
        Self::default()
    }
}

/// A media parse result. Movies (Phase 2), TV (Phase 5), and music (Phase 6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaParse {
    Movie(ParsedMovie),
    Episode(ParsedEpisode),
    Track(ParsedTrack),
    Unknown,
}
