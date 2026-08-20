//! Multi-selection entry point used by the GUI (Phase 7): run several
//! independently-typed source children against one shared (optionally
//! different) destination root, streaming progress as it happens.
//!
//! This runs the same scan → parse → group → plan → validate → execute
//! pipeline as the CLI's [`crate::run`], just once per assigned kind, and
//! adds one thing the CLI never needs: a check that a "movies" assignment
//! and a "tv" assignment never end up wanting the same destination path.
//!
//! Phase 6 uses a bounded pool for independent child scan/parse work, joins
//! globally for grouping and collision validation, then fans independent
//! destination identities back out to bounded execution workers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc::Sender, Mutex};

use rayon::prelude::*;

use crate::cancel::CancelToken;
use crate::exec::{ExecEvent, ExecReport};
use crate::group::{self, GroupOutcome, ParsedFolder, SkippedFolder};
use crate::journal::Journal;
use crate::parse::LibraryKind;
use crate::plan::{self, Plan, Skip};
use crate::vfs::RealFileSystem;
use crate::{scan, Error, Summary};

/// One independently scanned source child: either a folder or a loose video.
#[derive(Debug, Clone)]
pub struct WorkItem {
    pub path: PathBuf,
    pub kind: LibraryKind,
}

/// A single unit of progress, suitable for streaming to a UI as it happens.
#[derive(Debug)]
pub enum LogEvent {
    Scanning,
    JobStarted(PathBuf),
    JobFinished(PathBuf),
    CreateDir(PathBuf),
    PlannedMove {
        from: PathBuf,
        to: PathBuf,
    },
    Moved {
        from: PathBuf,
        to: PathBuf,
    },
    Skipped {
        path: PathBuf,
        reason: String,
    },
    Failed {
        path: PathBuf,
        reason: String,
    },
    Finished {
        moved: usize,
        merged: usize,
        skipped: usize,
        failed: usize,
        cancelled: bool,
    },
}

