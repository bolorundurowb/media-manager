//! Grouping unit tests. No real filesystem: media folders are represented by
//! fake paths, and only the grouping logic is exercised.

use media_manager::group::{group_folders, GroupOutcome};
use media_manager::parse::LibraryKind;
use media_manager::scan::MediaFolder;
use std::path::PathBuf;

fn folder(name: &str) -> MediaFolder {
    MediaFolder {
        path: PathBuf::from(name),
        videos: Vec::new(),
        loose: false,
    }
}

/// A subfolder nested one level under `parent`, e.g. a `Season 1/` directory
/// inside a season-pack release container.
fn nested_folder(parent: &str, leaf: &str) -> MediaFolder {
    MediaFolder {
        path: PathBuf::from(parent).join(leaf),
        videos: Vec::new(),
        loose: false,
    }
}

#[test]
fn movies_merge_versions_and_separate_years() {
    let folders = vec![
        folder("300 (2006) [1080p]"),
        folder("300 (2006) [2160p]"),
        folder("300 (2014) [1080p]"),
        folder("Onward.2020.2160p.HDR.WEB-DL.DD5.1.HEVC-EVO[TGx]"),
    ];

    let (outcome, skipped) = group_folders(LibraryKind::Movies, folders);
    assert!(skipped.is_empty(), "unexpected skips: {skipped:?}");

    let GroupOutcome::Movies(groups) = outcome else {
        panic!("expected movie groups");
    };
    assert_eq!(groups.len(), 3);

    let g300_2006 = &groups[0];
    assert_eq!(g300_2006.title, "300");
    assert_eq!(g300_2006.year, Some(2006));
    assert_eq!(g300_2006.folders.len(), 2);

    let g300_2014 = &groups[1];
    assert_eq!(g300_2014.title, "300");
    assert_eq!(g300_2014.year, Some(2014));
    assert_eq!(g300_2014.folders.len(), 1);

    let onward = &groups[2];
    assert_eq!(onward.title, "Onward");
    assert_eq!(onward.year, Some(2020));
    assert_eq!(onward.folders.len(), 1);
}

#[test]
fn movies_merge_punctuation_variants_by_identity() {
    let folders = vec![
        folder("Dungeons & Dragons (2000) [1080p]"),
        folder("Dungeons and Dragons (2000) [2160p]"),
    ];

    let (outcome, skipped) = group_folders(LibraryKind::Movies, folders);
    assert!(skipped.is_empty());

    let GroupOutcome::Movies(groups) = outcome else {
        panic!("expected movie groups");
    };
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].folders.len(), 2);
    assert_eq!(groups[0].year, Some(2000));
}

#[test]
fn movies_do_not_merge_year_vs_yearless() {
    let folders = vec![folder("300 (2006) [1080p]"), folder("300 [2160p]")];

    let (outcome, _) = group_folders(LibraryKind::Movies, folders);
    let GroupOutcome::Movies(groups) = outcome else {
        panic!("expected movie groups");
    };
    assert_eq!(groups.len(), 2);
}

#[test]
fn tv_groups_seasons_under_one_show() {
    let folders = vec![
        folder("Narcos (2015) Season 1 S01 (1080p BluRay x265 HEVC 10bit AAC 5.1 Vyndros)"),
        folder("Narcos (2015) Season 2 S02 (1080p BluRay x265 HEVC 10bit AAC 5.1 Vyndros)"),
        folder("The.Wire.S01.1080p.BluRay.x265-RARBG"),
        folder("The.Wire.S02.1080p.BluRay.x265-RARBG"),
    ];

    let (outcome, skipped) = group_folders(LibraryKind::Tv, folders);
    assert!(skipped.is_empty(), "unexpected skips: {skipped:?}");

    let GroupOutcome::Tv(groups) = outcome else {
        panic!("expected tv groups");
    };
    assert_eq!(groups.len(), 2);

    let narcos = &groups[0];
    assert_eq!(narcos.title, "Narcos");
    assert_eq!(narcos.year, Some(2015));
    assert_eq!(narcos.seasons.len(), 2);
    assert!(narcos.seasons.contains_key(&1));
    assert!(narcos.seasons.contains_key(&2));

    let wire = &groups[1];
    assert_eq!(wire.title, "The Wire");
    assert_eq!(wire.year, None);
    assert_eq!(wire.seasons.len(), 2);
    assert!(wire.seasons.contains_key(&1));
    assert!(wire.seasons.contains_key(&2));
}

