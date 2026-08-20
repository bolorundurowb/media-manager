//! Music resolve stage (Phase 6, §5.2, §3.3, §8.4).
//!
//! Merges filename-derived candidates (`mm_parse::ParsedTrack`, `Source::Filename`)
//! with embedded-tag candidates (`mm_probe::AudioTags`, `Source::EmbeddedTag`) via
//! the same per-field source-preference table movies/TV use
//! (`mm_core::identity::pick_best`, exposed generically as
//! `crate::resolve::merge_field`). Tags outrank filenames in the conservative
//! default table (`embedded_tag: 100` vs `filename: 20`), which is exactly
//! §3.3/§8.4's "prefer embedded tags" requirement — no music-specific ranking
//! logic is needed, only feeding both candidates through the existing table.

use mm_core::config::Config;
use mm_core::identity::SourcePreference;
use mm_core::plan::{FieldName, Readiness};
use mm_core::provenance::{Confidence, Field, Source};

use mm_parse::ParsedTrack;
use mm_probe::AudioTags;

use crate::resolve::merge_field;

/// A music track with resolved fields, ready for grouping/routing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedTrack {
    pub album_artist: Field<String>,
    pub album: Field<String>,
    pub year: Field<u16>,
    pub disc: Field<u16>,
    pub track: Field<u16>,
    pub title: Field<String>,
    /// The track (not album) artist, used only for the opt-in
    /// `naming.music.compilation_prefix` (§5.5). Sourced from tags only —
    /// this crate's filename parser never attempts to extract a track artist
    /// from the filename (§3.3: guessing artist-vs-title from `N - A - B` is
    /// exactly the case that must not be guessed).
    pub track_artist: Field<String>,
    /// `true` if this track's tags (or filename fallback) indicate the album
    /// is a compilation — the `TCMP`/`FlagCompilation` flag, or an
    /// album-artist tag matching a "Various Artists" spelling (§5.3, §8.6).
    /// Used only to decide the `album_artist` fallback below; it is not part
    /// of any identity key.
    pub compilation: bool,
    /// Set when the filename looked like `N - A - B` and tags supplied no
    /// title — §3.3's "does not guess" case. Surfaced as a diagnostic and
    /// keeps `title` `Unknown` rather than picking a reading.
    pub ambiguous_artist_title: bool,
}

impl ResolvedTrack {
    /// Baseline resolution from filename candidates alone (pre-probe).
    pub fn from_parsed(p: &ParsedTrack) -> Self {
        ResolvedTrack {
            album_artist: p.album_artist.clone(),
            album: p.album.clone(),
            year: p.year.clone(),
            disc: p.disc.clone(),
            track: p.track.clone(),
            title: p.title.clone(),
            track_artist: Field::unknown(vec![]),
            compilation: false,
            ambiguous_artist_title: p.ambiguous_artist_title,
        }
    }