/// Run a specific set of source children, each independently typed, against
/// an optional destination root. `dest == None` means in-place, matching the
/// CLI. Progress is streamed to `sink` as each stage completes; the return
/// value is the same aggregate a caller would get from [`crate::run`].
pub fn run_items(
    root: &Path,
    dest: Option<&Path>,
    items: Vec<WorkItem>,
    apply: bool,
    cancel: &CancelToken,
    sink: Sender<LogEvent>,
) -> Result<Summary, Error> {
    let root = match absolute(root) {
        Ok(r) => r,
        Err(err) => return Err(bail(&sink, root.to_path_buf(), err)),
    };
    match std::fs::metadata(&root) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            return Err(bail(&sink, root.clone(), "not a directory"));
        }
        Err(err) => return Err(bail(&sink, root.clone(), err)),
    }

    let dest_root = match prepare_dest(&root, dest, &items, apply) {
        Ok(d) => d,
        Err((path, err)) => return Err(bail(&sink, path, err)),
    };

    let _ = sink.send(LogEvent::Scanning);
    let batches = parallel_scan_parse(items, cancel, &sink)?;
    let mut summary = Summary::default();
    let mut movie_parsed = Vec::new();
    let mut movie_skips = Vec::new();
    let mut tv_parsed = Vec::new();
    let mut tv_skips = Vec::new();
    for batch in batches {
        summary.failed += batch.scan_failures;
        match batch.kind {
            LibraryKind::Movies => {
                movie_parsed.extend(batch.parsed);
                movie_skips.extend(batch.skips);
            }
            LibraryKind::Tv => {
                tv_parsed.extend(batch.parsed);
                tv_skips.extend(batch.skips);
            }
        }
    }
    summary.cancelled = cancel.is_cancelled();

    let mut plans = Vec::new();
    for (kind, parsed, mut skips) in [
        (LibraryKind::Movies, movie_parsed, movie_skips),
        (LibraryKind::Tv, tv_parsed, tv_skips),
    ] {
        if parsed.is_empty() {
            if !skips.is_empty() {
                plans.push(skip_plan(skips));
            }
            continue;
        }
        let outcome = group::group_parsed(kind, parsed, &mut skips);
        summary.merged += count_merged(&outcome);
        plans.extend(identity_plans(&dest_root, outcome, skips));
    }

    resolve_plan_collisions(&mut plans);

    for plan in &plans {
        summary.planned_moves += plan.moves.len();
        summary.skipped += plan.skips.len();
        for skip in &plan.skips {
            let _ = sink.send(LogEvent::Skipped {
                path: skip.path.clone(),
                reason: skip.reason.clone(),
            });
        }
        if !apply {
            for dir in &plan.dirs {
                let _ = sink.send(LogEvent::CreateDir(dir.clone()));
            }
            for mv in &plan.moves {
                let _ = sink.send(LogEvent::PlannedMove {
                    from: mv.from.clone(),
                    to: mv.to.clone(),
                });
            }
        }
    }

    if apply {
        let executable: Vec<Plan> = plans
            .into_iter()
            .filter(|p| !p.moves.is_empty() || !p.dirs.is_empty())
            .collect();
        if !executable.is_empty() {
            let journal = Journal::open(&dest_root);
            tracing::info!(path = %journal.path().display(), "apply journal");
            journal.record(&format!(
                "RUN START dest={} jobs={} moves={}",
                dest_root.display(),
                executable.len(),
                executable.iter().map(|p| p.moves.len()).sum::<usize>()
            ));
            let reports = execute_jobs(
                &executable,
                worker_count(executable.len()),
                cancel,
                &sink,
                &journal,
                &RealFileSystem,
            );
            for report in &reports {
                summary.moved += report.moved;
                summary.failed += report.failed.len();
                summary.cancelled |= report.cancelled;
            }
            journal.record(&format!(
                "RUN END moved={} failed={} cancelled={}",
                summary.moved, summary.failed, summary.cancelled
            ));
        }
        summary.cancelled |= cancel.is_cancelled();
    }

    let _ = sink.send(LogEvent::Finished {
        moved: summary.moved,
        merged: summary.merged,
        skipped: summary.skipped,
        failed: summary.failed,
        cancelled: summary.cancelled,
    });

    Ok(summary)
}

struct ParsedBatch {
    kind: LibraryKind,
    parsed: Vec<ParsedFolder>,
    skips: Vec<SkippedFolder>,
    scan_failures: usize,
}

fn parallel_scan_parse(
    items: Vec<WorkItem>,
    cancel: &CancelToken,
    sink: &Sender<LogEvent>,
) -> Result<Vec<ParsedBatch>, Error> {
    let threads = worker_count(items.len());
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .map_err(|err| Error::Io(std::io::Error::other(err.to_string())))?;
    Ok(pool.install(|| {
        items
            .into_par_iter()
            .map(|item| {
                if cancel.is_cancelled() {
                    return ParsedBatch {
                        kind: item.kind,
                        parsed: Vec::new(),
                        skips: vec![SkippedFolder {
                            path: item.path,
                            reason: "cancelled before scan started".into(),
                        }],
                        scan_failures: 0,
                    };
                }
                match scan::scan_child(&item.path) {
                    Ok(found) => {
                        if found.is_empty() {
                            return ParsedBatch {
                                kind: item.kind,
                                parsed: Vec::new(),
                                skips: vec![SkippedFolder {
                                    path: item.path,
                                    reason: "no supported video files found".into(),
                                }],
                                scan_failures: 0,
                            };
                        }
                        let (parsed, skips) = group::parse_folders(item.kind, found);
                        ParsedBatch {
                            kind: item.kind,
                            parsed,
                            skips,
                            scan_failures: 0,
                        }
                    }
                    Err(err) => {
                        let _ = sink.send(LogEvent::Failed {
                            path: item.path.clone(),
                            reason: format!("could not scan: {err}"),
                        });
                        ParsedBatch {
                            kind: item.kind,
                            parsed: Vec::new(),
                            skips: Vec::new(),
                            scan_failures: 1,
                        }
                    }
                }
            })
            .collect()
    }))
}

