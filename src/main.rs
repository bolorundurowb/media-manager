use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use media_manager::{CancelToken, LibraryKind, Options};
use tracing::level_filters::LevelFilter;

#[derive(Copy, Clone, Debug, ValueEnum)]
enum TypeArg {
    #[value(alias = "movie")]
    Movies,
    #[value(alias = "show", alias = "shows")]
    Tv,
}

impl From<TypeArg> for LibraryKind {
    fn from(value: TypeArg) -> Self {
        match value {
            TypeArg::Movies => LibraryKind::Movies,
            TypeArg::Tv => LibraryKind::Tv,
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "media-manager",
    about = "Rewrite a movie or TV library in place into a Jellyfin-friendly layout.",
    version
)]
struct Cli {
    /// Library root. Renamed files are written back into this directory.
    root: PathBuf,

    /// How to interpret every folder under the root. Required; there is no auto-detection.
    #[arg(long = "type", value_enum)]
    library_type: TypeArg,

    /// Perform moves. Without this flag the tool only prints a plan.
    #[arg(long)]
    apply: bool,

    /// Verbose logging.
    #[arg(long, short)]
    verbose: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let filter = if cli.verbose {
        LevelFilter::DEBUG
    } else {
        LevelFilter::INFO
    };
    tracing_subscriber::fmt()
        .with_max_level(filter)
        .with_target(false)
        .init();

    let cancel = CancelToken::new();
    {
        let cancel = cancel.clone();
        if let Err(err) = ctrlc::set_handler(move || {
            eprintln!("\nreceived interrupt; finishing any move already in flight and stopping...");
            cancel.cancel();
        }) {
            eprintln!("warning: could not install Ctrl+C handler: {err}");
        }
    }

    match media_manager::run(Options {
        root: cli.root,
        kind: cli.library_type.into(),
        apply: cli.apply,
        cancel,
    }) {
        Ok(summary) => {
            if summary.cancelled {
                ExitCode::from(130)
            } else if summary.failed > 0 {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_type_is_rejected() {
        let result = Cli::try_parse_from(["media-manager", "/tmp/library"]);
        assert!(result.is_err(), "--type should be required");
    }

    #[test]
    fn type_aliases_are_accepted() {
        let cli = Cli::try_parse_from(["media-manager", "/tmp/library", "--type", "movie"])
            .expect("movie alias should parse");
        assert!(matches!(cli.library_type, TypeArg::Movies));

        let cli = Cli::try_parse_from(["media-manager", "/tmp/library", "--type", "shows"])
            .expect("shows alias should parse");
        assert!(matches!(cli.library_type, TypeArg::Tv));
    }

    #[test]
    fn dry_run_is_the_default() {
        let cli = Cli::try_parse_from(["media-manager", "/tmp/library", "--type", "movies"])
            .expect("valid args should parse");
        assert!(!cli.apply);
    }
}
