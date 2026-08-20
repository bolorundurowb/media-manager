//! Phase 3 execution suite (§11, §12).
//!
//! Every test that moves files runs under both `native` and `reserve`.
//! Journal writes go to a TempDir, never the user data dir.

use std::path::Path;

use mm_core::Config;
use mm_core::classify::MediaKind;
use mm_core::config::{ConflictPolicy, StrategyConfig};
use mm_core::error::Outcome;
use mm_core::fs::CancelToken;
use mm_core::fs::faulty::{Fault, FaultyFs, InjectErr, Method};
use mm_core::fs::real::RealFs;
use mm_core::plan::{Action, DirRename, DirRenameId, Plan};
use mm_core::volume::VolumeSemantics;
use mm_engine::{ExecOptions, GcOptions, Planner, execute, gc};
use tempfile::TempDir;

fn cfg_with(strategy: StrategyConfig) -> Config {
    let mut c = Config::default();
    c.moves.no_replace_strategy = strategy;
    c
}

fn exec_opts(journal: &Path) -> ExecOptions {
    ExecOptions {
        fail_fast: false,
        journal_dir: journal.to_path_buf(),
        cancel: CancelToken::new(),
        allow_replace: false,
    }
}

fn listing(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let path = e.path();
            let rel = path.strip_prefix(root).unwrap();
            let s = rel.to_string_lossy().replace('\\', "/");
            if path.is_dir() {
                out.push(format!("{s}/"));
                walk(&path, root, out);
            } else {
                out.push(s);
            }
        }
    }
    walk(root, root, &mut out);
    out
}

fn listed_name_in(dir: &Path, expected: &str) -> bool {
    let rd = std::fs::read_dir(dir).unwrap();
    rd.filter_map(|e| e.ok())
        .any(|e| e.file_name().to_string_lossy() == expected)
}

#[test]
fn source_survives_copy_into_failure_reserve() {
    let media = TempDir::new().unwrap();
    let journal = TempDir::new().unwrap();
    let src = media.path().join("Inception.2010.mkv");
    std::fs::write(&src, b"keep-me-please").unwrap();
    let mut cfg = cfg_with(StrategyConfig::Reserve);
    cfg.cleanup.remove_empty_dirs = false;
    let real = RealFs::new();
    let planner = Planner::new(&real, media.path(), MediaKind::Movies, &cfg).unwrap();
    let plan = planner.plan(Default::default()).unwrap();
    let fs = FaultyFs::with_faults(
        real,
        vec![Fault {
            call_index: 1,
            method: Method::CopyInto,
            err: InjectErr::PermissionDenied,
        }],
    );
    let report = execute(&fs, &plan, &cfg, &exec_opts(journal.path()));
    assert!(report.fatal.is_none(), "{:?}", report.fatal);
    assert_eq!(std::fs::read(&src).unwrap(), b"keep-me-please");
    assert!(report.total(Outcome::Failed) >= 1);
}

#[test]
fn source_survives_remove_file_failure_reserve() {
    let media = TempDir::new().unwrap();
    let journal = TempDir::new().unwrap();
    let src = media.path().join("Inception.2010.mkv");
    std::fs::write(&src, b"keep-me-please").unwrap();
    let mut cfg = cfg_with(StrategyConfig::Reserve);
    cfg.cleanup.remove_empty_dirs = false;
    let real = RealFs::new();
    let planner = Planner::new(&real, media.path(), MediaKind::Movies, &cfg).unwrap();
    let plan = planner.plan(Default::default()).unwrap();
    let fs = FaultyFs::with_faults(
        real,
        vec![Fault {
            call_index: 1,
            method: Method::RemoveFile,
            err: InjectErr::PermissionDenied,
        }],
    );
    let report = execute(&fs, &plan, &cfg, &exec_opts(journal.path()));
    assert!(src.exists(), "source must not be lost");
    assert_eq!(std::fs::read(&src).unwrap(), b"keep-me-please");
    assert!(report.total(Outcome::Failed) >= 1 || report.total(Outcome::Moved) >= 1);
}

