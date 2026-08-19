//! Execution (§6): phased apply of a [`Plan`].
//!
//! 1. Create directories (serial, shallowest-first)
//! 2. Move files (serial in Phase 3; destination partitioning is Phase 7)
//! 3. Rename directories (serial, two-step for case/normalisation)
//! 4. Remove empty directories (serial, deepest-first)
//! 5. Reclaim unmatched MOVE reservations for this run

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use mm_core::config::{Config, VerifyMode};
use mm_core::error::{Diagnostic, Outcome, RunMode, RunReport, Severity};
use mm_core::fs::{CancelToken, FileSystem, destination_occupied_error, is_cross_device};
use mm_core::plan::{Action, Plan, Readiness};
use mm_core::volume::NoReplaceStrategy;
use uuid::Uuid;

use crate::journal::{Journal, JournalOp};
use crate::reconcile::{OccupiedDecision, decide_occupied, same_path};

/// Options for [`execute`].
#[derive(Debug, Clone)]
pub struct ExecOptions {
    pub fail_fast: bool,
    /// Directory for `journal.jsonl` and `plans/`. Tests must pass a temp dir.
    pub journal_dir: PathBuf,
    pub cancel: CancelToken,
}

/// Options for [`gc`].
#[derive(Debug, Clone)]
pub struct GcOptions {
    pub journal_dir: PathBuf,
    pub yes: bool,
}

/// Apply `plan` to `fs`. Never aborts mid-run except [`mm_core::error::FatalReason`].
pub fn execute<F: FileSystem>(fs: &F, plan: &Plan, cfg: &Config, opts: &ExecOptions) -> RunReport {
    let started = Instant::now();
    let mut report = RunReport::new(plan.run_id, RunMode::Apply, plan.kind, plan.root.clone());

    let journal_path = opts.journal_dir.join("journal.jsonl");
    let mut journal = match Journal::create(&journal_path) {
        Ok(j) => j,
        Err(fatal) => {
            report.fatal = Some(fatal);
            report.duration = started.elapsed();
            return report;
        }
    };
    journal.bind(plan.run_id, plan.root.clone(), plan.config_digest.clone());
    if let Err(fatal) = journal.persist_plan(plan) {
        report.fatal = Some(fatal);
        report.duration = started.elapsed();
        return report;
    }

    if let Err(fatal) = create_directories(fs, plan, &mut journal, &mut report) {
        report.fatal = Some(fatal);
        report.duration = started.elapsed();
        return report;
    }

    let mut successful_sources: HashSet<PathBuf> = HashSet::new();
    if let Err(fatal) = move_files(
        fs,
        plan,
        cfg,
        opts,
        &mut journal,
        &mut report,
        &mut successful_sources,
    ) {
        report.fatal = Some(fatal);
        report.duration = started.elapsed();
        return report;
    }

    if !report.cancelled
        && let Err(fatal) = rename_directories(fs, plan, &mut journal, &mut report)
    {
        report.fatal = Some(fatal);
        report.duration = started.elapsed();
        return report;
    }

    if let Err(fatal) = remove_empty_directories(
        fs,
        plan,
        cfg,
        &successful_sources,
        &mut journal,
        &mut report,
    ) {
        report.fatal = Some(fatal);
        report.duration = started.elapsed();
        return report;
    }

    if let Err(fatal) = reclaim_this_run(fs, plan, &mut journal, &mut report) {
        report.fatal = Some(fatal);
        report.duration = started.elapsed();
        return report;
    }

    report.duration = started.elapsed();
    report
}

/// Fill a [`RunReport`] from a plan without writing (verify / dry-run).
pub fn report_from_plan(plan: &Plan, mode: RunMode) -> RunReport {
    let mut report = RunReport::new(plan.run_id, mode, plan.kind, plan.root.clone());
    report.diagnostics.extend(plan.diagnostics.clone());
    for item in &plan.items {
        match &item.action {
            Action::NoOp => report.count(Outcome::NoOp),
            Action::Move { .. } => {
                report.pending_count(Outcome::Moved);
            }
            Action::Skip { .. } => {
                report.count(Outcome::Skipped);
                report.pending_count(Outcome::Skipped);
            }
            Action::Conflict { .. } => {
                report.count(Outcome::Conflicted);
                report.pending_count(Outcome::Conflicted);
            }
            Action::Duplicate { .. } => {
                report.count(Outcome::Duplicated);
                report.pending_count(Outcome::Duplicated);
            }
            Action::NeedsReview { .. } => {
                let outcome = if matches!(item.readiness, Readiness::Ambiguous { .. }) {
                    Outcome::Ambiguous
                } else if item.class == mm_core::classify::FileClass::Unknown {
                    Outcome::Unclassified
                } else {
                    Outcome::NeedsReview
                };
                report.count(outcome);
                report.pending_count(outcome);
            }
        }
    }
    report
}

