//! TV route stage (§5.5, Phase 5).
//!
//! Mirrors `crate::route` (movies) in shape: templates → destination path
//! components → sanitise → §5.0 root-prefix stripping. Keyed on
//! `mm_core::identity::EpisodeId` instead of `MovieId`, and season 0 routes
//! under `specials_dir` instead of a numbered season directory (§6.5).
//!
//! ## Known simplification: artwork/metadata have no dedicated TV template
//!
//! `mm_core::config::TvNaming` (already scaffolded — see `crates/mm_core/src/config/default.rs`)
//! has `show_dir`, `season_dir`, `specials_dir`, `file`, `sub_file` — no
//! `artwork`/`nfo` fields the way `MovieNaming` has. Rather than invent config
//! fields outside this phase's scope, artwork/metadata sidecars are routed
//! next to the episode they're associated with (same `show_dir`/season-or-
//! specials directory as the video), keeping their original sanitised
//! filename rather than a templated one. This is a deliberately narrower
//! answer than §5.4's "artwork and metadata need their own templates" and is
//! flagged in the Phase 5 report as a follow-up rather than silently assumed
//! complete.

use std::path::{Path, PathBuf};

use mm_core::classify::FileClass;
use mm_core::config::Config;
use mm_core::identity::EpisodeId;
use mm_core::path::{SanitiseMap, sanitise_component};
use mm_core::template::ValueSource;
use mm_core::volume::VolumeSemantics;

use crate::tv_resolve::ResolvedEpisode;

/// Routing context for one episode.
pub struct RouteContext<'a> {
    pub root: &'a Path,
    pub id: &'a EpisodeId,
    pub resolved: &'a ResolvedEpisode,
    pub volume: &'a VolumeSemantics,
    pub cfg: &'a Config,
    /// Only meaningful for subtitle routing; ISO 639-1 code, `"und"` when
    /// undetected (§5.4 — language always renders).
    pub language: String,
    /// Only meaningful for subtitle routing; `forced`/`sdh` flags, dot-joined,
    /// `None` when there are none (§5.4).
    pub flags: Option<String>,
}

/// Route a single file to its destination paths (absolute and root-relative).
pub fn route(ctx: &RouteContext, class: FileClass, original: &Path) -> Option<(PathBuf, PathBuf)> {
    let ext = original.extension().and_then(|e| e.to_str()).unwrap_or("");
    let mut parts = match class {
        FileClass::Video => route_video(ctx),
        FileClass::Subtitle => route_subtitle(ctx),
        FileClass::Artwork | FileClass::Metadata => route_sidecar(ctx, original),
        _ => return None,
    }?;

    // §5.0: if root already satisfies a prefix of the destination, strip it.
    strip_root_prefix(&mut parts, ctx.root, ctx.volume);

    let mut relative = PathBuf::new();
    for p in &parts {
        relative.push(p);
    }
    let mut absolute = ctx.root.join(&relative);
    if !ext.is_empty() {
        relative.set_extension(ext);
        absolute.set_extension(ext);
    }
    Some((absolute, relative))
}

/// Strip the longest prefix of `parts` that root already satisfies (§5.0).
/// Identical in shape to `crate::route`'s private helper of the same name —
/// duplicated here rather than shared because that module is movie-owned.
fn strip_root_prefix(parts: &mut Vec<String>, root: &Path, volume: &VolumeSemantics) {
    let root_comps: Vec<String> = root
        .iter()
        .filter_map(|c| c.to_str().map(|s| volume.collision_key(s)))
        .collect();
    let dest_comps: Vec<String> = parts.iter().map(|p| volume.collision_key(p)).collect();

    let max_k = root_comps.len().min(dest_comps.len());
    let mut best_k = 0usize;
    for k in 1..=max_k {
        let root_suffix = &root_comps[root_comps.len() - k..];
        let dest_prefix = &dest_comps[..k];
        if root_suffix == dest_prefix {
            best_k = k;
        }
    }
    if best_k > 0 {
        parts.drain(0..best_k);
    }
}

