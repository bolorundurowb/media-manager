//! Music route stage (Phase 6, §5.5, §5.0).
//!
//! Destination shape: `artist_dir/album_dir/[disc_dir/]file`. Mirrors
//! `crate::route`'s structure (same §5.0 root-prefix-stripping rule, same
//! sanitise-then-cap pipeline) but keyed on `AlbumId`/`ResolvedTrack` instead
//! of `MovieId`/`ResolvedMovie`, and with its own small `strip_root_prefix`
//! copy: `crate::route`'s version is a private fn, not reusable across
//! modules, and duplicating ~15 lines here is cheaper and less surprising
//! than making it `pub(crate)` in a file this phase does not own.

use std::path::{Path, PathBuf};

use mm_core::classify::FileClass;
use mm_core::config::Config;
use mm_core::identity::AlbumId;
use mm_core::path::{SanitiseMap, sanitise_component};
use mm_core::template::ValueSource;
use mm_core::volume::VolumeSemantics;

use crate::music_resolve::ResolvedTrack;

/// Routing context for one track, within its album group.
pub struct MusicRouteContext<'a> {
    pub root: &'a Path,
    pub id: &'a AlbumId,
    pub resolved: &'a ResolvedTrack,
    /// Whether this track's album spans more than one disc (§5.5): only then
    /// does `disc_dir` apply.
    pub multi_disc: bool,
    pub volume: &'a VolumeSemantics,
    pub cfg: &'a Config,
}

/// Route a single music file to its destination paths (absolute and
/// root-relative). Returns `None` for classes this stage doesn't route
/// (video/subtitle never occur in a music-classified library).
pub fn route(
    ctx: &MusicRouteContext,
    class: FileClass,
    original: &Path,
) -> Option<(PathBuf, PathBuf)> {
    let ext = original.extension().and_then(|e| e.to_str()).unwrap_or("");
    let mut parts = match class {
        FileClass::Audio => route_audio(ctx),
        FileClass::Artwork => route_artwork(ctx),
        FileClass::Metadata => route_metadata(ctx, original),
        _ => return None,
    }?;

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

/// Same rule as `crate::route::strip_root_prefix` (§5.0): find the longest
/// prefix of the destination that root already satisfies (case/normalisation
/// aware) and strip it, so pointing the tool at an artist or album folder
/// does not nest `Artist/Artist/Album/...`.
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

fn base_dirs(ctx: &MusicRouteContext, map: &SanitiseMap) -> Option<Vec<String>> {
    let naming = &ctx.cfg.naming.music;
    let vs = ctx.volume;

    let artist_dir = sanitise_component(
        &naming.artist_dir.render(&ValueSourceForTrack::new(ctx)),
        map,
        vs.max_component,
    )?;
    let album_dir = sanitise_component(
        &naming.album_dir.render(&ValueSourceForTrack::new(ctx)),
        map,
        vs.max_component,
    )?;
    let mut parts = vec![artist_dir, album_dir];

    if ctx.multi_disc {
        if let Some(_disc) = ctx.resolved.disc.as_value() {
            let disc_dir = sanitise_component(
                &naming.disc_dir.render(&ValueSourceForTrack::new(ctx)),
                map,
                vs.max_component,
            )?;
            parts.push(disc_dir);
        }
    }
    Some(parts)
}

fn route_audio(ctx: &MusicRouteContext) -> Option<Vec<String>> {
    let naming = &ctx.cfg.naming.music;
    let map = SanitiseMap::default();
    let vs = ctx.volume;

    let mut parts = base_dirs(ctx, &map)?;

    let rendered_file = naming.file.render(&ValueSourceForTrack::new(ctx));
    let with_prefix = if naming.compilation_prefix {
        // §5.5 config comment: `compilation_prefix = true` "adds
        // `[{track_artist} - ]`" — implemented here as a literal prefix on
        // the rendered file name rather than requiring users to hand-edit
        // `file` to reference `{track_artist}` themselves, since
        // `MusicNaming` has no separate compilation-file-template field to
        // hold that. Only applied when a track artist is actually known;
        // an absent one renders no prefix, same as the bracketed-segment
        // convention used everywhere else in §5.5. This is a judgment call
        // — the plan states the intended rendering but not the exact
        // mechanism by which a single boolean flag reaches into the
        // template, since the default `file` template does not mention
        // `{track_artist}` at all.
        match ctx.resolved.track_artist.as_value() {
            Some(artist) => format!("{artist} - {rendered_file}"),
            None => rendered_file,
        }
    } else {
        rendered_file
    };

    let file_name = sanitise_component(&with_prefix, &map, vs.max_component)?;
    parts.push(file_name);
    Some(parts)
}

fn route_artwork(ctx: &MusicRouteContext) -> Option<Vec<String>> {
    let naming = &ctx.cfg.naming.music;
    let map = SanitiseMap::default();
    let vs = ctx.volume;

    let mut parts = base_dirs(ctx, &map)?;
    let art_name = sanitise_component(&naming.artwork, &map, vs.max_component)?;
    parts.push(art_name);
    Some(parts)
}

/// Metadata sidecars (`.cue`, `.log`, …) have no dedicated naming template in
/// `MusicNaming` (unlike movies' `nfo`), so they travel with the album
/// keeping their own sanitised original stem — a documented scope choice for
/// Phase 6, since inventing a new config surface for a handful of rarely
/// re-derived sidecar formats is not asked for anywhere in §5.5.
fn route_metadata(ctx: &MusicRouteContext, original: &Path) -> Option<Vec<String>> {
    let map = SanitiseMap::default();
    let vs = ctx.volume;

    let mut parts = base_dirs(ctx, &map)?;
    let stem = original.file_stem().and_then(|s| s.to_str()).unwrap_or("metadata");
    let name = sanitise_component(stem, &map, vs.max_component)?;
    parts.push(name);
    Some(parts)
}

struct ValueSourceForTrack<'a> {
    ctx: &'a MusicRouteContext<'a>,
}

