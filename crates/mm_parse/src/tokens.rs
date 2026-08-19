//! Filename tokenisation (§3.1).
//!
//! Normalises separators, preserves bracketed/parenthesised spans, and strips
//! a trailing release-group token.

use regex::Regex;

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
    let normalised = normalise(name);
    split_tokens(&normalised)
}

/// Strip extension, release group suffix, and collapse separators.
pub fn normalise(name: &str) -> String {
    // 1. Strip extension.
    let base = match name.rfind('.') {
        Some(i) => &name[..i],
        None => name,
    };

    // 2. Strip trailing release-group token: `-RARBG`, `[YTS.MX]`, etc.
    let base = strip_release_group(base);

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
    collapse_whitespace(&out)
}

fn strip_release_group(s: &str) -> String {
    static RE_DASH: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let dash = RE_DASH.get_or_init(|| Regex::new(r"-[A-Za-z0-9]{2,}$").unwrap());
    dash.replace(s, "").into_owned()
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
    }
}
