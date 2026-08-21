//! Filename / folder-name parsing.
//!
//! Rules are table-driven. Nothing here is hard-coded to a particular title.
//! Tokens are consumed in this order: resolution / source / edition / HDR /
//! audio / codec, then a (bare) year, and whatever is left is the title.

use std::sync::OnceLock;

use regex::Regex;
use unicode_normalization::UnicodeNormalization;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LibraryKind {
    #[default]
    Movies,
    Tv,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedName {
    pub title: String,
    pub year: Option<u16>,
    pub resolution: Option<String>,
    pub source: Option<String>,
    pub edition: Option<String>,
    /// Last-resort, known non-title tag used only when resolution, edition,
    /// and source are all absent.
    pub fallback_tag: Option<String>,
    pub season: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedEpisode {
    pub season: u8,
    pub episode: u16,
    pub episode_end: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    EmptyTitle,
    MissingSeason,
    SeasonMismatch { a: u8, b: u8 },
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::EmptyTitle => write!(f, "could not extract a title"),
            ParseError::MissingSeason => write!(f, "no season number found"),
            ParseError::SeasonMismatch { a, b } => {
                write!(f, "conflicting season numbers {a} and {b}")
            }
        }
    }
}

impl std::error::Error for ParseError {}

// ---------------------------------------------------------------------------
// Additive vocabulary tables. Each token maps to a canonical display value.
// ---------------------------------------------------------------------------

/// (token, canonical) — resolution output is the Jellyfin resolution string.
const RESOLUTIONS: &[(&str, &str)] = &[
    ("480p", "480p"),
    ("576p", "576p"),
    ("720p", "720p"),
    ("1080p", "1080p"),
    ("1440p", "1440p"),
    ("2160p", "2160p"),
    ("4k", "2160p"),
    ("8k", "4320p"),
];

/// (token, canonical) — source names.
const SOURCES: &[(&str, &str)] = &[
    ("bluray", "BluRay"),
    ("blu-ray", "BluRay"),
    ("bdrip", "BluRay"),
    ("brrip", "BluRay"),
    ("web-dl", "WEB-DL"),
    ("webdl", "WEB-DL"),
    ("webrip", "WEBRip"),
    ("remux", "Remux"),
    ("hdtv", "HDTV"),
    ("dvdrip", "DVD"),
];

/// Single-token editions. Multi-word forms live in `EDITION_PHRASES` so that
/// e.g. "cut" is never stolen out of a title by itself.
const EDITIONS: &[(&str, &str)] = &[
    ("extended", "Extended"),
    ("unrated", "Unrated"),
    ("uncut", "Uncut"),
    ("directors", "Director's Cut"),
    ("theatrical", "Theatrical"),
    ("imax", "IMAX"),
    ("remastered", "Remastered"),
    ("criterion", "Criterion"),
    ("alternate", "Alternate"),
];

/// Multi-token edition phrases, matched before single tokens.
const EDITION_PHRASES: &[(&[&str], &str)] = &[
    (&["directors", "cut"], "Director's Cut"),
    (&["director", "cut"], "Director's Cut"),
    (&["extended", "cut"], "Extended"),
    (&["theatrical", "cut"], "Theatrical"),
    (&["special", "edition"], "Special Edition"),
    (&["collectors", "edition"], "Collector's Edition"),
    (&["ultimate", "edition"], "Ultimate Edition"),
];

/// Release-group / tag noise that is never part of a title.
const JUNK_TOKENS: &[&str] = &[
    "proper", "repack", "internal", "readnfo", "multi", "dubbed", "subbed", "limited",
];

/// HDR / dynamic-range tokens (consumed, not stored).
const HDR_TOKENS: &[&str] = &[
    "hdr",
    "hdr10",
    "hdr10+",
    "hdr10plus",
    "dolby",
    "vision",
    "dv",
    "dovi",
    "hlg",
    "sdr",
];

