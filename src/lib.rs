mod cancel;
mod exec;
pub mod group;
mod journal;
mod multi;
mod os_rename;
pub mod parse;
mod plan;
mod report;
pub mod scan;
mod vfs;

use std::path::{Path, PathBuf};

pub use cancel::CancelToken;
pub use multi::{run_items, LogEvent, WorkItem};
pub use parse::LibraryKind;
pub use plan::Plan;
pub use report::{print_exec, print_plan};
pub use vfs::{FaultyFileSystem, FileSystem, InMemoryFileSystem, RealFileSystem};

#[derive(Debug)]
pub enum Error {
    NotADirectory(PathBuf),
    Io(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotADirectory(p) => write!(f, "not a directory: {}", p.display()),
            Error::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Error::Io(value)
    }
}

#[derive(Debug, Default)]
pub struct Options {
    pub root: PathBuf,
    pub kind: LibraryKind,
    pub apply: bool,
    /// Checked between top-level steps during `--apply`; set it (e.g. from a
    /// Ctrl+C handler) to stop starting new work without interrupting a
    /// rename already in flight. Defaults to a token that is never
    /// cancelled.
    pub cancel: CancelToken,
}

#[derive(Debug, Default)]
pub struct Summary {
    pub planned_moves: usize,
    /// Number of movie/show groups made of more than one source folder.
    pub merged: usize,
    pub skipped: usize,
    pub moved: usize,
    pub failed: usize,
    pub cancelled: bool,
}

pub fn run(opts: Options) -> Result<Summary, Error> {
    let root = absolute(&opts.root)?;
    let meta = std::fs::metadata(&root)?;
    if !meta.is_dir() {
        return Err(Error::NotADirectory(root));
    }

    tracing::info!(root = %root.display(), kind = ?opts.kind, apply = opts.apply, "scanning");

    let folders = scan::scan_root(&root)?;
    tracing::info!(count = folders.len(), "media folders");

    let (outcome, prior_skips) = group::group_folders(opts.kind, folders);
    let merged = count_merged(&outcome);
    let plan = plan::build_plan(&root, outcome, prior_skips);

    report::print_plan(&plan, opts.apply, merged);

    let mut summary = Summary {
        planned_moves: plan.moves.len(),
        merged,
        skipped: plan.skips.len(),
        ..Summary::default()
    };

    if opts.apply {
        let mut journal = journal::Journal::open(&root);
        tracing::info!(path = %journal.path().display(), "apply journal");
        journal.record(&format!(
            "RUN START root={} kind={:?} moves={}",
            root.display(),
            opts.kind,
            plan.moves.len()
        ));
        let exec = exec::execute(&plan, &vfs::RealFileSystem, &opts.cancel, &mut journal);
        journal.record(&format!(
            "RUN END moved={} failed={} cancelled={}",
            exec.moved,
            exec.failed.len(),
            exec.cancelled
        ));
        report::print_exec(&exec);
        summary.moved = exec.moved;
        summary.failed = exec.failed.len();
        summary.cancelled = exec.cancelled;
    }

    tracing::info!(
        processed = summary.moved,
        merged = summary.merged,
        skipped = summary.skipped,
        failed = summary.failed,
        cancelled = summary.cancelled,
        "run summary"
    );

    Ok(summary)
}

/// Number of groups assembled from more than one source folder (movie
/// versions merged under one title/year, or TV seasons merged under one
/// show).
fn count_merged(outcome: &group::GroupOutcome) -> usize {
    match outcome {
        group::GroupOutcome::Movies(groups) => {
            groups.iter().filter(|g| g.folders.len() > 1).count()
        }
        group::GroupOutcome::Tv(groups) => groups
            .iter()
            .filter(|g| g.seasons.values().map(|v| v.len()).sum::<usize>() > 1)
            .count(),
    }
}

