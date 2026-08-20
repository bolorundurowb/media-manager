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
