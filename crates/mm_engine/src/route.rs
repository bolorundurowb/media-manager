//! Route stage (§5.5).
//!
//! Compute destination paths from resolved fields, templates, and the path
//! layer (sanitise, length caps). Handles the §5.0 "root is itself a level"
//! prefix-stripping rule.

use std::path::{Path, PathBuf};

use mm_core::classify::FileClass;
use mm_core::config::Config;
use mm_core::identity::MovieId;
use mm_core::path::{SanitiseMap, sanitise_component};
use mm_core::template::ValueSource;
use mm_core::volume::VolumeSemantics;

use crate::resolve::ResolvedMovie;

/// Routing context for one movie group.
pub struct RouteContext<'a> {
    pub root: &'a Path,
    pub id: &'a MovieId,
    pub resolved: &'a ResolvedMovie,
    pub volume: &'a VolumeSemantics,
    pub cfg: &'a Config,
}

/// Route a single file to its destination paths (absolute and root-relative).
pub fn route(ctx: &RouteContext, class: FileClass, original: &Path) -> Option<(PathBuf, PathBuf)> {
    let ext = original.extension().and_then(|e| e.to_str()).unwrap_or("");
    let mut parts = match class {
        FileClass::Video => route_video(ctx),
        FileClass::Subtitle => route_subtitle(ctx),
        FileClass::Artwork => route_artwork(ctx),
        FileClass::Metadata => route_metadata(ctx),
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

fn route_video(ctx: &RouteContext) -> Option<Vec<String>> {
    let naming = &ctx.cfg.naming.movies;
    let map = SanitiseMap::default();
    let vs = ctx.volume;

    let dir_name = sanitise_component(
        &naming.dir.render(&ValueSourceForMovie::new(ctx, false)),
        &map,
        vs.max_component,
    )?;
    let file_name = sanitise_component(
        &naming.file.render(&ValueSourceForMovie::new(ctx, true)),
        &map,
        vs.max_component,
    )?;
    Some(vec![dir_name, file_name])
}

fn route_subtitle(ctx: &RouteContext) -> Option<Vec<String>> {
    let naming = &ctx.cfg.naming.movies;
    let map = SanitiseMap::default();
    let vs = ctx.volume;

    let dir_name = sanitise_component(
        &naming.dir.render(&ValueSourceForMovie::new(ctx, false)),
        &map,
        vs.max_component,
    )?;
    let sub_name = sanitise_component(
        &naming.sub_file.render(&SubtitleValueSource::new(ctx)),
        &map,
        vs.max_component,
    )?;
    if ctx.cfg.behaviour.create_subs_dir {
        Some(vec![dir_name, naming.subs_dir.clone(), sub_name])
    } else {
        Some(vec![dir_name, sub_name])
    }
}

fn route_artwork(ctx: &RouteContext) -> Option<Vec<String>> {
    let naming = &ctx.cfg.naming.movies;
    let map = SanitiseMap::default();
    let vs = ctx.volume;

    let dir_name = sanitise_component(
        &naming.dir.render(&ValueSourceForMovie::new(ctx, false)),
        &map,
        vs.max_component,
    )?;
    let art_name = sanitise_component(&naming.artwork, &map, vs.max_component)?;
    Some(vec![dir_name, art_name])
}

fn route_metadata(ctx: &RouteContext) -> Option<Vec<String>> {
    let naming = &ctx.cfg.naming.movies;
    let map = SanitiseMap::default();
    let vs = ctx.volume;

    let dir_name = sanitise_component(
        &naming.dir.render(&ValueSourceForMovie::new(ctx, false)),
        &map,
        vs.max_component,
    )?;
    let nfo_name = sanitise_component(
        &naming.nfo.render(&ValueSourceForMovie::new(ctx, false)),
        &map,
        vs.max_component,
    )?;
    Some(vec![dir_name, nfo_name])
}

/// Build the escalating discriminator chain (§5.3).
pub fn build_discriminators(r: &ResolvedMovie) -> String {
    let mut out = String::new();
    push_opt(&mut out, " - ", &r.edition);
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

struct ValueSourceForMovie<'a> {
    ctx: &'a RouteContext<'a>,
    include_discriminators: bool,
}

impl<'a> ValueSourceForMovie<'a> {
    fn new(ctx: &'a RouteContext<'a>, include_discriminators: bool) -> Self {
        Self {
            ctx,
            include_discriminators,
        }
    }
}

impl<'a> ValueSource for ValueSourceForMovie<'a> {
    fn get(&self, name: &str) -> Option<String> {
        let r = self.ctx.resolved;
        match name {
            "title" => r.title.as_value().cloned(),
            "year" => r.year.as_value().map(|y| y.to_string()),
            "edition" => r.edition.as_value().cloned(),
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

struct SubtitleValueSource<'a> {
    inner: ValueSourceForMovie<'a>,
}

impl<'a> SubtitleValueSource<'a> {
    fn new(ctx: &'a RouteContext<'a>) -> Self {
        Self {
            inner: ValueSourceForMovie::new(ctx, true),
        }
    }
}

impl<'a> ValueSource for SubtitleValueSource<'a> {
    fn get(&self, name: &str) -> Option<String> {
        match name {
            "language" => Some("und".to_string()),
            "flags" => None,
            _ => self.inner.get(name),
        }
    }
}
