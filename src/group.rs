//! Group parsed media folders by movie / show identity.

use crate::parse::{
    bare_season_folder, identity_key, parse_media_name, LibraryKind, ParseError, ParsedName,
};
use crate::scan::MediaFolder;
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct ParsedFolder {
    pub folder: MediaFolder,
    pub parsed: ParsedName,
}

#[derive(Debug, Clone)]
pub struct MovieGroup {
    pub title: String,
    pub year: Option<u16>,
    pub folders: Vec<ParsedFolder>,
}

#[derive(Debug, Clone)]
pub struct TvShowGroup {
    pub title: String,
    pub year: Option<u16>,
    pub seasons: BTreeMap<u8, Vec<ParsedFolder>>,
}

pub enum GroupOutcome {
    Movies(Vec<MovieGroup>),
    Tv(Vec<TvShowGroup>),
}

#[derive(Debug, Clone)]
pub struct SkippedFolder {
    pub path: std::path::PathBuf,
    pub reason: String,
}

pub fn group_folders(
    kind: LibraryKind,
    folders: Vec<MediaFolder>,
) -> (GroupOutcome, Vec<SkippedFolder>) {
    let (parsed, mut skipped) = parse_folders(kind, folders);
    let outcome = group_parsed(kind, parsed, &mut skipped);
    (outcome, skipped)
}

/// Parse a batch independently. Phase 6 runs this per root child in the
/// bounded worker pool, then joins all parsed folders before global grouping.
pub(crate) fn parse_folders(
    kind: LibraryKind,
    folders: Vec<MediaFolder>,
) -> (Vec<ParsedFolder>, Vec<SkippedFolder>) {
    let mut skipped = Vec::new();
    let mut parsed = Vec::new();

    for folder in folders {
        let name = folder
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| folder.path.to_string_lossy().into_owned());

        match parse_media_name(&name, kind) {
            Ok(p) => parsed.push(ParsedFolder { folder, parsed: p }),
            Err(ParseError::EmptyTitle) if kind == LibraryKind::Tv => {
                match parse_bare_season_subfolder(&folder, &name) {
                    Some(p) => parsed.push(ParsedFolder { folder, parsed: p }),
                    None => {
                        tracing::warn!(path = %folder.path.display(), "no parseable title; skipping");
                        skipped.push(SkippedFolder {
                            path: folder.path,
                            reason: ParseError::EmptyTitle.to_string(),
                        });
                    }
                }
            }
            Err(ParseError::MissingSeason) if kind == LibraryKind::Tv => {
                tracing::warn!(path = %folder.path.display(), "no season number; skipping");
                skipped.push(SkippedFolder {
                    path: folder.path,
                    reason: "no parseable season number".into(),
                });
            }
            Err(err) => {
                tracing::warn!(path = %folder.path.display(), error = %err, "parse failed; skipping");
                skipped.push(SkippedFolder {
                    path: folder.path,
                    reason: err.to_string(),
                });
            }
        }
    }

    (parsed, skipped)
}

/// A subfolder whose own name is nothing but a bare season indicator
/// ("Season 1", "S01", "Specials") has no title of its own to parse.
/// Release packs commonly nest these directly under a container that *does*
/// carry the show's title and year (and often a season range, e.g. "Show
/// Name (2005) Season 1-4 S01-S04 (1080p ...)"), one level up:
///
/// ```text
/// Show Name (2005) Season 1-4 S01-S04 (1080p WEB-DL ...) [UTR]/
///   Season 1/
///   Season 2/
///   Extras/
/// ```
///
/// Borrow the title/year from the parent directory and combine it with the
/// season this subfolder names. The parent is parsed in `Movies` mode
/// because it carries no reliable *single* season of its own (it's often a
/// range) — only a title and year are taken from it. Returns `None` when
/// the leaf isn't a bare season indicator, the parent path/name is
/// unavailable, or the parent's own name doesn't parse into a title.
fn parse_bare_season_subfolder(folder: &MediaFolder, leaf_name: &str) -> Option<ParsedName> {
    let season = bare_season_folder(leaf_name)?;
    let parent_name = folder.path.parent()?.file_name()?.to_str()?;
    let mut parsed = parse_media_name(parent_name, LibraryKind::Movies).ok()?;
    parsed.season = Some(season);
    Some(parsed)
}

/// Group already-parsed folders globally so versions/seasons discovered by
/// different workers still merge and validate as one destination identity.
pub(crate) fn group_parsed(
    kind: LibraryKind,
    parsed: Vec<ParsedFolder>,
    skipped: &mut Vec<SkippedFolder>,
) -> GroupOutcome {
    match kind {
        LibraryKind::Movies => {
            let groups = group_movies(parsed);
            GroupOutcome::Movies(groups)
        }
        LibraryKind::Tv => {
            let groups = group_tv(parsed, skipped);
            GroupOutcome::Tv(groups)
        }
    }
}

fn group_movies(parsed: Vec<ParsedFolder>) -> Vec<MovieGroup> {
    let mut buckets: BTreeMap<(String, Option<u16>), Vec<ParsedFolder>> = BTreeMap::new();
    for item in parsed {
        let id = (identity_key(&item.parsed.title), item.parsed.year);
        buckets.entry(id).or_default().push(item);
    }

    let mut groups = Vec::new();
    for ((_key, _), folders) in buckets {
        let (title, year) = pick_display(&folders);
        if folders.len() > 1 {
            tracing::info!(
                title = %title,
                year = ?year,
                count = folders.len(),
                "merging movie versions"
            );
        }
        groups.push(MovieGroup {
            title,
            year,
            folders,
        });
    }
    groups
}

fn group_tv(parsed: Vec<ParsedFolder>, skipped: &mut Vec<SkippedFolder>) -> Vec<TvShowGroup> {
    let mut by_show: BTreeMap<(String, Option<u16>), Vec<ParsedFolder>> = BTreeMap::new();
    for item in parsed {
        let id = (identity_key(&item.parsed.title), item.parsed.year);
        by_show.entry(id).or_default().push(item);
    }

    let mut groups = Vec::new();
    for ((_key, _), folders) in by_show {
        let (title, year) = pick_display(&folders);
        let mut seasons: BTreeMap<u8, Vec<ParsedFolder>> = BTreeMap::new();
        for folder in folders {
            match folder.parsed.season {
                Some(s) => seasons.entry(s).or_default().push(folder),
                None => skipped.push(SkippedFolder {
                    path: folder.folder.path.clone(),
                    reason: "no parseable season number".into(),
                }),
            }
        }
        if seasons.is_empty() {
            continue;
        }
        tracing::info!(
            title = %title,
            year = ?year,
            seasons = seasons.len(),
            "grouping TV seasons"
        );
        groups.push(TvShowGroup {
            title,
            year,
            seasons,
        });
    }
    groups
}

fn pick_display(folders: &[ParsedFolder]) -> (String, Option<u16>) {
    let best = folders
        .iter()
        .max_by_key(|f| (f.parsed.year.is_some(), f.parsed.title.len()))
        .expect("non-empty group");
    (best.parsed.title.clone(), best.parsed.year)
}
