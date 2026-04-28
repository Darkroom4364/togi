use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Terminal,
    Json,
    Github,
    Html,
}

#[derive(Parser)]
#[command(name = "togi", version, about = "Fast, diff-targeted mutation testing")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
pub enum Commands {
    /// Run mutation testing on the current diff
    Check {
        /// Mutate all supported files instead of just the diff
        #[arg(long, conflicts_with = "base")]
        all: bool,

        /// Limit --all to files under this path (repeatable)
        #[arg(long, requires = "all")]
        path: Vec<PathBuf>,

        /// Base branch to diff against
        #[arg(long)]
        base: Option<String>,

        /// Path to config file
        #[arg(short, long)]
        config: Option<PathBuf>,

        /// Output format
        #[arg(short, long, default_value = "terminal")]
        format: OutputFormat,

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

        /// Override build check command (e.g., 'cargo check')
        #[arg(long)]
        build_cmd: Option<String>,

        /// Stop test suite on first failure per mutation (faster kills)
        #[arg(long)]
        fail_fast: bool,

        /// Disable built-in exclusion of test files, migrations, seeds, etc.
        #[arg(long)]
        no_skip_defaults: bool,

        /// Filter operators: category or id, prefix with - to exclude
        /// e.g. --operators=-string_to_empty,-increment_numeric
        /// Categories: binary, literal, boundary, removal, unary, negate, return
        #[arg(long, value_delimiter = ',')]
        operators: Option<Vec<String>>,

        /// Fail if mutation score is below this percentage (e.g. --fail-under 80)
        #[arg(long)]
        fail_under: Option<f64>,

        /// Run only a subset of mutations for parallel CI (e.g. --shard 1/4)
        #[arg(long)]
        shard: Option<String>,

        /// Save current results as baseline for future regression checks
        #[arg(long, conflicts_with = "check_baseline")]
        save_baseline: bool,

        /// Compare results against saved baseline, exit non-zero on regression
        #[arg(long, conflicts_with = "save_baseline")]
        check_baseline: bool,

        /// Write a PR comment as markdown to a file (e.g. --pr-comment togi-pr-comment.md)
        #[arg(long)]
        pr_comment: Option<PathBuf>,
    },
    /// Generate a togi.toml config template
    Init,
    /// Delete the .togi-cache directory
    Clean,
    /// List all available mutation operators
    ListOperators,
}