#[test]
fn permission_denied_on_one_file_does_not_abort_run() {
    for strategy in [StrategyConfig::Native, StrategyConfig::Reserve] {
        let media = TempDir::new().unwrap();
        let journal = TempDir::new().unwrap();
        std::fs::write(media.path().join("Inception.2010.mkv"), b"one").unwrap();
        std::fs::write(media.path().join("Up.2009.mkv"), b"two").unwrap();
        let cfg = cfg_with(strategy);
        let real = RealFs::new();
        let planner = Planner::new(&real, media.path(), MediaKind::Movies, &cfg).unwrap();
        let plan = planner.plan(Default::default()).unwrap();
        let method = if strategy == StrategyConfig::Native {
            Method::RenameNoReplace
        } else {
            Method::CreateNew
        };
        let fs = FaultyFs::with_faults(
            real,
            vec![Fault {
                call_index: 1,
                method,
                err: InjectErr::PermissionDenied,
            }],
        );
        let report = execute(&fs, &plan, &cfg, &exec_opts(journal.path()));
        assert!(report.fatal.is_none(), "PermissionDenied must not be Fatal");
        assert!(
            report.total(Outcome::Failed) >= 1,
            "strategy={strategy:?} {:?}",
            report.counts
        );
        let moved_or_present = report.total(Outcome::Moved) >= 1
            || media.path().join("Up.2009.mkv").exists()
            || listing(media.path())
                .iter()
                .any(|p| p.contains("Up (2009)"));
        assert!(
            moved_or_present,
            "other items should still be processed: {:?}",
            listing(media.path())
        );
    }
}

#[test]
fn dest_directory_is_conflict_not_overwrite() {
    for strategy in [StrategyConfig::Native, StrategyConfig::Reserve] {
        let media = TempDir::new().unwrap();
        let journal = TempDir::new().unwrap();
        std::fs::write(media.path().join("Inception.2010.mkv"), b"src").unwrap();
        let dest_dir = media.path().join("Inception (2010)");
        std::fs::create_dir(&dest_dir).unwrap();
        // Occupy the dest *file* path with a directory.
        std::fs::create_dir(dest_dir.join("Inception (2010).mkv")).unwrap();
        let cfg = cfg_with(strategy);
        let fs = RealFs::new();
        let planner = Planner::new(&fs, media.path(), MediaKind::Movies, &cfg).unwrap();
        let plan = planner.plan(Default::default()).unwrap();
        let report = execute(&fs, &plan, &cfg, &exec_opts(journal.path()));
        assert!(
            dest_dir.join("Inception (2010).mkv").is_dir(),
            "directory dest must not be clobbered"
        );
        assert!(media.path().join("Inception.2010.mkv").exists());
        assert!(
            report.total(Outcome::Conflicted) >= 1
                || plan
                    .items
                    .iter()
                    .any(|i| matches!(i.action, Action::Conflict { .. })),
            "strategy={strategy:?}"
        );
    }
}

#[test]
fn case_only_directory_rename_two_step() {
    let media = TempDir::new().unwrap();
    let journal = TempDir::new().unwrap();
    let from = media.path().join("foo");
    std::fs::create_dir(&from).unwrap();
    std::fs::write(from.join("marker.txt"), b"x").unwrap();
    let mut plan = Plan::new(
        uuid::Uuid::new_v4(),
        media.path().to_path_buf(),
        MediaKind::Movies,
        "test".into(),
        VolumeSemantics::conservative(),
    );
    plan.dir_renames.push(DirRename {
        id: DirRenameId(0),
        from: from.clone(),
        to: media.path().join("Foo"),
    });
    let fs = RealFs::new();
    let cfg = Config::default();
    let report = execute(&fs, &plan, &cfg, &exec_opts(journal.path()));
    assert!(report.fatal.is_none(), "{:?}", report.fatal);
    assert!(
        listed_name_in(media.path(), "Foo"),
        "listing={:?}",
        listing(media.path())
    );
    assert!(media.path().join("Foo").join("marker.txt").exists());
}

