use clap::Parser;
use std::path::{Path, PathBuf};
use std::process;
use std::time::Duration;

use togi::{ChangedFile, Mutation, MutationReport};

struct CheckConfig {
    all: bool,
    base: Option<String>,
    config_path: Option<PathBuf>,
    output_format: String,
    jobs: Option<usize>,
    timeout: Option<u64>,
    dry_run: bool,
    verbose: bool,
    show_output: bool,
    test_cmd: Option<String>,
    coverage_file: Option<PathBuf>,
    build_cmd: Option<String>,
    fail_fast: bool,
}

#[tokio::main]
async fn main() {
    let cli = togi::cli::Cli::parse();

    match cli.command {
        togi::cli::Commands::Check {
            all,
            base,
            config,
            format,
            jobs,
            timeout,
            dry_run,
            verbose,
            show_output,
            test_cmd,
            coverage_file,
            build_cmd,
            fail_fast,
        } => {
            let cfg = CheckConfig {
                all,
                base,
                config_path: config,
                output_format: format,
                jobs,
                timeout,
                dry_run,
                verbose,
                show_output,
                test_cmd,
                coverage_file,
                build_cmd,
                fail_fast,
            };
            if let Err(e) = run_check(cfg).await {
                eprintln!("Error: {e:#}");
                process::exit(2);
            }
        }
        togi::cli::Commands::Init => {
            let path = std::path::Path::new("togi.toml");
            if path.exists() {
                eprintln!("togi.toml already exists");
                process::exit(2);
            }
            if let Err(e) = togi::config::Config::write_template(path) {
                eprintln!("Error: {e}");
                process::exit(2);
            }
            println!("Created togi.toml");
        }
    }
}

async fn run_check(cfg: CheckConfig) -> anyhow::Result<()> {
    let all = cfg.all;
    let dry_run = cfg.dry_run;
    let verbose = cfg.verbose;
    let show_output = cfg.show_output;
    let output_format = cfg.output_format.clone();

    let (mut config, fail_fast) = resolve_config(cfg)?;
    let project_root = get_project_root()?;
    let _lock = togi::lock::acquire(&project_root)?;

    config.resolve_test_command(&project_root);
    config.resolve_build_command(&project_root);

    if fail_fast {
        let args = togi::config::failfast_args(&config.test.command);
        config.test.command.extend(args);
        for lang_config in config.test.languages.values_mut() {
            let args = togi::config::failfast_args(&lang_config.command);
            lang_config.command.extend(args);
        }
    }

    let all_langs = togi::languages::all();
    let known: Vec<&str> = all_langs.iter().map(|l| l.name()).collect();
    config.warn_unknown_languages(&known);

    let changed_files = collect_files(&config, all, dry_run, &project_root)?;
    if changed_files.is_empty() {
        return Ok(());
    }

    let mutations = generate_mutations(&changed_files, &config, &project_root)?;
    let mutations = filter_mutations(mutations, &config, &project_root)?;

    if mutations.is_empty() {
        println!(
            "No mutations generated. This can happen if the changed files are in an unsupported language.\nSupported: Go (.go), Rust (.rs), Python (.py), TypeScript (.ts/.tsx)"
        );
        return Ok(());
    }

    if dry_run {
        print_dry_run(&mutations);
        return Ok(());
    }

    eprintln!("Running {} mutations...", mutations.len());

    let report = execute(mutations, config, project_root, verbose, show_output).await;

    togi::report::print_report(&report, &output_format)?;

    if report.survived > 0 {
        drop(_lock);
        process::exit(1);
    }

    Ok(())
}

fn resolve_config(cfg: CheckConfig) -> anyhow::Result<(togi::config::Config, bool)> {
    let mut config = togi::config::Config::load(cfg.config_path.as_deref())?;
    let has_custom_test_cmd = cfg.test_cmd.is_some();

    if let Some(b) = cfg.base {
        config.diff.base = b;
    }
    if let Some(j) = cfg.jobs {
        config.test.jobs = j;
    }
    if let Some(t) = cfg.timeout {
        config.test.timeout = t;
    }
    if let Some(cmd) = cfg.test_cmd {
        config.test.command =
            shell_words::split(&cmd).map_err(|e| anyhow::anyhow!("bad --test-cmd: {e}"))?;
    }
    if let Some(path) = cfg.coverage_file {
        config.mutations.coverage_file = Some(path);
    }
    if let Some(cmd) = cfg.build_cmd {
        config.test.build_command =
            shell_words::split(&cmd).map_err(|e| anyhow::anyhow!("bad --build-cmd: {e}"))?;
    }

    let fail_fast = cfg.fail_fast && !has_custom_test_cmd;
    Ok((config, fail_fast))
}