fn identity_plans(
    dest_root: &Path,
    outcome: GroupOutcome,
    prior_skips: Vec<SkippedFolder>,
) -> Vec<Plan> {
    let mut plans = Vec::new();
    if !prior_skips.is_empty() {
        plans.push(skip_plan(prior_skips));
    }
    match outcome {
        GroupOutcome::Movies(groups) => {
            for group in groups {
                plans.push(plan::build_plan(
                    dest_root,
                    GroupOutcome::Movies(vec![group]),
                    Vec::new(),
                ));
            }
        }
        GroupOutcome::Tv(groups) => {
            for group in groups {
                plans.push(plan::build_plan(
                    dest_root,
                    GroupOutcome::Tv(vec![group]),
                    Vec::new(),
                ));
            }
        }
    }
    plans
}

fn skip_plan(skips: Vec<SkippedFolder>) -> Plan {
    Plan {
        skips: skips
            .into_iter()
            .map(|s| Skip {
                path: s.path,
                reason: s.reason,
            })
            .collect(),
        ..Plan::default()
    }
}

/// No two independently executable identity plans may overlap a destination
/// directory. Global validation happens before any worker starts.
fn resolve_plan_collisions(plans: &mut [Plan]) {
    if plans.len() < 2 {
        return;
    }

    let mut owner: HashMap<PathBuf, usize> = HashMap::new();
    let mut colliding: Vec<PathBuf> = Vec::new();
    for (i, plan) in plans.iter().enumerate() {
        for dir in &plan.dirs {
            let key = lower_path(dir);
            match owner.get(&key) {
                Some(&o) if o != i => {
                    if !colliding.contains(&key) {
                        colliding.push(key);
                    }
                }
                Some(_) => {}
                None => {
                    owner.insert(key, i);
                }
            }
        }
    }
    if colliding.is_empty() {
        return;
    }

    // Path-component-aware (not raw string) prefix check, so e.g. a
    // colliding "Foo (2020)" never wrongly swallows a sibling folder like
    // "Foo (2020) Extended" that merely shares a string prefix.
    let under_colliding = |p: &Path| {
        let key = lower_path(p);
        colliding.iter().any(|c| key.starts_with(c))
    };

    for plan in plans.iter_mut() {
        let mut kept = Vec::with_capacity(plan.moves.len());
        for mv in plan.moves.drain(..) {
            if under_colliding(&mv.to) {
                let reason = "destination claimed by multiple identity jobs".to_string();
                plan.skips.push(Skip {
                    path: mv.from,
                    reason,
                });
            } else {
                kept.push(mv);
            }
        }
        plan.moves = kept;
        plan.dirs.retain(|d| !under_colliding(d));
    }
}

fn execute_jobs(
    plans: &[Plan],
    workers: usize,
    cancel: &CancelToken,
    sink: &Sender<LogEvent>,
    journal: &Journal,
    fs: &dyn crate::vfs::FileSystem,
) -> Vec<ExecReport> {
    let next = AtomicUsize::new(0);
    let reports = Mutex::new(Vec::with_capacity(plans.len()));

    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                if cancel.is_cancelled() {
                    break;
                }
                let index = next.fetch_add(1, Ordering::SeqCst);
                let Some(plan) = plans.get(index) else {
                    break;
                };
                if cancel.is_cancelled() {
                    break;
                }
                let identity = plan_identity(plan);
                let _ = sink.send(LogEvent::JobStarted(identity.clone()));
                journal.record(&format!(
                    "JOB START dest={} moves={}",
                    identity.display(),
                    plan.moves.len()
                ));
                let report =
                    crate::exec::execute_with_events(plan, fs, cancel, journal, &|event| {
                        let log = match event {
                            ExecEvent::CreatedDir(path) => LogEvent::CreateDir(path),
                            ExecEvent::Moved { from, to } => LogEvent::Moved { from, to },
                            ExecEvent::Failed { path, reason } => LogEvent::Failed { path, reason },
                        };
                        let _ = sink.send(log);
                    });
                journal.record(&format!(
                    "JOB END dest={} moved={} failed={} cancelled={}",
                    identity.display(),
                    report.moved,
                    report.failed.len(),
                    report.cancelled
                ));
                let _ = sink.send(LogEvent::JobFinished(identity));
                reports.lock().unwrap().push(report);
            });
        }
    });
    reports.into_inner().unwrap()
}