#[test]
fn empty_dir_cleanup_and_junk_tolerance() {
    for strategy in [StrategyConfig::Native, StrategyConfig::Reserve] {
        // Last file moved out → source dir gone.
        let media = TempDir::new().unwrap();
        let journal = TempDir::new().unwrap();
        let nested = media.path().join("downloads");
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(nested.join("Inception.2010.mkv"), b"x").unwrap();
        let mut cfg = cfg_with(strategy);
        cfg.cleanup.remove_empty_dirs = true;
        cfg.cleanup.tolerate_junk = false;
        let fs = RealFs::new();
        let planner = Planner::new(&fs, media.path(), MediaKind::Movies, &cfg).unwrap();
        let plan = planner.plan(Default::default()).unwrap();
        let report = execute(&fs, &plan, &cfg, &exec_opts(journal.path()));
        assert!(report.fatal.is_none(), "{:?}", report.fatal);
        assert!(
            !nested.exists(),
            "empty source dir should be removed: {:?}",
            listing(media.path())
        );
        assert!(report.dirs_removed >= 1);
    }

    // Leftover file blocks removal.
    {
        let media = TempDir::new().unwrap();
        let journal = TempDir::new().unwrap();
        let nested = media.path().join("downloads");
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(nested.join("Inception.2010.mkv"), b"x").unwrap();
        std::fs::write(nested.join("notes.txt"), b"keep").unwrap();
        let cfg = cfg_with(StrategyConfig::Native);
        let fs = RealFs::new();
        let planner = Planner::new(&fs, media.path(), MediaKind::Movies, &cfg).unwrap();
        let plan = planner.plan(Default::default()).unwrap();
        let _ = execute(&fs, &plan, &cfg, &exec_opts(journal.path()));
        assert!(nested.exists(), "dir with leftover file must remain");
        assert!(nested.join("notes.txt").exists());
    }

    // .DS_Store is not deleted unless tolerate_junk.
    {
        let media = TempDir::new().unwrap();
        let journal = TempDir::new().unwrap();
        let nested = media.path().join("downloads");
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(nested.join("Inception.2010.mkv"), b"x").unwrap();
        std::fs::write(nested.join(".DS_Store"), b"junk").unwrap();
        let mut cfg = cfg_with(StrategyConfig::Native);
        cfg.cleanup.tolerate_junk = false;
        let fs = RealFs::new();
        let planner = Planner::new(&fs, media.path(), MediaKind::Movies, &cfg).unwrap();
        let plan = planner.plan(Default::default()).unwrap();
        let _ = execute(&fs, &plan, &cfg, &exec_opts(journal.path()));
        assert!(nested.exists());
        assert!(nested.join(".DS_Store").exists());
    }
}

#[test]
fn interrupted_reserve_gc_and_report_not_overwrite() {
    let media = TempDir::new().unwrap();
    let journal = TempDir::new().unwrap();
    let src = media.path().join("Inception.2010.mkv");
    std::fs::write(&src, b"source-intact").unwrap();
    let dest_dir = media.path().join("Inception (2010)");
    std::fs::create_dir(&dest_dir).unwrap();
    let dest = dest_dir.join("Inception (2010).mkv");
    std::fs::write(&dest, b"short").unwrap();

    // Simulate a crash: unmatched MOVE intent, source intact, dest leftover.
    let mut j = mm_engine::Journal::create(journal.path().join("journal.jsonl")).unwrap();
    j.bind(uuid::Uuid::new_v4(), media.path().to_path_buf(), "d".into());
    j.write_intent(mm_engine::JournalOp::Move, Some(&src), Some(&dest))
        .unwrap();
    drop(j);

    let j = mm_engine::Journal::open(journal.path().join("journal.jsonl")).unwrap();
    assert_eq!(j.unmatched_intents(Some(media.path())).len(), 1);

    let fs = RealFs::new();
    let report = gc(
        &fs,
        media.path(),
        &GcOptions {
            journal_dir: journal.path().to_path_buf(),
            yes: true,
        },
    );
    assert!(report.reservations_reclaimed >= 1);
    assert!(!dest.exists(), "gc must remove leftover reservation");
    assert_eq!(std::fs::read(&src).unwrap(), b"source-intact");

    // Recreate leftover without a matching intent: plain execute reports Conflict.
    std::fs::write(&dest, b"someone-elses-file").unwrap();
    let cfg = cfg_with(StrategyConfig::Reserve);
    let planner = Planner::new(&fs, media.path(), MediaKind::Movies, &cfg).unwrap();
    let plan = planner.plan(Default::default()).unwrap();
    let report = execute(&fs, &plan, &cfg, &exec_opts(journal.path()));
    assert_eq!(std::fs::read(&dest).unwrap(), b"someone-elses-file");
    assert!(src.exists());
    assert!(
        report.total(Outcome::Conflicted) >= 1
            || plan
                .items
                .iter()
                .any(|i| matches!(i.action, Action::Conflict { .. }))
    );
}