/// Audio codec tokens (consumed, not stored). Channel counts are matched
/// separately by `channel_re`.
const AUDIO_TOKENS: &[&str] = &[
    "aac", "ac3", "eac3", "dts", "dtshd", "dts-hd", "truehd", "atmos", "dd", "ddp", "flac", "mp3",
    "opus",
];

/// Video codec / bit-depth tokens (consumed, not stored).
const CODEC_TOKENS: &[&str] = &[
    "x265", "x264", "h265", "h264", "hevc", "avc", "av1", "10bit", "10-bit", "8bit", "mpeg2",
    "vc1", "xvid", "divx", "vp9",
];

/// Resolutions that look numeric and must never be treated as a year.
const NOT_YEARS: &[u16] = &[480, 576, 720, 1080, 1440, 2160, 4320];

pub fn max_year() -> u16 {
    current_year().saturating_add(2)
}

fn current_year() -> u16 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    1970 + (secs / 31_557_600) as u16
}

pub fn is_valid_year(n: u16) -> bool {
    n >= 1888 && n <= max_year() && !NOT_YEARS.contains(&n)
}

/// Map `.`, `_` and `+` to spaces and collapse runs of whitespace. Used before
/// tokenising so dotted names (`Onward.2020.…`) parse like spaced names.
pub fn unify_separators(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for ch in s.chars() {
        let mapped = match ch {
            '.' | '_' | '+' => ' ',
            other => other,
        };
        if mapped.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(mapped);
            prev_space = false;
        }
    }
    out.trim().to_string()
}

fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
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
    out.trim().to_string()
}

fn year_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(\d{4})$").expect("year regex"))
}

fn season_token_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)^s(\d{1,2})(?:e\d{1,3}(?:-e?\d{1,3})?)?$").expect("season token regex")
    })
}

fn episode_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)\bS(\d{1,2})E(\d{1,3})(?:-E?(\d{1,3}))?\b").expect("episode regex")
    })
}

fn x_episode_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\b(\d{1,2})x(\d{1,3})\b").expect("x episode regex"))
}

fn codec_group_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)^(x264|x265|h264|h265|hevc|avc|av1)-.+$").expect("codec-group regex")
    })
}

fn channel_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^\d\.\d$").expect("channel regex"))
}

/// Matches a leading tracker/indexer watermark: a domain-like token
/// (`www.UIndex.org`, `YTS.MX`, `1337x.to`, ...) followed by a dash
/// separator that is surrounded by real whitespace on both sides. Captures
/// everything after the separator.
///
/// Two things distinguish a genuine site watermark from an ordinary dotted
/// release name (`Onward.2020.2160p.HDR.WEB-DL...`) or a title with periods
/// in it (`Mr. Robot`, `S.W.A.T.`):
/// - every dot in the domain segment must be immediately followed by a
///   letter/digit, never by whitespace (rules out sentence-style periods);
/// - the dash must have at least one space on each side, and domain
///   segments cannot themselves contain a dash (rules out glued release
///   tags like `WEB-DL` or `x264-GROUP`, which are never space-separated).
fn site_prefix_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)^[a-z0-9]+(?:\.[a-z0-9]+)*\.[a-z]{2,}\s+[-–—]+\s+(.+)$")
            .expect("site prefix regex")
    })
}

/// Strip a leading tracker/indexer watermark (e.g. `www.UIndex.org - `,
/// `YTS.MX - `) from a raw release name. Only strips when the leading token
/// looks like a domain name and the remainder after the dash is non-empty;
/// otherwise the input is returned unchanged. Must run before
/// `unify_separators`, which would otherwise turn the domain's dots into
/// spaces and hide the pattern.
pub fn strip_release_site_prefix(raw: &str) -> &str {
    match site_prefix_re().captures(raw) {
        Some(caps) => {
            let rest = caps.get(1).expect("group 1 always present on match").as_str();
            if rest.trim().is_empty() {
                raw
            } else {
                rest
            }
        }
        None => raw,
    }
}

