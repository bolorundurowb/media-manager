//! Identity keys (§2.2).
//!
//! Keys are derived over mandatory discriminators only, so an optional year
//! present on some-but-not-all files does not split a group. Grouping is
//! two-pass: pass 1 keys on the mandatory discriminator; pass 2 resolves one
//! canonical year/id per group and writes it back into every member.

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use crate::provenance::{Field, Source};

/// A normalised, case-folded, punctuation-collapsed form carrying its display
/// string alongside. NFC, case-folded, punctuation-collapsed, with optional
/// article stripping controlled by [`NormOptions::strip_articles`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Norm {
    /// The normalised form used for comparison and hashing.
    pub key: String,
    /// The display form (NFC, original casing of the most authoritative source).
    pub display: String,
}

impl Norm {
    /// Build a normalised key from a display string.
    pub fn from_display(display: &str) -> Norm {
        Self::from_display_with(display, NormOptions::default())
    }

    pub fn from_display_with(display: &str, opts: NormOptions) -> Norm {
        // NFC for storage and comparison (§2.4).
        let nfc = display.chars().nfc().collect::<String>();
        let key = normalise_key(&nfc, opts);
        Norm {
            key,
            display: nfc,
        }
    }    /// Empty normalised value.
    pub fn empty() -> Self {
        Norm {
            key: String::new(),
            display: String::new(),
        }
    }
}

fn normalise_key(s: &str, opts: NormOptions) -> String {
    // NFKD helps decompose ligatures etc., then case-fold, then collapse
    // punctuation and whitespace. This is intentionally lossy for the *key*;
    // the display form is untouched.
    let folded: String = s.chars().nfkd().collect::<String>().to_lowercase();

    let mut out = String::with_capacity(folded.len());
    let mut prev_space = true; // collapse runs, trim ends
    for ch in folded.chars() {
        if ch.is_alphanumeric() {
            out.push(ch);
            prev_space = false;
        } else if ch.is_whitespace() || is_collapsible_punct(ch) {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        }
        // else: drop other punctuation entirely
    }
    let mut out = out.trim_end().to_string();

    if opts.strip_articles {
        out = strip_leading_article(&out).to_string();
    }
    out
}

fn is_collapsible_punct(ch: char) -> bool {
    matches!(
        ch,
        '-' | '_' | '.' | ',' | ';' | ':' | '/' | '\\' | '(' | ')' | '[' | ']' | '{' | '}' | '\'' | '"' | '`' | '+' | '=' | '*' | '&' | '|' | '!' | '?'
    )
}

fn strip_leading_article(s: &str) -> &str {
    for art in ["the ", "a ", "an ", "le ", "la ", "les ", "el ", "los ", "las ", "der ", "die ", "das "] {
        if s.starts_with(art) {
            return &s[art.len()..];
        }
    }
    s
}

/// Options controlling normalisation.
#[derive(Debug, Clone, Copy, Default)]
pub struct NormOptions {
    pub strip_articles: bool,
}

impl Norm {
    pub fn options(strip_articles: bool) -> NormOptions {
        NormOptions { strip_articles }
    }
}

/// Edition — "Director's Cut is not a duplicate of Theatrical" (§4.5) made
/// structural inside [`MovieId`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Edition(pub String);

impl Edition {
    pub fn new(s: impl Into<String>) -> Self {
        Edition(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A movie identity. `year` participates in display but equality is on
/// `title + edition` only by default (the canonical year is resolved two-pass
/// in [`crate::identity`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MovieId {
    pub title: Norm,
    pub year: Option<u16>,
    pub edition: Option<Edition>,
}

impl MovieId {
    pub fn new(title: Norm) -> Self {
        MovieId {
            title,
            year: None,
            edition: None,
        }
    }
}

/// A show identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ShowId {
    pub title: Norm,
    pub year: Option<u16>,
}

impl ShowId {
    pub fn new(title: Norm) -> Self {
        ShowId { title, year: None }
    }
}

/// An episode identity. `episodes` is a `Vec<u16>` so multi-episode files
/// (§6.5) are first-class, not a pair some later code might split.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EpisodeId {
    pub show: ShowId,
    pub season: u16,
    pub episodes: Vec<u16>,
}

impl EpisodeId {
    pub fn new(show: ShowId, season: u16, episodes: Vec<u16>) -> Self {
        EpisodeId {
            show,
            season,
            episodes,
        }
    }

    /// `S01E02`-style code for display, or `S00` for specials.
    pub fn code(&self) -> String {
        if self.season == 0 || self.episodes.is_empty() {
            return format!("S{:02}", self.season);
        }
        let eps: Vec<String> = self.episodes.iter().map(|e| format!("E{:02}", e)).collect();
        format!("S{:02}{}", self.season, eps.join(""))
    }
}