/// List unmatched MOVE reservations under `root`. With `yes`, delete dest
/// leftovers only when the journal proves we created them and the source still
/// exists.
pub fn gc<F: FileSystem>(fs: &F, root: &Path, opts: &GcOptions) -> RunReport {
    let started = Instant::now();
    let mut report = RunReport::new(
        Uuid::nil(),
        RunMode::Apply,
        mm_core::MediaKind::Movies,
        root.to_path_buf(),
    );
    let journal_path = opts.journal_dir.join("journal.jsonl");
    if !journal_path.exists() {
        report.duration = started.elapsed();
        return report;
    }
    let mut journal = match Journal::open(&journal_path) {
        Ok(j) => j,
        Err(fatal) => {
            report.fatal = Some(fatal);
            report.duration = started.elapsed();
            return report;
        }
    };

    let unmatched = journal.unmatched_intents(Some(root));
    for intent in unmatched {
        if intent.op != JournalOp::Move {
            continue;
        }
        let Some(from) = intent.from.as_deref() else {
            continue;
        };
        let Some(to) = intent.to.as_deref() else {
            continue;
        };
        if fs.metadata(from).is_err() {
            report.diagnostics.push(Diagnostic::warning(
                "gc",
                format!(
                    "unmatched intent seq={} dest={} but source missing; not deleting",
                    intent.seq,
                    to.display()
                ),
            ));
            continue;
        }
        if fs.metadata(to).is_err() {
            continue;
        }
        report.diagnostics.push(Diagnostic::info(
            "gc",
            format!("reservation leftover: {} (source intact)", to.display()),
        ));
        if !opts.yes {
            continue;
        }
        journal.bind(
            intent.run_id,
            intent.root.clone(),
            intent.config_digest.clone(),
        );
        let seq = match journal.write_intent(JournalOp::Reclaim, Some(from), Some(to)) {
            Ok(s) => s,
            Err(fatal) => {
                report.fatal = Some(fatal);
                break;
            }
        };
        match fs.remove_file(to) {
            Ok(()) => {
                report.reservations_reclaimed += 1;
                let _ = journal.write_outcome(
                    seq,
                    JournalOp::Reclaim,
                    Some(from),
                    Some(to),
                    "SUCCESS",
                    None,
                    None,
                );
            }
            Err(e) => {
                report.diagnostics.push(Diagnostic::warning(
                    "gc",
                    format!("could not delete {}: {e}", to.display()),
                ));
                let _ = journal.write_outcome(
                    seq,
                    JournalOp::Reclaim,
                    Some(from),
                    Some(to),
                    "FAIL",
                    None,
                    Some(&e.to_string()),
                );
            }
        }
    }
    report.duration = started.elapsed();
    report
}

fn create_directories<F: FileSystem>(
    fs: &F,
    plan: &Plan,
    journal: &mut Journal,
    report: &mut RunReport,
) -> Result<(), mm_core::error::FatalReason> {
    let mut dirs: Vec<PathBuf> = plan.dir_creates.iter().cloned().collect();
    dirs.sort_by_key(|p| (p.components().count(), p.as_os_str().len()));
    for dir in dirs {
        if fs.metadata(&dir).map(|m| m.is_dir).unwrap_or(false) {
            continue;
        }
        let seq = journal.write_intent(JournalOp::DirCreate, None, Some(&dir))?;
        match create_dir_retry(fs, &dir) {
            Ok(()) => {
                journal.write_outcome(
                    seq,
                    JournalOp::DirCreate,
                    None,
                    Some(&dir),
                    "SUCCESS",
                    None,
                    None,
                )?;
            }
            Err(e) => {
                journal.write_outcome(
                    seq,
                    JournalOp::DirCreate,
                    None,
                    Some(&dir),
                    "FAIL",
                    None,
                    Some(&e.to_string()),
                )?;
                report.diagnostics.push(Diagnostic {
                    severity: Severity::Warning,
                    item: None,
                    stage: "exec".into(),
                    message: format!("create_dir {} failed: {e}", dir.display()),
                    io_kind: Some(format!("{:?}", e.kind())),
                });
            }
        }
    }
    Ok(())
}

fn create_dir_retry<F: FileSystem>(fs: &F, dir: &Path) -> io::Result<()> {
    const ATTEMPTS: u32 = 5;
    let mut last = None;
    for i in 0..ATTEMPTS {
        match fs.create_dir_all(dir) {
            Ok(()) => return Ok(()),
            Err(e) if is_transient_dir_error(&e) && i + 1 < ATTEMPTS => {
                std::thread::sleep(Duration::from_millis(15));
                last = Some(e);
            }
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("create_dir retry exhausted")))
}

fn is_transient_dir_error(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::PermissionDenied)
        || e.raw_os_error() == Some(32) // ERROR_SHARING_VIOLATION
        || e.raw_os_error() == Some(5)
}

