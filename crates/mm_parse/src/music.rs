//! Music filename parsing (Phase 6, §3.3).
//!
//! Filename-derived fields only (track number, disc number, and a title
//! guess) — this crate is I/O-free and never reads embedded tags. Per §3.3
//! the track/artist *decision* belongs to tags read by `mm_probe`/
//! `mm_engine::music_plan`, which should prefer tag-sourced `Field`s over
//! whatever this module produces via `mm_core::identity::pick_best`. Owns
//! `crate::model::ParsedTrack`.
//!
//! Deliberately **not** built on `crate::tokens`: that module's
//! release-group stripping and dot/underscore-to-space unification are
//! movie/TV-shaped heuristics (a release group like `-GROUP`, vocab words
//! like `WEB-DL`). Applied to a music filename they would misfire — e.g. a
//! track titled `Nina Simone - Feeling Good - Live` ends in `Live`, a
//! plausible-looking "release group" tag by the same heuristic that
//! correctly strips `-RARBG` from a movie name. Music gets its own, much
//! smaller, normalisation here.

use std::sync::OnceLock;

use regex::Regex;

use mm_core::{Confidence, Source};

use crate::model::{ParseOptions, ParsedTrack, known};

/// Parse a filename as a music track (filename-only fields; see module docs).
pub fn parse_track_filename(filename: &str, _opts: &ParseOptions) -> ParsedTrack {
    let mut track = ParsedTrack::new();

    let stem = strip_extension(filename);
    let stem = stem.trim();
    if stem.is_empty() {
        return track;
    }

    let (rest, disc, track_num) = extract_track_prefix(stem);

    if let Some(d) = disc {
        track.disc = known(d, Source::Filename, Confidence::Medium);
    }
    if let Some(t) = track_num {
        track.track = known(t, Source::Filename, Confidence::Medium);
    }

    let title_part = normalise_whitespace(rest.trim());
    if title_part.is_empty() {
        return track;
    }

    // §3.3 scopes the ambiguity to the `N - A - B` shape specifically — a
    // leading track number *and* a dash-separated residual. Without a track
    // number at all there is no `N` anchor, so a plain `Artist - Title.mp3`
    // (a common convention with no track number) is treated as a literal
    // title rather than flagged; there is no more evidence either way, and
    // this module never had track-artist extraction as a goal for that case.
    if track_num.is_some() && has_ambiguous_artist_title_split(&title_part) {
        // The parser cannot tell artist from title in this shape — it must
        // not guess, so `title` stays `Unknown` (default `Field::unknown`
        // from `ParsedTrack::new`) and the ambiguity is recorded for the
        // engine to explain in a diagnostic.
        track.ambiguous_artist_title = true;
    } else {
        track.title = known(title_part, Source::Filename, Confidence::Medium);
    }

    track
}

/// Strip a trailing extension (last `.` segment), same convention as
/// `crate::tokens::normalise`'s first step, but standalone so this module has
/// no dependency on that module's release-group/vocab-driven behaviour.
fn strip_extension(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) => &name[..i],
        None => name,
    }
}

fn normalise_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = true; // trim leading
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    out.trim_end().to_string()
}

/// Extract a leading `disc-track` or `track` number prefix, returning the
/// residual text.
///
/// Two forms are recognised, both requiring the number(s) to sit at the very
/// start with no leading whitespace (a real track prefix never does):
///   - `disc-track` / `disc.track` (no space between disc and separator):
///     `1-01 Title`, `1.01 Title` → disc=1, track=1
///   - plain `track` followed by a separator: `01 - Title`, `01. Title` →
///     track=1
///
/// A leading run of digits with *no* trailing separator (`01Title`) or more
/// than 3 digits (a bare year like `1999 Title`) is left alone — in both
/// cases there is no reliable place to cut, and guessing would risk treating
/// part of a real title as a track number.
fn extract_track_prefix(stem: &str) -> (&str, Option<u16>, Option<u16>) {
    static DISC_TRACK_RE: OnceLock<Regex> = OnceLock::new();
    static TRACK_ONLY_RE: OnceLock<Regex> = OnceLock::new();

    let disc_track_re =
        DISC_TRACK_RE.get_or_init(|| Regex::new(r"^(\d{1,2})[-.](\d{1,3})[\s.\-]+(.+)$").unwrap());
    if let Some(caps) = disc_track_re.captures(stem) {
        let disc = caps.get(1).and_then(|m| m.as_str().parse::<u16>().ok());
        let trackn = caps.get(2).and_then(|m| m.as_str().parse::<u16>().ok());
        let rest = caps.get(3).map(|m| m.as_str()).unwrap_or("");
        return (rest, disc, trackn);
    }

    let track_only_re =
        TRACK_ONLY_RE.get_or_init(|| Regex::new(r"^(\d{1,3})[\s.\-]+(.+)$").unwrap());
    if let Some(caps) = track_only_re.captures(stem) {
        let trackn = caps.get(1).and_then(|m| m.as_str().parse::<u16>().ok());
        let rest = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        return (rest, None, trackn);
    }

    (stem, None, None)
}

