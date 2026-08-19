//! Movie parsing (§3, Phase 1).
//!
//! Orchestrates the extractor pipeline in priority order and assigns the
//! leading residual as the title. `mm-parse` is pure and has no I/O (not even
//! a clock read): the caller supplies the valid year range, which in practice
//! means `mm-engine` computing `current_year + 2` from wall-clock and passing
//! it in — keeping this crate deterministic and independently testable.

use mm_core::{Confidence, Field, Source};

use crate::extractors::{
    detect_release_group, extract_episode_marker, extract_year, find_vocab,
};
use crate::mask::Consumption;
use crate::normalize::normalize_stem;
use crate::vocab;

/// The valid year range and any other caller-supplied parsing parameters.
#[derive(Debug, Clone, Copy)]
pub struct ParseOptions {
    pub min_year: u16,
    pub max_year: u16,
}

impl Default for ParseOptions {
    fn default() -> Self {
        // Permissive default for callers that don't care to be precise; the
        // engine should override `max_year` with `current_year + 2` (§3.2).
        ParseOptions {
            min_year: 1888,
            max_year: 2999,
        }
    }
}

/// The parsed fields for a movie filename. Every field carries provenance
/// (§2.1) — nothing here is ever a bare `String`/`u16`.
#[derive(Debug, Clone)]
pub struct MovieParse {
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
    /// `true` if an episode-style marker (`S01E01`, `1x01`, …) was found in
    /// the name — the movie/episode ambiguity case (§3.2). The caller (the
    /// engine, which knows whether this file was scanned under `--type tv`
    /// or `--type movies`) decides what to do with that; the parser just
    /// reports it rather than silently guessing one interpretation.
    pub ambiguous_episode_like: bool,
}

fn take_vocab(text: &str, cons: &mut Consumption, matcher: &vocab::VocabMatcher) -> Option<String> {
    let found = find_vocab(text, cons, matcher);
    if let Some((range, value)) = found {
        cons.mark(range);
        Some(value)
    } else {
        None
    }
}