/// Peel `(...)` and `[...]` groups. A parenthesised year is preferred over a
/// bracketed one; everything else becomes a tag for `apply_tag_blob`.
fn peel_groups(s: &str) -> (String, Option<u16>, Vec<String>) {
    let mut core = String::new();
    let mut paren_year: Option<u16> = None;
    let mut bracket_year: Option<u16> = None;
    let mut tags = Vec::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let (is_paren, close) = match c {
            '(' => (true, ')'),
            '[' => (false, ']'),
            _ => {
                core.push(c);
                i += 1;
                continue;
            }
        };
        if let Some(rel) = chars[i + 1..].iter().position(|&ch| ch == close) {
            let inner: String = chars[i + 1..i + 1 + rel].iter().collect();
            let trimmed = inner.trim();
            if let Some(y) = parse_year_token(trimmed) {
                if is_paren {
                    if paren_year.is_none() {
                        paren_year = Some(y);
                    }
                } else if bracket_year.is_none() {
                    bracket_year = Some(y);
                }
            } else if !trimmed.is_empty() {
                tags.push(trimmed.to_string());
            }
            i += rel + 2;
            if !core.ends_with(' ') {
                core.push(' ');
            }
            continue;
        }
        core.push(c);
        i += 1;
    }
    (unify_separators(&core), paren_year.or(bracket_year), tags)
}

fn parse_year_token(tok: &str) -> Option<u16> {
    let cap = year_re().captures(tok.trim())?;
    let n: u16 = cap.get(1)?.as_str().parse().ok()?;
    is_valid_year(n).then_some(n)
}

fn normalize_resolution(tok: &str) -> Option<String> {
    let t = tok.trim().to_ascii_lowercase();
    RESOLUTIONS
        .iter()
        .find(|(k, _)| *k == t)
        .map(|(_, v)| (*v).to_string())
}

fn source_token(tok: &str) -> Option<&'static str> {
    let t = tok.trim().to_ascii_lowercase();
    SOURCES.iter().find(|(k, _)| *k == t).map(|(_, v)| *v)
}

/// Lowercase and strip apostrophes so `Director's` matches `directors`.
fn norm_match_token(tok: &str) -> String {
    tok.trim()
        .chars()
        .filter(|c| *c != '\'' && *c != '\u{2019}' && *c != '\u{02bc}')
        .collect::<String>()
        .to_ascii_lowercase()
}

fn edition_token(tok: &str) -> Option<&'static str> {
    let t = norm_match_token(tok);
    EDITIONS.iter().find(|(k, _)| *k == t).map(|(_, v)| *v)
}

fn match_edition_phrase(tokens: &[&str]) -> Option<(usize, String)> {
    let norms: Vec<String> = tokens.iter().take(3).map(|t| norm_match_token(t)).collect();
    for (phrase, label) in EDITION_PHRASES {
        if norms.len() >= phrase.len()
            && phrase.iter().enumerate().all(|(i, want)| norms[i] == *want)
        {
            return Some((phrase.len(), (*label).to_string()));
        }
    }
    tokens
        .first()
        .and_then(|t| edition_token(t))
        .map(|e| (1, e.to_string()))
}

fn is_junk_token(tok: &str) -> bool {
    JUNK_TOKENS.contains(&norm_match_token(tok).as_str())
}

fn is_hdr_token(tok: &str) -> bool {
    HDR_TOKENS.contains(&tok.trim().to_ascii_lowercase().as_str())
}

fn is_audio_token(tok: &str) -> bool {
    let t = tok.trim().to_ascii_lowercase();
    AUDIO_TOKENS.contains(&t.as_str()) || channel_re().is_match(&t)
}

fn is_codec_token(tok: &str) -> bool {
    let t = tok.trim().to_ascii_lowercase();
    CODEC_TOKENS.contains(&t.as_str()) || codec_group_re().is_match(&t)
}

