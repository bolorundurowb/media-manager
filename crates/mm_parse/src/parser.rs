//! Main parser entry point (§3).
//!
//! Runs extractors in priority order, claims spans, then assigns residual runs
//! positionally against the movie shape.

use mm_core::{Confidence, Source};

use crate::extractors::{
    AudioFormatExtractor, Claim, EditionExtractor, Extractor, HdrExtractor, ParseField,
    ReleaseGroupExtractor, ResolutionExtractor, SourceExtractor, VideoCodecExtractor, YearExtractor,
};
use crate::model::{MediaParse, ParseOptions, ParsedMovie, known};
use crate::tokens::{normalise, tokenize};

/// Parse a filename as a movie, returning the structured fields.
pub fn parse_movie_filename(filename: &str, opts: &ParseOptions) -> ParsedMovie {
    let normalised = normalise(filename);
    if normalised.is_empty() {
        return ParsedMovie::new();
    }
    let tokens = tokenize(filename);
    if tokens.is_empty() {
        return ParsedMovie::new();
    }

    let mut claimed = Vec::<(usize, usize)>::new();
    let mut movie = ParsedMovie::new();

    let extractors: Vec<Box<dyn Extractor>> = vec![
        Box::new(ResolutionExtractor),
        Box::new(SourceExtractor),
        Box::new(VideoCodecExtractor),
        Box::new(AudioFormatExtractor),
        Box::new(HdrExtractor),
        Box::new(EditionExtractor),
        // LanguageExtractor is intentionally omitted from the movie parser in
        // Phase 2: short language codes like "no" collide with common title
        // words (e.g. "No Country for Old Men"). Subtitle language is handled
        // separately during association (§5.4).
        Box::new(YearExtractor::new(opts.min_year, opts.max_year)),
        Box::new(ReleaseGroupExtractor),
    ];

    for ex in extractors {
        if let Some(claim) = ex.extract(&tokens, &normalised) {
            if !overlaps(&claimed, claim.start, claim.end) {
                claimed.push((claim.start, claim.end));
                set_field(&mut movie, claim);
            }
        }
    }

    // Assign title from the leading residual run (before the first claimed tag).
    let first_anchor = claimed.iter().map(|(s, _)| *s).min().unwrap_or(normalised.len());
    let title_text: String = tokens
        .iter()
        .filter(|t| t.end <= first_anchor)
        .map(|t| t.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let title_text = title_text.trim().to_string();
    if !title_text.is_empty() {
        movie.title = known(title_text, Source::Filename, Confidence::Medium);
    }

    movie
}

/// Parse a filename as a movie (convenience with default options).
pub fn parse_movie(filename: &str) -> MediaParse {
    MediaParse::Movie(parse_movie_filename(filename, &ParseOptions::default()))
}

fn overlaps(claimed: &[(usize, usize)], start: usize, end: usize) -> bool {
    claimed
        .iter()
        .any(|(cs, ce)| start < *ce && end > *cs)
}

fn set_field(movie: &mut ParsedMovie, claim: Claim) {
    let src = Source::Filename;
    match claim.field {
        ParseField::Year => {
            if let Ok(y) = claim.value.parse::<u16>() {
                movie.year = known(y, src, Confidence::High);
            }
        }
        ParseField::Resolution => {
            movie.resolution = known(claim.value, src, Confidence::Medium);
        }
        ParseField::Source => {
            let canonical = match claim.value.as_str() {
                "BRRip" => "BDRip",
                other => other,
            };
            movie.source = known(canonical.to_string(), src, Confidence::Medium);
        }
        ParseField::VideoCodec => {
            movie.video_codec = known(claim.value, src, Confidence::Medium);
        }
        ParseField::AudioFormat => {
            let canonical = match claim.value.as_str() {
                "DD5.1" => "AC3",
                "DDP5.1" => "DDP",
                other => other,
            };
            movie.audio_format = known(canonical.to_string(), src, Confidence::Medium);
        }
        ParseField::Hdr => {
            movie.hdr = known(claim.value, src, Confidence::Medium);
        }
        ParseField::Edition => {
            movie.edition = known(claim.value, src, Confidence::Medium);
        }
        ParseField::Language => {
            movie.language = known(claim.value, src, Confidence::Medium);
        }
        ParseField::ReleaseGroup => {
            movie.release_group = known(claim.value, src, Confidence::Low);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn movie(filename: &str) -> ParsedMovie {
        match parse_movie(filename) {
            MediaParse::Movie(m) => m,
            _ => panic!("expected movie"),
        }
    }

    #[test]
    fn parses_inception() {
        let m = movie("Inception.2010.1080p.BluRay.x264.mkv");
        assert_eq!(m.title.as_value().unwrap(), "Inception");
        assert_eq!(m.year.as_value().copied().unwrap(), 2010);
        assert_eq!(m.resolution.as_value().unwrap(), "1080p");
        assert_eq!(m.source.as_value().unwrap(), "BluRay");
        assert_eq!(m.video_codec.as_value().unwrap(), "x264");
    }

    #[test]
    fn blade_runner_year_in_title() {
        let m = movie("Blade Runner 2049 (2017).mkv");
        assert_eq!(m.title.as_value().unwrap(), "Blade Runner 2049");
        assert_eq!(m.year.as_value().copied().unwrap(), 2017);
    }

    #[test]
    fn edition_at_end() {
        let m = movie("Inception (2010) Director's Cut 1080p.mkv");
        assert_eq!(m.title.as_value().unwrap(), "Inception");
        assert_eq!(m.edition.as_value().unwrap(), "Director's Cut");
    }
}