impl<'a> ValueSourceForTrack<'a> {
    fn new(ctx: &'a MusicRouteContext<'a>) -> Self {
        Self { ctx }
    }
}

impl<'a> ValueSource for ValueSourceForTrack<'a> {
    fn get(&self, name: &str) -> Option<String> {
        let r = self.ctx.resolved;
        match name {
            "album_artist" => r.album_artist.as_value().cloned(),
            "album" => r.album.as_value().cloned(),
            "year" => r.year.as_value().map(|y| y.to_string()),
            // Bare `{disc}` in the default `disc_dir` template ("CD {disc}")
            // — no zero-padding requested there, unlike `{track:02}` below.
            "disc" => r.disc.as_value().map(|d| d.to_string()),
            // `{track:02}` in the default `file` template. The template
            // engine discards format specs at parse time (`Template::parse`
            // keeps only the placeholder name), so the zero-padding has to
            // be baked into this `ValueSource` rather than driven by the
            // template string — same convention `mm_core::template`'s own
            // doctest (`disc_separator_only_when_present`) relies on.
            "track" => r.track.as_value().map(|t| format!("{t:02}")),
            "title" => r.title.as_value().cloned(),
            "track_artist" => r.track_artist.as_value().cloned(),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mm_core::identity::Norm;
    use mm_core::provenance::{Confidence, Field, Source};

    fn ctx<'a>(
        root: &'a Path,
        id: &'a AlbumId,
        resolved: &'a ResolvedTrack,
        multi_disc: bool,
        volume: &'a VolumeSemantics,
        cfg: &'a Config,
    ) -> MusicRouteContext<'a> {
        MusicRouteContext {
            root,
            id,
            resolved,
            multi_disc,
            volume,
            cfg,
        }
    }

    fn resolved_track() -> ResolvedTrack {
        let mut r = ResolvedTrack::default();
        r.album_artist = Field::known("Nina Simone".to_string(), Source::EmbeddedTag, Confidence::High);
        r.album = Field::known("Wild Is the Wind".to_string(), Source::EmbeddedTag, Confidence::High);
        r.year = Field::known(1966u16, Source::EmbeddedTag, Confidence::High);
        r.track = Field::known(3u16, Source::EmbeddedTag, Confidence::High);
        r.title = Field::known("Four Women".to_string(), Source::EmbeddedTag, Confidence::High);
        r
    }

    #[test]
    fn routes_basic_track() {
        let root = Path::new("/music");
        let resolved = resolved_track();
        let id = AlbumId::new(
            Norm::from_display("Nina Simone"),
            Norm::from_display("Wild Is the Wind"),
        );
        let volume = VolumeSemantics::conservative();
        let cfg = Config::default();
        let c = ctx(root, &id, &resolved, false, &volume, &cfg);

        let (_abs, rel) = route(&c, FileClass::Audio, Path::new("03 Four Women.flac")).unwrap();
        assert_eq!(
            rel,
            PathBuf::from("Nina Simone/Wild Is the Wind (1966)/03 - Four Women.flac")
        );
    }

    #[test]
    fn disc_dir_only_when_multi_disc_and_known() {
        let root = Path::new("/music");
        let mut resolved = resolved_track();
        resolved.disc = Field::known(2u16, Source::EmbeddedTag, Confidence::High);
        let id = AlbumId::new(
            Norm::from_display("Nina Simone"),
            Norm::from_display("Wild Is the Wind"),
        );
        let volume = VolumeSemantics::conservative();
        let cfg = Config::default();
        let c = ctx(root, &id, &resolved, true, &volume, &cfg);

        let (_abs, rel) = route(&c, FileClass::Audio, Path::new("2-03 Four Women.flac")).unwrap();
        assert_eq!(
            rel,
            PathBuf::from("Nina Simone/Wild Is the Wind (1966)/CD 2/03 - Four Women.flac")
        );
    }

    #[test]
    fn root_prefix_stripped_for_artist_folder() {
        let root = Path::new("/music/Nina Simone");
        let resolved = resolved_track();
        let id = AlbumId::new(
            Norm::from_display("Nina Simone"),
            Norm::from_display("Wild Is the Wind"),
        );
        let volume = VolumeSemantics::conservative();
        let cfg = Config::default();
        let c = ctx(root, &id, &resolved, false, &volume, &cfg);

        let (_abs, rel) = route(&c, FileClass::Audio, Path::new("03 Four Women.flac")).unwrap();
        assert_eq!(
            rel,
            PathBuf::from("Wild Is the Wind (1966)/03 - Four Women.flac")
        );
    }
}
