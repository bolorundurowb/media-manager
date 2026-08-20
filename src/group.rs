//! Group parsed media folders by movie / show identity.

use crate::parse::{identity_key, parse_media_name, LibraryKind, ParseError, ParsedName};
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

    match kind {
        LibraryKind::Movies => {
            let groups = group_movies(parsed);
            (GroupOutcome::Movies(groups), skipped)
        }
        LibraryKind::Tv => {
            let groups = group_tv(parsed, &mut skipped);
            (GroupOutcome::Tv(groups), skipped)
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