fn fallback_version_tag(tok: &str) -> Option<String> {
    let normalized = tok.trim().to_ascii_lowercase();
    if is_hdr_token(tok) {
        return Some(match normalized.as_str() {
            "hdr10+" | "hdr10plus" => "HDR10+".into(),
            "dolby" | "vision" | "dv" | "dovi" => "Dolby Vision".into(),
            "sdr" => "SDR".into(),
            "hlg" => "HLG".into(),
            _ => normalized.to_ascii_uppercase(),
        });
    }
    if is_codec_token(tok) {
        return Some(match normalized.as_str() {
            "h265" | "hevc" => "HEVC".into(),
            "h264" | "avc" => "H264".into(),
            "av1" => "AV1".into(),
            _ => normalized,
        });
    }
    None
}

fn parse_season_token(tok: &str) -> Option<u8> {
    let cap = season_token_re().captures(tok)?;
    cap.get(1)?.as_str().parse().ok()
}

/// Bare numeric range following the word "season" (`Season 1-4`). Unlike a
/// single `Season 5`, a range doesn't identify one season, so it is dropped
/// as noise rather than parsed into `ParsedName::season`.
fn season_range_word_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\d{1,3}-\d{1,3}$").expect("season range word regex"))
}

/// Bare `SxxSyy`-style range token (`S01-S04`, `S1-S4`), as opposed to
/// `season_token_re`'s `SxxExx` episode form. Also dropped as noise.
fn season_range_token_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^s\d{1,3}-s?\d{1,3}$").expect("season range token regex"))
}

/// A directory whose entire name is a bare season indicator (`Season 1`,
/// `Season 01`, `S01`, `Specials`/`Special` → season 0) and nothing else.
/// Used for release packs laid out as `Show Name (Year) Season 1-4/Season
/// 1/`, `.../Season 2/`, ..., where the title lives on the container
/// directory and each subfolder is only the season split out.
pub fn bare_season_folder(name: &str) -> Option<u8> {
    let unified = unify_separators(name);
    let trimmed = unified.trim();
    if trimmed.eq_ignore_ascii_case("specials") || trimmed.eq_ignore_ascii_case("special") {
        return Some(0);
    }
    match trimmed.split_whitespace().collect::<Vec<_>>().as_slice() {
        [word, num] if word.eq_ignore_ascii_case("season") => num.parse::<u8>().ok(),
        [tok] => parse_season_token(tok),
        _ => None,
    }
}

fn apply_tag_blob(
    blob: &str,
    resolution: &mut Option<String>,
    source: &mut Option<String>,
    edition: &mut Option<String>,
    fallback_tag: &mut Option<String>,
) {
    let unified = unify_separators(blob);
    let tag_tokens: Vec<&str> = unified.split_whitespace().collect();
    if edition.is_none() {
        if let Some((_, label)) = match_edition_phrase(&tag_tokens) {
            *edition = Some(label);
        }
    }
    for tok in &tag_tokens {
        if resolution.is_none() {
            if let Some(r) = normalize_resolution(tok) {
                *resolution = Some(r);
                continue;
            }
        }
        if source.is_none() {
            if let Some(s) = source_token(tok) {
                *source = Some(s.to_string());
            }
        }
        if fallback_tag.is_none() {
            *fallback_tag = fallback_version_tag(tok);
        }
    }
}