/// Collects files to mutate. Returns an empty vec with user-facing messages
/// when there's nothing to do.
fn collect_files(
    config: &togi::config::Config,
    all: bool,
    dry_run: bool,
    project_root: &Path,
) -> anyhow::Result<Vec<ChangedFile>> {
    if all {
        let files = togi::diff::collect_all_supported_files(project_root)?;
        if files.is_empty() {
            println!("No supported source files found. Nothing to mutate.");
            return Ok(vec![]);
        }
        println!("Scanning all {} supported files...", files.len());
        return Ok(files);
    }

    let diff_output = get_git_diff(&config.diff.base)?;
    if diff_output.is_empty() {
        println!(
            "No changes found in diff against `{}`. Nothing to mutate.",
            config.diff.base
        );
        if dry_run {
            println!("Hint: use --all --dry-run to preview mutations across all files.");
        }
        return Ok(vec![]);
    }

    let files = togi::diff::parse_diff(&diff_output);
    if files.is_empty() {
        println!("No added/modified lines found. Nothing to mutate.");
        return Ok(vec![]);
    }
    Ok(files)
}

fn generate_mutations(
    changed_files: &[ChangedFile],
    config: &togi::config::Config,
    project_root: &Path,
) -> anyhow::Result<Vec<Mutation>> {
    let generation_limit = if config.mutations.coverage_file.is_some() {
        usize::MAX
    } else if !config.test.build_command.is_empty() {
        config.mutations.max_per_run * 2
    } else {
        config.mutations.max_per_run
    };
    togi::mutator::generate_mutations(changed_files, project_root, generation_limit)
}

fn filter_mutations(
    mutations: Vec<Mutation>,
    config: &togi::config::Config,
    project_root: &Path,
) -> anyhow::Result<Vec<Mutation>> {
    let mut mutations = if let Some(ref cov_path) = config.mutations.coverage_file {
        let resolved_cov_path = if std::path::Path::new(cov_path).is_relative() {
            project_root.join(cov_path)
        } else {
            PathBuf::from(cov_path)
        };
        let cov_content = std::fs::read_to_string(&resolved_cov_path).map_err(|e| {
            anyhow::anyhow!(
                "Could not read coverage file {}: {e}",
                resolved_cov_path.display()
            )
        })?;
        let coverage = togi::coverage::parse_lcov(&cov_content, project_root);
        let before = mutations.len();
        let filtered = togi::coverage::filter_by_coverage(mutations, &coverage, project_root);
        if before > filtered.len() {
            eprintln!(
                "Coverage filter: {} of {} mutations on covered lines",
                filtered.len(),
                before
            );
        }
        filtered
    } else {
        mutations
    };
    if mutations.len() > config.mutations.max_per_run {
        eprintln!(
            "warning: mutation count capped at max_per_run ({}). Increase in togi.toml or use --dry-run to preview.",
            config.mutations.max_per_run
        );
        mutations.truncate(config.mutations.max_per_run);
    }

    Ok(mutations)
}

fn print_dry_run(mutations: &[Mutation]) {
    println!(
        "Dry run — {} mutations would be generated:",
        mutations.len()
    );
    for m in mutations {
        println!(
            "  [{}] {}:{} — {}: {} → {}",
            m.id + 1,
            m.file.display(),
            m.line,
            m.operator,
            m.original,
            m.replacement
        );
    }
}

async fn execute(
    mutations: Vec<Mutation>,
    config: togi::config::Config,
    project_root: PathBuf,
    verbose: bool,
    show_output: bool,
) -> MutationReport {
    let language_commands: std::collections::HashMap<String, Vec<String>> = config
        .test
        .languages
        .into_iter()
        .map(|(k, v)| (k, v.command))
        .collect();

    let runner = togi::runner::TestRunner {
        command: config.test.command,
        language_commands,
        build_command: config.test.build_command,
        timeout: Duration::from_secs(config.test.timeout),
        parallelism: config.test.jobs,
        project_root,
        verbose,
        show_output,
        max_tested: Some(config.mutations.max_per_run),
    };

    runner.run(mutations).await
}

fn get_project_root() -> anyhow::Result<PathBuf> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()?;
    if !output.status.success() {
        anyhow::bail!("Not a git repository. Run togi from inside a git project.");
    }
    let path = String::from_utf8(output.stdout)?.trim().to_string();
    Ok(PathBuf::from(path))
}

fn get_git_diff(base: &str) -> anyhow::Result<String> {
    let output = std::process::Command::new("git")
        .args(["diff", base])
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "Could not diff against '{base}'. Is the branch up to date? Try running 'git fetch' first.\n\nDetails: {stderr}"
        );
    }
    Ok(String::from_utf8(output.stdout)?)
}
