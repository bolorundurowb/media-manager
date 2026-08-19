//! Extractors (§3.1).
//!
//! Each extractor claims a *span* in the normalised string and records a
//! `Field`. Claimed spans leave the residual for positional title assignment.

use std::sync::OnceLock;

use regex::Regex;

use crate::tokens::Token;
use crate::vocab;

/// Which parse field an extractor produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParseField {
    Year,
    Resolution,
    Source,
    VideoCodec,
    AudioFormat,
    Hdr,
    Edition,
    Language,
    ReleaseGroup,
    Copy,
}

/// A claimed span with its extracted value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    pub field: ParseField,
    pub value: String,
    pub start: usize,
    pub end: usize,
}

impl Claim {
    fn new(field: ParseField, value: impl Into<String>, start: usize, end: usize) -> Self {
        Claim {
            field,
            value: value.into(),
            start,
            end,
        }
    }
}

pub trait Extractor: Send + Sync {
    fn extract(&self, tokens: &[Token], normalised: &str) -> Option<Claim>;
}

// ---------------------------------------------------------------------------
// Year extractor
// ---------------------------------------------------------------------------

pub struct YearExtractor {
    min_year: u16,
    max_year: u16,
}

impl YearExtractor {
    pub fn new(min_year: u16, max_year: u16) -> Self {
        YearExtractor { min_year, max_year }
    }
}

impl Extractor for YearExtractor {
    fn extract(&self, tokens: &[Token], _normalised: &str) -> Option<Claim> {
        let range = self.min_year..=self.max_year;

        // Prefer parenthesised/bracketed year.
        for tok in tokens {
            let inner = strip_brackets(&tok.text);
            if let Ok(y) = inner.parse::<u16>() {
                if range.contains(&y) {
                    return Some(Claim::new(
                        ParseField::Year,
                        y.to_string(),
                        tok.start,
                        tok.end,
                    ));
                }
            }
        }

        // Otherwise rightmost bare year.
        for tok in tokens.iter().rev() {
            if let Ok(y) = tok.text.parse::<u16>() {
                if range.contains(&y) && !would_empty_title(tokens, tok.start, tok.end) {
                    return Some(Claim::new(
                        ParseField::Year,
                        y.to_string(),
                        tok.start,
                        tok.end,
                    ));
                }
            }
        }
        None
    }
}

fn strip_brackets(s: &str) -> &str {
    if s.len() >= 2 {
        let bytes = s.as_bytes();
        let first = bytes[0] as char;
        let last = bytes[bytes.len() - 1] as char;
        if (first == '(' && last == ')') || (first == '[' && last == ']') {
            return &s[1..s.len() - 1];
        }
    }
    s
}

fn would_empty_title(tokens: &[Token], start: usize, _end: usize) -> bool {
    // True if removing this token leaves no preceding non-trivial tokens.
    let before: Vec<_> = tokens
        .iter()
        .filter(|t| t.end <= start && !t.text.is_empty())
        .collect();
    before.is_empty()
}

// ---------------------------------------------------------------------------
// Vocabulary extractors (resolution, source, codec, audio, hdr)
// ---------------------------------------------------------------------------

macro_rules! vocab_extractor {
    ($name:ident, $field:expr, $vocab_fn:ident) => {
        pub struct $name;
        impl Extractor for $name {
            fn extract(&self, tokens: &[Token], _normalised: &str) -> Option<Claim> {
                let vocab = vocab::$vocab_fn();
                for tok in tokens {
                    let probe = tok.text.to_ascii_lowercase();
                    let probe_inner = strip_brackets(&tok.text).to_ascii_lowercase();
                    for pat in vocab {
                        let pat_norm = pat.to_ascii_lowercase();
                        if probe == pat_norm || probe_inner == pat_norm {
                            return Some(Claim::new($field, pat.to_string(), tok.start, tok.end));
                        }
                    }
                }
                None
            }
        }
    };
}

vocab_extractor!(
    ResolutionExtractor,
    ParseField::Resolution,
    resolution_patterns
);
vocab_extractor!(SourceExtractor, ParseField::Source, source_patterns);
vocab_extractor!(
    VideoCodecExtractor,
    ParseField::VideoCodec,
    video_codec_patterns
);
vocab_extractor!(
    AudioFormatExtractor,
    ParseField::AudioFormat,
    audio_patterns
);
vocab_extractor!(HdrExtractor, ParseField::Hdr, hdr_patterns);

// Edition needs multi-token matching, so it gets a regex implementation.
pub struct EditionExtractor;

