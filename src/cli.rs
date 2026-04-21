use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "togi", about = "Fast, diff-targeted mutation testing")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Run mutation testing on the current diff
    Check {
        /// Mutate all supported files instead of just the diff
        #[arg(long, conflicts_with = "base")]
        all: bool,

        /// Base branch to diff against
        #[arg(long, default_value = "origin/main")]
        base: String,

        /// Path to config file
        #[arg(short, long)]
        config: Option<PathBuf>,

        /// Output format
        #[arg(short, long, default_value = "terminal")]
        format: String,

        /// Number of parallel jobs
        #[arg(short, long)]
        jobs: Option<usize>,

        /// Per-mutation timeout in seconds
        #[arg(short, long)]
        timeout: Option<u64>,

        /// Show mutations without running tests
        #[arg(long)]
        dry_run: bool,

        /// Show each mutation as it runs
        #[arg(long)]
        verbose: bool,

        /// Show test output for survived mutations
        #[arg(long)]
        show_output: bool,

        /// Override test command (e.g., 'go test ./...')
        #[arg(long)]
        test_cmd: Option<String>,

        /// LCOV coverage file \u2014 only mutate lines with test coverage
        #[arg(long)]
        coverage_file: Option<PathBuf>,
    },
    /// Generate a togi.toml config template
    Init,
}
