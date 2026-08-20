//! TV episode filename parsing (Phase 5, §3.2).
//!
//! Mirrors `crate::parser`'s shape (tokenise → run extractors in priority
//! order, claiming spans → assign residual positionally) but keyed on season/
//! episode markers instead of a year.
//!
//! ## Why this doesn't just call `crate::tokens::tokenize` on the raw name
//!
//! `crate::tokens::normalise_and_capture` strips a *trailing* release-group
//! token by looking for `-WORD` at the end of the (dot-joined) stem. A
//! multi-episode marker like `S01E01-E02` or `S01E01-02` is exactly that
//! shape — `-E02`/`-02` looks identical to `-RARBG` to that heuristic, and
//! when the marker happens to be the last dash-bearing thing in the name (no
//! release group after it), the movie tokeniser silently eats the second
//! episode number as a fake "release group". That bug is real and would
//! reproduce here if this module fed raw filenames straight into
//! `tokens::tokenize`.
//!
//! So the season/episode marker is found and extracted **first**, directly
//! against the raw filename via regex (never through `tokens`), and its span
//! is masked out (replaced with a single placeholder token) before the
//! (reused, unmodified) `crate::tokens`/`crate::extractors` machinery ever
//! sees the string. By the time `ResolutionExtractor`/`YearExtractor`/etc. and
//! the release-group stripper run, the marker text no longer exists in the
//! stream, so it cannot be mistaken for a year, a resolution tag, or a
//! release group, and the release-group stripper cannot mistake the marker's
//! own dash for a `-GROUP` suffix.
//!
//! Positional residual assignment (§3.1) then works exactly as it does for
//! movies, generalised to two slots instead of one: the placeholder's
//! position is one more "claimed span" among the others, so `title` is the
//! residual before the *first* claimed span (year, marker, or whichever comes
//! first) and `episode_title` is the residual strictly between the marker and
//! the next claimed span after it (or the end of the string).

use std::sync::OnceLock;

use regex::Regex;

use crate::extractors::{
    AudioFormatExtractor, Claim, CopyNumberExtractor, Extractor, HdrExtractor, ParseField,
    ResolutionExtractor, SourceExtractor, VideoCodecExtractor, YearExtractor,
};
use crate::model::{ParseOptions, ParsedEpisode, known};
use crate::tokens::{normalise, release_group_of, tokenize};
use mm_core::{Confidence, Source};

/// A single unicode private-use-area character used to stand in for the
/// season/episode marker (or title-only special phrase) once it has been
/// found and removed from consideration by the vocab extractors. Chosen
/// because it can never appear in a real filename and is not whitespace, a
/// separator, or a digit, so it always tokenises to exactly one token and
/// never collides with anything a real extractor is looking for.
const PLACEHOLDER: char = '\u{E000}';

/// A found season/episode (or special) marker in the raw filename.
#[derive(Debug, Clone)]
struct MarkerMatch {
    start: usize,
    end: usize,
    season: Option<u16>,
    episodes: Option<Vec<u16>>,
    ambiguous: bool,
    /// Set only for the "title-only special" shape (`Christmas Special`,
    /// bare `OVA`): the matched phrase itself becomes `episode_title`
    /// directly, since there is no separate "after the marker" residual to
    /// derive it from — the phrase *is* the whole designation.
    special_phrase: Option<String>,
}

