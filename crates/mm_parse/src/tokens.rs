//! Filename tokenisation (§3.1).
//!
//! Normalises separators, preserves bracketed/parenthesised spans, and strips
//! a trailing release-group token.

/// A token with its span in the normalised string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub text: String,
    pub start: usize,
    pub end: usize,
}

impl Token {
    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn len(&self) -> usize {
        self.text.len()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
}

/// Normalise a media filename into a token stream.
pub fn tokenize(name: &str) -> Vec<Token> {
    let (normalised, _group) = normalise_and_capture(name);
    split_tokens(&normalised)
}

/// Strip extension, release group suffix, and collapse separators.
pub fn normalise(name: &str) -> String {
    normalise_and_capture(name).0
}

/// The trailing release-group token stripped during normalisation, if any
/// (§3.1). Captured separately from the token stream because the group is
/// removed from the string *before* tokenisation ever runs, so no extractor
/// operating on `tokens` can ever see it.
pub fn release_group_of(name: &str) -> Option<String> {
    normalise_and_capture(name).1
}

fn normalise_and_capture(name: &str) -> (String, Option<String>) {
    // 1. Strip extension.
    let base = match name.rfind('.') {
        Some(i) => &name[..i],
        None => name,
    };

    // 2. Strip trailing release-group token: `-RARBG`, `[YTS.MX]`, etc.
    let (base, group) = strip_release_group(base);

    // 3. Unify separators to spaces. Keep brackets/parentheses intact.
    let mut out = String::with_capacity(base.len());
    for ch in base.chars() {
        if ch == '.' || ch == '_' || ch == '+' {
            out.push(' ');
        } else {
            out.push(ch);
        }
    }

    // 4. Collapse whitespace.
    (collapse_whitespace(&out), group)
}

/// Strip a trailing release-group token from a name that's already had its
/// extension removed. Handles both the dash-suffix form (`-RARBG`) and the
/// bracketed form (`[YTS.MX]`), guarding against two look-alikes that are
/// *not* release groups:
///   - a trailing bracketed year, e.g. `(2010)`/`[2010]`
///   - a trailing known vocab tag that happens to contain a dash or sits
///     inside brackets, e.g. `WEB-DL`, `DTS-HD`, `[x264]`
fn strip_release_group(s: &str) -> (String, Option<String>) {
    // Bracketed form: `[GROUP]` at the very end.
    if let Some(rest) = s.strip_suffix(']') {
        if let Some(open) = rest.rfind('[') {
            let inner = &rest[open + 1..];
            if !inner.is_empty() && inner.parse::<u16>().is_err() && !is_reserved_word(inner) {
                let stripped = rest[..open]
                    .trim_end_matches(['.', '_', '+', ' '])
                    .to_string();
                return (stripped, Some(inner.to_string()));
            }
        }
    }

    // Dash-suffix form: `-RARBG`.
    if let Some(dash) = s.rfind('-') {
        let candidate = &s[dash + 1..];
        let looks_like_group =
            candidate.len() >= 2 && candidate.chars().all(|c| c.is_ascii_alphanumeric());
        if looks_like_group {
            // The whole tag since the previous separator, e.g. `WEB-DL` or
            // `x264-SPARKS` — checked as a unit so a real vocab tag that
            // contains a dash is never split in half.
            let seg_start = s[..dash].rfind(['.', '_', '+']).map(|i| i + 1).unwrap_or(0);
            let whole_tag = &s[seg_start..];
            if !is_reserved_word(whole_tag) {
                let stripped = s[..dash].trim_end_matches(['.', '_', '+', ' ']).to_string();
                return (stripped, Some(candidate.to_string()));
            }
        }
    }

    (s.to_string(), None)
}

fn is_reserved_word(tag: &str) -> bool {
    crate::vocab::all_reserved_words()
        .iter()
        .any(|w| w.eq_ignore_ascii_case(tag))
}

fn collapse_whitespace(s: &str) -> String {
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

fn split_tokens(s: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut start = 0usize;
    let bytes = s.as_bytes();
    let mut in_token = false;

    for (i, ch) in s.char_indices() {
        if ch.is_whitespace() {
            if in_token {
                tokens.push(Token {
                    text: s[start..i].to_string(),
                    start,
                    end: i,
                });
                in_token = false;
            }
        } else if !in_token {
            start = i;
            in_token = true;
        }
        let _ = bytes;
    }
    if in_token {
        tokens.push(Token {
            text: s[start..].to_string(),
            start,
            end: s.len(),
        });
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenises_basic_movie() {
        let toks = tokenize("Inception.2010.1080p.BluRay.x264.mkv");
        assert_eq!(
            toks.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(),
            vec!["Inception", "2010", "1080p", "BluRay", "x264"]
        );
    }

    #[test]
    fn strips_release_group() {
        let toks = tokenize("Movie.2020.1080p-RARBG.mkv");
        assert!(!toks.iter().any(|t| t.text == "RARBG"));
        assert_eq!(
            release_group_of("Movie.2020.1080p-RARBG.mkv").as_deref(),
            Some("RARBG")
        );
    }

    #[test]
    fn strips_bracketed_release_group() {
        let name = "Whiplash.2014.720p.WEB-DL.AV1.TrueHD[YTS.MX].mkv";
        assert_eq!(release_group_of(name).as_deref(), Some("YTS.MX"));
        let toks = tokenize(name);
        assert!(!toks.iter().any(|t| t.text.contains("YTS")));
        // WEB-DL, a legitimate dash-containing source tag, must survive.
        assert!(toks.iter().any(|t| t.text == "WEB-DL"));
    }

    #[test]
    fn bracketed_year_is_not_a_release_group() {
        assert_eq!(release_group_of("Inception (2010).mkv"), None);
        assert_eq!(release_group_of("Movie.[2010].mkv"), None);
    }

    #[test]
    fn trailing_vocab_tag_is_not_a_release_group() {
        // A source tag ending the name (no group after it) must not be
        // mistaken for a release group, even though it contains a dash.
        assert_eq!(release_group_of("Movie.2020.1080p.WEB-DL.mkv"), None);
        assert_eq!(
            release_group_of("Spirited Away (2001) [1080p] [BluRay] [x264].mkv"),
            None
        );
    }
}
