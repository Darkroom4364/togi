use clap::Parser;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use togi::{ChangedFile, Mutation, MutationReport};

struct CheckConfig {
    all: bool,
    paths: Vec<PathBuf>,
    base: Option<String>,
    config_path: Option<PathBuf>,
    output_format: togi::cli::OutputFormat,
    jobs: Option<usize>,
    timeout: Option<u64>,
    dry_run: bool,
    verbose: bool,
    show_output: bool,
    test_cmd: Option<String>,
    coverage_file: Option<PathBuf>,
    build_cmd: Option<String>,
    fail_fast: bool,
    no_skip_defaults: bool,
    operators: Option<Vec<String>>,
    fail_under: Option<f64>,
    shard: Option<String>,
    save_baseline: bool,
    check_baseline: bool,
    pr_comment: Option<PathBuf>,
}

#[tokio::main]
async fn main() {
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_handler = cancelled.clone();

    // First Ctrl+C sets the flag so the runner can stop gracefully.
    // Second Ctrl+C force-exits for impatient users.
    ctrlc::set_handler(move || {
        if cancelled_handler.swap(true, Ordering::SeqCst) {
            eprintln!("\nForce exit — files may need manual restoration (check git status)");
            process::exit(130);
        }
        eprintln!("\nInterrupted — finishing current mutation and cleaning up...");
    })
    .expect("failed to set Ctrl+C handler");

    let cli = togi::cli::Cli::parse();

    match cli.command {
        togi::cli::Commands::Check {
            all,
            path,
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
            no_skip_defaults,
            operators,
            fail_under,
            shard,
            save_baseline,
            check_baseline,
            pr_comment,
        } => {
            let cfg = CheckConfig {
                all,
                paths: path,
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
                no_skip_defaults,
                operators,
                fail_under,
                shard,
                save_baseline,
                check_baseline,
                pr_comment,
            };
            if let Err(e) = run_check(cfg, cancelled).await {
                eprintln!("Error: {e:#}");
                process::exit(2);
            }
        }
        togi::cli::Commands::Clean => {
            let project_root = get_project_root().unwrap_or_else(|e| {
                eprintln!("Error: {e:#}");
                process::exit(2);
            });
            match togi::cache::clear(&project_root) {
                Ok(()) => println!("Cache cleared."),
                Err(e) => {
                    eprintln!("Error clearing cache: {e}");
                    process::exit(2);
                }
            }
        }
        togi::cli::Commands::ListOperators => {
            print_operators();
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
            println!("Created togi.toml (auto-detected from project)");
        }
    }
}

fn print_operators() {
    let ops = togi::operators::all_operators();
    let mut by_category: std::collections::BTreeMap<&str, Vec<(&str, &str)>> =
        std::collections::BTreeMap::new();
    for op in &ops {
        let cat = togi::operators::operator_category(op.id());
        by_category
            .entry(cat)
            .or_default()
            .push((op.id(), op.description()));
    }
    for (category, ops) in &by_category {
        println!("{category}:");
        for (id, desc) in ops {
            println!("  {id:<30} {desc}");
        }
        println!();
    }
}