#[test]
fn movies_do_not_merge_similar_but_different_titles() {
    let folders = vec![
        folder("300 (2006) [1080p]"),
        folder("300 Rise of an Empire (2014) [1080p]"),
    ];

    let (outcome, skipped) = group_folders(LibraryKind::Movies, folders);
    assert!(skipped.is_empty());

    let GroupOutcome::Movies(groups) = outcome else {
        panic!("expected movie groups");
    };
    assert_eq!(groups.len(), 2);
}

#[test]
fn movies_merge_ampersand_with_and() {
    let folders = vec![
        folder("Dungeons & Dragons (2000) [1080p]"),
        folder("Dungeons and Dragons (2000) [2160p]"),
    ];

    let (outcome, skipped) = group_folders(LibraryKind::Movies, folders);
    assert!(skipped.is_empty());
    let GroupOutcome::Movies(groups) = outcome else {
        panic!("expected movie groups");
    };
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].folders.len(), 2);
}

#[test]
fn tv_does_not_merge_similar_show_names() {
    let folders = vec![folder("The.Wire.S01.1080p"), folder("The.Wired.S01.1080p")];
    let (outcome, skipped) = group_folders(LibraryKind::Tv, folders);
    assert!(skipped.is_empty());
    let GroupOutcome::Tv(groups) = outcome else {
        panic!("expected tv groups");
    };
    assert_eq!(groups.len(), 2);
}

#[test]
fn tv_skips_folder_without_season() {
    let folders = vec![folder("Narcos (2015)")];

    let (outcome, skipped) = group_folders(LibraryKind::Tv, folders);
    assert_eq!(skipped.len(), 1);

    let GroupOutcome::Tv(groups) = outcome else {
        panic!("expected tv groups");
    };
    assert!(groups.is_empty());
}

#[test]
fn tv_resolves_season_subfolders_under_a_titled_container() {
    // Common season-pack layout: the title/year live on the container
    // folder, each season is split into its own bare "Season N" subfolder,
    // and "Extras" has no season and should still be skipped.
    let container =
        "Ben 10 (2005) Season 1-4 S01-S04 (1080p WEB-DL x265 HEVC 10bit AAC 2.0 RCVR) [UTR]";
    let folders = vec![
        nested_folder(container, "Season 1"),
        nested_folder(container, "Season 2"),
        nested_folder(container, "Season 3"),
        nested_folder(container, "Season 4"),
        nested_folder(container, "Extras"),
    ];

    let (outcome, skipped) = group_folders(LibraryKind::Tv, folders);
    assert_eq!(skipped.len(), 1, "unexpected skips: {skipped:?}");
    assert!(skipped[0].path.ends_with("Extras"));
    assert_eq!(skipped[0].reason, "no parseable season number");

    let GroupOutcome::Tv(groups) = outcome else {
        panic!("expected tv groups");
    };
    assert_eq!(groups.len(), 1);
    let ben10 = &groups[0];
    assert_eq!(ben10.title, "Ben 10");
    assert_eq!(ben10.year, Some(2005));
    assert_eq!(ben10.seasons.len(), 4);
    for s in 1..=4u8 {
        assert!(ben10.seasons.contains_key(&s), "missing season {s}");
    }
}

#[test]
fn tv_bare_season_folder_without_a_parseable_parent_is_skipped() {
    // If the parent directory's own name doesn't parse into a title (empty
    // or otherwise unusable), a bare "Season 1" leaf still has nowhere to
    // borrow a title from and must be skipped rather than guessed at.
    let folders = vec![nested_folder("", "Season 1")];

    let (outcome, skipped) = group_folders(LibraryKind::Tv, folders);
    assert_eq!(skipped.len(), 1);
    let GroupOutcome::Tv(groups) = outcome else {
        panic!("expected tv groups");
    };
    assert!(groups.is_empty());
}