    /// Fold embedded tag candidates in, per-field, via the source-preference
    /// table. Tags almost always win over filename candidates in the
    /// conservative default table — this is the enforcement point for
    /// §3.3/§8.4's "prefer tags over filenames" requirement.
    pub fn merge_tags(&mut self, tags: &AudioTags, prefs: &SourcePreference) {
        // Compilation detection first: it decides how the album-artist
        // fallback below behaves. Two signals, both documented in §5.3:
        // the `TCMP`/`FlagCompilation` flag, or an album-artist tag that
        // already spells out a "Various Artists" convention. (The third
        // signal §5.3 mentions — "≥ N distinct track artists in one album
        // key" — needs whole-group context this per-track merge does not
        // have; that heuristic is intentionally not implemented here, see
        // `music_plan` module docs.)
        let looks_like_va = tags
            .album_artist
            .as_deref()
            .map(is_various_artists_spelling)
            .unwrap_or(false);
        if tags.compilation == Some(true) || looks_like_va {
            self.compilation = true;
        }

        if let Some(v) = &tags.album {
            let candidate = Field::known(v.clone(), Source::EmbeddedTag, Confidence::High);
            self.album = merge_field(&self.album, candidate, prefs);
        }
        if let Some(y) = tags.year {
            let candidate = Field::known(y, Source::EmbeddedTag, Confidence::High);
            self.year = merge_field(&self.year, candidate, prefs);
        }
        if let Some(d) = tags.disc {
            let candidate = Field::known(d, Source::EmbeddedTag, Confidence::High);
            self.disc = merge_field(&self.disc, candidate, prefs);
        }
        if let Some(t) = tags.track {
            let candidate = Field::known(t, Source::EmbeddedTag, Confidence::High);
            self.track = merge_field(&self.track, candidate, prefs);
        }
        if let Some(v) = &tags.title {
            let candidate = Field::known(v.clone(), Source::EmbeddedTag, Confidence::High);
            self.title = merge_field(&self.title, candidate, prefs);
        }
        if let Some(v) = &tags.artist {
            let candidate = Field::known(v.clone(), Source::EmbeddedTag, Confidence::High);
            self.track_artist = merge_field(&self.track_artist, candidate, prefs);
        }

        // Album-artist fallback (§5.3/§8.6 judgment call, documented in
        // `music_plan` module docs): many single-artist rips only carry a
        // track `artist` tag, never a dedicated album-artist tag, and the
        // real-world convention (Jellyfin/Plex/Picard) is to fall back to
        // the track artist in that case. But that fallback is exactly wrong
        // for a compilation: it would key every track under its own
        // artist and split one album into N one-track "albums". So the
        // fallback is gated on `!self.compilation`, and a compilation with
        // no explicit album-artist tag gets a canonical "Various Artists"
        // sentinel instead — which also makes `artist_dir` render sensibly
        // with the default `{album_artist}` template, no routing-side
        // special case required.
        let album_artist_candidate = match (&tags.album_artist, self.compilation) {
            (Some(v), _) => Some((v.clone(), Confidence::High)),
            (None, true) => Some(("Various Artists".to_string(), Confidence::Medium)),
            (None, false) => tags.artist.clone().map(|a| (a, Confidence::Medium)),
        };
        if let Some((v, confidence)) = album_artist_candidate {
            let candidate = Field::known(v, Source::EmbeddedTag, confidence);
            self.album_artist = merge_field(&self.album_artist, candidate, prefs);
        }
    }

    /// Required fields per §5.2: `album_artist`, `album`, `title`. `track` is
    /// deliberately not required — `[{track:02} - ]{title}` brackets it, so a
    /// track without a number still has a well-defined destination.
    pub fn readiness(&self, cfg: &Config) -> Readiness {
        let mut missing = Vec::new();
        let mut reasons = Vec::new();

        if !self.album_artist.is_known() {
            missing.push(FieldName::AlbumArtist);
            reasons.push("album artist could not be determined".into());
        }
        if !self.album.is_known() {
            missing.push(FieldName::Album);
            reasons.push("album could not be determined".into());
        }
        if !self.title.is_known() {
            missing.push(FieldName::TrackTitle);
            if self.ambiguous_artist_title {
                reasons.push(
                    "filename looks like 'track - artist - title' with no tags to disambiguate; not guessing (§3.3)"
                        .into(),
                );
            } else {
                reasons.push("track title could not be determined".into());
            }
        }
        if !self.meets_min_confidence(cfg) {
            reasons.push("confidence below configured minimum".into());
        }

        if missing.is_empty() && reasons.is_empty() {
            Readiness::Ready
        } else {
            Readiness::NeedsReview { missing, reasons }
        }
    }

    fn meets_min_confidence(&self, cfg: &Config) -> bool {
        let min = cfg.behaviour.min_confidence;
        [
            self.album_artist.confidence(),
            self.album.confidence(),
            self.title.confidence(),
        ]
        .into_iter()
        .flatten()
        .all(|c| c.meets(min))
    }
}