fn plan_identity(plan: &Plan) -> PathBuf {
    plan.dirs
        .iter()
        .min_by_key(|p| p.components().count())
        .cloned()
        .or_else(|| plan.moves.first().map(|m| m.to.clone()))
        .unwrap_or_default()
}

fn worker_count(work_len: usize) -> usize {
    std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .clamp(1, 8)
        .min(work_len.max(1))
}

fn prepare_dest(
    root: &Path,
    dest: Option<&Path>,
    items: &[WorkItem],
    apply: bool,
) -> Result<PathBuf, (PathBuf, std::io::Error)> {
    let Some(requested) = dest else {
        return Ok(root.to_path_buf());
    };
    let dest = absolute(requested).map_err(|err| (requested.to_path_buf(), err))?;
    let root_key = comparable_path(root);
    let dest_key = comparable_path(&dest);
    if dest_key == root_key {
        return Ok(dest);
    }
    for item in items {
        let source = comparable_path(&item.path);
        if dest_key.starts_with(&source) || source.starts_with(&dest_key) {
            return Err((
                dest,
                std::io::Error::other(format!(
                    "destination overlaps selected source: {}",
                    item.path.display()
                )),
            ));
        }
    }

    if dest.exists() {
        if !dest.is_dir() {
            return Err((
                dest,
                std::io::Error::other("destination exists but is not a directory"),
            ));
        }
    } else if apply {
        std::fs::create_dir_all(&dest).map_err(|err| (dest.clone(), err))?;
    } else {
        let parent_ok = dest.parent().is_some_and(Path::is_dir);
        if !parent_ok {
            return Err((
                dest,
                std::io::Error::other("destination parent does not exist"),
            ));
        }
    }
    Ok(dest)
}

fn comparable_path(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    let mut missing = Vec::new();
    let mut cursor = path;
    while !cursor.exists() {
        let Some(name) = cursor.file_name() else {
            return path.to_path_buf();
        };
        missing.push(name.to_os_string());
        let Some(parent) = cursor.parent() else {
            return path.to_path_buf();
        };
        cursor = parent;
    }
    let mut canonical = cursor
        .canonicalize()
        .unwrap_or_else(|_| cursor.to_path_buf());
    for component in missing.into_iter().rev() {
        canonical.push(component);
    }
    canonical
}

/// Case-folded `PathBuf` so path comparisons behave the same on
/// case-insensitive (Windows, macOS) and case-sensitive filesystems, while
/// still using `Path::starts_with`'s component-aware matching rather than a
/// raw string prefix.
fn lower_path(path: &Path) -> PathBuf {
    PathBuf::from(path.to_string_lossy().to_ascii_lowercase())
}

fn count_merged(outcome: &GroupOutcome) -> usize {
    match outcome {
        GroupOutcome::Movies(groups) => groups.iter().filter(|g| g.folders.len() > 1).count(),
        GroupOutcome::Tv(groups) => groups
            .iter()
            .filter(|g| g.seasons.values().map(|v| v.len()).sum::<usize>() > 1)
            .count(),
    }
}