/// Parse a filename as a TV episode.
pub fn parse_episode_filename(filename: &str, opts: &ParseOptions) -> ParsedEpisode {
    let mut episode = ParsedEpisode::new();

    // Underscores and `+` are word characters to the regex engine, so a
    // marker directly touching one (`Show_S01E02_1080p`) would fail every
    // `\b` boundary check in `find_marker`. Replace them with spaces first —
    // this is a 1-for-1 char substitution, so byte offsets found against this
    // string are still valid against `filename` itself for masking. Dots are
    // deliberately left alone: they are not word characters, so `\b` already
    // treats them as boundaries, and leaving them intact keeps the later
    // `tokens::normalise`/`release_group_of` extension-stripping logic (which
    // looks for the last `.`) working exactly as it does for movies.
    let prescanned = replace_word_separators(filename);

    let marker = find_marker(&prescanned, opts.min_year, opts.max_year);

    let masked = match &marker {
        Some(m) => mask_span(&prescanned, m.start, m.end),
        None => prescanned.clone(),
    };

    let normalised = normalise(&masked);
    if normalised.is_empty() {
        return episode;
    }
    let tokens = tokenize(&masked);
    if tokens.is_empty() {
        return episode;
    }

    // Run the same vocab/year/copy extractors movies use, in the same
    // priority order, over the marker-masked token stream. Language is
    // intentionally omitted here for the same reason movies omit it from
    // filename parsing: short codes collide with ordinary title words.
    // Subtitle language is handled separately during association (§5.4).
    let mut claimed: Vec<(usize, usize)> = Vec::new();
    let mut claims: Vec<Claim> = Vec::new();
    let extractors: Vec<Box<dyn Extractor>> = vec![
        Box::new(ResolutionExtractor),
        Box::new(SourceExtractor),
        Box::new(VideoCodecExtractor),
        Box::new(AudioFormatExtractor),
        Box::new(HdrExtractor),
        Box::new(YearExtractor::new(opts.min_year, opts.max_year)),
        Box::new(CopyNumberExtractor),
    ];
    for ex in extractors {
        if let Some(claim) = ex.extract(&tokens, &normalised) {
            if !overlaps(&claimed, claim.start, claim.end) {
                claimed.push((claim.start, claim.end));
                claims.push(claim);
            }
        }
    }
    for claim in &claims {
        apply_claim(&mut episode, claim);
    }

    // Locate the placeholder token, if a marker was found.
    let placeholder_span: Option<(usize, usize)> = tokens
        .iter()
        .find(|t| t.text == PLACEHOLDER.to_string())
        .map(|t| (t.start, t.end));

    // `title` = residual before the *first* claimed span overall (mirrors
    // `parser::parse_movie_filename`'s `first_anchor`, generalised to also
    // include the marker placeholder as a claim for this purpose).
    let mut anchor_starts: Vec<usize> = claims.iter().map(|c| c.start).collect();
    if let Some((s, _)) = placeholder_span {
        anchor_starts.push(s);
    }
    let first_anchor = anchor_starts.into_iter().min().unwrap_or(normalised.len());
    let title_text = join_tokens(&tokens, |t| {
        t.end <= first_anchor && t.text != PLACEHOLDER.to_string() && !is_bare_separator(&t.text)
    });
    if !title_text.is_empty() {
        episode.title = known(title_text, Source::Filename, Confidence::Medium);
    }

    // `episode_title`.
    if let Some(m) = &marker {
        if let Some(phrase) = &m.special_phrase {
            episode.episode_title = known(phrase.clone(), Source::Filename, Confidence::Medium);
        } else if let Some((_, ph_end)) = placeholder_span {
            let mut after_starts: Vec<usize> = claims
                .iter()
                .filter(|c| c.start >= ph_end)
                .map(|c| c.start)
                .collect();
            after_starts.sort_unstable();
            let episode_title_end = after_starts.first().copied().unwrap_or(normalised.len());
            let et_text = join_tokens(&tokens, |t| {
                t.start >= ph_end
                    && t.end <= episode_title_end
                    && t.text != PLACEHOLDER.to_string()
                    && !is_bare_separator(&t.text)
            });
            if !et_text.is_empty() {
                episode.episode_title = known(et_text, Source::Filename, Confidence::Medium);
            }
        }
    }

    // season / episodes / ambiguous from the marker itself.
    if let Some(m) = &marker {
        if let Some(season) = m.season {
            episode.season = known(season, Source::Filename, Confidence::High);
        }
        if let Some(eps) = &m.episodes {
            let conf = if m.ambiguous {
                Confidence::Low
            } else {
                Confidence::High
            };
            episode.episodes = known(eps.clone(), Source::Filename, conf);
        }
        episode.ambiguous = m.ambiguous;
    }

    if let Some(group) = release_group_of(&masked) {
        episode.release_group = known(group, Source::Filename, Confidence::Low);
    }

    episode
}

fn replace_word_separators(s: &str) -> String {
    s.chars()
        .map(|c| if c == '_' || c == '+' { ' ' } else { c })
        .collect()
}