pub fn parse_media_name(raw: &str, kind: LibraryKind) -> Result<ParsedName, ParseError> {
    let raw = strip_extension(raw);
    let raw = strip_release_site_prefix(raw);
    let (core, mut year, tags) = peel_groups(&unify_separators(raw));
    let mut resolution = None;
    let mut source = None;
    let mut edition = None;
    let mut fallback_tag = None;
    for tag in &tags {
        apply_tag_blob(
            tag,
            &mut resolution,
            &mut source,
            &mut edition,
            &mut fallback_tag,
        );
    }

    let tokens: Vec<&str> = core.split_whitespace().collect();
    let mut title_parts: Vec<&str> = Vec::new();
    let mut season_from_word: Option<u8> = None;
    let mut season_from_s: Option<u8> = None;
    // Once a quality/season/year token is seen, later unknown tokens are
    // release-group noise and are dropped rather than treated as title.
    let mut in_tags = false;
    let mut i = 0;

    while i < tokens.len() {
        let tok = tokens[i];

        // Season *ranges* ("Season 1-4", "S01-S04") describe a pack spanning
        // several seasons, not one; they carry no single season number worth
        // keeping, so they're dropped as noise in both movie and TV parsing
        // (a single "Season 5" is still handled per-kind below).
        if tok.eq_ignore_ascii_case("season") {
            if let Some(next) = tokens.get(i + 1) {
                if season_range_word_re().is_match(next) {
                    in_tags = true;
                    i += 2;
                    continue;
                }
            }
        }
        if season_range_token_re().is_match(tok) {
            in_tags = true;
            i += 1;
            continue;
        }

        // Season tokens only make sense in TV mode.
        if kind == LibraryKind::Tv {
            if tok.eq_ignore_ascii_case("season") {
                if let Some(next) = tokens.get(i + 1) {
                    if let Ok(n) = next.parse::<u8>() {
                        match season_from_word {
                            None => season_from_word = Some(n),
                            Some(prev) if prev != n => {
                                return Err(ParseError::SeasonMismatch { a: prev, b: n });
                            }
                            Some(_) => {}
                        }
                        in_tags = true;
                        i += 2;
                        continue;
                    }
                }
                title_parts.push(tok);
                i += 1;
                continue;
            }

            if let Some(n) = parse_season_token(tok) {
                match season_from_s {
                    None => season_from_s = Some(n),
                    Some(prev) if prev != n => {
                        return Err(ParseError::SeasonMismatch { a: prev, b: n });
                    }
                    Some(_) => {}
                }
                in_tags = true;
                i += 1;
                continue;
            }
        }

        if tok == "-" || tok == "--" || tok == "—" {
            if !title_parts.is_empty() {
                in_tags = true;
            }
            i += 1;
            continue;
        }

        if let Some(r) = normalize_resolution(tok) {
            if resolution.is_none() {
                resolution = Some(r);
            }
            in_tags = true;
            i += 1;
            continue;
        }

        let after_title = !title_parts.is_empty() || in_tags;

        if after_title {
            if let Some((n, label)) = match_edition_phrase(&tokens[i..]) {
                if edition.is_none() {
                    edition = Some(label);
                }
                in_tags = true;
                i += n;
                continue;
            }
        }

        if after_title {
            if let Some(s) = source_token(tok) {
                if source.is_none() {
                    source = Some(s.to_string());
                }
                in_tags = true;
                i += 1;
                continue;
            }
        }

        if after_title
            && (is_hdr_token(tok)
                || is_audio_token(tok)
                || is_codec_token(tok)
                || is_junk_token(tok))
        {
            if fallback_tag.is_none() {
                fallback_tag = fallback_version_tag(tok);
            }
            in_tags = true;
            i += 1;
            continue;
        }

        // Bare year token: only consumed as the year when it follows the title
        // and no year is known yet. A leading year-like token (`2012`) or one
        // after a parenthesised year (`Blade Runner 2049 (2017)`) stays title.
        if let Some(y) = parse_year_token(tok) {
            if year.is_none() && !title_parts.is_empty() {
                year = Some(y);
                in_tags = true;
            } else if !in_tags {
                title_parts.push(tok);
            }
            i += 1;
            continue;
        }

        if in_tags {
            i += 1;
            continue;
        }

        title_parts.push(tok);
        i += 1;
    }

    let title = title_parts.join(" ");
    if title.is_empty() {
        return Err(ParseError::EmptyTitle);
    }

    let season = match (season_from_word, season_from_s) {
        (Some(a), Some(b)) if a != b => {
            return Err(ParseError::SeasonMismatch { a, b });
        }
        (Some(a), _) => Some(a),
        (_, Some(b)) => Some(b),
        (None, None) => None,
    };

    if kind == LibraryKind::Tv && season.is_none() {
        return Err(ParseError::MissingSeason);
    }

    Ok(ParsedName {
        title,
        year,
        resolution,
        source,
        edition,
        fallback_tag,
        season,
    })
}

