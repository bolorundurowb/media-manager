//! Filename / folder-name parsing for Phase 1.
//!
//! Rules are table-driven. Nothing here is hard-coded to a particular title.

use std::sync::OnceLock;

use regex::Regex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibraryKind {
    Movies,
    Tv,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedName {
    pub title: String,
    pub year: Option<u16>,
    pub resolution: Option<String>,
    pub source: Option<String>,
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

const RESOLUTIONS: &[&str] = &["480p", "720p", "1080p", "1440p", "2160p", "4k", "8k"];
const SOURCES: &[&str] = &[
    "bluray", "blu-ray", "bdrip", "web-dl", "webdl", "webrip", "remux", "hdtv", "dvdrip",
];
const QUALITY: &[&str] = &[
    "x265",
    "x264",
    "h265",
    "h264",
    "hevc",
    "avc",
    "av1",
    "10bit",
    "10-bit",
    "8bit",
    "hdr",
    "hdr10",
    "dv",
    "dovi",
    "aac",
    "ac3",
    "dts",
    "truehd",
    "atmos",
    "dd5",
    "ddp5",
    "ddp",
    "dd",
    "5.1",
    "7.1",
    "2.0",
    "proper",
    "repack",
    "internal",
    "extended",
    "unrated",
    "directors",
    "cut",
    "multi",
    "subs",
    "dubbed",
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

fn year_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(\d{4})$").expect("year regex"))
}

fn res_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)\b(480p|720p|1080p|1440p|2160p|4k|8k)\b").expect("res regex")
    })
}

fn season_token_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^s(\d{1,2})$").expect("season token regex"))
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

