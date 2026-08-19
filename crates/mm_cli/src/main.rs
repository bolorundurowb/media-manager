//! Command-line interface for media-manager (§8).
//!
//! Phase 2 implements `scan`, `plan`, and `organize --dry-run` for movies,
//! plus `config print`/`path`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use mm_core::MediaKind;
use mm_core::config::{Config, ConfigOverrides};
use mm_core::fs::real::RealFs;
use mm_engine::{Planner, render_json, render_text};

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

fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(&cli)?;

    let overrides = ConfigOverrides {
        workers: cli.workers,
        ..Default::default()
    };
    let cfg = Config::layered(None, &overrides)?;

    match cli.command {
        Commands::Scan { dir, r#type } => cmd_scan(&cfg, &dir, r#type.into(), cli.json),
        Commands::Plan {
            dir,
            r#type,
            output,
        } => cmd_plan(&cfg, &dir, r#type.into(), cli.json, output),
        Commands::Organize {
            dir,
            r#type,
            dry_run,
            yes: _,
            from_plan,
            strict: _,
            fail_fast: _,
        } => cmd_organize(&cfg, &dir, r#type.into(), dry_run, from_plan, cli.json),
        Commands::Config { action } => cmd_config(&cfg, action),
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

fn cmd_organize(
    _cfg: &Config,
    dir: &Path,
    kind: MediaKind,
    dry_run: bool,
    from_plan: Option<PathBuf>,
    json: bool,
) -> Result<()> {
    ensure_dir(dir)?;

    let plan = if let Some(path) = from_plan {
        let txt = std::fs::read_to_string(&path).context("read plan")?;
        serde_json::from_str(&txt).context("parse plan")?
    } else {
        let fs = RealFs::new();
        let planner = Planner::new(&fs, dir, kind, _cfg)?;
        planner.plan(mm_engine::PlanOptions { dry_run })?
    };

    if dry_run {
        if json {
            println!("{}", render_json(&plan));
        } else {
            println!("{}", render_text(&plan, true));
        }
        Ok(())
    } else {
        bail!("apply is not implemented until Phase 3; use --dry-run")
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