/// `show_dir` plus either `season_dir` (season > 0) or `specials_dir`
/// (season == 0, §6.5) — shared by video, subtitle, and sidecar routing so
/// every file belonging to one episode lands in the same directory.
fn show_and_season_parts(ctx: &RouteContext) -> Option<Vec<String>> {
    let naming = &ctx.cfg.naming.tv;
    let map = SanitiseMap::default();
    let vs = ctx.volume;

    let show_dir = sanitise_component(
        &naming.show_dir.render(&ValueSourceForEpisode::new(ctx, false)),
        &map,
        vs.max_component,
    )?;
    let season_part = if ctx.id.season == 0 {
        sanitise_component(&naming.specials_dir, &map, vs.max_component)?
    } else {
        sanitise_component(
            &naming.season_dir.render(&ValueSourceForEpisode::new(ctx, false)),
            &map,
            vs.max_component,
        )?
    };
    Some(vec![show_dir, season_part])
}

fn route_video(ctx: &RouteContext) -> Option<Vec<String>> {
    let naming = &ctx.cfg.naming.tv;
    let map = SanitiseMap::default();
    let vs = ctx.volume;

    let mut parts = show_and_season_parts(ctx)?;
    let mut file_name = sanitise_component(
        &naming.file.render(&ValueSourceForEpisode::new(ctx, true)),
        &map,
        vs.max_component,
    )?;
    if let Some(n) = ctx.resolved.copy.as_value() {
        file_name = format!("{file_name} ({n})");
    }
    parts.push(file_name);
    Some(parts)
}

fn route_subtitle(ctx: &RouteContext) -> Option<Vec<String>> {
    let naming = &ctx.cfg.naming.tv;
    let map = SanitiseMap::default();
    let vs = ctx.volume;

    let mut parts = show_and_season_parts(ctx)?;
    let mut sub_name = sanitise_component(
        &naming.sub_file.render(&SubtitleValueSourceForEpisode::new(ctx)),
        &map,
        vs.max_component,
    )?;
    if let Some(n) = ctx.resolved.copy.as_value() {
        sub_name = format!("{sub_name} ({n})");
    }
    parts.push(sub_name);
    Some(parts)
}

/// Artwork/metadata: routed next to the associated episode, original
/// filename preserved (see module docs — no dedicated TV template exists
/// for these yet).
fn route_sidecar(ctx: &RouteContext, original: &Path) -> Option<Vec<String>> {
    let mut parts = show_and_season_parts(ctx)?;
    let stem = original
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("file");
    let name = sanitise_component(stem, &SanitiseMap::default(), ctx.volume.max_component)?;
    parts.push(name);
    Some(parts)
}

/// Build the escalating discriminator chain (§5.3), minus `{edition}` —
/// `ParsedEpisode`/`ResolvedEpisode` deliberately has no edition field
/// (TV episodes don't have theatrical/director's-cut style editions the way
/// movies do), so the TV chain is resolution/hdr/source/video_codec/
/// audio_format only.
pub fn build_discriminators(r: &ResolvedEpisode) -> String {
    let mut out = String::new();
    push_opt(&mut out, " - ", &r.resolution);
    push_opt(&mut out, " - ", &r.hdr);
    push_opt(&mut out, " - ", &r.source);
    push_opt(&mut out, " - ", &r.video_codec);
    push_opt(&mut out, " - ", &r.audio_format);
    out
}

fn push_opt(out: &mut String, prefix: &str, f: &mm_core::Field<String>) {
    if let Some(v) = f.as_value() {
        out.push_str(prefix);
        out.push_str(v);
    }
}

struct ValueSourceForEpisode<'a> {
    ctx: &'a RouteContext<'a>,
    include_discriminators: bool,
}

impl<'a> ValueSourceForEpisode<'a> {
    fn new(ctx: &'a RouteContext<'a>, include_discriminators: bool) -> Self {
        Self {
            ctx,
            include_discriminators,
        }
    }
}

