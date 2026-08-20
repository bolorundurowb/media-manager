mod exec;
mod group;
mod parse;
mod plan;
mod report;
mod scan;

use std::path::{Path, PathBuf};

pub use parse::LibraryKind;
pub use plan::Plan;
pub use report::{print_exec, print_plan};

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

#[derive(Debug)]
pub struct Options {
    pub root: PathBuf,
    pub kind: LibraryKind,
    pub apply: bool,
}

#[derive(Debug, Default)]
pub struct Summary {
    pub planned_moves: usize,
    pub skipped: usize,
    pub moved: usize,
    pub failed: usize,
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
    let plan = plan::build_plan(&root, outcome, prior_skips);

    report::print_plan(&plan, opts.apply);

    let mut summary = Summary {
        planned_moves: plan.moves.len(),
        skipped: plan.skips.len(),
        ..Summary::default()
    };

    if opts.apply {
        let exec = exec::execute(&plan);
        report::print_exec(&exec);
        summary.moved = exec.moved;
        summary.failed = exec.failed.len();
    }

    Ok(summary)
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
        })
        .unwrap();

        assert!(root.join("300 (2006)/300 (2006) - 1080p.mkv").is_file());
        assert!(root.join("300 (2014)/300 (2014) - 1080p.mkv").is_file());
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
        })
        .unwrap();
        assert!(root.join("300 (2006) [1080p]/300.1080p.mkv").is_file());
        assert!(!root.join("300 (2006)").exists());
        let _ = fs::remove_dir_all(&root);
    }
}