/// An album identity. Equality is on `album_artist + album` (pass-1), with the
/// year resolved two-pass.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AlbumId {
    pub album_artist: Norm,
    pub album: Norm,
    pub year: Option<u16>,
}

impl AlbumId {
    pub fn new(album_artist: Norm, album: Norm) -> Self {
        AlbumId {
            album_artist,
            album,
            year: None,
        }
    }
}

/// A track identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TrackId {
    pub album: AlbumId,
    pub disc: Option<u16>,
    pub track: Option<u16>,
    pub title: Option<String>,
}

/// A short, in-order, comparable wrapper around a field source rank, used to
/// pick the best candidate during resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceRank(pub u8);

impl PartialOrd for SourceRank {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SourceRank {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.cmp(&other.0)
    }
}

/// Per-field source-preference table (§2.1). The single place the preference
/// order is expressed; validated at config load.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SourcePreference {
    /// Rank assigned to each source. Higher is better. Missing sources default
    /// to 0 (never wins).
    pub embedded_tag: u8,
    pub container_header: u8,
    pub nfo: u8,
    pub provider: u8,
    pub filename: u8,
    pub parent_dir: u8,
    pub fallback: u8,
}

impl SourcePreference {
    pub fn rank(&self, s: Source) -> SourceRank {
        let v = match s {
            Source::EmbeddedTag => self.embedded_tag,
            Source::ContainerHeader => self.container_header,
            Source::Nfo => self.nfo,
            Source::Provider => self.provider,
            Source::Filename => self.filename,
            Source::ParentDir => self.parent_dir,
            Source::Fallback => self.fallback,
        };
        SourceRank(v)
    }

    /// The conservative default. A provider never overrides an embedded tag
    /// (§10), and `ContainerHeader` beats `Nfo` for dimensions.
    pub fn conservative_default() -> Self {
        SourcePreference {
            embedded_tag: 100,
            container_header: 90,
            nfo: 50,
            provider: 40,
            filename: 20,
            parent_dir: 15,
            fallback: 0,
        }
    }
}

/// Pick the best of two candidates for the same field by source rank, breaking
/// ties by confidence. Returns a reference to the winner.
pub fn pick_best<'a, T>(
    a: &'a Field<T>,
    b: &'a Field<T>,
    prefs: &SourcePreference,
) -> &'a Field<T> {
    match (a, b) {
        (Field::Unknown { .. }, Field::Unknown { .. }) => a,
        (Field::Unknown { .. }, _) => b,
        (_, Field::Unknown { .. }) => a,
        (
            Field::Known {
                source: sa,
                confidence: ca,
                ..
            },
            Field::Known {
                source: sb,
                confidence: cb,
                ..
            },
        ) => {
            let ra = prefs.rank(*sa);
            let rb = prefs.rank(*sb);
            if rb > ra || (rb == ra && cb > ca) {
                b
            } else {
                a
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provenance::Confidence;

    #[test]
    fn normalises_case_and_punctuation() {
        let n = Norm::from_display("The Matrix: Reloaded!");
        // Article stripping is opt-in (`NormOptions::strip_articles`), off by
        // default — see `strips_articles_when_configured` below.
        assert_eq!(n.key, "the matrix reloaded");
        assert_eq!(n.display, "The Matrix: Reloaded!");
    }

    #[test]
    fn strips_articles_when_configured() {
        let n = Norm::from_display_with("The Matrix", Norm::options(true));
        assert_eq!(n.key, "matrix");
    }

    #[test]
    fn episode_code_multi() {
        let id = EpisodeId::new(ShowId::new(Norm::from_display("Foo")), 1, vec![1, 2]);
        assert_eq!(id.code(), "S01E01E02");
    }

    #[test]
    fn confidence_ordering() {
        assert!(Confidence::High > Confidence::Medium);
        assert!(Confidence::Medium > Confidence::Low);
        assert!(Confidence::High.meets(Confidence::Medium));
        assert!(!Confidence::Low.meets(Confidence::Medium));
    }

    #[test]
    fn pick_best_prefers_higher_source() {
        let prefs = SourcePreference::conservative_default();
        let a = Field::known("from-file".to_string(), Source::Filename, Confidence::High);
        let b = Field::known("from-tag".to_string(), Source::EmbeddedTag, Confidence::High);
        let best = pick_best(&a, &b, &prefs);
        assert_eq!(best.as_value().unwrap(), &"from-tag".to_string());
    }
}