impl<'a> ValueSource for ValueSourceForEpisode<'a> {
    fn get(&self, name: &str) -> Option<String> {
        let r = self.ctx.resolved;
        match name {
            "title" => r.title.as_value().cloned(),
            "year" => r.year.as_value().map(|y| y.to_string()),
            // Zero-padded regardless of the `:02` in the template string —
            // `Template` drops everything after `:` when validating/looking
            // up the placeholder name and renders whatever `get` returns
            // verbatim (see `mm_core::template`'s `disc_separator_only_when_present`
            // test, which pre-formats "01" the same way).
            "season" => Some(format!("{:02}", self.ctx.id.season)),
            "episode_code" => Some(self.ctx.id.code()),
            "episode_title" => r.episode_title.as_value().cloned(),
            "resolution" => r.resolution.as_value().cloned(),
            "source" => r.source.as_value().cloned(),
            "video_codec" => r.video_codec.as_value().cloned(),
            "audio_format" => r.audio_format.as_value().cloned(),
            "hdr" => r.hdr.as_value().cloned(),
            "discriminators" => {
                if self.include_discriminators {
                    Some(build_discriminators(r))
                } else {
                    Some(String::new())
                }
            }
            _ => None,
        }
    }
}

struct SubtitleValueSourceForEpisode<'a> {
    inner: ValueSourceForEpisode<'a>,
    language: String,
    flags: Option<String>,
}

impl<'a> SubtitleValueSourceForEpisode<'a> {
    fn new(ctx: &'a RouteContext<'a>) -> Self {
        Self {
            inner: ValueSourceForEpisode::new(ctx, true),
            language: ctx.language.clone(),
            flags: ctx.flags.clone(),
        }
    }
}

impl<'a> ValueSource for SubtitleValueSourceForEpisode<'a> {
    fn get(&self, name: &str) -> Option<String> {
        match name {
            "language" => Some(self.language.clone()),
            "flags" => self.flags.clone(),
            _ => self.inner.get(name),
        }
    }
}

/// Detect a subtitle's language code and forced/SDH flags from its filename
/// stem (§5.4). Reuses `mm_parse::vocab::normalise_language` — the same
/// compiled-in ISO 639-1/639-2 table the spec requires — rather than
/// reinventing it. Tokenises on the same separator set the parser uses
/// (`.`, `_`, `+`, whitespace, and `-`, since subtitle stems commonly look
/// like `Show.S01E01.en.forced.srt`).
pub fn detect_language_and_flags(stem: &str) -> (String, Option<String>) {
    let mut language = "und".to_string();
    let mut found_language = false;
    let mut flags: Vec<String> = Vec::new();

    for tok in stem.split(['.', '_', '+', '-', ' ']) {
        if tok.is_empty() {
            continue;
        }
        let lower = tok.to_ascii_lowercase();
        match lower.as_str() {
            "forced" => {
                if !flags.contains(&"forced".to_string()) {
                    flags.push("forced".to_string());
                }
                continue;
            }
            // `hi`/`sdh`/`cc` are the common "hearing impaired" markers, but
            // `hi` is *also* the real ISO 639-1 code for Hindi. Only treat it
            // as the SDH flag once a language has already been found earlier
            // in the stem (`en.hi.srt`) — a standalone/first `hi` (`hi.srt`)
            // is far more likely to mean the subtitle *is* Hindi than that it
            // is an English SDH track with no language token at all.
            "hi" if found_language => {
                if !flags.contains(&"sdh".to_string()) {
                    flags.push("sdh".to_string());
                }
                continue;
            }
            "sdh" | "cc" => {
                if !flags.contains(&"sdh".to_string()) {
                    flags.push("sdh".to_string());
                }
                continue;
            }
            _ => {}
        }
        if !found_language {
            let code = mm_parse::vocab::normalise_language(tok);
            if code != "und" {
                language = code;
                found_language = true;
            }
        }
    }

    let flags = if flags.is_empty() {
        None
    } else {
        Some(flags.join("."))
    };
    (language, flags)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_plain_language() {
        let (lang, flags) = detect_language_and_flags("Show.S01E01.en");
        assert_eq!(lang, "en");
        assert!(flags.is_none());
    }

    #[test]
    fn detects_forced_flag() {
        let (lang, flags) = detect_language_and_flags("Show.S01E01.en.forced");
        assert_eq!(lang, "en");
        assert_eq!(flags.as_deref(), Some("forced"));
    }

    #[test]
    fn detects_sdh_flag_from_hi() {
        let (lang, flags) = detect_language_and_flags("Show.S01E01.en.hi");
        assert_eq!(lang, "en");
        assert_eq!(flags.as_deref(), Some("sdh"));
    }

    #[test]
    fn undetected_language_is_und() {
        let (lang, _) = detect_language_and_flags("Show.S01E01");
        assert_eq!(lang, "und");
    }
}