fn move_files<F: FileSystem>(
    fs: &F,
    plan: &Plan,
    cfg: &Config,
    opts: &ExecOptions,
    journal: &mut Journal,
    report: &mut RunReport,
    successful_sources: &mut HashSet<PathBuf>,
) -> Result<(), mm_core::error::FatalReason> {
    for item in &plan.items {
        if opts.cancel.is_cancelled() {
            report.cancelled = true;
            break;
        }
        match &item.action {
            Action::NoOp => report.count(Outcome::NoOp),
            Action::Skip { .. } => report.count(Outcome::Skipped),
            Action::Conflict { .. } => report.count(Outcome::Conflicted),
            Action::Duplicate { .. } => report.count(Outcome::Duplicated),
            Action::NeedsReview { .. } => {
                let outcome = if matches!(item.readiness, Readiness::Ambiguous { .. }) {
                    Outcome::Ambiguous
                } else if item.class == mm_core::classify::FileClass::Unknown {
                    Outcome::Unclassified
                } else {
                    Outcome::NeedsReview
                };
                report.count(outcome);
            }
            Action::Move { from, to } => match move_one(fs, plan, cfg, opts, journal, from, to)? {
                MoveResult::Moved { source_dir } => {
                    report.count(Outcome::Moved);
                    if let Some(dir) = source_dir {
                        successful_sources.insert(dir);
                    }
                }
                MoveResult::Skipped => report.count(Outcome::Skipped),
                MoveResult::Conflicted => report.count(Outcome::Conflicted),
                MoveResult::Failed { message, io_kind } => {
                    report.count(Outcome::Failed);
                    report.diagnostics.push(Diagnostic {
                        severity: Severity::Failure,
                        item: Some(item.id),
                        stage: "exec".into(),
                        message,
                        io_kind,
                    });
                    if opts.fail_fast {
                        break;
                    }
                }
                MoveResult::Cancelled => {
                    report.cancelled = true;
                    break;
                }
                MoveResult::NoOp => report.count(Outcome::NoOp),
            },
        }
    }
    Ok(())
}

enum MoveResult {
    Moved {
        source_dir: Option<PathBuf>,
    },
    Skipped,
    Conflicted,
    Failed {
        message: String,
        io_kind: Option<String>,
    },
    Cancelled,
    NoOp,
}

fn move_one<F: FileSystem>(
    fs: &F,
    plan: &Plan,
    cfg: &Config,
    opts: &ExecOptions,
    journal: &mut Journal,
    from: &Path,
    to: &Path,
) -> Result<MoveResult, mm_core::error::FatalReason> {
    if opts.cancel.is_cancelled() {
        return Ok(MoveResult::Cancelled);
    }
    if fs.metadata(from).is_err() {
        return Ok(fail("source missing", None));
    }

    let strategy = cfg
        .moves
        .no_replace_strategy
        .resolve(fs.no_replace_strategy(&plan.root));

    let mut dest = to.to_path_buf();
    if fs.metadata(&dest).is_ok() {
        match decide_occupied(fs, cfg, &plan.volume, from, &dest, &opts.cancel) {
            OccupiedDecision::Move { to: next } => {
                if same_path(from, &next, &plan.volume) {
                    return Ok(MoveResult::NoOp);
                }
                dest = next;
            }
            OccupiedDecision::Skip { .. } => return Ok(MoveResult::Skipped),
            OccupiedDecision::Conflict { .. } => return Ok(MoveResult::Conflicted),
            OccupiedDecision::Replace => {
                return move_replace(fs, cfg, opts, journal, from, &dest);
            }
        }
    }

    if let Some(parent) = dest.parent() {
        let _ = create_dir_retry(fs, parent);
    }

    match strategy {
        NoReplaceStrategy::Reserve => move_reserve(fs, cfg, opts, journal, from, &dest),
        NoReplaceStrategy::Native | NoReplaceStrategy::CheckThenRename => {
            match move_native(fs, cfg, opts, journal, from, &dest)? {
                MoveResult::Failed { message, io_kind }
                    if message.contains("cross-device")
                        || io_kind.as_deref() == Some("CrossesDevices") =>
                {
                    move_reserve(fs, cfg, opts, journal, from, &dest)
                }
                other => Ok(other),
            }
        }
    }
}

fn fail(message: impl Into<String>, io_kind: Option<String>) -> MoveResult {
    MoveResult::Failed {
        message: message.into(),
        io_kind,
    }
}

fn io_fail(e: &io::Error, context: &str) -> MoveResult {
    MoveResult::Failed {
        message: format!("{context}: {e}"),
        io_kind: Some(format!("{:?}", e.kind())),
    }
}

