//! Human-readable plan / result reporting.

use crate::exec::ExecReport;
use crate::multi::LogEvent;
use crate::plan::Plan;

pub fn print_plan(plan: &Plan, apply: bool, merged: usize) {
    if apply {
        println!("APPLY — writing changes into the library root");
    } else {
        println!("DRY-RUN — no files will be changed (pass --apply to write)");
    }
    println!();

    if !plan.dirs.is_empty() {
        println!("Directories:");
        for dir in &plan.dirs {
            println!("  CREATE  {}", dir.display());
        }
        println!();
    }

    if !plan.moves.is_empty() {
        println!("Moves:");
        for mv in &plan.moves {
            println!("  MOVE    {} -> {}", mv.from.display(), mv.to.display());
        }
        println!();
    }

    if !plan.skips.is_empty() {
        println!("Skipped:");
        for s in &plan.skips {
            println!("  SKIP    {} ({})", s.path.display(), s.reason);
        }
        println!();
    }

    println!(
        "Summary: {} move(s), {} dir(s), {} skipped, {} merged group(s)",
        plan.moves.len(),
        plan.dirs.len(),
        plan.skips.len(),
        merged
    );
}

pub fn print_exec(report: &ExecReport) {
    println!();
    println!(
        "Applied: {} moved, {} dirs created, {} empty folders removed, {} failed",
        report.moved,
        report.created_dirs,
        report.removed_dirs,
        report.failed.len()
    );
    for f in &report.failed {
        println!("  FAIL    {} ({})", f.path.display(), f.reason);
    }
    if report.cancelled {
        println!("  CANCELLED — stopped early; nothing already moved was rolled back");
    }
}

pub fn print_event(event: &LogEvent) {
    match event {
        LogEvent::Scanning => println!("SCANNING"),
        LogEvent::JobStarted(path) => println!("JOB     {} (started)", path.display()),
        LogEvent::JobFinished(path) => println!("JOB     {} (finished)", path.display()),
        LogEvent::CreateDir(path) => println!("CREATE  {}", path.display()),
        LogEvent::PlannedMove { from, to } | LogEvent::Moved { from, to } => {
            println!("MOVE    {} -> {}", from.display(), to.display());
        }
        LogEvent::Skipped { path, reason } => {
            println!("SKIP    {} ({reason})", path.display());
        }
        LogEvent::Failed { path, reason } => {
            println!("FAIL    {} ({reason})", path.display());
        }
        LogEvent::Finished {
            moved,
            merged,
            skipped,
            failed,
            cancelled,
        } => println!(
            "Summary: {moved} moved, {merged} merged, {skipped} skipped, \
             {failed} failed, cancelled={cancelled}"
        ),
    }
}