async fn run_check(cfg: CheckConfig, cancelled: Arc<AtomicBool>) -> anyhow::Result<()> {
    let all = cfg.all;
    let paths = cfg.paths.clone();
    let dry_run = cfg.dry_run;
    let verbose = cfg.verbose;
    let show_output = cfg.show_output;
    let output_format = cfg.output_format;
    let fail_under = cfg.fail_under;
    let shard = cfg.shard.as_deref().map(parse_shard).transpose()?;
    let save_baseline = cfg.save_baseline;
    let check_baseline = cfg.check_baseline;
    let pr_comment = cfg.pr_comment.clone();

    let (mut config, fail_fast, has_explicit_build_cmd) = resolve_config(cfg)?;
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

    let changed_files = collect_files(&config, all, &paths, dry_run, &project_root)?;
    if changed_files.is_empty() {
        return Ok(());
    }

    let mutations = generate_mutations(&changed_files, &config, &project_root)?;
    let mut mutations = filter_mutations(mutations, &config, &project_root)?;

    if let Some((k, n)) = shard {
        let total = mutations.len();
        mutations.retain(|m| m.id as usize % n == k - 1);
        eprintln!("Shard {k}/{n}: {} of {total} mutations", mutations.len());
    }

    if mutations.is_empty() {
        println!("No mutations generated. Possible causes:");
        println!("  - Changed files are in an unsupported language");
        println!("  - All mutable nodes were filtered out (test files, noisy patterns)");
        println!("  - max_per_run or max_per_file is set to 0 in togi.toml");
        return Ok(());
    }

    if dry_run {
        print_dry_run(&mutations);
        return Ok(());
    }

    eprintln!("Running {} mutations...", mutations.len());

    let project_root_ref = project_root.clone();
    let report = execute(
        mutations,
        config,
        project_root,
        verbose,
        show_output,
        has_explicit_build_cmd,
        cancelled,
    )
    .await;

    togi::report::print_report(&report, output_format)?;

    let current = togi::baseline::from_report(&report, &project_root_ref);
    let mut should_fail = false;

    if save_baseline {
        togi::baseline::save_baseline(&current, &project_root_ref)?;
        eprintln!("Baseline saved to .togi-baseline");
    }

    let mut baseline_score: Option<f64> = None;
    if check_baseline {
        if let Some(baseline) = togi::baseline::load_baseline(&project_root_ref) {
            baseline_score = Some(baseline.killed as f64 / baseline.total.max(1) as f64 * 100.0);
            if togi::baseline::check_regression(&current, &baseline) {
                let regressions = togi::baseline::per_file_regressions(&current, &baseline);
                eprintln!("Mutation score regression detected!");
                for r in &regressions {
                    eprintln!(
                        "  {} — {:.1}% → {:.1}%",
                        r.file, r.baseline_pct, r.current_pct
                    );
                }
                should_fail = true;
            }
        } else {
            eprintln!("warning: no baseline found — use --save-baseline first");
        }
    }

    if let Some(ref path) = pr_comment {
        togi::report::write_pr_comment(&report, path, baseline_score)?;
        eprintln!("PR comment written to {}", path.display());
    }

    let score = togi::report::mutation_score(&report);
    if should_fail {
        drop(_lock);
        process::exit(1);
    } else if let Some(threshold) = fail_under {
        if score < threshold {
            eprintln!("Mutation score {score:.1}% is below --fail-under threshold {threshold:.1}%");
            drop(_lock);
            process::exit(1);
        }
    } else if report.survived > 0 {
        drop(_lock);
        process::exit(1);
    }

    Ok(())
}

fn resolve_config(cfg: CheckConfig) -> anyhow::Result<(togi::config::Config, bool, bool)> {
    let mut config = togi::config::Config::load(cfg.config_path.as_deref())?;
    let has_custom_test_cmd = cfg.test_cmd.is_some();
    let has_cli_build_cmd = cfg.build_cmd.is_some();

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

    if cfg.no_skip_defaults {
        config.mutations.skip_noisy_files = false;
    }
    if let Some(ops) = cfg.operators {
        config.mutations.operators = ops;
    }

    let has_explicit_build_cmd = has_cli_build_cmd || !config.test.build_command.is_empty();
    let fail_fast = cfg.fail_fast && !has_custom_test_cmd;
    Ok((config, fail_fast, has_explicit_build_cmd))
}