fn move_native<F: FileSystem>(
    fs: &F,
    cfg: &Config,
    opts: &ExecOptions,
    journal: &mut Journal,
    from: &Path,
    to: &Path,
) -> Result<MoveResult, mm_core::error::FatalReason> {
    let src_meta = match fs.metadata(from) {
        Ok(m) => m,
        Err(e) => return Ok(io_fail(&e, "stat source")),
    };
    let src_hash = if cfg.moves.verify == VerifyMode::Hash {
        match fs.hash(from, &opts.cancel) {
            Ok(h) => Some(h),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => return Ok(MoveResult::Cancelled),
            Err(e) => return Ok(io_fail(&e, "hash source")),
        }
    } else {
        None
    };

    let seq = journal.write_intent(JournalOp::Move, Some(from), Some(to))?;
    match fs.rename_no_replace(from, to) {
        Ok(()) => {}
        Err(e) if is_cross_device(&e) => {
            journal.write_outcome(
                seq,
                JournalOp::Move,
                Some(from),
                Some(to),
                "FAIL",
                None,
                Some("cross-device"),
            )?;
            return Ok(MoveResult::Failed {
                message: "cross-device".into(),
                io_kind: Some("CrossesDevices".into()),
            });
        }
        Err(e) if destination_occupied_error(&e) => {
            journal.write_outcome(
                seq,
                JournalOp::Move,
                Some(from),
                Some(to),
                "FAIL",
                None,
                Some("occupied"),
            )?;
            return Ok(MoveResult::Conflicted);
        }
        Err(e) => {
            journal.write_outcome(
                seq,
                JournalOp::Move,
                Some(from),
                Some(to),
                "FAIL",
                None,
                Some(&e.to_string()),
            )?;
            return Ok(io_fail(&e, "rename_no_replace"));
        }
    }

    match verify_dest(fs, cfg, to, src_meta.len, src_hash.as_ref(), &opts.cancel) {
        Verify::Ok => {
            journal.write_outcome(
                seq,
                JournalOp::Move,
                Some(from),
                Some(to),
                "SUCCESS",
                Some(src_meta.len),
                None,
            )?;
            Ok(MoveResult::Moved {
                source_dir: from.parent().map(Path::to_path_buf),
            })
        }
        Verify::Cancelled => Ok(MoveResult::Cancelled),
        Verify::Fail(msg) => {
            journal.write_outcome(
                seq,
                JournalOp::Move,
                Some(from),
                Some(to),
                "FAIL",
                None,
                Some(&msg),
            )?;
            Ok(fail(msg, None))
        }
    }
}

fn move_reserve<F: FileSystem>(
    fs: &F,
    cfg: &Config,
    opts: &ExecOptions,
    journal: &mut Journal,
    from: &Path,
    to: &Path,
) -> Result<MoveResult, mm_core::error::FatalReason> {
    let src_meta = match fs.metadata(from) {
        Ok(m) => m,
        Err(e) => return Ok(io_fail(&e, "stat source")),
    };
    let src_hash = if cfg.moves.verify == VerifyMode::Hash {
        match fs.hash(from, &opts.cancel) {
            Ok(h) => Some(h),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => return Ok(MoveResult::Cancelled),
            Err(e) => return Ok(io_fail(&e, "hash source")),
        }
    } else {
        None
    };

    let seq = journal.write_intent(JournalOp::Move, Some(from), Some(to))?;
    let mut handle = match fs.create_new(to) {
        Ok(h) => h,
        Err(e) if destination_occupied_error(&e) => {
            journal.write_outcome(
                seq,
                JournalOp::Move,
                Some(from),
                Some(to),
                "FAIL",
                None,
                Some("occupied"),
            )?;
            return Ok(MoveResult::Conflicted);
        }
        Err(e) => {
            journal.write_outcome(
                seq,
                JournalOp::Move,
                Some(from),
                Some(to),
                "FAIL",
                None,
                Some(&e.to_string()),
            )?;
            return Ok(io_fail(&e, "create_new"));
        }
    };

    let copy_res = fs.copy_into(from, &mut handle, &opts.cancel);
    drop(handle);

    match copy_res {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::Interrupted => {
            let _ = fs.remove_file(to);
            // Unmatched intent: dest gone, source intact.
            return Ok(MoveResult::Cancelled);
        }
        Err(e) => {
            let _ = fs.remove_file(to);
            journal.write_outcome(
                seq,
                JournalOp::Move,
                Some(from),
                Some(to),
                "FAIL",
                None,
                Some(&e.to_string()),
            )?;
            return Ok(io_fail(&e, "copy_into"));
        }
    }

    if let Some(parent) = to.parent() {
        let _ = fs.sync_dir(parent);
    }

    match verify_dest(fs, cfg, to, src_meta.len, src_hash.as_ref(), &opts.cancel) {
        Verify::Cancelled => {
            let _ = fs.remove_file(to);
            return Ok(MoveResult::Cancelled);
        }
        Verify::Fail(msg) => {
            // Leave dest (may be short) only if we didn't delete it; source stays.
            // Source must not be removed.
            journal.write_outcome(
                seq,
                JournalOp::Move,
                Some(from),
                Some(to),
                "FAIL",
                None,
                Some(&msg),
            )?;
            return Ok(fail(msg, None));
        }
        Verify::Ok => {}
    }

    if cfg.moves.preserve_mtime
        && let Some(mtime) = src_meta.modified
        && let Err(e) = fs.set_mtime(to, mtime)
    {
        journal.write_outcome(
            seq,
            JournalOp::Move,
            Some(from),
            Some(to),
            "FAIL",
            None,
            Some(&e.to_string()),
        )?;
        return Ok(io_fail(&e, "set_mtime"));
    }

    match fs.remove_file(from) {
        Ok(()) => {
            journal.write_outcome(
                seq,
                JournalOp::Move,
                Some(from),
                Some(to),
                "SUCCESS",
                Some(src_meta.len),
                None,
            )?;
            Ok(MoveResult::Moved {
                source_dir: from.parent().map(Path::to_path_buf),
            })
        }
        Err(e) => {
            journal.write_outcome(
                seq,
                JournalOp::Move,
                Some(from),
                Some(to),
                "FAIL",
                None,
                Some(&e.to_string()),
            )?;
            Ok(io_fail(&e, "remove_file source"))
        }
    }
}