/// Parse a movie filename (including extension — it's stripped internally).
pub fn parse_movie_filename(filename: &str, opts: &ParseOptions) -> MovieParse {
    let text = normalize_stem(filename);
    let mut cons = Consumption::new(&text);

    // Release group: detected now so Year doesn't misread a trailing token,
    // formalised into a `Field` at the end (§3.1 extractor order).
    let release_group_match = detect_release_group(&text);
    if let Some(rg) = &release_group_match {
        cons.mark(rg.range.clone());
    }

    // 1. SubtitleFlags — rarely present on a movie's main file; consumed so
    //    it can never end up mistaken for part of the title.
    let _ = take_vocab(&text, &mut cons, &vocab::SUBTITLE_FLAGS);

    // 2. EpisodeMarkers — must run before Year (§3.1: "so 1x01 is never read
    //    as a year").
    let episode = extract_episode_marker(&text, &cons);
    let ambiguous_episode_like = episode.is_some();
    if let Some(e) = &episode {
        cons.mark(e.range.clone());
    }

    // 3-8. Vocabulary-driven categories, in priority order.
    let resolution = take_vocab(&text, &mut cons, &vocab::RESOLUTION);
    let source = take_vocab(&text, &mut cons, &vocab::SOURCE);
    let video_codec = take_vocab(&text, &mut cons, &vocab::VIDEO_CODEC);
    let audio_format = take_vocab(&text, &mut cons, &vocab::AUDIO_FORMAT);
    let hdr = take_vocab(&text, &mut cons, &vocab::HDR);
    let edition = take_vocab(&text, &mut cons, &vocab::EDITION);
    let language = take_vocab(&text, &mut cons, &vocab::LANGUAGE);

    // 9. Year.
    let year_match = extract_year(&text, &cons, opts.min_year, opts.max_year);
    let year_bracketed = year_match
        .as_ref()
        .map(|y| {
            let before = text[..y.range.start].chars().next_back();
            matches!(before, Some('(') | Some('['))
        })
        .unwrap_or(false);
    if let Some(y) = &year_match {
        cons.mark(y.range.clone());
    }

    // 10/11. Disc/track number are music-specific (Phase 6) and deliberately
    // not run here: on a movie name, "CD"/leading-digit patterns are a
    // false-positive risk (e.g. a title that happens to start with a
    // number) with no upside, since `MovieParse` has nowhere to put them.

    // 12. Title: the leading non-empty residual run.
    let residuals = cons.residual_ranges();
    let title_raw = residuals
        .iter()
        .map(|r| text[r.clone()].trim())
        .find(|s| !s.is_empty())
        .unwrap_or("")
        .trim_matches(|c: char| matches!(c, '-' | ' ' | ':'))
        .to_string();

    let title = if title_raw.is_empty() {
        Field::unknown(vec![Source::Filename])
    } else {
        Field::known(title_raw, Source::Filename, Confidence::Medium)
    };

    let year = match year_match {
        Some(y) => Field::known(
            y.value,
            Source::Filename,
            if year_bracketed { Confidence::High } else { Confidence::Medium },
        ),
        None => Field::unknown(vec![Source::Filename]),
    };

    let vocab_field = |v: Option<String>| match v {
        Some(s) => Field::known(s, Source::Filename, Confidence::High),
        None => Field::unknown(vec![Source::Filename]),
    };

    let release_group = match release_group_match {
        Some(rg) => Field::known(
            rg.value,
            Source::Filename,
            if rg.bracketed { Confidence::High } else { Confidence::Medium },
        ),
        None => Field::unknown(vec![Source::Filename]),
    };

    MovieParse {
        title,
        year,
        edition: vocab_field(edition),
        resolution: vocab_field(resolution),
        source: vocab_field(source),
        video_codec: vocab_field(video_codec),
        audio_format: vocab_field(audio_format),
        hdr: vocab_field(hdr),
        language: vocab_field(language),
        release_group,
        ambiguous_episode_like,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> ParseOptions {
        ParseOptions { min_year: 1888, max_year: 2027 }
    }

    #[test]
    fn simple_title_year_resolution_source_codec() {
        let p = parse_movie_filename("Inception.2010.1080p.BluRay.x264-RARBG.mkv", &opts());
        assert_eq!(p.title.as_value().unwrap(), "Inception");
        assert_eq!(*p.year.as_value().unwrap(), 2010);
        assert_eq!(p.resolution.as_value().unwrap(), "1080p");
        assert_eq!(p.source.as_value().unwrap(), "BluRay");
        assert_eq!(p.video_codec.as_value().unwrap(), "x264");
        assert_eq!(p.release_group.as_value().unwrap(), "RARBG");
    }

    #[test]
    fn year_in_title_uses_rightmost_bare_or_bracketed() {
        let p = parse_movie_filename("Blade Runner 2049 (2017).mkv", &opts());
        assert_eq!(p.title.as_value().unwrap(), "Blade Runner 2049");
        assert_eq!(*p.year.as_value().unwrap(), 2017);
    }

    #[test]
    fn edition_is_vocabulary_matched_not_left_in_title() {
        let p = parse_movie_filename(
            "Blade Runner 2049 (2017) - Director's Cut - 1080p.mkv",
            &opts(),
        );
        assert_eq!(p.title.as_value().unwrap(), "Blade Runner 2049");
        assert_eq!(*p.year.as_value().unwrap(), 2017);
        assert_eq!(p.edition.as_value().unwrap(), "Director's Cut");
        assert_eq!(p.resolution.as_value().unwrap(), "1080p");
    }

    #[test]
    fn flags_episode_like_names_as_ambiguous() {
        let p = parse_movie_filename("Show.Name.S01E01.1080p.mkv", &opts());
        assert!(p.ambiguous_episode_like);
    }

    #[test]
    fn no_year_present_is_unknown_not_guessed() {
        let p = parse_movie_filename("Movie Title 1080p.mkv", &opts());
        assert!(p.year.as_value().is_none());
        assert_eq!(p.title.as_value().unwrap(), "Movie Title");
    }

    #[test]
    fn never_panics_on_arbitrary_input() {
        for s in ["", ".", "🎬🎬.mkv", "(((((", "S01E01E02E03E04.mkv", "1999.1999.1999.mkv"] {
            let _ = parse_movie_filename(s, &opts());
        }
    }
}