/// `true` if `s` looks like `A - B` (a single dash-separated split with
/// non-empty content on both sides) — the shape that's ambiguous between "the
/// title contains a literal ` - `" and "artist - title" (§3.3). `A - B - C`
/// also matches (splitting on the *first* ` - `): it is at least as
/// ambiguous, not less.
fn has_ambiguous_artist_title_split(s: &str) -> bool {
    match s.split_once(" - ") {
        Some((a, b)) => !a.trim().is_empty() && !b.trim().is_empty(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> ParseOptions {
        ParseOptions::default()
    }

    #[test]
    fn plain_track_and_title() {
        let t = parse_track_filename("01 - Feeling Good.mp3", &opts());
        assert_eq!(t.track.as_value().copied(), Some(1));
        assert_eq!(t.disc.as_value().copied(), None);
        assert_eq!(t.title.as_value().map(String::as_str), Some("Feeling Good"));
        assert!(!t.ambiguous_artist_title);
    }

    #[test]
    fn dotted_track_prefix() {
        let t = parse_track_filename("07. Mr. Bojangles.flac", &opts());
        assert_eq!(t.track.as_value().copied(), Some(7));
        assert_eq!(t.title.as_value().map(String::as_str), Some("Mr. Bojangles"));
    }

    #[test]
    fn disc_track_dash_form() {
        let t = parse_track_filename("1-07 - Money.flac", &opts());
        assert_eq!(t.disc.as_value().copied(), Some(1));
        assert_eq!(t.track.as_value().copied(), Some(7));
        assert_eq!(t.title.as_value().map(String::as_str), Some("Money"));
    }

    #[test]
    fn disc_track_dot_form() {
        let t = parse_track_filename("2.03 Come Together.m4a", &opts());
        assert_eq!(t.disc.as_value().copied(), Some(2));
        assert_eq!(t.track.as_value().copied(), Some(3));
        assert_eq!(t.title.as_value().map(String::as_str), Some("Come Together"));
    }

    #[test]
    fn no_track_number_at_all() {
        let t = parse_track_filename("Bohemian Rhapsody.mp3", &opts());
        assert!(t.track.as_value().is_none());
        assert!(t.disc.as_value().is_none());
        assert_eq!(t.title.as_value().map(String::as_str), Some("Bohemian Rhapsody"));
    }

    #[test]
    fn bare_year_like_prefix_is_not_a_track_number() {
        // Four digits never match the track/disc prefixes (max 3), so this
        // is left as a plain title rather than misread as a track number.
        let t = parse_track_filename("1999 Little Red Corvette.mp3", &opts());
        assert!(t.track.as_value().is_none());
        assert_eq!(
            t.title.as_value().map(String::as_str),
            Some("1999 Little Red Corvette")
        );
    }

    #[test]
    fn ambiguous_artist_title_split_is_not_guessed() {
        // Tagless compilation-style filename: `N - A - B`. §3.3: NeedsReview,
        // never a guess.
        let t = parse_track_filename("03 - Nina Simone - Feeling Good.mp3", &opts());
        assert_eq!(t.track.as_value().copied(), Some(3));
        assert!(t.ambiguous_artist_title);
        assert!(t.title.as_value().is_none());
    }

    #[test]
    fn title_containing_a_literal_dash_without_track_number_is_not_flagged_ambiguous() {
        // No leading track number at all, so there is no `N` anchor for the
        // §3.3 `N - A - B` ambiguity: the whole string is taken as a literal
        // title, dash and all, rather than flagged for review.
        let t = parse_track_filename("Nina Simone - Feeling Good.mp3", &opts());
        assert!(!t.ambiguous_artist_title);
        assert_eq!(
            t.title.as_value().map(String::as_str),
            Some("Nina Simone - Feeling Good")
        );
    }

    #[test]
    fn no_separator_after_number_is_left_alone() {
        // `01Title` has no separator between the number and the rest, so
        // there's no reliable cut point.
        let t = parse_track_filename("01Title.mp3", &opts());
        assert!(t.track.as_value().is_none());
        assert_eq!(t.title.as_value().map(String::as_str), Some("01Title"));
    }

    #[test]
    fn empty_filename_yields_all_unknown() {
        let t = parse_track_filename(".mp3", &opts());
        assert!(t.title.as_value().is_none());
        assert!(t.track.as_value().is_none());
    }
}
