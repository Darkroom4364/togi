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

        /// Resource profile for worker count and nested runner limits
        #[arg(long, value_enum)]
        profile: Option<crate::config::ResourceProfile>,

        /// Number of parallel mutation workers
        #[arg(short, long)]
        jobs: Option<usize>,

        /// Per-mutation timeout in seconds
        #[arg(short, long)]
        timeout: Option<u64>,

        /// Measure the unmutated test runtime and derive the mutation timeout
        #[arg(long, conflicts_with = "skip_baseline_timing")]
        calibrate_timeout: bool,

        /// Disable configured baseline timing calibration for this run
        #[arg(long)]
        skip_baseline_timing: bool,

        /// Multiplier applied to baseline runtime when calibrating timeout
        #[arg(long)]
        timeout_multiplier: Option<f64>,

        /// Constant seconds added when calibrating timeout
        #[arg(long)]
        timeout_slack: Option<u64>,

        /// Maximum mutations to run (0 = unlimited)
        #[arg(long)]
        max_per_run: Option<usize>,

        /// Stop scheduling new mutations after the first survived mutant
        #[arg(long, conflicts_with = "max_survivors")]
        first_survivor: bool,

        /// Stop scheduling new mutations after this many survived mutants
        #[arg(long)]
        max_survivors: Option<usize>,

        /// Use mutant schemata for supported languages
        #[arg(long)]
        schemata: bool,

        /// Disable mutant schemata and run every mutation individually
        #[arg(long, conflicts_with = "schemata")]
        no_schemata: bool,

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

        /// Fail if overall LCOV line coverage is below this percentage
        #[arg(long)]
        min_line_coverage: Option<f64>,

        /// Fail if changed-line coverage is below this percentage
        #[arg(long)]
        min_diff_coverage: Option<f64>,

        /// Fail if any changed line is uncovered in LCOV
        #[arg(long)]
        fail_on_uncovered_diff: bool,

        /// JSON source-line to test-name map for targeted test runs
        #[arg(long)]
        test_selection_file: Option<PathBuf>,

        /// Disable structured incremental history reuse
        #[arg(long)]
        no_incremental_history: bool,

        /// Re-run mutations even when cache or history has a matching result
        #[arg(long)]
        force_rerun: bool,

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
        /// Categories: binary, literal, boundary, removal, unary, loop, negate, return
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
    /// Generate a source-line to test-name map for targeted test runs
    TestMap {
        /// Project root to inspect
        #[arg(long)]
        path: Option<PathBuf>,

        /// Output JSON file
        #[arg(short, long, default_value = "coverage/test-selection.json")]
        output: PathBuf,
    },
    /// Generate a togi.toml config template
    Init,
    /// Delete the .togi-cache directory
    Clean,
    /// Explain a mutation from a JSON report
    Explain {
        /// Mutation id from `togi check --format json`
        mutant_id: u32,

        /// JSON report file produced by `togi check --format json`
        #[arg(short, long, default_value = "togi-report.json")]
        report: PathBuf,
    },
    /// List all available mutation operators
    ListOperators,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn check_all_conflicts_with_base() {
        let res = Cli::try_parse_from(["togi", "check", "--all", "--base", "main"]);
        assert!(res.is_err(), "--all and --base should conflict");
    }

    #[test]
    fn check_save_baseline_conflicts_with_check_baseline() {
        let args = ["togi", "check", "--save-baseline", "--check-baseline"];
        let res = Cli::try_parse_from(args);
        assert!(res.is_err(), "save/check baseline should conflict");
    }

    #[test]
    fn check_path_requires_all() {
        let res = Cli::try_parse_from(["togi", "check", "--path", "src/"]);
        assert!(res.is_err(), "--path should require --all");
    }

    #[test]
    fn shard_argument_is_accepted() {
        let cli = Cli::try_parse_from(["togi", "check", "--shard", "1/3"]).unwrap();
        match cli.command {
            Commands::Check { shard, .. } => {
                assert_eq!(shard.as_deref(), Some("1/3"));
            }
            _ => panic!("expected Check command"),
        }
    }

    #[test]
    fn test_map_output_argument_is_accepted() {
        let cli = Cli::try_parse_from(["togi", "test-map", "--output", "map.json"]).unwrap();
        match cli.command {
            Commands::TestMap { output, .. } => {
                assert_eq!(output, PathBuf::from("map.json"));
            }
            _ => panic!("expected TestMap command"),
        }
    }

    #[test]
    fn incremental_history_arguments_are_accepted() -> anyhow::Result<()> {
        let cli =
            Cli::try_parse_from(["togi", "check", "--no-incremental-history", "--force-rerun"])?;
        match cli.command {
            Commands::Check {
                no_incremental_history,
                force_rerun,
                ..
            } => {
                assert!(no_incremental_history);
                assert!(force_rerun);
            }
            _ => panic!("expected Check command"),
        }
        Ok(())
    }

    #[test]
    fn coverage_gate_arguments_are_accepted() -> anyhow::Result<()> {
        let cli = Cli::try_parse_from([
            "togi",
            "check",
            "--coverage-file",
            "coverage.lcov",
            "--min-line-coverage",
            "80",
            "--min-diff-coverage",
            "90",
            "--fail-on-uncovered-diff",
        ])?;
        match cli.command {
            Commands::Check {
                coverage_file,
                min_line_coverage,
                min_diff_coverage,
                fail_on_uncovered_diff,
                ..
            } => {
                assert_eq!(coverage_file, Some(PathBuf::from("coverage.lcov")));
                assert_eq!(min_line_coverage, Some(80.0));
                assert_eq!(min_diff_coverage, Some(90.0));
                assert!(fail_on_uncovered_diff);
            }
            _ => panic!("expected Check command"),
        }
        Ok(())
    }

    #[test]
    fn max_per_run_argument_is_accepted() {
        let cli = Cli::try_parse_from(["togi", "check", "--max-per-run", "25"]).unwrap();
        match cli.command {
            Commands::Check { max_per_run, .. } => {
                assert_eq!(max_per_run, Some(25));
            }
            _ => panic!("expected Check command"),
        }
    }

    #[test]
    fn profile_argument_is_accepted() {
        let cli = Cli::try_parse_from(["togi", "check", "--profile", "cool"])
            .expect("--profile cool should parse");
        match cli.command {
            Commands::Check { profile, .. } => {
                assert_eq!(profile, Some(crate::config::ResourceProfile::Cool));
            }
            _ => panic!("expected Check command"),
        }
    }

    #[test]
    fn timeout_calibration_arguments_are_accepted() -> Result<(), clap::Error> {
        let cli = Cli::try_parse_from([
            "togi",
            "check",
            "--calibrate-timeout",
            "--timeout-multiplier",
            "3.5",
            "--timeout-slack",
            "4",
        ])?;
        match cli.command {
            Commands::Check {
                calibrate_timeout,
                timeout_multiplier,
                timeout_slack,
                ..
            } => {
                assert!(calibrate_timeout);
                assert_eq!(timeout_multiplier, Some(3.5));
                assert_eq!(timeout_slack, Some(4));
            }
            _ => panic!("expected Check command"),
        }
        Ok(())
    }

    #[test]
    fn calibrate_timeout_conflicts_with_skip_baseline_timing() {
        let res = Cli::try_parse_from([
            "togi",
            "check",
            "--calibrate-timeout",
            "--skip-baseline-timing",
        ]);
        assert!(
            res.is_err(),
            "--calibrate-timeout and --skip-baseline-timing should conflict"
        );
    }

    #[test]
    fn first_survivor_argument_is_accepted() {
        let cli = Cli::try_parse_from(["togi", "check", "--first-survivor"])
            .expect("--first-survivor should parse");
        match cli.command {
            Commands::Check { first_survivor, .. } => {
                assert!(first_survivor);
            }
            _ => panic!("expected Check command"),
        }
    }

    #[test]
    fn max_survivors_argument_is_accepted() {
        let cli = Cli::try_parse_from(["togi", "check", "--max-survivors", "2"])
            .expect("--max-survivors should parse");
        match cli.command {
            Commands::Check { max_survivors, .. } => {
                assert_eq!(max_survivors, Some(2));
            }
            _ => panic!("expected Check command"),
        }
    }

    #[test]
    fn first_survivor_conflicts_with_max_survivors() {
        let res =
            Cli::try_parse_from(["togi", "check", "--first-survivor", "--max-survivors", "2"]);
        assert!(
            res.is_err(),
            "--first-survivor and --max-survivors should conflict"
        );
    }

    #[test]
    fn schemata_argument_is_accepted() {
        let cli = Cli::try_parse_from(["togi", "check", "--schemata"]).unwrap();
        match cli.command {
            Commands::Check { schemata, .. } => {
                assert!(schemata);
            }
            _ => panic!("expected Check command"),
        }
    }

    #[test]
    fn no_schemata_conflicts_with_schemata() {
        let res = Cli::try_parse_from(["togi", "check", "--schemata", "--no-schemata"]);
        assert!(res.is_err(), "--schemata and --no-schemata should conflict");
    }

    #[test]
    fn operators_flag_splits_on_comma() {
        let args = ["togi", "check", "--operators=binary,literal"];
        let cli = Cli::try_parse_from(args).unwrap();
        match cli.command {
            Commands::Check { operators, .. } => {
                let expected = vec!["binary".to_string(), "literal".to_string()];
                assert_eq!(operators, Some(expected));
            }
            _ => panic!("expected Check command"),
        }
    }

    #[test]
    fn operators_flag_accepts_negation() {
        let args = [
            "togi",
            "check",
            "--operators=-string_to_empty,-increment_numeric",
        ];
        let cli = Cli::try_parse_from(args).unwrap();
        match cli.command {
            Commands::Check { operators, .. } => {
                let expected = vec![
                    "-string_to_empty".to_string(),
                    "-increment_numeric".to_string(),
                ];
                assert_eq!(operators, Some(expected));
            }
            _ => panic!("expected Check command"),
        }
    }

    #[test]
    fn validate_patterns_rejects_unknown_operator() {
        let ops = crate::operators::all_operators();
        let res = crate::operators::validate_patterns(&ops, &["arith".into()]);
        let err = res.unwrap_err();
        assert!(err.contains("unknown operator or category 'arith'"));
    }

    #[test]
    fn validate_patterns_accepts_known_categories() {
        let ops = crate::operators::all_operators();
        let patterns = vec!["binary".to_string(), "literal".to_string()];
        assert!(crate::operators::validate_patterns(&ops, &patterns).is_ok());
    }
}