fn absolute(path: &Path) -> Result<PathBuf, Error> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_lib() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "media-manager-p1-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, b"").unwrap();
    }

    #[test]
    fn movies_300_versions_and_onward() {
        let root = temp_lib();
        touch(&root.join("300 (2006) [1080p]/300.1080p.mkv"));
        touch(&root.join("300 (2006) [1080p]/300.1080p.en.srt"));
        touch(&root.join("300 (2006) [2160p]/300.2160p.mkv"));
        touch(&root.join("Onward.2020.2160p.HDR.WEB-DL.DD5.1.HEVC-EVO[TGx]/Onward.2020.2160p.mkv"));

        let summary = run(Options {
            root: root.clone(),
            kind: LibraryKind::Movies,
            apply: true,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(summary.failed, 0);
        assert!(root.join("300 (2006)/300 (2006) - 1080p.mkv").is_file());
        assert!(root.join("300 (2006)/300 (2006) - 1080p.en.srt").is_file());
        assert!(root.join("300 (2006)/300 (2006) - 2160p.mkv").is_file());
        assert!(root
            .join("Onward (2020)/Onward (2020) - 2160p.mkv")
            .is_file());
        assert!(!root.join("300 (2006) [1080p]").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn movies_different_years_do_not_merge() {
        let root = temp_lib();
        touch(&root.join("300 (2006) [1080p]/a.mkv"));
        touch(&root.join("300 (2014) [1080p]/b.mkv"));

        run(Options {
            root: root.clone(),
            kind: LibraryKind::Movies,
            apply: true,
            ..Default::default()
        })
        .unwrap();

        assert!(root.join("300 (2006)/300 (2006) - 1080p.mkv").is_file());
        assert!(root.join("300 (2014)/300 (2014) - 1080p.mkv").is_file());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn nested_movie_folder_lands_under_root() {
        let root = temp_lib();
        touch(&root.join("Movies/300 (2006) [1080p]/300.1080p.mkv"));

        run(Options {
            root: root.clone(),
            kind: LibraryKind::Movies,
            apply: true,
            ..Default::default()
        })
        .unwrap();

        assert!(root.join("300 (2006)/300 (2006) - 1080p.mkv").is_file());
        assert!(!root.join("Movies/300 (2006) [1080p]").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn tv_narcos_and_the_wire() {
        let root = temp_lib();
        touch(
            &root.join(
                "Narcos (2015) Season 1 S01 (1080p BluRay x265 HEVC 10bit AAC 5.1 Vyndros)/Narcos.S01E01.mkv",
            ),
        );
        touch(
            &root.join(
                "Narcos (2015) Season 2 S02 (1080p BluRay x265 HEVC 10bit AAC 5.1 Vyndros)/Narcos.S02E01.mkv",
            ),
        );
        touch(&root.join("The.Wire.S01.1080p.BluRay.x265-RARBG/the.wire.s01e01.mkv"));
        touch(&root.join("The.Wire.S02.1080p.BluRay.x265-RARBG/the.wire.s02e02.mkv"));

        run(Options {
            root: root.clone(),
            kind: LibraryKind::Tv,
            apply: true,
            ..Default::default()
        })
        .unwrap();

        assert!(root
            .join("Narcos (2015)/Season 01/Narcos S01E01.mkv")
            .is_file());
        assert!(root
            .join("Narcos (2015)/Season 02/Narcos S02E01.mkv")
            .is_file());
        assert!(root
            .join("The Wire/Season 01/The Wire S01E01.mkv")
            .is_file());
        assert!(root
            .join("The Wire/Season 02/The Wire S02E02.mkv")
            .is_file());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn tv_skips_folder_without_season() {
        let root = temp_lib();
        touch(&root.join("Narcos (2015)/episode.mkv"));
        let summary = run(Options {
            root: root.clone(),
            kind: LibraryKind::Tv,
            apply: true,
            ..Default::default()
        })
        .unwrap();
        assert!(summary.skipped >= 1);
        assert!(root.join("Narcos (2015)/episode.mkv").is_file());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn dry_run_writes_nothing() {
        let root = temp_lib();
        touch(&root.join("300 (2006) [1080p]/300.1080p.mkv"));
        run(Options {
            root: root.clone(),
            kind: LibraryKind::Movies,
            apply: false,
            ..Default::default()
        })
        .unwrap();
        assert!(root.join("300 (2006) [1080p]/300.1080p.mkv").is_file());
        assert!(!root.join("300 (2006)").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn multiple_videos_in_one_movie_folder_need_per_file_labels() {
        let root = temp_lib();
        touch(&root.join("Inception (2010)/inception.1080p.mkv"));
        touch(&root.join("Inception (2010)/inception.2160p.mkv"));

        run(Options {
            root: root.clone(),
            kind: LibraryKind::Movies,
            apply: true,
            ..Default::default()
        })
        .unwrap();

        assert!(root
            .join("Inception (2010)/Inception (2010) - 1080p.mkv")
            .is_file());
        assert!(root
            .join("Inception (2010)/Inception (2010) - 2160p.mkv")
            .is_file());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn unlabeled_second_video_is_skipped() {
        let root = temp_lib();
        touch(&root.join("Movie (2020) [1080p]/labelled.1080p.mkv"));
        touch(&root.join("Movie (2020) [1080p]/other.mkv"));

        let summary = run(Options {
            root: root.clone(),
            kind: LibraryKind::Movies,
            apply: true,
            ..Default::default()
        })
        .unwrap();
        assert!(summary.skipped >= 1);
        assert!(root.join("Movie (2020) [1080p]/other.mkv").is_file());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn nested_subs_and_language_suffixes() {
        let root = temp_lib();
        touch(&root.join("300 (2006) [1080p]/300.1080p.mkv"));
        touch(&root.join("300 (2006) [1080p]/subs/300.1080p.en.srt"));
        touch(&root.join("300 (2006) [1080p]/subs/en/300.1080p.fr.srt"));
        touch(&root.join("300 (2006) [1080p]/subs/unrelated.srt"));

        run(Options {
            root: root.clone(),
            kind: LibraryKind::Movies,
            apply: true,
            ..Default::default()
        })
        .unwrap();

        assert!(root.join("300 (2006)/300 (2006) - 1080p.mkv").is_file());
        assert!(root
            .join("300 (2006)/subs/300 (2006) - 1080p.en.srt")
            .is_file());
        assert!(root
            .join("300 (2006)/subs/300 (2006) - 1080p.fr.srt")
            .is_file());
        assert!(root.join("300 (2006) [1080p]/subs/unrelated.srt").is_file());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn extras_move_when_folder_is_fully_processed() {
        let root = temp_lib();
        touch(&root.join("Onward.2020.2160p/Onward.2020.2160p.mkv"));
        touch(&root.join("Onward.2020.2160p/movie.nfo"));
        touch(&root.join("Onward.2020.2160p/poster.jpg"));

        run(Options {
            root: root.clone(),
            kind: LibraryKind::Movies,
            apply: true,
            ..Default::default()
        })
        .unwrap();

        assert!(root
            .join("Onward (2020)/Onward (2020) - 2160p.mkv")
            .is_file());
        assert!(root.join("Onward (2020)/movie.nfo").is_file());
        assert!(root.join("Onward (2020)/poster.jpg").is_file());
        assert!(!root.join("Onward.2020.2160p").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn extras_stay_when_a_video_is_skipped() {
        let root = temp_lib();
        touch(&root.join("Movie (2020) [1080p]/a.1080p.mkv"));
        touch(&root.join("Movie (2020) [1080p]/b.mkv"));
        touch(&root.join("Movie (2020) [1080p]/movie.nfo"));

        run(Options {
            root: root.clone(),
            kind: LibraryKind::Movies,
            apply: true,
            ..Default::default()
        })
        .unwrap();

        assert!(root.join("Movie (2020) [1080p]/movie.nfo").is_file());
        assert!(root.join("Movie (2020) [1080p]/b.mkv").is_file());
        let _ = fs::remove_dir_all(&root);
    }

    // -- Phase 4/5: safety, resilience, and architecture tests --------------

    #[test]
    fn empty_source_folder_is_removed_after_successful_move() {
        let root = temp_lib();
        touch(&root.join("300 (2006) [1080p]/300.1080p.mkv"));
        run(Options {
            root: root.clone(),
            kind: LibraryKind::Movies,
            apply: true,
            ..Default::default()
        })
        .unwrap();
        assert!(!root.join("300 (2006) [1080p]").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn source_folder_with_a_skipped_file_is_never_removed() {
        let root = temp_lib();
        touch(&root.join("Movie (2020) [1080p]/a.1080p.mkv"));
        touch(&root.join("Movie (2020) [1080p]/b.mkv"));
        run(Options {
            root: root.clone(),
            kind: LibraryKind::Movies,
            apply: true,
            ..Default::default()
        })
        .unwrap();
        // `b.mkv` has no reliable version label and stays put, so the
        // folder must never be deleted even though the labelled video
        // moved out of it.
        assert!(root.join("Movie (2020) [1080p]").is_dir());
        assert!(root.join("Movie (2020) [1080p]/b.mkv").is_file());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn case_insensitive_duplicate_destinations_are_skipped() {
        let root = temp_lib();
        // Two distinct source folders (so NTFS will keep both extras) whose
        // extras would land on dest names that differ only by case.
        touch(&root.join("Movie (2020) [1080p]/video.1080p.mkv"));
        touch(&root.join("Movie (2020) [1080p]/Cover.jpg"));
        touch(&root.join("Movie (2020) [2160p]/video.2160p.mkv"));
        touch(&root.join("Movie (2020) [2160p]/cover.jpg"));

        let summary = run(Options {
            root: root.clone(),
            kind: LibraryKind::Movies,
            apply: true,
            ..Default::default()
        })
        .unwrap();

        assert!(root
            .join("Movie (2020)/Movie (2020) - 1080p.mkv")
            .is_file());
        assert!(root
            .join("Movie (2020)/Movie (2020) - 2160p.mkv")
            .is_file());
        assert!(summary.skipped >= 1);
        // Exactly one extra should have moved; the case-colliding extra
        // stays in its source folder.
        let dest_has_one_cover = root.join("Movie (2020)/Cover.jpg").is_file()
            || root.join("Movie (2020)/cover.jpg").is_file();
        assert!(dest_has_one_cover);
        let left_behind = root.join("Movie (2020) [1080p]/Cover.jpg").is_file()
            || root.join("Movie (2020) [2160p]/cover.jpg").is_file();
        assert!(left_behind);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn ambiguous_matches_with_identical_labels_are_skipped_not_overwritten() {
        let root = temp_lib();
        touch(&root.join("Movie (2020) [1080p]/a.1080p.mkv"));
        touch(&root.join("Movie (2020) [1080p]/b.1080p.mkv"));

        let summary = run(Options {
            root: root.clone(),
            kind: LibraryKind::Movies,
            apply: true,
            ..Default::default()
        })
        .unwrap();

        assert_eq!(summary.moved, 1);
        assert!(summary.skipped >= 1);
        assert!(root
            .join("Movie (2020)/Movie (2020) - 1080p.mkv")
            .is_file());
        // Exactly one of the two same-labelled videos moved; the other was
        // left in place rather than overwritten or renamed with a made-up
        // label.
        let one_left_behind = root.join("Movie (2020) [1080p]/a.1080p.mkv").is_file()
            || root.join("Movie (2020) [1080p]/b.1080p.mkv").is_file();
        assert!(one_left_behind);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn wrong_mode_folder_is_parsed_not_autocorrected() {
        let root = temp_lib();
        // A TV-shaped folder run under `--type movies`: the season markers
        // are never specially recognised in movie mode, so they simply stay
        // in the title instead of being "fixed" into a Season 01 layout.
        touch(&root.join(
            "Narcos (2015) Season 1 S01 (1080p BluRay x265 HEVC 10bit AAC 5.1 Vyndros)/Narcos.S01E01.mkv",
        ));

        let summary = run(Options {
            root: root.clone(),
            kind: LibraryKind::Movies,
            apply: true,
            ..Default::default()
        })
        .unwrap();

        assert_eq!(summary.failed, 0);
        assert!(!root.join("Narcos (2015)/Season 01").exists());
        assert!(root
            .join("Narcos Season 1 S01 (2015)/Narcos Season 1 S01 (2015) - 1080p.mkv")
            .is_file());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn apply_writes_a_journal_for_crash_diagnosis() {
        let root = temp_lib();
        touch(&root.join("300 (2006) [1080p]/300.1080p.mkv"));
        run(Options {
            root: root.clone(),
            kind: LibraryKind::Movies,
            apply: true,
            ..Default::default()
        })
        .unwrap();
        let journal_path = root.join(".media-manager-journal.log");
        assert!(journal_path.is_file());
        let contents = fs::read_to_string(&journal_path).unwrap();
        assert!(contents.contains("RUN START"));
        assert!(contents.contains("MOVE OK"));
        assert!(contents.contains("RUN END"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn dry_run_never_writes_a_journal() {
        let root = temp_lib();
        touch(&root.join("300 (2006) [1080p]/300.1080p.mkv"));
        run(Options {
            root: root.clone(),
            kind: LibraryKind::Movies,
            apply: false,
            ..Default::default()
        })
        .unwrap();
        assert!(!root.join(".media-manager-journal.log").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn pre_cancelled_run_moves_nothing() {
        let root = temp_lib();
        touch(&root.join("300 (2006) [1080p]/300.1080p.mkv"));
        let cancel = CancelToken::new();
        cancel.cancel();
        let summary = run(Options {
            root: root.clone(),
            kind: LibraryKind::Movies,
            apply: true,
            cancel,
        })
        .unwrap();
        assert!(summary.cancelled);
        assert_eq!(summary.moved, 0);
        assert!(root.join("300 (2006) [1080p]/300.1080p.mkv").is_file());
        let _ = fs::remove_dir_all(&root);
    }
}
