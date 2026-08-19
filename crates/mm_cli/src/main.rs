//! Command-line interface for media-manager (§8).
//!
//! Phase 3: `scan`, `plan`, `organize` (dry-run / apply), `verify`, `gc`,
//! plus `config print`/`path`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use mm_core::MediaKind;
use mm_core::config::{Config, ConfigOverrides};
use mm_core::error::{RunMode, RunReport};
use mm_core::fs::CancelToken;
use mm_core::fs::real::RealFs;
use mm_engine::{
    ExecOptions, GcOptions, Planner, execute, gc, render_json, render_text, report_from_plan,
};

#[derive(Parser)]
#[command(name = "media-manager")]
#[command(about = "Organise media libraries without guessing")]
struct Cli {
    #[arg(short, long, help = "Path to a TOML config file")]
    config: Option<PathBuf>,

    #[arg(long, help = "Log level")]
    log_level: Option<String>,

    #[arg(long, help = "Log file")]
    log_file: Option<PathBuf>,

    #[arg(long, help = "Emit JSON instead of human-readable output")]
    json: bool,

    #[arg(long, help = "Number of worker threads")]
    workers: Option<u16>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Scan a directory and list classified files.
    Scan {
        dir: PathBuf,
        #[arg(short, long, value_enum)]
        #[arg(required = true)]
        r#type: MediaTypeArg,
    },
    /// Plan what changes would be made.
    Plan {
        dir: PathBuf,
        #[arg(short, long, value_enum)]
        #[arg(required = true)]
        r#type: MediaTypeArg,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Apply a plan (or preview with --dry-run).
    Organize {
        dir: PathBuf,
        #[arg(short, long, value_enum)]
        #[arg(required = true)]
        r#type: MediaTypeArg,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        from_plan: Option<PathBuf>,
        #[arg(long)]
        strict: bool,
        #[arg(long)]
        fail_fast: bool,
    },
    /// Plan only: report pending work (exit 10 if changes are needed).
    Verify {
        dir: PathBuf,
        #[arg(short, long, value_enum)]
        #[arg(required = true)]
        r#type: MediaTypeArg,
    },
    /// Reclaim unmatched reservation leftovers for a root.
    Gc {
        dir: PathBuf,
        #[arg(long)]
        yes: bool,
    },
    /// Print or locate the resolved config.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
}

#[derive(Subcommand)]
enum ConfigAction {
    Print,
    Path,
}

#[derive(Clone, Copy, ValueEnum)]
enum MediaTypeArg {
    Movies,
}

impl From<MediaTypeArg> for MediaKind {
    fn from(a: MediaTypeArg) -> Self {
        match a {
            MediaTypeArg::Movies => MediaKind::Movies,
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{e:#}");
            ExitCode::from(64)
        }
    }
}

fn run() -> Result<ExitCode> {
    let cli = Cli::parse();
    init_tracing(&cli)?;

    let overrides = ConfigOverrides {
        workers: cli.workers,
        ..Default::default()
    };

    match cli.command {
        Commands::Scan { dir, r#type } => {
            let cfg = Config::layered(Some(&dir), &overrides)?;
            cmd_scan(&cfg, &dir, r#type.into(), cli.json)?;
            Ok(ExitCode::SUCCESS)
        }
        Commands::Plan {
            dir,
            r#type,
            output,
        } => {
            let cfg = Config::layered(Some(&dir), &overrides)?;
            cmd_plan(&cfg, &dir, r#type.into(), cli.json, output)?;
            Ok(ExitCode::SUCCESS)
        }
        Commands::Organize {
            dir,
            r#type,
            dry_run,
            yes,
            from_plan,
            strict,
            fail_fast,
        } => {
            let cfg = Config::layered(Some(&dir), &overrides)?;
            cmd_organize(
                &cfg,
                &dir,
                r#type.into(),
                dry_run,
                yes,
                from_plan,
                strict,
                fail_fast,
                cli.json,
            )
        }
        Commands::Verify { dir, r#type } => {
            let cfg = Config::layered(Some(&dir), &overrides)?;
            cmd_verify(&cfg, &dir, r#type.into(), cli.json)
        }
        Commands::Gc { dir, yes } => {
            let cfg = Config::layered(Some(&dir), &overrides)?;
            let _ = cfg;
            cmd_gc(&dir, yes, cli.json)
        }
        Commands::Config { action } => {
            let cfg = Config::layered(None, &overrides)?;
            cmd_config(&cfg, action)?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn init_tracing(cli: &Cli) -> Result<()> {
    let level = cli.log_level.as_deref().unwrap_or("info");
    let filter = tracing_subscriber::EnvFilter::try_new(level).context("invalid log level")?;
    let sub = tracing_subscriber::fmt().with_env_filter(filter);
    if let Some(path) = &cli.log_file {
        let file = std::fs::File::create(path).context("cannot create log file")?;
        sub.with_writer(std::sync::Arc::new(file)).init();
    } else {
        sub.init();
    }
    Ok(())
}

fn cmd_scan(_cfg: &Config, dir: &Path, kind: MediaKind, json: bool) -> Result<()> {
    ensure_dir(dir)?;
    let fs = RealFs::new();
    let planner = Planner::new(&fs, dir, kind, _cfg)?;
    let plan = planner.plan(Default::default())?;
    if json {
        println!("{}", render_json(&plan));
    } else {
        for item in &plan.items {
            println!(
                "{} {:?} {}",
                item.class.as_str(),
                item.action,
                item.relative.display()
            );
        }
    }
    Ok(())
}

fn cmd_plan(
    _cfg: &Config,
    dir: &Path,
    kind: MediaKind,
    json: bool,
    output: Option<PathBuf>,
) -> Result<()> {
    ensure_dir(dir)?;
    let fs = RealFs::new();
    let planner = Planner::new(&fs, dir, kind, _cfg)?;
    let plan = planner.plan(Default::default())?;

    if let Some(path) = output {
        std::fs::write(&path, render_json(&plan)).context("write plan")?;
    }

    if json {
        println!("{}", render_json(&plan));
    } else {
        println!("{}", render_text(&plan, true));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_organize(
    cfg: &Config,
    dir: &Path,
    kind: MediaKind,
    dry_run: bool,
    yes: bool,
    from_plan: Option<PathBuf>,
    strict: bool,
    fail_fast: bool,
    json: bool,
) -> Result<ExitCode> {
    ensure_dir(dir)?;

    let plan = if let Some(path) = from_plan {
        let txt = std::fs::read_to_string(&path).context("read plan")?;
        serde_json::from_str(&txt).context("parse plan")?
    } else {
        let fs = RealFs::new();
        let planner = Planner::new(&fs, dir, kind, cfg)?;
        planner.plan(mm_engine::PlanOptions { dry_run })?
    };

    if dry_run {
        if json {
            println!("{}", render_json(&plan));
        } else {
            println!("{}", render_text(&plan, true));
        }
        return Ok(ExitCode::SUCCESS);
    }

    if !yes {
        if json {
            println!("{}", render_json(&plan));
        } else {
            println!("{}", render_text(&plan, true));
            eprintln!("refusing to apply without --yes (pass --dry-run to preview only)");
        }
        return Ok(ExitCode::SUCCESS);
    }

    let journal_dir = journal_dir()?;
    let fs = RealFs::new();
    let opts = ExecOptions {
        fail_fast,
        journal_dir,
        cancel: CancelToken::new(),
    };
    let report = execute(&fs, &plan, cfg, &opts);
    print_report(&report, json);
    Ok(ExitCode::from(report.exit_code(strict)))
}

fn cmd_verify(cfg: &Config, dir: &Path, kind: MediaKind, json: bool) -> Result<ExitCode> {
    ensure_dir(dir)?;
    let fs = RealFs::new();
    let planner = Planner::new(&fs, dir, kind, cfg)?;
    let plan = planner.plan(Default::default())?;
    if json {
        println!("{}", render_json(&plan));
    } else {
        println!("{}", render_text(&plan, true));
    }
    let report = report_from_plan(&plan, RunMode::Verify);
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_report(&report, false);
    }
    Ok(ExitCode::from(report.exit_code(false)))
}

fn cmd_gc(dir: &Path, yes: bool, json: bool) -> Result<ExitCode> {
    ensure_dir(dir)?;
    let journal_dir = journal_dir()?;
    let fs = RealFs::new();
    if !yes {
        let path = journal_dir.join("journal.jsonl");
        if path.exists() {
            let j = mm_engine::Journal::open(&path)
                .map_err(|e| anyhow::anyhow!("journal unreadable: {e:?}"))?;
            let unmatched = j.unmatched_intents(Some(dir));
            if unmatched.is_empty() {
                eprintln!("no unmatched reservations for {}", dir.display());
            } else {
                eprintln!("unmatched reservations (pass --yes to delete dest leftovers):");
                for e in &unmatched {
                    eprintln!(
                        "  seq={} {} -> {}",
                        e.seq,
                        e.from
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default(),
                        e.to.as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default()
                    );
                }
            }
        } else {
            eprintln!("no journal at {}", path.display());
        }
        return Ok(ExitCode::SUCCESS);
    }
    let report = gc(
        &fs,
        dir,
        &GcOptions {
            journal_dir,
            yes: true,
        },
    );
    print_report(&report, json);
    Ok(ExitCode::from(report.exit_code(false)))
}

fn journal_dir() -> Result<PathBuf> {
    let dir =
        mm_core::config::data_dir().unwrap_or_else(|| std::env::temp_dir().join("media-manager"));
    std::fs::create_dir_all(&dir).context("create journal dir")?;
    Ok(dir)
}

fn print_report(report: &RunReport, json: bool) {
    if json {
        match serde_json::to_string_pretty(report) {
            Ok(s) => println!("{s}"),
            Err(e) => eprintln!("failed to serialise report: {e}"),
        }
        return;
    }
    println!(
        "run {}  {:?}  {}  {}ms",
        report.run_id,
        report.mode,
        report.root.display(),
        report.duration.as_millis()
    );
    for (outcome, n) in &report.counts {
        println!("  {:<12} {n}", outcome.as_str());
    }
    if !report.pending.is_empty() {
        println!("pending:");
        for (outcome, n) in &report.pending {
            println!("  {:<12} {n}", outcome.as_str());
        }
    }
    println!(
        "dirs_removed={}  reservations_reclaimed={}",
        report.dirs_removed, report.reservations_reclaimed
    );
    if !report.dirs_not_removable.is_empty() {
        println!("dirs_not_removable:");
        for (p, why) in &report.dirs_not_removable {
            println!("  {} ({why})", p.display());
        }
    }
    for d in &report.diagnostics {
        println!("  [{:?}] {}: {}", d.severity, d.stage, d.message);
    }
    if let Some(fatal) = &report.fatal {
        println!("fatal: {fatal:?}");
    }
    if report.cancelled {
        println!("cancelled");
    }
}

fn cmd_config(cfg: &Config, action: ConfigAction) -> Result<()> {
    match action {
        ConfigAction::Print => {
            let printed = mm_core::config::config_print(cfg);
            for (k, v) in printed {
                println!("{k:24} {v}");
            }
        }
        ConfigAction::Path => {
            if let Some(dir) = mm_core::config::data_dir() {
                println!("{}", dir.display());
            }
        }
    }
    Ok(())
}

fn ensure_dir(dir: &std::path::Path) -> Result<()> {
    if !dir.is_dir() {
        bail!("{} is not a directory", dir.display());
    }
    Ok(())
}