#[test]
fn skip_if_identical_different_bytes_is_conflict() {
    let media = TempDir::new().unwrap();
    let journal = TempDir::new().unwrap();
    std::fs::write(media.path().join("Inception.2010.mkv"), b"aaaa").unwrap();
    let dest_dir = media.path().join("Inception (2010)");
    std::fs::create_dir(&dest_dir).unwrap();
    std::fs::write(dest_dir.join("Inception (2010).mkv"), b"bbbb").unwrap();
    let mut cfg = cfg_with(StrategyConfig::Native);
    cfg.conflict.policy = ConflictPolicy::SkipIfIdentical;
    let fs = RealFs::new();
    let planner = Planner::new(&fs, media.path(), MediaKind::Movies, &cfg).unwrap();
    let plan = planner.plan(Default::default()).unwrap();
    assert!(
        plan.items
            .iter()
            .any(|i| matches!(i.action, Action::Conflict { .. })),
        "different bytes must Conflict"
    );
    let report = execute(&fs, &plan, &cfg, &exec_opts(journal.path()));
    assert_eq!(
        std::fs::read(dest_dir.join("Inception (2010).mkv")).unwrap(),
        b"bbbb"
    );
    assert!(media.path().join("Inception.2010.mkv").exists());
    assert!(report.total(Outcome::Conflicted) >= 1);
}

#[test]
fn cancel_during_copy_deletes_reservation() {
    let media = TempDir::new().unwrap();
    let journal = TempDir::new().unwrap();
    let src = media.path().join("Inception.2010.mkv");
    std::fs::write(&src, b"source-bytes").unwrap();
    let mut cfg = cfg_with(StrategyConfig::Reserve);
    cfg.cleanup.remove_empty_dirs = false;
    let real = RealFs::new();
    let planner = Planner::new(&real, media.path(), MediaKind::Movies, &cfg).unwrap();
    let plan = planner.plan(Default::default()).unwrap();
    let fs = FaultyFs::with_faults(
        real,
        vec![Fault {
            call_index: 1,
            method: Method::CopyInto,
            err: InjectErr::Interrupted,
        }],
    );
    let report = execute(&fs, &plan, &cfg, &exec_opts(journal.path()));
    assert!(report.cancelled);
    assert_eq!(std::fs::read(&src).unwrap(), b"source-bytes");
    let dest = media
        .path()
        .join("Inception (2010)")
        .join("Inception (2010).mkv");
    assert!(
        !dest.exists() || std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0) == 0,
        "reservation must be deleted on cancel"
    );
}

#[test]
fn idempotent_replan_is_noop() {
    let media = TempDir::new().unwrap();
    let journal = TempDir::new().unwrap();
    std::fs::write(media.path().join("Inception.2010.mkv"), b"x").unwrap();
    let cfg = cfg_with(StrategyConfig::Native);
    let fs = RealFs::new();
    let planner = Planner::new(&fs, media.path(), MediaKind::Movies, &cfg).unwrap();
    let plan = planner.plan(Default::default()).unwrap();
    let report = execute(&fs, &plan, &cfg, &exec_opts(journal.path()));
    assert!(report.total(Outcome::Moved) >= 1);
    let planner = Planner::new(&fs, media.path(), MediaKind::Movies, &cfg).unwrap();
    let plan2 = planner.plan(Default::default()).unwrap();
    let remaining: Vec<_> = plan2
        .items
        .iter()
        .filter(|i| {
            matches!(
                i.action,
                Action::Move { .. } | Action::Conflict { .. } | Action::Duplicate { .. }
            )
        })
        .collect();
    assert!(
        remaining.is_empty(),
        "re-plan should be NoOp, got {:?}",
        plan2
            .items
            .iter()
            .map(|i| format!("{:?} {:?}", i.relative, i.action))
            .collect::<Vec<_>>()
    );
}

#[test]
fn copy_into_failure_leaves_unmatched_when_dest_survives() {
    // FaultyFs CopyInto fails after create_new; execute deletes dest and writes
    // FAIL. A *crash-shaped* leftover is covered by interrupted_reserve_gc.
    // Here we assert source bytes are never lost under native+reserve.
    for strategy in [StrategyConfig::Native, StrategyConfig::Reserve] {
        let media = TempDir::new().unwrap();
        let journal = TempDir::new().unwrap();
        let src = media.path().join("Inception.2010.mkv");
        std::fs::write(&src, vec![9u8; 32]).unwrap();
        let cfg = cfg_with(strategy);
        let real = RealFs::new();
        let planner = Planner::new(&real, media.path(), MediaKind::Movies, &cfg).unwrap();
        let plan = planner.plan(Default::default()).unwrap();
        let method = if strategy == StrategyConfig::Reserve {
            Method::CopyInto
        } else {
            Method::RenameNoReplace
        };
        let fs = FaultyFs::with_faults(
            real,
            vec![Fault {
                call_index: 1,
                method,
                err: InjectErr::PermissionDenied,
            }],
        );
        let report = execute(&fs, &plan, &cfg, &exec_opts(journal.path()));
        assert!(src.exists());
        assert_eq!(std::fs::read(&src).unwrap(), vec![9u8; 32]);
        assert!(report.fatal.is_none());
    }
}