/// Replace `raw[start..end)` with ` <PLACEHOLDER> ` — same total structure
/// (a single non-whitespace token bounded by separators), so the rest of the
/// pipeline (which does its own whitespace collapsing) handles it uniformly
/// regardless of what separators originally surrounded the marker.
fn mask_span(raw: &str, start: usize, end: usize) -> String {
    let mut out = String::with_capacity(raw.len() + 2);
    out.push_str(&raw[..start]);
    out.push(' ');
    out.push(PLACEHOLDER);
    out.push(' ');
    out.push_str(&raw[end..]);
    out
}

fn overlaps(claimed: &[(usize, usize)], start: usize, end: usize) -> bool {
    claimed.iter().any(|(cs, ce)| start < *ce && end > *cs)
}

/// `true` for a token that is nothing but separator punctuation left over
/// from a literal ` - ` in the source name (e.g. the naming template's own
/// `{title}[ ({year})] - {episode_code}[ - {episode_title}]` shape). Movie
/// filenames don't hit this because edition/year are claimed directly by
/// regex rather than via positional residual; TV's two-slot residual
/// (title *and* episode_title) does hit it, so a bare `-` token must be
/// dropped rather than joined into the residual text.
fn is_bare_separator(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c == '-')
}

fn apply_claim(episode: &mut ParsedEpisode, claim: &Claim) {
    let src = Source::Filename;
    match claim.field {
        ParseField::Year => {
            if let Ok(y) = claim.value.parse::<u16>() {
                episode.year = known(y, src, Confidence::High);
            }
        }
        ParseField::Resolution => {
            episode.resolution = known(claim.value.clone(), src, Confidence::Medium);
        }
        ParseField::Source => {
            let canonical = match claim.value.as_str() {
                "BRRip" => "BDRip",
                other => other,
            };
            episode.source = known(canonical.to_string(), src, Confidence::Medium);
        }
        ParseField::VideoCodec => {
            episode.video_codec = known(claim.value.clone(), src, Confidence::Medium);
        }
        ParseField::AudioFormat => {
            let canonical = match claim.value.as_str() {
                "DD5.1" => "AC3",
                "DDP5.1" => "DDP",
                other => other,
            };
            episode.audio_format = known(canonical.to_string(), src, Confidence::Medium);
        }
        ParseField::Hdr => {
            episode.hdr = known(claim.value.clone(), src, Confidence::Medium);
        }
        ParseField::Copy => {
            if let Ok(n) = claim.value.parse::<u16>() {
                episode.copy = known(n, src, Confidence::High);
            }
        }
        // Edition/Language/ReleaseGroup are not produced by the extractor
        // list above for TV (see the comment in `parse_episode_filename`).
        ParseField::Edition | ParseField::Language | ParseField::ReleaseGroup => {}
    }
}