pub fn parse_episode(filename: &str) -> Option<ParsedEpisode> {
    let stem = strip_extension(filename);
    let unified = unify_separators(stem);
    if let Some(c) = episode_re().captures(&unified) {
        let season: u8 = c.get(1)?.as_str().parse().ok()?;
        let episode: u16 = c.get(2)?.as_str().parse().ok()?;
        let episode_end = c.get(3).and_then(|m| m.as_str().parse().ok());
        return Some(ParsedEpisode {
            season,
            episode,
            episode_end,
        });
    }
    if let Some(c) = x_episode_re().captures(&unified) {
        let season: u8 = c.get(1)?.as_str().parse().ok()?;
        let episode: u16 = c.get(2)?.as_str().parse().ok()?;
        return Some(ParsedEpisode {
            season,
            episode,
            episode_end: None,
        });
    }
    None
}

pub fn strip_extension(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i > 0 => {
            let ext = name[i + 1..].to_ascii_lowercase();
            if VIDEO_EXTS.contains(&ext.as_str()) || SUB_EXTS.contains(&ext.as_str()) {
                &name[..i]
            } else {
                name
            }
        }
        _ => name,
    }
}

/// Matching-only identity: Unicode NFC, case-fold, `&`/`+` → "and", strip
/// apostrophes, and collapse remaining punctuation to spaces. The display
/// title is never derived from this key.
pub fn identity_key(title: &str) -> String {
    let nfc: String = title.nfc().collect();
    let lower = nfc.to_lowercase();
    let mut out = String::with_capacity(lower.len());
    for ch in lower.chars() {
        match ch {
            '&' | '+' => out.push_str(" and "),
            '\'' | '\u{2019}' | '\u{02bc}' => {}
            c if c.is_alphanumeric() => out.push(c),
            _ => out.push(' '),
        }
    }
    collapse_whitespace(&out)
}

pub fn movie_folder_name(title: &str, year: Option<u16>) -> String {
    match year {
        Some(y) => format!("{title} ({y})"),
        None => title.to_string(),
    }
}

/// Version fallback: resolution → edition → source → known non-title tag.
pub fn version_label(parsed: &ParsedName) -> Option<String> {
    parsed
        .resolution
        .clone()
        .or_else(|| parsed.edition.clone())
        .or_else(|| parsed.source.clone())
        .or_else(|| parsed.fallback_tag.clone())
}

pub fn show_folder_name(title: &str, year: Option<u16>) -> String {
    movie_folder_name(title, year)
}

pub fn season_folder_name(season: u8) -> String {
    format!("Season {season:02}")
}

pub fn episode_file_stem(show: &str, ep: &ParsedEpisode) -> String {
    match ep.episode_end {
        Some(end) => format!("{show} S{:02}E{:02}-E{end:02}", ep.season, ep.episode),
        None => format!("{show} S{:02}E{:02}", ep.season, ep.episode),
    }
}

pub const VIDEO_EXTS: &[&str] = &[
    "mkv", "mp4", "m4v", "avi", "mov", "wmv", "ts", "m2ts", "webm",
];
pub const SUB_EXTS: &[&str] = &["srt", "ass", "ssa", "vtt", "sub", "idx", "sup"];

pub fn extension_lower(path: &std::path::Path) -> Option<String> {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
}

pub fn is_video_path(path: &std::path::Path) -> bool {
    extension_lower(path)
        .map(|e| VIDEO_EXTS.contains(&e.as_str()))
        .unwrap_or(false)
}

pub fn is_sub_path(path: &std::path::Path) -> bool {
    extension_lower(path)
        .map(|e| SUB_EXTS.contains(&e.as_str()))
        .unwrap_or(false)
}

/// Skip extras whose filename contains a `sample` or `trailer` token.
pub fn is_extra_filename(name: &str) -> bool {
    let stem = unify_separators(strip_extension(name)).to_ascii_lowercase();
    stem.split(|c: char| !c.is_ascii_alphanumeric())
        .any(|tok| tok == "sample" || tok == "trailer")
}