/// `true` if `s` already spells out a "Various Artists" convention. Kept
/// small and case-insensitive; not configurable in this phase (§5.3 calls
/// for a *configurable* list, which would live in `mm_core::config` — adding
/// that config surface is left for a follow-up since the two built-in
/// spellings cover the overwhelming majority of tagged compilations).
fn is_various_artists_spelling(s: &str) -> bool {
    let lower = s.trim().to_ascii_lowercase();
    lower == "various artists" || lower == "va"
}

#[cfg(test)]
mod tests {
    use super::*;
    use mm_core::provenance::{Confidence as Conf, Source as Src};

    fn parsed_track() -> ParsedTrack {
        ParsedTrack::new()
    }

    #[test]
    fn tags_outrank_filename_for_album() {
        let prefs = SourcePreference::conservative_default();
        let mut r = ResolvedTrack::from_parsed(&parsed_track());
        r.album = Field::known("From Filename".to_string(), Src::Filename, Conf::High);

        let tags = AudioTags {
            album: Some("From Tag".to_string()),
            ..Default::default()
        };
        r.merge_tags(&tags, &prefs);

        assert_eq!(r.album.as_value().map(String::as_str), Some("From Tag"));
        assert_eq!(r.album.source(), Some(Src::EmbeddedTag));
    }

    #[test]
    fn album_artist_falls_back_to_track_artist_when_not_compilation() {
        let prefs = SourcePreference::conservative_default();
        let mut r = ResolvedTrack::from_parsed(&parsed_track());
        let tags = AudioTags {
            artist: Some("Nina Simone".to_string()),
            album_artist: None,
            ..Default::default()
        };
        r.merge_tags(&tags, &prefs);
        assert_eq!(
            r.album_artist.as_value().map(String::as_str),
            Some("Nina Simone")
        );
    }

    #[test]
    fn compilation_does_not_fall_back_to_per_track_artist() {
        let prefs = SourcePreference::conservative_default();
        let mut r = ResolvedTrack::from_parsed(&parsed_track());
        let tags = AudioTags {
            artist: Some("Some One-Off Artist".to_string()),
            album_artist: None,
            compilation: Some(true),
            ..Default::default()
        };
        r.merge_tags(&tags, &prefs);
        assert_eq!(
            r.album_artist.as_value().map(String::as_str),
            Some("Various Artists"),
            "compilation with no album-artist tag must not key on the individual track artist"
        );
    }

    #[test]
    fn explicit_various_artists_album_artist_is_respected_and_detected_as_compilation() {
        let prefs = SourcePreference::conservative_default();
        let mut r = ResolvedTrack::from_parsed(&parsed_track());
        let tags = AudioTags {
            artist: Some("Track Artist".to_string()),
            album_artist: Some("Various Artists".to_string()),
            ..Default::default()
        };
        r.merge_tags(&tags, &prefs);
        assert!(r.compilation);
        assert_eq!(
            r.album_artist.as_value().map(String::as_str),
            Some("Various Artists")
        );
    }

    #[test]
    fn readiness_requires_album_artist_album_title_but_not_track() {
        let cfg = Config::default();
        let mut r = ResolvedTrack::from_parsed(&parsed_track());
        r.album_artist = Field::known("Artist".to_string(), Src::Filename, Conf::Medium);
        r.album = Field::known("Album".to_string(), Src::Filename, Conf::Medium);
        r.title = Field::known("Title".to_string(), Src::Filename, Conf::Medium);
        assert!(matches!(r.readiness(&cfg), Readiness::Ready));
    }

    #[test]
    fn ambiguous_title_surfaces_specific_reason() {
        let cfg = Config::default();
        let mut r = ResolvedTrack::from_parsed(&parsed_track());
        r.album_artist = Field::known("Artist".to_string(), Src::Filename, Conf::Medium);
        r.album = Field::known("Album".to_string(), Src::Filename, Conf::Medium);
        r.ambiguous_artist_title = true;
        match r.readiness(&cfg) {
            Readiness::NeedsReview { reasons, .. } => {
                assert!(reasons.iter().any(|r| r.contains("§3.3")));
            }
            other => panic!("expected NeedsReview, got {other:?}"),
        }
    }
}