fn join_tokens(tokens: &[crate::tokens::Token], pred: impl Fn(&crate::tokens::Token) -> bool) -> String {
    tokens
        .iter()
        .filter(|t| pred(t))
        .map(|t| t.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

// ---------------------------------------------------------------------------
// Marker detection (§3.2 tricky-cases table)
// ---------------------------------------------------------------------------

fn find_marker(text: &str, min_year: u16, max_year: u16) -> Option<MarkerMatch> {
    // 1. Cross-season range: `S01E01-S01E02`. Ranges expand only within one
    //    season (§3.2); a genuine cross-season range is a diagnostic, not a
    //    guess.
    if let Some(caps) = re_cross_season().captures(text) {
        let m = caps.get(0).unwrap();
        let (s1, e1, s2, e2) = (
            caps[1].parse::<u16>().ok()?,
            caps[2].parse::<u16>().ok()?,
            caps[3].parse::<u16>().ok()?,
            caps[4].parse::<u16>().ok()?,
        );
        return Some(if s1 == s2 {
            MarkerMatch {
                start: m.start(),
                end: m.end(),
                season: Some(s1),
                episodes: Some(expand_range(e1, e2)),
                ambiguous: false,
                special_phrase: None,
            }
        } else {
            MarkerMatch {
                start: m.start(),
                end: m.end(),
                season: None,
                episodes: None,
                ambiguous: true,
                special_phrase: None,
            }
        });
    }

    // 2. Concatenated multi-episode: `S01E01E02[E03...]`.
    if let Some(m) = re_concat_e().find(text) {
        let whole = m.as_str();
        if let Some(season) = leading_season(whole) {
            let episodes: Vec<u16> = re_all_e_numbers()
                .captures_iter(whole)
                .filter_map(|c| c[1].parse::<u16>().ok())
                .collect();
            if !episodes.is_empty() {
                return Some(MarkerMatch {
                    start: m.start(),
                    end: m.end(),
                    season: Some(season),
                    episodes: Some(episodes),
                    ambiguous: false,
                    special_phrase: None,
                });
            }
        }
    }

    // 3. Dash-chained multi-episode: `S01E01-E02`, `S01E01-02`, chained
    //    (`S01E01-E02-E03`).
    if let Some(m) = re_dash_chain().find(text) {
        let whole = m.as_str();
        if let Some(caps) = re_leading_season_episode().captures(whole) {
            let season: u16 = caps[1].parse().ok()?;
            let first_ep: u16 = caps[2].parse().ok()?;
            let mut episodes = vec![first_ep];
            episodes.extend(
                re_dash_numbers()
                    .captures_iter(whole)
                    .filter_map(|c| c[1].parse::<u16>().ok()),
            );
            return Some(MarkerMatch {
                start: m.start(),
                end: m.end(),
                season: Some(season),
                episodes: Some(episodes),
                ambiguous: false,
                special_phrase: None,
            });
        }
    }

    // 4. Ampersand-chained multi-episode: `S01E01 & S01E02`.
    if let Some(m) = re_amp_chain().find(text) {
        let whole = m.as_str();
        let pairs: Vec<(u16, u16)> = re_all_se_pairs()
            .captures_iter(whole)
            .filter_map(|c| Some((c[1].parse::<u16>().ok()?, c[2].parse::<u16>().ok()?)))
            .collect();
        if let Some(&(base_season, _)) = pairs.first() {
            if pairs.iter().all(|(s, _)| *s == base_season) {
                let episodes: Vec<u16> = pairs.iter().map(|(_, e)| *e).collect();
                return Some(MarkerMatch {
                    start: m.start(),
                    end: m.end(),
                    season: Some(base_season),
                    episodes: Some(episodes),
                    ambiguous: false,
                    special_phrase: None,
                });
            } else {
                return Some(MarkerMatch {
                    start: m.start(),
                    end: m.end(),
                    season: None,
                    episodes: None,
                    ambiguous: true,
                    special_phrase: None,
                });
            }
        }
    }

    // 5. Plain single episode: `S01E01` (also covers specials `S00E01`).
    if let Some(caps) = re_single_sxxexx().captures(text) {
        let m = caps.get(0).unwrap();
        let season: u16 = caps[1].parse().ok()?;
        let ep: u16 = caps[2].parse().ok()?;
        return Some(MarkerMatch {
            start: m.start(),
            end: m.end(),
            season: Some(season),
            episodes: Some(vec![ep]),
            ambiguous: false,
            special_phrase: None,
        });
    }

    // 6. Concatenated `NxNN` multi-episode: `1x01x02`.
    if let Some(m) = re_nxnn_multi().find(text) {
        let whole = m.as_str();
        let parts: Vec<&str> = whole.split(['x', 'X']).collect();
        if parts.len() >= 2 {
            if let Ok(season) = parts[0].parse::<u16>() {
                let episodes: Vec<u16> = parts[1..].iter().filter_map(|p| p.parse().ok()).collect();
                if !episodes.is_empty() {
                    return Some(MarkerMatch {
                        start: m.start(),
                        end: m.end(),
                        season: Some(season),
                        episodes: Some(episodes),
                        ambiguous: false,
                        special_phrase: None,
                    });
                }
            }
        }
    }

    // 7. Plain `NxNN`: `1x05`.
    if let Some(caps) = re_nxnn_single().captures(text) {
        let m = caps.get(0).unwrap();
        let season: u16 = caps[1].parse().ok()?;
        let ep: u16 = caps[2].parse().ok()?;
        return Some(MarkerMatch {
            start: m.start(),
            end: m.end(),
            season: Some(season),
            episodes: Some(vec![ep]),
            ambiguous: false,
            special_phrase: None,
        });
    }

    // 8. Bare episode range with no season prefix: `E01-E03`.
    if let Some(caps) = re_bare_e_range().captures(text) {
        let m = caps.get(0).unwrap();
        let e1: u16 = caps[1].parse().ok()?;
        let e2: u16 = caps[2].parse().ok()?;
        return Some(MarkerMatch {
            start: m.start(),
            end: m.end(),
            season: None,
            episodes: Some(expand_range(e1, e2)),
            ambiguous: false,
            special_phrase: None,
        });
    }

    // 9. Numbered special: `Special 05`, `SP05`, `OVA 05`.
    if let Some(caps) = re_special_numeric().captures(text) {
        let m = caps.get(0).unwrap();
        let ep: u16 = caps[2].parse().ok()?;
        return Some(MarkerMatch {
            start: m.start(),
            end: m.end(),
            season: Some(0),
            episodes: Some(vec![ep]),
            ambiguous: false,
            special_phrase: None,
        });
    }

    // 10. Title-only special: `Christmas Special`, bare `OVA`. Season 0,
    //     episode deliberately left `Unknown` — never invented (§3.2).
    if let Some(m) = re_special_title_only().find(text) {
        return Some(MarkerMatch {
            start: m.start(),
            end: m.end(),
            season: Some(0),
            episodes: None,
            ambiguous: false,
            special_phrase: Some(tidy_phrase(m.as_str())),
        });
    }

    // 11. Anime absolute numbering: `Show - 137`. Recognised but ambiguous by
    //     design (§3.2) — never treated as season+episode. Guarded against a
    //     bare trailing year (which is not absolute numbering at all).
    let stem = strip_ext(text);
    if let Some(caps) = re_absolute().captures(stem) {
        let m = caps.get(0).unwrap();
        if let Ok(n) = caps[1].parse::<u16>() {
            if !(min_year..=max_year).contains(&n) {
                return Some(MarkerMatch {
                    start: m.start(),
                    end: m.end(),
                    season: None,
                    episodes: Some(vec![n]),
                    ambiguous: true,
                    special_phrase: None,
                });
            }
        }
    }

    None
}

fn expand_range(a: u16, b: u16) -> Vec<u16> {
    if b >= a {
        (a..=b).collect()
    } else {
        vec![a, b]
    }
}

fn leading_season(text: &str) -> Option<u16> {
    re_leading_season().captures(text)?[1].parse().ok()
}

fn strip_ext(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i > 0 => &name[..i],
        _ => name,
    }
}

fn tidy_phrase(s: &str) -> String {
    s.split_whitespace()
        .map(|w| {
            let lower = w.to_lowercase();
            match lower.as_str() {
                "ova" => "OVA".to_string(),
                "sp" => "SP".to_string(),
                _ => {
                    let mut chars = lower.chars();
                    match chars.next() {
                        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
                        None => String::new(),
                    }
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

macro_rules! static_re {
    ($fn_name:ident, $pat:expr) => {
        fn $fn_name() -> &'static Regex {
            static RE: OnceLock<Regex> = OnceLock::new();
            RE.get_or_init(|| Regex::new($pat).expect("static regex must compile"))
        }
    };
}

static_re!(
    re_cross_season,
    r"(?i)\bS(\d{1,2})E(\d{1,3})\s*-\s*S(\d{1,2})E(\d{1,3})\b"
);
static_re!(re_concat_e, r"(?i)\bS\d{1,2}(?:E\d{1,3}){2,}\b");
static_re!(re_dash_chain, r"(?i)\bS\d{1,2}E\d{1,3}(?:-E?\d{1,3})+\b");
static_re!(
    re_amp_chain,
    r"(?i)\bS\d{1,2}E\d{1,3}(?:[\s.]*&[\s.]*S\d{1,2}E\d{1,3})+\b"
);
static_re!(re_single_sxxexx, r"(?i)\bS(\d{1,2})E(\d{1,3})\b");
static_re!(re_nxnn_multi, r"(?i)\b\d{1,2}x\d{1,3}(?:x\d{1,3})+\b");
static_re!(re_nxnn_single, r"(?i)\b(\d{1,2})x(\d{1,3})\b");
static_re!(re_bare_e_range, r"(?i)\bE(\d{1,3})\s*-\s*E(\d{1,3})\b");
static_re!(
    re_special_numeric,
    r"(?i)\b(Special|SP|OVA)[.\-_ ]*(\d{1,4})\b"
);
// NOTE: the `regex` crate (unlike `fancy-regex`) has no lookaround support at
// all, by design (it guarantees linear-time matching) — an earlier draft of
// this pattern used a trailing `(?!...)` negative lookahead to rule out the
// numbered form, which would have failed to compile. Safety against matching
// a numbered special (`Special 05`) here instead comes from ordering:
// `re_special_numeric` above is always tried first and wins whenever a
// number is adjacent, so this pattern only fires once that has already
// failed. The one gap that leaves is a 5+ digit "special number", which is
// vanishingly unlikely in real filenames.
static_re!(
    re_special_title_only,
    r"(?i)\b(?:(?:Christmas|Holiday|Halloween|Anniversary|New Year'?s?)[.\-_ ]+)?(?:Special|OVA)\b"
);
static_re!(re_absolute, r"(?i)-\s*(\d{1,3})\s*$");
static_re!(re_leading_season, r"(?i)^S(\d{1,2})");
static_re!(re_leading_season_episode, r"(?i)^S(\d{1,2})E(\d{1,3})");
static_re!(re_all_e_numbers, r"(?i)E(\d{1,3})");
static_re!(re_dash_numbers, r"(?i)-E?(\d{1,3})");
static_re!(re_all_se_pairs, r"(?i)S(\d{1,2})E(\d{1,3})");

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(name: &str) -> ParsedEpisode {
        parse_episode_filename(name, &ParseOptions::default())
    }

    #[test]
    fn parses_plain_single_episode() {
        let p = ep("Show.Name.S01E02.1080p.WEB.x264-GROUP.mkv");
        assert_eq!(p.title.as_value().unwrap(), "Show Name");
        assert_eq!(p.season.as_value().copied().unwrap(), 1);
        assert_eq!(p.episodes.as_value().unwrap(), &vec![2]);
        assert_eq!(p.resolution.as_value().unwrap(), "1080p");
        assert_eq!(p.release_group.as_value().unwrap(), "GROUP");
    }

    #[test]
    fn parses_episode_title_between_marker_and_resolution() {
        let p = ep("Show (2011) - S01E01 - Winter Is Coming - 1080p.mkv");
        assert_eq!(p.title.as_value().unwrap(), "Show");
        assert_eq!(p.year.as_value().copied().unwrap(), 2011);
        assert_eq!(p.season.as_value().copied().unwrap(), 1);
        assert_eq!(p.episodes.as_value().unwrap(), &vec![1]);
        assert_eq!(p.episode_title.as_value().unwrap(), "Winter Is Coming");
        assert_eq!(p.resolution.as_value().unwrap(), "1080p");
    }

    #[test]
    fn concatenated_multi_episode() {
        let p = ep("Show.Name.S01E01E02.720p.mkv");
        assert_eq!(p.season.as_value().copied().unwrap(), 1);
        assert_eq!(p.episodes.as_value().unwrap(), &vec![1, 2]);
    }

    #[test]
    fn dash_e_multi_episode() {
        let p = ep("Show.Name.S01E01-E02.720p.mkv");
        assert_eq!(p.season.as_value().copied().unwrap(), 1);
        assert_eq!(p.episodes.as_value().unwrap(), &vec![1, 2]);
    }

    #[test]
    fn dash_bare_multi_episode() {
        let p = ep("Show.Name.S01E01-02.720p.mkv");
        assert_eq!(p.season.as_value().copied().unwrap(), 1);
        assert_eq!(p.episodes.as_value().unwrap(), &vec![1, 2]);
    }

    #[test]
    fn cross_season_dash_same_season_expands() {
        let p = ep("Show.Name.S01E01-S01E02.mkv");
        assert_eq!(p.season.as_value().copied().unwrap(), 1);
        assert_eq!(p.episodes.as_value().unwrap(), &vec![1, 2]);
    }

    #[test]
    fn cross_season_dash_different_season_is_ambiguous() {
        let p = ep("Show.Name.S01E10-S02E01.mkv");
        assert!(p.ambiguous);
        assert!(p.season.as_value().is_none());
        assert!(p.episodes.as_value().is_none());
    }

    #[test]
    fn ampersand_multi_episode() {
        let p = ep("Show.Name.S01E01.&.S01E02.mkv");
        assert_eq!(p.season.as_value().copied().unwrap(), 1);
        assert_eq!(p.episodes.as_value().unwrap(), &vec![1, 2]);
    }

    #[test]
    fn absolute_style_nxnn() {
        let p = ep("Show.Name.1x05.720p.mkv");
        assert_eq!(p.season.as_value().copied().unwrap(), 1);
        assert_eq!(p.episodes.as_value().unwrap(), &vec![5]);
        assert!(!p.ambiguous);
    }

    #[test]
    fn nxnn_multi_episode() {
        let p = ep("Show.Name.1x01x02.mkv");
        assert_eq!(p.season.as_value().copied().unwrap(), 1);
        assert_eq!(p.episodes.as_value().unwrap(), &vec![1, 2]);
    }

    #[test]
    fn bare_e_range_no_season() {
        let p = ep("Show.Name.E01-E03.mkv");
        assert!(p.season.as_value().is_none());
        assert_eq!(p.episodes.as_value().unwrap(), &vec![1, 2, 3]);
    }

    #[test]
    fn special_numeric_is_season_zero() {
        let p = ep("Show.Name.S00E01.720p.mkv");
        assert_eq!(p.season.as_value().copied().unwrap(), 0);
        assert_eq!(p.episodes.as_value().unwrap(), &vec![1]);
    }

    #[test]
    fn special_word_numeric_is_season_zero() {
        let p = ep("Show.Name.Special.05.mkv");
        assert_eq!(p.season.as_value().copied().unwrap(), 0);
        assert_eq!(p.episodes.as_value().unwrap(), &vec![5]);
    }

    #[test]
    fn title_only_special_never_invents_episode_number() {
        let p = ep("Show Name - Christmas Special.mkv");
        assert_eq!(p.title.as_value().unwrap(), "Show Name");
        assert_eq!(p.season.as_value().copied().unwrap(), 0);
        assert!(p.episodes.as_value().is_none(), "must never invent a number");
        assert_eq!(p.episode_title.as_value().unwrap(), "Christmas Special");
    }

    #[test]
    fn absolute_numbering_is_ambiguous_and_not_a_year() {
        let p = ep("Show Name - 137.mkv");
        assert!(p.ambiguous);
        assert_eq!(p.episodes.as_value().unwrap(), &vec![137]);
        assert!(p.season.as_value().is_none());
    }

    #[test]
    fn trailing_bare_year_is_not_absolute_numbering() {
        // A plain trailing "- 2011" must not be misread as absolute episode
        // numbering just because there is no S/E marker — 2011 is in the
        // configured year range and must be left alone by the marker finder.
        let p = ep("Some Show - 2011.mkv");
        assert!(!p.ambiguous);
        assert!(p.episodes.as_value().is_none());
    }

    #[test]
    fn underscore_separated_marker_is_still_found() {
        let p = ep("Show_Name_S01E02_1080p.mkv");
        assert_eq!(p.season.as_value().copied().unwrap(), 1);
        assert_eq!(p.episodes.as_value().unwrap(), &vec![2]);
        assert_eq!(p.title.as_value().unwrap(), "Show Name");
    }

    #[test]
    fn year_after_marker_is_not_confused_with_marker() {
        let p = ep("Show.Name.S01E02.2019.1080p.mkv");
        assert_eq!(p.season.as_value().copied().unwrap(), 1);
        assert_eq!(p.episodes.as_value().unwrap(), &vec![2]);
        assert_eq!(p.year.as_value().copied().unwrap(), 2019);
    }

    #[test]
    fn multi_episode_never_split_to_first_only() {
        let p = ep("Show.Name.S02E05E06E07.mkv");
        assert_eq!(p.episodes.as_value().unwrap().len(), 3);
        assert_eq!(p.episodes.as_value().unwrap(), &vec![5, 6, 7]);
    }
}