fn absolute(path: &Path) -> std::io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn bail(sink: &Sender<LogEvent>, path: PathBuf, err: impl std::fmt::Display) -> Error {
    let reason = err.to_string();
    let _ = sink.send(LogEvent::Failed {
        path: path.clone(),
        reason: reason.clone(),
    });
    let _ = sink.send(LogEvent::Finished {
        moved: 0,
        merged: 0,
        skipped: 0,
        failed: 1,
        cancelled: false,
    });
    Error::Io(std::io::Error::other(reason))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::{FileSystem, InMemoryFileSystem};
    use std::fs;
    use std::io;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::sync::Barrier;

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_lib() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "media-manager-multi-{}-{}",
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
    fn assigns_different_children_to_different_kinds_in_one_run() {
        let root = temp_lib();
        touch(&root.join("300 (2006) [1080p]/300.1080p.mkv"));
        touch(&root.join("The.Wire.S01.1080p.BluRay.x265-RARBG/the.wire.s01e01.mkv"));

        let (tx, rx) = mpsc::channel();
        let items = vec![
            WorkItem {
                path: root.join("300 (2006) [1080p]"),
                kind: LibraryKind::Movies,
            },
            WorkItem {
                path: root.join("The.Wire.S01.1080p.BluRay.x265-RARBG"),
                kind: LibraryKind::Tv,
            },
        ];
        let summary = run_items(&root, None, items, true, &CancelToken::new(), tx).unwrap();
        assert_eq!(summary.failed, 0);
        assert!(root.join("300 (2006)/300 (2006) - 1080p.mkv").is_file());
        assert!(root
            .join("The Wire/Season 01/The Wire S01E01.mkv")
            .is_file());
        // At least a Scanning event and a final Finished event were sent.
        let events: Vec<_> = rx.try_iter().collect();
        assert!(matches!(events.first(), Some(LogEvent::Scanning)));
        assert!(matches!(events.last(), Some(LogEvent::Finished { .. })));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn unassigned_children_are_left_untouched() {
        let root = temp_lib();
        touch(&root.join("300 (2006) [1080p]/300.1080p.mkv"));
        touch(&root.join("Untouched (2020)/video.mkv"));

        let (tx, _rx) = mpsc::channel();
        let items = vec![WorkItem {
            path: root.join("300 (2006) [1080p]"),
            kind: LibraryKind::Movies,
        }];
        run_items(&root, None, items, true, &CancelToken::new(), tx).unwrap();

        assert!(root.join("300 (2006)/300 (2006) - 1080p.mkv").is_file());
        assert!(root.join("Untouched (2020)/video.mkv").is_file());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn optional_dest_places_output_outside_the_source_root() {
        let root = temp_lib();
        let dest = temp_lib();
        touch(&root.join("300 (2006) [1080p]/300.1080p.mkv"));

        let (tx, _rx) = mpsc::channel();
        let items = vec![WorkItem {
            path: root.join("300 (2006) [1080p]"),
            kind: LibraryKind::Movies,
        }];
        run_items(&root, Some(&dest), items, true, &CancelToken::new(), tx).unwrap();

        assert!(dest.join("300 (2006)/300 (2006) - 1080p.mkv").is_file());
        assert!(!root.join("300 (2006) [1080p]").exists());
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&dest);
    }

    #[test]
    fn movie_and_tv_assignments_never_write_the_same_destination() {
        let root = temp_lib();
        // Same title/year, assigned as both a movie and a show: both would
        // want to create "Ambiguous (2020)/" at the dest root.
        touch(&root.join("Ambiguous (2020) [1080p]/movie.mkv"));
        touch(&root.join("Ambiguous.2020.S01/Ambiguous.S01E01.mkv"));

        let (tx, _rx) = mpsc::channel();
        let items = vec![
            WorkItem {
                path: root.join("Ambiguous (2020) [1080p]"),
                kind: LibraryKind::Movies,
            },
            WorkItem {
                path: root.join("Ambiguous.2020.S01"),
                kind: LibraryKind::Tv,
            },
        ];
        let summary = run_items(&root, None, items, true, &CancelToken::new(), tx).unwrap();

        assert!(summary.skipped >= 2);
        // Neither source moved: the shared dest folder name was dropped
        // from both plans rather than being handed to whichever ran first.
        assert!(root.join("Ambiguous (2020) [1080p]/movie.mkv").is_file());
        assert!(root
            .join("Ambiguous.2020.S01/Ambiguous.S01E01.mkv")
            .is_file());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn loose_movie_and_tv_files_are_processed_as_root_children() {
        let root = temp_lib();
        touch(&root.join("Onward.2020.2160p.mkv"));
        touch(&root.join("The.Sopranos.S06E14.1080p.mkv"));

        let (tx, _rx) = mpsc::channel();
        let items = vec![
            WorkItem {
                path: root.join("Onward.2020.2160p.mkv"),
                kind: LibraryKind::Movies,
            },
            WorkItem {
                path: root.join("The.Sopranos.S06E14.1080p.mkv"),
                kind: LibraryKind::Tv,
            },
        ];
        let summary = run_items(&root, None, items, true, &CancelToken::new(), tx).unwrap();

        assert_eq!(summary.moved, 2);
        assert!(root
            .join("Onward (2020)/Onward (2020) - 2160p.mkv")
            .is_file());
        assert!(root
            .join("The Sopranos/Season 06/The Sopranos S06E14.mkv")
            .is_file());
        assert!(root.is_dir(), "a loose file must never make root removable");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn unrelated_identity_jobs_emit_independent_job_events() {
        let root = temp_lib();
        touch(&root.join("First (2020) [1080p]/first.1080p.mkv"));
        touch(&root.join("Second (2021) [2160p]/second.2160p.mkv"));
        let (tx, rx) = mpsc::channel();
        let items = vec![
            WorkItem {
                path: root.join("First (2020) [1080p]"),
                kind: LibraryKind::Movies,
            },
            WorkItem {
                path: root.join("Second (2021) [2160p]"),
                kind: LibraryKind::Movies,
            },
        ];

        let summary = run_items(&root, None, items, true, &CancelToken::new(), tx).unwrap();
        let events: Vec<_> = rx.try_iter().collect();
        assert_eq!(summary.moved, 2);
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, LogEvent::JobStarted(_)))
                .count(),
            2
        );
        assert!(root.join("First (2020)/First (2020) - 1080p.mkv").is_file());
        assert!(root
            .join("Second (2021)/Second (2021) - 2160p.mkv")
            .is_file());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn dry_run_does_not_create_optional_destination() {
        let root = temp_lib();
        let dest = root.parent().unwrap().join(format!(
            "media-manager-dry-dest-{}",
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&dest);
        touch(&root.join("Movie (2020) [1080p]/movie.1080p.mkv"));
        let (tx, _rx) = mpsc::channel();
        let items = vec![WorkItem {
            path: root.join("Movie (2020) [1080p]"),
            kind: LibraryKind::Movies,
        }];

        run_items(&root, Some(&dest), items, false, &CancelToken::new(), tx).unwrap();
        assert!(!dest.exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn destination_inside_selected_source_is_rejected() {
        let root = temp_lib();
        let source = root.join("Movie (2020) [1080p]");
        touch(&source.join("movie.1080p.mkv"));
        let dest = source.join("organized");
        let (tx, _rx) = mpsc::channel();
        let items = vec![WorkItem {
            path: source,
            kind: LibraryKind::Movies,
        }];

        assert!(run_items(&root, Some(&dest), items, true, &CancelToken::new(), tx).is_err());
        assert!(!dest.exists(), "unsafe destination must not be created");
        let _ = fs::remove_dir_all(&root);
    }

    struct CancelAfterFirstRename {
        inner: InMemoryFileSystem,
        cancel: CancelToken,
    }

    impl FileSystem for CancelAfterFirstRename {
        fn read_dir(&self, dir: &Path) -> io::Result<Vec<PathBuf>> {
            self.inner.read_dir(dir)
        }

        fn exists(&self, path: &Path) -> bool {
            self.inner.exists(path)
        }

        fn is_dir(&self, path: &Path) -> bool {
            self.inner.is_dir(path)
        }

        fn create_dir_all(&self, path: &Path) -> io::Result<()> {
            self.inner.create_dir_all(path)
        }

        fn rename_no_replace(&self, from: &Path, to: &Path) -> io::Result<()> {
            self.inner.rename_no_replace(from, to)?;
            self.cancel.cancel();
            Ok(())
        }

        fn remove_empty_dir(&self, path: &Path) -> io::Result<()> {
            self.inner.remove_empty_dir(path)
        }
    }

    #[test]
    fn cancellation_stops_single_worker_before_next_job_starts() {
        let cancel = CancelToken::new();
        let fs = CancelAfterFirstRename {
            inner: InMemoryFileSystem::new()
                .with_file("/root/a/a.mkv")
                .with_file("/root/b/b.mkv"),
            cancel: cancel.clone(),
        };
        let plans = vec![
            Plan {
                dirs: vec![PathBuf::from("/dest/a")],
                moves: vec![crate::plan::MoveOp {
                    from: PathBuf::from("/root/a/a.mkv"),
                    to: PathBuf::from("/dest/a/a.mkv"),
                }],
                source_folders: vec![PathBuf::from("/root/a")],
                ..Plan::default()
            },
            Plan {
                dirs: vec![PathBuf::from("/dest/b")],
                moves: vec![crate::plan::MoveOp {
                    from: PathBuf::from("/root/b/b.mkv"),
                    to: PathBuf::from("/dest/b/b.mkv"),
                }],
                source_folders: vec![PathBuf::from("/root/b")],
                ..Plan::default()
            },
        ];
        let (tx, rx) = mpsc::channel();
        let reports = execute_jobs(&plans, 1, &cancel, &tx, &Journal::disabled(), &fs);
        let events: Vec<_> = rx.try_iter().collect();

        assert_eq!(reports.iter().map(|r| r.moved).sum::<usize>(), 1);
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, LogEvent::JobStarted(_)))
                .count(),
            1
        );
        assert!(fs.exists(Path::new("/root/b/b.mkv")));
    }

    struct ConcurrentRenameProbe {
        inner: InMemoryFileSystem,
        barrier: Barrier,
        active: AtomicUsize,
        maximum: AtomicUsize,
    }

    impl FileSystem for ConcurrentRenameProbe {
        fn read_dir(&self, dir: &Path) -> io::Result<Vec<PathBuf>> {
            self.inner.read_dir(dir)
        }

        fn exists(&self, path: &Path) -> bool {
            self.inner.exists(path)
        }

        fn is_dir(&self, path: &Path) -> bool {
            self.inner.is_dir(path)
        }

        fn create_dir_all(&self, path: &Path) -> io::Result<()> {
            self.inner.create_dir_all(path)
        }

        fn rename_no_replace(&self, from: &Path, to: &Path) -> io::Result<()> {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.maximum.fetch_max(active, Ordering::SeqCst);
            self.barrier.wait();
            let result = self.inner.rename_no_replace(from, to);
            self.active.fetch_sub(1, Ordering::SeqCst);
            result
        }

        fn remove_empty_dir(&self, path: &Path) -> io::Result<()> {
            self.inner.remove_empty_dir(path)
        }
    }

    #[test]
    fn independent_jobs_execute_concurrently() {
        let fs = ConcurrentRenameProbe {
            inner: InMemoryFileSystem::new()
                .with_file("/root/a/a.mkv")
                .with_file("/root/b/b.mkv"),
            barrier: Barrier::new(2),
            active: AtomicUsize::new(0),
            maximum: AtomicUsize::new(0),
        };
        let plans = vec![
            Plan {
                dirs: vec![PathBuf::from("/dest/a")],
                moves: vec![crate::plan::MoveOp {
                    from: PathBuf::from("/root/a/a.mkv"),
                    to: PathBuf::from("/dest/a/a.mkv"),
                }],
                ..Plan::default()
            },
            Plan {
                dirs: vec![PathBuf::from("/dest/b")],
                moves: vec![crate::plan::MoveOp {
                    from: PathBuf::from("/root/b/b.mkv"),
                    to: PathBuf::from("/dest/b/b.mkv"),
                }],
                ..Plan::default()
            },
        ];
        let (tx, _rx) = mpsc::channel();
        let reports = execute_jobs(
            &plans,
            2,
            &CancelToken::new(),
            &tx,
            &Journal::disabled(),
            &fs,
        );

        assert_eq!(reports.iter().map(|r| r.moved).sum::<usize>(), 2);
        assert_eq!(fs.maximum.load(Ordering::SeqCst), 2);
    }
}