/// Parse a shard spec like "1/4" into (k, n) where k is 1-indexed.
fn parse_shard(s: &str) -> anyhow::Result<(usize, usize)> {
    let parts: Vec<&str> = s.split('/').collect();
    if parts.len() != 2 {
        anyhow::bail!("invalid --shard format '{s}', expected k/n (e.g. 1/4)");
    }
    let k: usize = parts[0]
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid shard index '{}'", parts[0]))?;
    let n: usize = parts[1]
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid shard count '{}'", parts[1]))?;
    if n == 0 {
        anyhow::bail!("invalid shard count: n must be >= 1 for --shard {s}");
    }
    if k == 0 || k > n {
        anyhow::bail!("--shard {s}: k must be 1..={n}");
    }
    Ok((k, n))
}

/// Collects files to mutate. Returns an empty vec with user-facing messages
/// when there's nothing to do.
fn collect_files(
    config: &togi::config::Config,
    all: bool,
    paths: &[PathBuf],
    dry_run: bool,
    project_root: &Path,
) -> anyhow::Result<Vec<ChangedFile>> {
    let skip_noisy = config.mutations.skip_noisy_files;
    let exclude_globs = &config.mutations.exclude_paths;

    if all {
        let mut files =
            togi::diff::collect_all_supported_files(project_root, skip_noisy, exclude_globs)?;
        if !paths.is_empty() {
            files.retain(|f| paths.iter().any(|p| f.path.starts_with(p)));
        }
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

    let mut files = togi::diff::parse_diff(&diff_output);
    if skip_noisy {
        files.retain(|f| !togi::diff::is_noisy_file(&f.path));
    }
    files.retain(|f| !togi::diff::matches_user_excludes(&f.path, exclude_globs));
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
    let max = if config.mutations.max_per_run == 0 {
        usize::MAX
    } else {
        config.mutations.max_per_run
    };
    let generation_limit = if config.mutations.coverage_file.is_some() {
        usize::MAX
    } else if !config.test.build_command.is_empty() {
        max.saturating_mul(2)
    } else {
        max
    };
    togi::mutator::generate_mutations(
        changed_files,
        project_root,
        generation_limit,
        config.mutations.max_per_file,
        &config.mutations.operators,
    )
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
        match std::fs::read_to_string(&resolved_cov_path) {
            Ok(cov_content) => {
                let coverage = togi::coverage::parse_lcov(&cov_content, project_root);
                let before = mutations.len();
                let filtered =
                    togi::coverage::filter_by_coverage(mutations, &coverage, project_root);
                if before > filtered.len() {
                    eprintln!(
                        "Coverage filter: {} of {} mutations on covered lines",
                        filtered.len(),
                        before
                    );
                }
                filtered
            }
            Err(e) => {
                eprintln!(
                    "warning: could not read coverage file {}: {e} — running all mutations",
                    resolved_cov_path.display()
                );
                mutations
            }
        }
    } else {
        mutations
    };
    if config.mutations.max_per_run > 0 && mutations.len() > config.mutations.max_per_run {
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
    build_command_explicit: bool,
    cancelled: Arc<AtomicBool>,
) -> MutationReport {
    let mut language_commands: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    let mut language_timeouts: std::collections::HashMap<String, Duration> =
        std::collections::HashMap::new();
    for (lang, lang_config) in config.test.languages {
        language_commands.insert(lang.clone(), lang_config.command);
        if let Some(t) = lang_config.timeout {
            language_timeouts.insert(lang, Duration::from_secs(t));
        }
    }

    let runner = togi::runner::TestRunner {
        commands: togi::runner::CommandConfig {
            command: config.test.command,
            language_commands,
            build_command: if build_command_explicit {
                config.test.build_command
            } else {
                vec![]
            },
            build_command_explicit,
            timeout: Duration::from_secs(config.test.timeout),
            language_timeouts,
        },
        parallelism: config.test.jobs,
        project_root,
        verbose,
        show_output,
        max_tested: if config.mutations.max_per_run == 0 {
            None
        } else {
            Some(config.mutations.max_per_run)
        },
        env: std::collections::HashMap::new(),
        cancelled,
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