/// Peel `(...)` and `[...]` groups. Parenthesised years win over later bare years.
fn peel_groups(s: &str) -> (String, Option<u16>, Vec<String>) {
    let mut core = String::new();
    let mut year = None;
    let mut tags = Vec::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let closer = match c {
            '(' => Some(')'),
            '[' => Some(']'),
            _ => None,
        };
        if let Some(end) = closer {
            if let Some(rel) = chars[i + 1..].iter().position(|&ch| ch == end) {
                let inner: String = chars[i + 1..i + 1 + rel].iter().collect();
                let trimmed = inner.trim();
                if let Some(y) = parse_year_token(trimmed) {
                    if year.is_none() {
                        year = Some(y);
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
        }
        core.push(c);
        i += 1;
    }
    (unify_separators(&core), year, tags)
}

fn parse_year_token(tok: &str) -> Option<u16> {
    let cap = year_re().captures(tok.trim())?;
    let n: u16 = cap.get(1)?.as_str().parse().ok()?;
    is_valid_year(n).then_some(n)
}

fn normalize_resolution(tok: &str) -> Option<String> {
    let t = tok.trim().to_ascii_lowercase();
    match t.as_str() {
        "480p" | "720p" | "1080p" | "1440p" | "2160p" => Some(t),
        "4k" => Some("2160p".to_string()),
        "8k" => Some("4320p".to_string()),
        _ => None,
    }
}

fn is_source_token(tok: &str) -> Option<String> {
    let t = tok.trim().to_ascii_lowercase();
    let canon = match t.as_str() {
        "bluray" | "blu-ray" | "bdrip" => "BluRay",
        "web-dl" | "webdl" => "WEB-DL",
        "webrip" => "WEBRip",
        "remux" => "Remux",
        "hdtv" => "HDTV",
        "dvdrip" => "DVD",
        _ => return None,
    };
    Some(canon.to_string())
}

fn is_quality_token(tok: &str) -> bool {
    let t = tok.trim().to_ascii_lowercase();
    if QUALITY.contains(&t.as_str()) {
        return true;
    }
    if RESOLUTIONS.contains(&t.as_str()) {
        return true;
    }
    if SOURCES.contains(&t.as_str()) {
        return true;
    }
    codec_group_re().is_match(&t)
}

fn parse_season_token(tok: &str) -> Option<u8> {
    let cap = season_token_re().captures(tok)?;
    cap.get(1)?.as_str().parse().ok()
}

fn apply_tag_blob(blob: &str, resolution: &mut Option<String>, source: &mut Option<String>) {
    let unified = unify_separators(blob);
    if let Some(m) = res_re().find(&unified) {
        if resolution.is_none() {
            *resolution = normalize_resolution(m.as_str());
        }
    }
    for tok in unified.split_whitespace() {
        if source.is_none() {
            if let Some(s) = is_source_token(tok) {
                *source = Some(s);
            }
        }
    }
}

pub fn parse_media_name(raw: &str, kind: LibraryKind) -> Result<ParsedName, ParseError> {
    let raw = strip_extension(raw);
    let (core, mut year, tags) = peel_groups(&unify_separators(raw));
    let mut resolution = None;
    let mut source = None;
    for tag in &tags {
        apply_tag_blob(tag, &mut resolution, &mut source);
    }

    let tokens: Vec<&str> = core.split_whitespace().filter(|t| !t.is_empty()).collect();
    let mut title_parts: Vec<&str> = Vec::new();
    let mut season_from_word: Option<u8> = None;
    let mut season_from_s: Option<u8> = None;
    let mut i = 0;
    let mut in_tags = false;

    while i < tokens.len() {
        let tok = tokens[i];

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
                    i += 2;
                    in_tags = true;
                    continue;
                }
            }
            if !in_tags {
                title_parts.push(tok);
            }
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

        if let Some(r) = normalize_resolution(tok) {
            if resolution.is_none() {
                resolution = Some(r);
            }
            in_tags = true;
            i += 1;
            continue;
        }

        if let Some(y) = parse_year_token(tok) {
            if year.is_none() {
                year = Some(y);
            }
            in_tags = true;
            i += 1;
            continue;
        }

        if let Some(s) = is_source_token(tok) {
            if source.is_none() {
                source = Some(s);
            }
            in_tags = true;
            i += 1;
            continue;
        }

        if is_quality_token(tok) {
            in_tags = true;
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
        Some(i) if i > 0 && name.len() - i <= 6 && name.len() - i > 1 => {
            let ext = &name[i + 1..];
            if ext.chars().all(|c| c.is_ascii_alphanumeric()) {
                &name[..i]
            } else {
                name
            }
        }
        _ => name,
    }
}

pub fn identity_key(title: &str) -> String {
    title.trim().to_lowercase()
}

pub fn movie_folder_name(title: &str, year: Option<u16>) -> String {
    match year {
        Some(y) => format!("{title} ({y})"),
        None => title.to_string(),
    }
}

pub fn version_label(parsed: &ParsedName) -> Option<String> {
    parsed.resolution.clone().or_else(|| parsed.source.clone())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn movie_300_1080() {
        let p = parse_media_name("300 (2006) [1080p]", LibraryKind::Movies).unwrap();
        assert_eq!(p.title, "300");
        assert_eq!(p.year, Some(2006));
        assert_eq!(p.resolution.as_deref(), Some("1080p"));
        assert_eq!(version_label(&p).as_deref(), Some("1080p"));
    }

    #[test]
    fn movie_300_2160() {
        let p = parse_media_name("300 (2006) [2160p]", LibraryKind::Movies).unwrap();
        assert_eq!(p.title, "300");
        assert_eq!(p.year, Some(2006));
        assert_eq!(p.resolution.as_deref(), Some("2160p"));
    }

    #[test]
    fn movie_onward_dotted() {
        let p = parse_media_name(
            "Onward.2020.2160p.HDR.WEB-DL.DD5.1.HEVC-EVO[TGx]",
            LibraryKind::Movies,
        )
        .unwrap();
        assert_eq!(p.title, "Onward");
        assert_eq!(p.year, Some(2020));
        assert_eq!(p.resolution.as_deref(), Some("2160p"));
        assert_eq!(p.source.as_deref(), Some("WEB-DL"));
    }

    #[test]
    fn tv_narcos_redundant_season() {
        let p = parse_media_name(
            "Narcos (2015) Season 1 S01 (1080p BluRay x265 HEVC 10bit AAC 5.1 Vyndros)",
            LibraryKind::Tv,
        )
        .unwrap();
        assert_eq!(p.title, "Narcos");
        assert_eq!(p.year, Some(2015));
        assert_eq!(p.season, Some(1));
        assert_eq!(p.resolution.as_deref(), Some("1080p"));
    }

    #[test]
    fn tv_the_wire_dotted() {
        let p = parse_media_name("The.Wire.S01.1080p.BluRay.x265-RARBG", LibraryKind::Tv).unwrap();
        assert_eq!(p.title, "The Wire");
        assert_eq!(p.year, None);
        assert_eq!(p.season, Some(1));
        assert_eq!(p.resolution.as_deref(), Some("1080p"));
        assert_eq!(p.source.as_deref(), Some("BluRay"));
    }

    #[test]
    fn tv_missing_season_is_error() {
        let err = parse_media_name("Narcos (2015)", LibraryKind::Tv).unwrap_err();
        assert_eq!(err, ParseError::MissingSeason);
    }

    #[test]
    fn tv_season_mismatch_is_error() {
        let err = parse_media_name("Show Season 1 S02 1080p", LibraryKind::Tv).unwrap_err();
        match err {
            ParseError::SeasonMismatch { a, b } => {
                assert!(a == 1 || b == 1);
                assert!(a == 2 || b == 2);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn different_years_are_different_identities() {
        let a = parse_media_name("300 (2006)", LibraryKind::Movies).unwrap();
        let b = parse_media_name("300 (2014)", LibraryKind::Movies).unwrap();
        assert_eq!(identity_key(&a.title), identity_key(&b.title));
        assert_ne!(a.year, b.year);
    }

    #[test]
    fn episode_sxxexx_and_span() {
        let e = parse_episode("Narcos.S01E03.1080p.mkv").unwrap();
        assert_eq!(e.season, 1);
        assert_eq!(e.episode, 3);
        assert_eq!(e.episode_end, None);
        let m = parse_episode("Show S01E01-E02.mkv").unwrap();
        assert_eq!(m.episode, 1);
        assert_eq!(m.episode_end, Some(2));
        let x = parse_episode("show.1x05.mkv").unwrap();
        assert_eq!(x.season, 1);
        assert_eq!(x.episode, 5);
    }

    #[test]
    fn resolution_not_a_year() {
        assert!(!is_valid_year(1080));
        assert!(!is_valid_year(2160));
        assert!(is_valid_year(2006));
    }

    #[test]
    fn sample_and_trailer_detected() {
        assert!(is_extra_filename("movie.sample.mkv"));
        assert!(is_extra_filename("movie-trailer.mp4"));
        assert!(!is_extra_filename("The.Wire.S01E01.mkv"));
    }

    #[test]
    fn numeric_title_is_not_eaten_as_a_tag() {
        let p = parse_media_name("42 (2013)", LibraryKind::Movies).unwrap();
        assert_eq!(p.title, "42");
        assert_eq!(p.year, Some(2013));
    }

    #[test]
    fn season_of_the_witch_stays_a_movie_title() {
        let p = parse_media_name("Season of the Witch (2011)", LibraryKind::Movies).unwrap();
        assert_eq!(p.title, "Season of the Witch");
        assert_eq!(p.year, Some(2011));
        assert_eq!(p.season, None);
    }
}