/// Replace: sibling reservation, copy, verify, replacing rename over the target.
fn move_replace<F: FileSystem>(
    fs: &F,
    cfg: &Config,
    opts: &ExecOptions,
    journal: &mut Journal,
    from: &Path,
    to: &Path,
) -> Result<MoveResult, mm_core::error::FatalReason> {
    let src_meta = match fs.metadata(from) {
        Ok(m) => m,
        Err(e) => return Ok(io_fail(&e, "stat source")),
    };
    let src_hash = if cfg.moves.verify == VerifyMode::Hash {
        match fs.hash(from, &opts.cancel) {
            Ok(h) => Some(h),
            Err(e) => return Ok(io_fail(&e, "hash source")),
        }
    } else {
        None
    };
    let temp = sibling_temp(to);
    if let Some(parent) = temp.parent() {
        let _ = create_dir_retry(fs, parent);
    }
    let seq = journal.write_intent(JournalOp::Move, Some(from), Some(to))?;
    let mut handle = match fs.create_new(&temp) {
        Ok(h) => h,
        Err(e) => {
            journal.write_outcome(
                seq,
                JournalOp::Move,
                Some(from),
                Some(to),
                "FAIL",
                None,
                Some(&e.to_string()),
            )?;
            return Ok(io_fail(&e, "create_new replace temp"));
        }
    };
    let copy_res = fs.copy_into(from, &mut handle, &opts.cancel);
    drop(handle);
    if let Err(e) = copy_res {
        let _ = fs.remove_file(&temp);
        if e.kind() == io::ErrorKind::Interrupted {
            return Ok(MoveResult::Cancelled);
        }
        journal.write_outcome(
            seq,
            JournalOp::Move,
            Some(from),
            Some(to),
            "FAIL",
            None,
            Some(&e.to_string()),
        )?;
        return Ok(io_fail(&e, "copy_into replace"));
    }
    if let Some(parent) = to.parent() {
        let _ = fs.sync_dir(parent);
    }
    match verify_dest(
        fs,
        cfg,
        &temp,
        src_meta.len,
        src_hash.as_ref(),
        &opts.cancel,
    ) {
        Verify::Ok => {}
        Verify::Cancelled => {
            let _ = fs.remove_file(&temp);
            return Ok(MoveResult::Cancelled);
        }
        Verify::Fail(msg) => {
            let _ = fs.remove_file(&temp);
            journal.write_outcome(
                seq,
                JournalOp::Move,
                Some(from),
                Some(to),
                "FAIL",
                None,
                Some(&msg),
            )?;
            return Ok(fail(msg, None));
        }
    }
    if cfg.moves.preserve_mtime
        && let Some(mtime) = src_meta.modified
    {
        let _ = fs.set_mtime(&temp, mtime);
    }
    if let Err(e) = fs.rename_replace(&temp, to) {
        let _ = fs.remove_file(&temp);
        journal.write_outcome(
            seq,
            JournalOp::Move,
            Some(from),
            Some(to),
            "FAIL",
            None,
            Some(&e.to_string()),
        )?;
        return Ok(io_fail(&e, "rename_replace"));
    }
    match fs.remove_file(from) {
        Ok(()) => {
            journal.write_outcome(
                seq,
                JournalOp::Move,
                Some(from),
                Some(to),
                "SUCCESS",
                Some(src_meta.len),
                None,
            )?;
            Ok(MoveResult::Moved {
                source_dir: from.parent().map(Path::to_path_buf),
            })
        }
        Err(e) => {
            journal.write_outcome(
                seq,
                JournalOp::Move,
                Some(from),
                Some(to),
                "FAIL",
                None,
                Some(&e.to_string()),
            )?;
            Ok(io_fail(&e, "remove_file source"))
        }
    }
}