impl Extractor for EditionExtractor {
    fn extract(&self, _tokens: &[Token], normalised: &str) -> Option<Claim> {
        static RE: OnceLock<Regex> = OnceLock::new();
        let re = RE.get_or_init(|| {
            let alts: Vec<String> = vocab::edition_patterns()
                .iter()
                .map(|p| regex::escape(p))
                .collect();
            Regex::new(&format!(r"(?i)\b({})\b", alts.join("|"))).unwrap()
        });
        re.find(normalised).map(|m| {
            Claim::new(
                ParseField::Edition,
                tidy_edition(m.as_str()),
                m.start(),
                m.end(),
            )
        })
    }
}

fn tidy_edition(s: &str) -> String {
    let lower = s.to_lowercase();
    match lower.as_str() {
        "directors cut" | "director's cut" => "Director's Cut".to_string(),
        "extended cut" => "Extended Cut".to_string(),
        "theatrical cut" => "Theatrical Cut".to_string(),
        "extended" => "Extended".to_string(),
        "theatrical" => "Theatrical".to_string(),
        "unrated" => "Unrated".to_string(),
        "uncut" => "Uncut".to_string(),
        "remastered" => "Remastered".to_string(),
        "imax" => "IMAX".to_string(),
        _ => s
            .split_whitespace()
            .map(|w| {
                let mut chars = w.chars();
                match chars.next() {
                    Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

// ---------------------------------------------------------------------------
// Language extractor
// ---------------------------------------------------------------------------

pub struct LanguageExtractor;

impl Extractor for LanguageExtractor {
    fn extract(&self, tokens: &[Token], _normalised: &str) -> Option<Claim> {
        for tok in tokens {
            let code = vocab::normalise_language(&tok.text);
            if code != "und" {
                return Some(Claim::new(ParseField::Language, code, tok.start, tok.end));
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Copy-number suffix ` (N)` from RenameNew (§5.6). Trailing parenthesised
// integer that is not a year. Optional; must not steal `(2010)` from Year.
// ---------------------------------------------------------------------------

pub struct CopyNumberExtractor;

impl Extractor for CopyNumberExtractor {
    fn extract(&self, tokens: &[Token], _normalised: &str) -> Option<Claim> {
        let tok = tokens.last()?;
        let n = parse_copy_token(&tok.text)?;
        Some(Claim::new(
            ParseField::Copy,
            n.to_string(),
            tok.start,
            tok.end,
        ))
    }
}

/// Parse a trailing ` (N)` copy-number from a file stem (not including the
/// extension). Years in `1888..=2100` are left alone so `Inception (2010)`
/// is not treated as copy 2010.
pub fn split_copy_suffix(stem: &str) -> (&str, Option<u16>) {
    let Some(idx) = stem.rfind(" (") else {
        return (stem, None);
    };
    let rest = &stem[idx + 2..];
    let Some(inner) = rest.strip_suffix(')') else {
        return (stem, None);
    };
    if rest.len() != inner.len() + 1 {
        return (stem, None);
    }
    match parse_copy_number(inner) {
        Some(n) => (&stem[..idx], Some(n)),
        None => (stem, None),
    }
}

fn parse_copy_token(text: &str) -> Option<u16> {
    let inner = strip_brackets(text);
    if inner.len() == text.len() {
        // Not bracketed — a bare trailing integer is not a copy suffix.
        return None;
    }
    parse_copy_number(inner)
}

fn parse_copy_number(s: &str) -> Option<u16> {
    let n: u16 = s.parse().ok()?;
    if (2..1888).contains(&n) || (2101..).contains(&n) {
        Some(n)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Release group is deliberately *not* an `Extractor` here. `tokens::normalise`
// strips it from the string before tokens ever exist (§3.1), so no extractor
// operating on `tokens`/`normalised` could ever observe it — an earlier
// implementation tried exactly that (checking `tokens.last()` for a `-`
// prefix) and was always a no-op as a result. `parser::parse_movie_filename`
// calls `tokens::release_group_of` directly instead. `ParseField::ReleaseGroup`
// stays defined for `set_field`'s exhaustive match / future use.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::tokenize;

    #[test]
    fn extracts_year_parenthesised() {
        let toks = tokenize("Inception (2010).mkv");
        let c = YearExtractor::new(1888, 2030).extract(&toks, "").unwrap();
        assert_eq!(c.value, "2010");
    }

    #[test]
    fn extracts_resolution() {
        let toks = tokenize("Inception.1080p.mkv");
        let c = ResolutionExtractor.extract(&toks, "").unwrap();
        assert_eq!(c.value, "1080p");
    }

    #[test]
    fn extracts_edition() {
        let norm = "Inception Directors Cut 2010";
        let toks = tokenize(norm);
        let c = EditionExtractor.extract(&toks, norm).unwrap();
        assert_eq!(c.value, "Director's Cut");
    }
}