fn sibling_temp(dest: &Path) -> PathBuf {
    let parent = dest.parent().unwrap_or_else(|| Path::new(""));
    let stem = dest.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let ext = dest.extension().and_then(|e| e.to_str()).unwrap_or("");
    let id = Uuid::new_v4().simple();
    let name = if ext.is_empty() {
        format!("{stem}.mm-replace-{id}")
    } else {
        format!("{stem}.mm-replace-{id}.{ext}")
    };
    parent.join(name)
}

enum Verify {
    Ok,
    Fail(String),
    Cancelled,
}

fn verify_dest<F: FileSystem>(
    fs: &F,
    cfg: &Config,
    dest: &Path,
    expected_len: u64,
    expected_hash: Option<&mm_core::fs::Hash>,
    cancel: &CancelToken,
) -> Verify {
    let meta = match fs.metadata(dest) {
        Ok(m) => m,
        Err(_) => return Verify::Fail("destination missing after move".into()),
    };
    if meta.len != expected_len {
        return Verify::Fail(format!(
            "size mismatch: dest {} != source {expected_len}",
            meta.len
        ));
    }
    if cfg.moves.verify == VerifyMode::Hash {
        match fs.hash(dest, cancel) {
            Ok(h) => {
                if let Some(exp) = expected_hash
                    && h != *exp
                {
                    return Verify::Fail("hash mismatch".into());
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => return Verify::Cancelled,
            Err(e) => return Verify::Fail(format!("hash dest: {e}")),
        }
    }
    Verify::Ok
}

fn rename_directories<F: FileSystem>(
    fs: &F,
    plan: &Plan,
    journal: &mut Journal,
    report: &mut RunReport,
) -> Result<(), mm_core::error::FatalReason> {
    let mut renames = plan.dir_renames.clone();
    renames.sort_by_key(|r| std::cmp::Reverse(r.from.components().count()));
    for r in renames {
        let from_name = r.from.file_name();
        let to_name = r.to.file_name();
        if from_name == to_name {
            continue;
        }
        if fs.metadata(&r.from).is_err() {
            report.diagnostics.push(Diagnostic::warning(
                "exec",
                format!("dir rename source missing: {}", r.from.display()),
            ));
            continue;
        }
        let parent = r.from.parent().unwrap_or_else(|| Path::new(""));
        let temp = unique_temp_dir(parent);
        let seq = journal.write_intent(JournalOp::DirRename, Some(&r.from), Some(&r.to))?;
        if let Err(e) = fs.rename_no_replace(&r.from, &temp) {
            journal.write_outcome(
                seq,
                JournalOp::DirRename,
                Some(&r.from),
                Some(&r.to),
                "FAIL",
                None,
                Some(&e.to_string()),
            )?;
            report.diagnostics.push(Diagnostic::warning(
                "exec",
                format!("dir rename {} failed: {e}", r.from.display()),
            ));
            continue;
        }
        if let Err(e) = fs.rename_no_replace(&temp, &r.to) {
            let _ = fs.rename_no_replace(&temp, &r.from);
            journal.write_outcome(
                seq,
                JournalOp::DirRename,
                Some(&r.from),
                Some(&r.to),
                "FAIL",
                None,
                Some(&e.to_string()),
            )?;
            report.diagnostics.push(Diagnostic::warning(
                "exec",
                format!("dir rename to {} failed: {e}", r.to.display()),
            ));
            continue;
        }
        journal.write_outcome(
            seq,
            JournalOp::DirRename,
            Some(&r.from),
            Some(&r.to),
            "SUCCESS",
            None,
            None,
        )?;
    }
    Ok(())
}

fn unique_temp_dir(parent: &Path) -> PathBuf {
    parent.join(format!(".mm-rename-{}", Uuid::new_v4().simple()))
}

fn remove_empty_directories<F: FileSystem>(
    fs: &F,
    plan: &Plan,
    cfg: &Config,
    successful_sources: &HashSet<PathBuf>,
    journal: &mut Journal,
    report: &mut RunReport,
) -> Result<(), mm_core::error::FatalReason> {
    if !cfg.cleanup.remove_empty_dirs {
        return Ok(());
    }
    let created: HashSet<&PathBuf> = plan.dir_creates.iter().collect();
    let mut candidates: HashSet<PathBuf> = successful_sources.clone();
    for r in &plan.dir_removals {
        candidates.insert(r.path.clone());
    }
    let mut ordered: Vec<PathBuf> = candidates.into_iter().collect();
    ordered.sort_by_key(|p| std::cmp::Reverse(p.as_os_str().len()));

    for path in ordered {
        // 1. Phase 2 recorded it OR we successfully moved a file out of it.
        let recorded =
            plan.dir_removals.iter().any(|r| r.path == path) || successful_sources.contains(&path);
        if !recorded {
            continue;
        }
        // 2. Canonical form inside root.
        if !inside_root(&path, &plan.root) {
            continue;
        }
        // 4. Not root, not a directory this run created.
        if path == plan.root || created.contains(&path) {
            continue;
        }
        // 3. Genuinely empty (junk tolerance opt-in).
        match dir_empty_enough(fs, &path, cfg) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(e) => {
                report
                    .dirs_not_removable
                    .push((path.clone(), e.to_string()));
                continue;
            }
        }
        if cfg.cleanup.tolerate_junk
            && let Err(e) = delete_junk(fs, &path, cfg)
        {
            report
                .dirs_not_removable
                .push((path.clone(), e.to_string()));
            continue;
        }
        let seq = journal.write_intent(JournalOp::DirRemove, Some(&path), None)?;
        match fs.remove_dir(&path) {
            Ok(()) => {
                journal.write_outcome(
                    seq,
                    JournalOp::DirRemove,
                    Some(&path),
                    None,
                    "SUCCESS",
                    None,
                    None,
                )?;
                report.dirs_removed += 1;
            }
            Err(e) => {
                journal.write_outcome(
                    seq,
                    JournalOp::DirRemove,
                    Some(&path),
                    None,
                    "FAIL",
                    None,
                    Some(&e.to_string()),
                )?;
                report.dirs_not_removable.push((path, e.to_string()));
            }
        }
    }
    Ok(())
}

fn inside_root(path: &Path, root: &Path) -> bool {
    path.starts_with(root) && path != root
}

fn dir_empty_enough<F: FileSystem>(fs: &F, path: &Path, cfg: &Config) -> io::Result<bool> {
    if fs.is_dir_empty(path)? {
        return Ok(true);
    }
    if !cfg.cleanup.tolerate_junk {
        return Ok(false);
    }
    let iter = fs.read_dir(path)?;
    for entry in iter {
        let entry = entry?;
        let name = entry.file_name.to_string_lossy();
        if !cfg
            .cleanup
            .junk_names
            .iter()
            .any(|j| j.eq_ignore_ascii_case(&name))
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn delete_junk<F: FileSystem>(fs: &F, path: &Path, cfg: &Config) -> io::Result<()> {
    let iter = fs.read_dir(path)?;
    for entry in iter {
        let entry = entry?;
        let name = entry.file_name.to_string_lossy();
        if cfg
            .cleanup
            .junk_names
            .iter()
            .any(|j| j.eq_ignore_ascii_case(&name))
        {
            fs.remove_file(&entry.path)?;
        }
    }
    Ok(())
}

fn reclaim_this_run<F: FileSystem>(
    fs: &F,
    plan: &Plan,
    journal: &mut Journal,
    report: &mut RunReport,
) -> Result<(), mm_core::error::FatalReason> {
    let unmatched: Vec<_> = journal
        .unmatched_intents(Some(&plan.root))
        .into_iter()
        .filter(|e| e.run_id == plan.run_id && e.op == JournalOp::Move)
        .collect();
    for intent in unmatched {
        let Some(from) = intent.from.as_deref() else {
            continue;
        };
        let Some(to) = intent.to.as_deref() else {
            continue;
        };
        if fs.metadata(from).is_err() {
            continue;
        }
        if fs.metadata(to).is_err() {
            continue;
        }
        let seq = journal.write_intent(JournalOp::Reclaim, Some(from), Some(to))?;
        match fs.remove_file(to) {
            Ok(()) => {
                journal.write_outcome(
                    seq,
                    JournalOp::Reclaim,
                    Some(from),
                    Some(to),
                    "SUCCESS",
                    None,
                    None,
                )?;
                report.reservations_reclaimed += 1;
            }
            Err(e) => {
                journal.write_outcome(
                    seq,
                    JournalOp::Reclaim,
                    Some(from),
                    Some(to),
                    "FAIL",
                    None,
                    Some(&e.to_string()),
                )?;
                report.diagnostics.push(Diagnostic::warning(
                    "exec",
                    format!("reclaim {} failed: {e}", to.display()),
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mm_core::classify::MediaKind;
    use mm_core::config::StrategyConfig;
    use mm_core::fs::real::RealFs;
    use mm_core::plan::SkipReason;
    use tempfile::TempDir;

    fn cfg_with(strategy: StrategyConfig) -> Config {
        let mut c = Config::default();
        c.moves.no_replace_strategy = strategy;
        c
    }

    fn listing(root: &Path) -> Vec<String> {
        let mut out = Vec::new();
        for e in walkdir::WalkDir::new(root).sort_by_file_name() {
            let e = e.unwrap();
            if e.path() == root {
                continue;
            }
            let rel = e.path().strip_prefix(root).unwrap();
            let s = rel.to_string_lossy().replace('\\', "/");
            if e.file_type().is_dir() {
                out.push(format!("{s}/"));
            } else {
                out.push(s);
            }
        }
        out
    }

    fn exec_opts(journal: &Path) -> ExecOptions {
        ExecOptions {
            fail_fast: false,
            journal_dir: journal.to_path_buf(),
            cancel: CancelToken::new(),
        }
    }

    #[test]
    fn simple_movie_move_native_and_reserve() {
        for strategy in [StrategyConfig::Native, StrategyConfig::Reserve] {
            let media = TempDir::new().unwrap();
            let journal = TempDir::new().unwrap();
            std::fs::write(media.path().join("Inception.2010.mkv"), b"inception-bytes").unwrap();
            let cfg = cfg_with(strategy);
            let fs = RealFs::new();
            let planner = crate::Planner::new(&fs, media.path(), MediaKind::Movies, &cfg).unwrap();
            let plan = planner.plan(Default::default()).unwrap();
            let report = execute(&fs, &plan, &cfg, &exec_opts(journal.path()));
            assert!(report.fatal.is_none(), "{:?}", report.fatal);
            assert!(!report.cancelled);
            assert!(
                report.total(Outcome::Moved) >= 1,
                "strategy={strategy:?} counts={:?} diags={:?}",
                report.counts,
                report.diagnostics
            );
            let tree = listing(media.path());
            assert!(
                tree.iter().any(|p| p.ends_with("Inception (2010).mkv")),
                "strategy={strategy:?} tree={tree:?}"
            );
            assert!(!media.path().join("Inception.2010.mkv").exists());
        }
    }

    #[test]
    fn pre_existing_dest_is_not_overwritten() {
        for strategy in [StrategyConfig::Native, StrategyConfig::Reserve] {
            let media = TempDir::new().unwrap();
            let journal = TempDir::new().unwrap();
            std::fs::write(media.path().join("Inception.2010.mkv"), b"new-bytes").unwrap();
            let dest_dir = media.path().join("Inception (2010)");
            std::fs::create_dir(&dest_dir).unwrap();
            let dest = dest_dir.join("Inception (2010).mkv");
            std::fs::write(&dest, b"original-dest").unwrap();
            let cfg = cfg_with(strategy);
            let fs = RealFs::new();
            let planner = crate::Planner::new(&fs, media.path(), MediaKind::Movies, &cfg).unwrap();
            let plan = planner.plan(Default::default()).unwrap();
            let report = execute(&fs, &plan, &cfg, &exec_opts(journal.path()));
            assert_eq!(std::fs::read(&dest).unwrap(), b"original-dest");
            assert!(media.path().join("Inception.2010.mkv").exists());
            assert!(
                report.total(Outcome::Conflicted) >= 1
                    || plan
                        .items
                        .iter()
                        .any(|i| matches!(i.action, Action::Conflict { .. })),
                "strategy={strategy:?} report={:?} plan actions={:?}",
                report.counts,
                plan.items
                    .iter()
                    .map(|i| format!("{:?}", i.action))
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn skip_if_identical() {
        for strategy in [StrategyConfig::Native, StrategyConfig::Reserve] {
            let media = TempDir::new().unwrap();
            let journal = TempDir::new().unwrap();
            let bytes = b"same-bytes-here";
            std::fs::write(media.path().join("Inception.2010.mkv"), bytes).unwrap();
            let dest_dir = media.path().join("Inception (2010)");
            std::fs::create_dir(&dest_dir).unwrap();
            std::fs::write(dest_dir.join("Inception (2010).mkv"), bytes).unwrap();
            let mut cfg = cfg_with(strategy);
            cfg.conflict.policy = mm_core::config::ConflictPolicy::SkipIfIdentical;
            let fs = RealFs::new();
            let planner = crate::Planner::new(&fs, media.path(), MediaKind::Movies, &cfg).unwrap();
            let plan = planner.plan(Default::default()).unwrap();
            assert!(
                plan.items.iter().any(|i| matches!(
                    i.action,
                    Action::Skip {
                        reason: SkipReason::Identical
                    }
                )),
                "expected Skip Identical, got {:?}",
                plan.items
                    .iter()
                    .map(|i| format!("{:?}", i.action))
                    .collect::<Vec<_>>()
            );
            let report = execute(&fs, &plan, &cfg, &exec_opts(journal.path()));
            assert!(media.path().join("Inception.2010.mkv").exists());
            assert_eq!(
                std::fs::read(dest_dir.join("Inception (2010).mkv")).unwrap(),
                bytes
            );
            assert!(report.total(Outcome::Skipped) >= 1 || report.total(Outcome::NoOp) >= 1);
        }
    }
}
