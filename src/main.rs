use anyhow::Context;
use clap::Parser;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Deserialize;
use togi::{BaselineTiming, ChangedFile, Mutation};

struct ExecuteOptions {
    verbose: bool,
    show_output: bool,
    build_command_explicit: bool,
    force_default_command: bool,
    force_default_timeout: bool,
    early_stop: togi::runner::EarlyStopConfig,
    env: HashMap<String, String>,
    force_rerun: bool,
    cancelled: Arc<AtomicBool>,
}

#[derive(Debug)]
struct ResolvedCheckConfig {
    config: togi::config::Config,
    fail_fast: bool,
    has_explicit_build_cmd: bool,
    has_custom_test_cmd: bool,
    has_cli_timeout: bool,
    profile: Option<togi::config::ResourceProfile>,
}

fn main() {
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
        togi::cli::Commands::Check(cfg) => {
            if let Err(e) = run_check(cfg, cancelled) {
                eprintln!("Error: {e:#}");
                process::exit(2);
            }
        }
        togi::cli::Commands::TestMap { path, output } => {
            if let Err(e) = run_test_map(path, output, &cancelled) {
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
        togi::cli::Commands::Explain { mutant_id, report } => {
            if let Err(e) = explain_mutation(mutant_id, &report) {
                eprintln!("Error: {e:#}");
                process::exit(2);
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

#[derive(Deserialize)]
struct ExplainReport {
    test_command: Option<Vec<String>>,
    build_command: Option<Vec<String>>,
    mutations: Vec<ExplainMutation>,
}

#[derive(Deserialize)]
struct ExplainMutation {
    id: u32,
    file: String,
    line: usize,
    operator: String,
    description: String,
    result: String,
    original: Option<String>,
    replacement: Option<String>,
    diff: Option<String>,
}

fn explain_mutation(mutant_id: u32, report_path: &Path) -> anyhow::Result<()> {
    let content = std::fs::read_to_string(report_path)
        .map_err(|e| anyhow::anyhow!("could not read {}: {e}", report_path.display()))?;
    let report: ExplainReport = serde_json::from_str(&content)
        .map_err(|e| anyhow::anyhow!("could not parse {} as JSON: {e}", report_path.display()))?;
    let mutation = report
        .mutations
        .iter()
        .find(|m| m.id == mutant_id)
        .ok_or_else(|| anyhow::anyhow!("mutation id {mutant_id} not found in report"))?;

    println!("Mutation #{}", mutation.id);
    println!(
        "{}:{} — {} ({})",
        mutation.file, mutation.line, mutation.operator, mutation.result
    );
    println!("{}", mutation.description);

    if let (Some(original), Some(replacement)) = (&mutation.original, &mutation.replacement) {
        println!();
        println!("Change: {original} -> {replacement}");
    }

    if let Some(diff) = &mutation.diff {
        println!();
        println!("{diff}");
    }

    println!();
    if let Some(command) = report.test_command.as_ref().filter(|cmd| !cmd.is_empty()) {
        println!("Test command: {}", serde_json::to_string(command)?);
    }
    if let Some(command) = report.build_command.as_ref().filter(|cmd| !cmd.is_empty()) {
        println!("Build check: {}", serde_json::to_string(command)?);
    }

    match mutation.result.as_str() {
        "survived" => {
            println!("Why it survived:");
            println!("  The configured test command completed successfully with this mutation.");
            println!(
                "  Add an assertion that distinguishes the original behavior from the mutated one."
            );
        }
        "killed" => {
            println!("Why it was killed:");
            println!(
                "  The configured test command failed with this mutation, so existing tests caught it."
            );
        }
        "timeout" => {
            println!("Why it timed out:");
            println!("  The configured test command exceeded the mutation timeout.");
        }
        "build_error" => {
            println!("Why it was not testable:");
            println!("  The mutation made the project fail its build check.");
        }
        other => {
            println!("Result: {other}");
        }
    }

    Ok(())
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

fn run_check(cfg: togi::cli::CheckArgs, cancelled: Arc<AtomicBool>) -> anyhow::Result<()> {
    let all = cfg.all;
    let paths = cfg.path.clone();
    let dry_run = cfg.dry_run;
    let verbose = cfg.verbose;
    let show_output = cfg.show_output;
    let output_format = cfg.format;
    let fail_under = cfg.fail_under;
    let max_survivors = match (cfg.first_survivor, cfg.max_survivors) {
        (true, _) => Some(1),
        (false, Some(0)) => anyhow::bail!("--max-survivors must be greater than 0"),
        (false, value) => value,
    };
    let early_stop = togi::runner::EarlyStopConfig {
        max_survivors,
        fail_under,
    };
    let shard = cfg.shard.as_deref().map(parse_shard).transpose()?;
    let save_baseline = cfg.save_baseline;
    let check_baseline = cfg.check_baseline;
    let pr_comment = cfg.pr_comment.clone();
    let force_rerun = cfg.force_rerun;

    let resolved = resolve_config(cfg)?;
    let ResolvedCheckConfig {
        mut config,
        fail_fast,
        has_explicit_build_cmd,
        has_custom_test_cmd,
        has_cli_timeout,
        profile,
    } = resolved;
    let project_root = get_project_root()?;
    let _lock = togi::lock::acquire(&project_root)?;

    config.resolve_test_command(&project_root);
    config.resolve_build_command(&project_root);
    warn_if_resource_oversubscribed(config.test.jobs);
    let profile_env = if has_custom_test_cmd {
        HashMap::new()
    } else {
        profile
            .map(|profile| resource_profile_env(profile, &config))
            .unwrap_or_default()
    };

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

    let coverage_gate_active = config.mutations.min_line_coverage.is_some()
        || config.mutations.min_diff_coverage.is_some()
        || config.mutations.fail_on_uncovered_diff;
    let coverage_stats = resolve_coverage_stats(&config, &project_root, coverage_gate_active)?;

    if coverage_gate_active {
        let stats = coverage_stats
            .as_ref()
            .expect("coverage stats should exist when coverage gates are enabled");
        let mut coverage_report =
            togi::coverage::diff_coverage_report(stats, &changed_files, &project_root);
        coverage_report.line_coverage.threshold = config.mutations.min_line_coverage;
        coverage_report.diff_coverage.threshold = config.mutations.min_diff_coverage;
        coverage_report.fail_on_uncovered_diff = config.mutations.fail_on_uncovered_diff;
        if !coverage_report.passes() {
            togi::report::print_coverage_gate_report(&coverage_report, output_format)?;
            exit_with(_lock, 1);
        }
    }

    let mutations = generate_mutations(&changed_files, &config, &project_root)?;
    let mut mutations =
        filter_mutations(mutations, &config, &project_root, coverage_stats.as_ref())?;

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

    let baseline_timing = if config.test.calibrate_timeout {
        eprintln!("Measuring baseline test runtime...");
        let measurement = togi::runner::measure_baseline_timing(
            &project_root,
            togi::runner::BaselineTimingConfig {
                test_command: &config.test.command,
                build_command: &config.test.build_command,
                sandbox_command: &config.test.sandbox_command,
                build_command_explicit: has_explicit_build_cmd,
                timeout: baseline_measurement_timeout(config.test.timeout),
                env: &profile_env,
                cancelled: &cancelled,
                respect_workspace_ignores: config.mutations.respect_workspace_ignores,
            },
        )?;
        let timeout_secs = calibrated_timeout_seconds(
            measurement.build_duration,
            measurement.test_duration,
            config.test.timeout_multiplier,
            config.test.timeout_slack,
        );
        config.test.timeout = timeout_secs;
        let timing = BaselineTiming {
            build_command: if has_explicit_build_cmd {
                config.test.build_command.clone()
            } else {
                vec![]
            },
            build_duration: measurement.build_duration,
            test_command: config.test.command.clone(),
            test_duration: measurement.test_duration,
            calibrated_timeout: Duration::from_secs(timeout_secs),
        };
        eprintln!("{}", baseline_timing_summary(&timing));
        Some(timing)
    } else {
        None
    };

    eprintln!("Running {} mutations...", mutations.len());

    let project_root_ref = project_root.clone();
    let outcome = execute(
        mutations,
        config,
        project_root,
        ExecuteOptions {
            verbose,
            show_output,
            build_command_explicit: has_explicit_build_cmd,
            force_default_command: has_custom_test_cmd,
            force_default_timeout: has_cli_timeout,
            early_stop,
            env: profile_env,
            force_rerun,
            cancelled,
        },
    );

    let mut report = outcome.report;
    report.baseline_timing = baseline_timing;
    togi::report::print_report(&report, output_format)?;

    if outcome.cancelled {
        eprintln!("Interrupted; skipping baseline and PR comment updates.");
        exit_with(_lock, 130);
    }

    let current = togi::baseline::from_report(&report, &project_root_ref);
    let mut should_fail = false;

    let partial_report = report.total < report.planned_total;

    if partial_report && (save_baseline || check_baseline) {
        eprintln!("Partial early-stop report; skipping baseline save/check.");
    } else if save_baseline {
        togi::baseline::save_baseline(&current, &project_root_ref)?;
        eprintln!("Baseline saved to .togi-baseline");
    }

    let mut baseline_score: Option<f64> = None;
    let mut loaded_baseline = false;
    if check_baseline && !partial_report {
        if let Some(baseline) = togi::baseline::load_baseline(&project_root_ref)? {
            loaded_baseline = true;
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
        exit_with(_lock, 1);
    } else if let Some(threshold) = fail_under {
        if score < threshold {
            eprintln!("Mutation score {score:.1}% is below --fail-under threshold {threshold:.1}%");
            exit_with(_lock, 1);
        }
    } else if report.survived > 0 && !loaded_baseline {
        exit_with(_lock, 1);
    }

    Ok(())
}

fn exit_with(lock: togi::lock::LockGuard, code: i32) -> ! {
    drop(lock);
    process::exit(code);
}

fn resolve_config(cfg: togi::cli::CheckArgs) -> anyhow::Result<ResolvedCheckConfig> {
    let mut config = togi::config::Config::load(cfg.config.as_deref())?;
    let has_custom_test_cmd = cfg.test_cmd.is_some();
    let has_cli_build_cmd = cfg.build_cmd.is_some();
    let has_cli_timeout = cfg.timeout.is_some();
    let profile = cfg.profile.or(config.test.profile);

    if let Some(b) = cfg.base {
        config.diff.base = b;
    }
    if let Some(profile) = profile {
        if cfg.jobs.is_none() && !config.test.jobs_was_explicit() {
            config.test.jobs = profile.default_jobs();
        }
    }
    if let Some(j) = cfg.jobs {
        config.test.jobs = j;
    }
    if let Some(t) = cfg.timeout {
        config.test.timeout = t;
    }
    if cfg.calibrate_timeout {
        config.test.calibrate_timeout = true;
    }
    if cfg.skip_baseline_timing {
        config.test.calibrate_timeout = false;
    }
    if let Some(multiplier) = cfg.timeout_multiplier {
        config.test.timeout_multiplier = multiplier;
    }
    if let Some(slack) = cfg.timeout_slack {
        config.test.timeout_slack = slack;
    }
    validate_timeout_calibration(config.test.timeout_multiplier)?;
    if has_cli_timeout && config.test.calibrate_timeout {
        eprintln!("warning: --timeout overrides baseline timing calibration for this run");
        config.test.calibrate_timeout = false;
    }
    if let Some(max) = cfg.max_per_run {
        config.mutations.max_per_run = max;
    }
    if cfg.schemata {
        config.mutations.schemata = true;
    }
    if cfg.no_schemata {
        config.mutations.schemata = false;
    }
    if let Some(cmd) = cfg.test_cmd {
        config.test.command =
            shell_words::split(&cmd).map_err(|e| anyhow::anyhow!("bad --test-cmd: {e}"))?;
    }
    if let Some(path) = cfg.coverage_file {
        config.mutations.coverage_file = Some(path);
    }
    if let Some(cmd) = cfg.coverage_cmd {
        config.mutations.coverage_command =
            shell_words::split(&cmd).map_err(|e| anyhow::anyhow!("bad --coverage-cmd: {e}"))?;
    }
    if let Some(value) = cfg.min_line_coverage {
        validate_coverage_percentage(value, "--min-line-coverage")?;
        config.mutations.min_line_coverage = Some(value);
    }
    if let Some(value) = cfg.min_diff_coverage {
        validate_coverage_percentage(value, "--min-diff-coverage")?;
        config.mutations.min_diff_coverage = Some(value);
    }
    if cfg.fail_on_uncovered_diff {
        config.mutations.fail_on_uncovered_diff = true;
    }
    if let Some(path) = cfg.test_selection_file {
        config.mutations.test_selection_file = Some(path);
    }
    if cfg.no_incremental_history {
        config.mutations.incremental_history = false;
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

    if !config.mutations.coverage_command.is_empty() && config.mutations.coverage_file.is_none() {
        anyhow::bail!(
            "coverage collection command requires an LCOV output path; set [mutations] coverage_file or --coverage-file"
        );
    }
    if (config.mutations.min_line_coverage.is_some()
        || config.mutations.min_diff_coverage.is_some()
        || config.mutations.fail_on_uncovered_diff)
        && config.mutations.coverage_file.is_none()
        && config.mutations.coverage_command.is_empty()
    {
        anyhow::bail!(
            "coverage gates require a coverage source; set [mutations] coverage_file, coverage_command, or use the corresponding CLI flags"
        );
    }

    let has_explicit_build_cmd = has_cli_build_cmd || !config.test.build_command.is_empty();
    let profile_fail_fast = profile.is_some_and(|profile| profile.default_fail_fast());
    let requested_fail_fast = cfg.fail_fast || profile_fail_fast;
    let fail_fast = requested_fail_fast && !has_custom_test_cmd;
    if cfg.fail_fast && has_custom_test_cmd {
        eprintln!(
            "warning: --fail-fast is ignored when --test-cmd is set; include fail-fast flags in the custom command"
        );
    } else if profile_fail_fast && has_custom_test_cmd {
        eprintln!(
            "warning: --profile cool fail-fast default is ignored when --test-cmd is set; include fail-fast flags in the custom command"
        );
    }
    Ok(ResolvedCheckConfig {
        config,
        fail_fast,
        has_explicit_build_cmd,
        has_custom_test_cmd,
        has_cli_timeout,
        profile,
    })
}

fn validate_coverage_percentage(value: f64, flag: &str) -> anyhow::Result<()> {
    if !value.is_finite() || !(0.0..=100.0).contains(&value) {
        anyhow::bail!("{flag} must be a finite percentage between 0 and 100");
    }
    Ok(())
}

fn resolve_coverage_stats(
    config: &togi::config::Config,
    project_root: &Path,
    coverage_gate_active: bool,
) -> anyhow::Result<Option<togi::coverage::CoverageStats>> {
    if !config.mutations.coverage_command.is_empty() {
        let coverage_file = config
            .mutations
            .coverage_file
            .as_ref()
            .expect("coverage command should be validated to require coverage_file");
        let resolved_cov_path = resolve_coverage_path(coverage_file, project_root);
        run_coverage_command(
            &config.mutations.coverage_command,
            &resolved_cov_path,
            project_root,
        )?;
    }

    let Some(cov_path) = config.mutations.coverage_file.as_ref() else {
        return Ok(None);
    };
    let resolved_cov_path = resolve_coverage_path(cov_path, project_root);
    let coverage_required = coverage_gate_active || !config.mutations.coverage_command.is_empty();

    match std::fs::read_to_string(&resolved_cov_path) {
        Ok(cov_content) => Ok(Some(togi::coverage::parse_lcov_stats(
            &cov_content,
            project_root,
        ))),
        Err(e) => {
            if coverage_required {
                return Err(anyhow::anyhow!(
                    "could not read coverage file {}: {e}",
                    resolved_cov_path.display()
                ));
            }
            eprintln!(
                "warning: could not read coverage file {}: {e} — running all mutations",
                resolved_cov_path.display()
            );
            Ok(None)
        }
    }
}

fn resolve_coverage_path(path: &Path, project_root: &Path) -> PathBuf {
    if path.is_relative() {
        project_root.join(path)
    } else {
        path.to_path_buf()
    }
}

fn run_coverage_command(
    command: &[String],
    coverage_file: &Path,
    project_root: &Path,
) -> anyhow::Result<()> {
    let Some(program) = command.first() else {
        anyhow::bail!("coverage command is empty");
    };
    if let Some(parent) = coverage_file.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "could not create parent directory for coverage file {}",
                coverage_file.display()
            )
        })?;
    }

    // foxguard: ignore[rs/no-command-injection]
    // The coverage command is explicit user configuration and is executed
    // directly as argv without a shell.
    let output = std::process::Command::new(program)
        .args(&command[1..])
        .current_dir(project_root)
        .env("TOGI_COVERAGE_FILE", coverage_file)
        .output()
        .with_context(|| format!("failed to run coverage command `{}`", command.join(" ")))?;

    if !output.status.success() {
        anyhow::bail!(
            "coverage command `{}` failed with status {}.\nstdout:\n{}\nstderr:\n{}",
            command.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    if !coverage_file.is_file() {
        anyhow::bail!(
            "coverage command `{}` completed but did not produce {}",
            command.join(" "),
            coverage_file.display()
        );
    }

    Ok(())
}

fn warn_if_resource_oversubscribed(jobs: usize) {
    if let Ok(available) = std::thread::available_parallelism() {
        if jobs > available.get() {
            eprintln!(
                "warning: {jobs} togi jobs exceed available parallelism ({}); test runners may oversubscribe CPUs",
                available.get()
            );
        }
    }
}

fn validate_timeout_calibration(multiplier: f64) -> anyhow::Result<()> {
    if !multiplier.is_finite() || multiplier <= 0.0 {
        anyhow::bail!("timeout_multiplier must be a positive finite number");
    }
    Ok(())
}

fn calibrated_timeout_seconds(
    build_duration: Option<Duration>,
    test_duration: Duration,
    multiplier: f64,
    slack: u64,
) -> u64 {
    let baseline = build_duration
        .filter(|duration| *duration > test_duration)
        .unwrap_or(test_duration);
    let seconds = baseline.as_secs_f64() * multiplier + slack as f64;
    seconds.ceil().max(1.0).min(u64::MAX as f64) as u64
}

fn baseline_timing_summary(timing: &BaselineTiming) -> String {
    let build = timing
        .build_duration
        .map(|duration| format!(", build {:.2}s", duration.as_secs_f64()))
        .unwrap_or_default();
    format!(
        "Baseline timing: test {:.2}s{build}; mutation timeout {:.2}s",
        timing.test_duration.as_secs_f64(),
        timing.calibrated_timeout.as_secs_f64()
    )
}

fn baseline_measurement_timeout(configured_timeout_seconds: u64) -> Duration {
    Duration::from_secs(configured_timeout_seconds.saturating_mul(10).max(60))
}

fn resource_profile_env(
    profile: togi::config::ResourceProfile,
    config: &togi::config::Config,
) -> HashMap<String, String> {
    let mut commands: Vec<&[String]> = Vec::new();
    commands.push(&config.test.command);
    commands.extend(
        config
            .test
            .languages
            .values()
            .map(|lang_config| lang_config.command.as_slice()),
    );
    commands.extend(config.projects.values().filter_map(|project| {
        project
            .test
            .as_ref()
            .and_then(|test| test.command.as_deref())
    }));
    resource_profile_env_for_commands(profile, &commands, |name| std::env::var_os(name).is_some())
}

fn resource_profile_env_for_commands(
    profile: togi::config::ResourceProfile,
    commands: &[&[String]],
    env_exists: impl Fn(&str) -> bool,
) -> HashMap<String, String> {
    let mut env = HashMap::new();
    if profile != togi::config::ResourceProfile::Cool {
        return env;
    }

    let has_runner = |runner: &str| {
        commands
            .iter()
            .filter_map(|command| command.first())
            .any(|program| program == runner)
    };
    let mut set_if_missing = |key: &str, value: &str| {
        if !env_exists(key) {
            env.insert(key.to_string(), value.to_string());
        }
    };

    if has_runner("cargo") {
        set_if_missing("CARGO_BUILD_JOBS", "1");
        set_if_missing("RUST_TEST_THREADS", "1");
    }
    if has_runner("go") {
        set_if_missing("GOMAXPROCS", "1");
    }
    if has_runner("pytest") {
        set_if_missing("PYTEST_XDIST_AUTO_NUM_WORKERS", "1");
    }

    env
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
    coverage_stats: Option<&togi::coverage::CoverageStats>,
) -> anyhow::Result<Vec<Mutation>> {
    let mut mutations = if let Some(coverage) = coverage_stats {
        let before = mutations.len();
        let filtered =
            togi::coverage::filter_by_coverage(mutations, &coverage.covered_lines, project_root);
        if before > filtered.len() {
            eprintln!(
                "Coverage filter: {} of {} mutations on covered lines",
                filtered.len(),
                before
            );
        }
        filtered
    } else if let Some(ref cov_path) = config.mutations.coverage_file {
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

fn execute(
    mutations: Vec<Mutation>,
    config: togi::config::Config,
    project_root: PathBuf,
    options: ExecuteOptions,
) -> togi::runner::RunOutcome {
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
    let project_commands = config
        .projects
        .into_values()
        .map(|project| {
            let (command, timeout) = project
                .test
                .map(|test| (test.command, test.timeout))
                .unwrap_or((None, None));
            togi::runner::ProjectCommandConfig {
                path: project.path,
                command,
                timeout: timeout.map(Duration::from_secs),
            }
        })
        .collect();
    let test_selection = load_test_selection(
        config.mutations.test_selection_file.as_deref(),
        &project_root,
    );
    let use_schemata = config.mutations.schemata;

    let runner = togi::runner::TestRunner {
        commands: togi::runner::CommandConfig {
            command: config.test.command,
            sandbox_command: config.test.sandbox_command,
            force_default_command: options.force_default_command,
            force_default_timeout: options.force_default_timeout,
            project_commands,
            language_commands,
            build_command: if options.build_command_explicit {
                config.test.build_command
            } else {
                vec![]
            },
            build_command_explicit: options.build_command_explicit,
            timeout: Duration::from_secs(config.test.timeout),
            language_timeouts,
            test_selection,
        },
        parallelism: config.test.jobs,
        project_root,
        verbose: options.verbose,
        show_output: options.show_output,
        max_tested: if config.mutations.max_per_run == 0 {
            None
        } else {
            Some(config.mutations.max_per_run)
        },
        early_stop: options.early_stop,
        respect_workspace_ignores: config.mutations.respect_workspace_ignores,
        env: options.env,
        incremental_history: config.mutations.incremental_history,
        force_rerun: options.force_rerun,
        cancelled: options.cancelled,
    };

    if use_schemata {
        runner.run_with_schemata(mutations)
    } else {
        runner.run(mutations)
    }
}

fn load_test_selection(
    path: Option<&Path>,
    project_root: &Path,
) -> Option<togi::runner::TestSelectionConfig> {
    let path = path?;
    let resolved_path = if path.is_relative() {
        project_root.join(path)
    } else {
        path.to_path_buf()
    };

    match std::fs::read_to_string(&resolved_path)
        .with_context(|| {
            format!(
                "could not read test selection file {}",
                resolved_path.display()
            )
        })
        .and_then(|content| parse_test_selection_json(&content, project_root))
    {
        Ok(selection) => Some(selection),
        Err(e) => {
            eprintln!("warning: {e:#} — running full test commands");
            None
        }
    }
}

fn parse_test_selection_json(
    content: &str,
    project_root: &Path,
) -> anyhow::Result<togi::runner::TestSelectionConfig> {
    let raw: std::collections::HashMap<
        String,
        std::collections::HashMap<String, Vec<RawSelectedTest>>,
    > = serde_json::from_str(content).context("could not parse test selection JSON")?;
    let mut selection = togi::runner::TestSelectionConfig::new();

    for (file, lines) in raw {
        for (line, raw_tests) in lines {
            let line = line
                .parse::<usize>()
                .with_context(|| format!("invalid line number '{line}' for {file}"))?;
            if line == 0 {
                anyhow::bail!("invalid line number '0' for {file}");
            }
            let tests = raw_tests
                .into_iter()
                .map(RawSelectedTest::into_selected)
                .collect::<anyhow::Result<Vec<_>>>()
                .with_context(|| format!("invalid test selection entry for {file}:{line}"))?;
            selection.insert_tests(project_root, Path::new(&file), line, tests);
        }
    }

    Ok(selection)
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawSelectedTest {
    Name(String),
    Timed {
        name: String,
        #[serde(default)]
        duration_ms: Option<u64>,
    },
}

impl RawSelectedTest {
    fn into_selected(self) -> anyhow::Result<togi::runner::SelectedTest> {
        match self {
            Self::Name(name) => selected_test(name, None),
            Self::Timed { name, duration_ms } => selected_test(name, duration_ms),
        }
    }
}

fn selected_test(
    name: String,
    duration_ms: Option<u64>,
) -> anyhow::Result<togi::runner::SelectedTest> {
    if name.trim().is_empty() {
        anyhow::bail!("test name cannot be empty");
    }
    Ok(togi::runner::SelectedTest::new(name, duration_ms))
}

type TestSelectionJson = BTreeMap<String, BTreeMap<String, Vec<String>>>;

fn run_test_map(
    path: Option<PathBuf>,
    output: PathBuf,
    cancelled: &AtomicBool,
) -> anyhow::Result<()> {
    let module_root = match path {
        Some(path) => path,
        None => get_project_root()?,
    }
    .canonicalize()
    .context("could not resolve test-map path")?;
    let repo_root = git_root_for_path(&module_root).unwrap_or_else(|_| module_root.clone());
    let map = generate_go_test_selection_map(&module_root, &repo_root, cancelled)?;
    let output_path = if output.is_relative() {
        module_root.join(output)
    } else {
        output
    };

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(&map)?;
    std::fs::write(&output_path, format!("{json}\n"))
        .with_context(|| format!("could not write {}", output_path.display()))?;
    println!(
        "Wrote test selection map for {} source files to {}",
        map.len(),
        output_path.display()
    );
    Ok(())
}

fn generate_go_test_selection_map(
    module_root: &Path,
    repo_root: &Path,
    cancelled: &AtomicBool,
) -> anyhow::Result<TestSelectionJson> {
    let module_path = go_module_path(module_root)?;
    let tests = go_test_names(module_root)?;
    let mut map = TestSelectionJson::new();

    for test in tests {
        if cancelled.load(Ordering::SeqCst) {
            anyhow::bail!("interrupted");
        }
        let profile = run_go_test_coverage(module_root, &test, cancelled)?;
        add_go_coverage_to_selection_map(
            &mut map,
            repo_root,
            module_root,
            &module_path,
            &profile,
            &test,
        )?;
    }

    Ok(map)
}

fn git_root_for_path(path: &Path) -> anyhow::Result<PathBuf> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .with_context(|| format!("failed to find git root for {}", path.display()))?;
    if !output.status.success() {
        anyhow::bail!(
            "could not find git root for {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(PathBuf::from(String::from_utf8(output.stdout)?.trim()))
}

fn go_module_path(project_root: &Path) -> anyhow::Result<String> {
    let output = std::process::Command::new("go")
        .args(["list", "-m"])
        .current_dir(project_root)
        .output()
        .context("failed to run go list -m")?;
    if !output.status.success() {
        anyhow::bail!(
            "go list -m failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

fn go_test_names(project_root: &Path) -> anyhow::Result<Vec<String>> {
    let output = std::process::Command::new("go")
        .args(["test", "-list", ".", "./..."])
        .current_dir(project_root)
        .output()
        .context("failed to list Go tests")?;
    if !output.status.success() {
        anyhow::bail!(
            "go test -list failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(unique_go_test_names(
        String::from_utf8(output.stdout)?
            .lines()
            .filter(|line| line.starts_with("Test"))
            .map(str::to_string),
    ))
}

fn unique_go_test_names(names: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut unique = Vec::new();
    for name in names {
        if seen.insert(name.clone()) {
            unique.push(name);
        }
    }
    unique
}

fn run_go_test_coverage(
    project_root: &Path,
    test: &str,
    cancelled: &AtomicBool,
) -> anyhow::Result<String> {
    if cancelled.load(Ordering::SeqCst) {
        anyhow::bail!("interrupted");
    }
    let tempdir = tempfile::tempdir()?;
    let profile_path = tempdir.path().join("coverage.out");
    let output = std::process::Command::new("go")
        .arg("test")
        .arg("./...")
        .arg("-run")
        .arg(format!("^{}$", escape_go_regex(test)))
        .arg("-coverpkg")
        .arg("./...")
        .arg("-coverprofile")
        .arg(&profile_path)
        .current_dir(project_root)
        .output()
        .with_context(|| format!("failed to run Go test {test}"))?;
    if !output.status.success() {
        anyhow::bail!(
            "go test -run {test} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    std::fs::read_to_string(&profile_path)
        .with_context(|| format!("could not read coverage profile for {test}"))
}

fn add_go_coverage_to_selection_map(
    map: &mut TestSelectionJson,
    repo_root: &Path,
    module_root: &Path,
    module_path: &str,
    profile: &str,
    test: &str,
) -> anyhow::Result<()> {
    for line in profile.lines().skip(1) {
        let Some((location, fields)) = line.split_once(' ') else {
            continue;
        };
        let fields: Vec<&str> = fields.split_whitespace().collect();
        if fields.len() < 2 || fields[1] == "0" {
            continue;
        }

        let Some((file, range)) = location.rsplit_once(':') else {
            continue;
        };
        let Some((start, end)) = range.split_once(',') else {
            continue;
        };
        let start_line = parse_go_cover_line(start)?;
        let end_line = parse_go_cover_line(end)?;
        let file = normalize_go_cover_file(repo_root, module_root, module_path, file);

        for line in start_line..=end_line {
            let tests = map
                .entry(file.clone())
                .or_default()
                .entry(line.to_string())
                .or_default();
            if !tests.iter().any(|existing| existing == test) {
                tests.push(test.to_string());
            }
        }
    }

    Ok(())
}

fn parse_go_cover_line(position: &str) -> anyhow::Result<usize> {
    position
        .split_once('.')
        .map(|(line, _)| line)
        .unwrap_or(position)
        .parse::<usize>()
        .with_context(|| format!("invalid Go coverage position '{position}'"))
}

fn normalize_go_cover_file(
    repo_root: &Path,
    module_root: &Path,
    module_path: &str,
    file: &str,
) -> String {
    let module_relative = file
        .strip_prefix(module_path)
        .and_then(|path| path.strip_prefix('/'))
        .unwrap_or(file);
    let repo_relative = module_root.join(module_relative);

    repo_relative
        .strip_prefix(repo_root)
        .unwrap_or(Path::new(module_relative))
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => Some(part.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn escape_go_regex(test: &str) -> String {
    let mut escaped = String::new();
    for ch in test.chars() {
        if matches!(
            ch,
            '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$'
        ) {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
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
    validate_diff_base(base)?;
    let output = std::process::Command::new("git")
        .args(["diff", "--no-ext-diff", base])
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "Could not diff against '{base}'. Is the branch up to date? Try running 'git fetch' first.\n\nDetails: {stderr}"
        );
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn validate_diff_base(base: &str) -> anyhow::Result<()> {
    if base.trim().is_empty() {
        anyhow::bail!("diff base cannot be empty");
    }
    if base.starts_with('-') {
        anyhow::bail!("diff base must be a ref, commit, or tag, not an option: {base}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check_config() -> togi::cli::CheckArgs {
        togi::cli::CheckArgs {
            all: false,
            path: vec![],
            base: None,
            config: None,
            format: togi::cli::OutputFormat::Terminal,
            profile: None,
            jobs: None,
            timeout: None,
            calibrate_timeout: false,
            skip_baseline_timing: false,
            timeout_multiplier: None,
            timeout_slack: None,
            max_per_run: None,
            first_survivor: false,
            max_survivors: None,
            schemata: false,
            no_schemata: false,
            dry_run: false,
            verbose: false,
            show_output: false,
            test_cmd: None,
            coverage_file: None,
            coverage_cmd: None,
            min_line_coverage: None,
            min_diff_coverage: None,
            fail_on_uncovered_diff: false,
            test_selection_file: None,
            no_incremental_history: false,
            force_rerun: false,
            build_cmd: None,
            fail_fast: false,
            no_skip_defaults: false,
            operators: None,
            fail_under: None,
            shard: None,
            save_baseline: false,
            check_baseline: false,
            pr_comment: None,
        }
    }

    #[test]
    fn resolve_config_applies_profile_jobs_when_implicit() {
        let dir = tempfile::tempdir().expect("tempdir should be created");
        let config_path = dir.path().join("togi.toml");
        std::fs::write(&config_path, "").expect("empty config should be written");
        let mut cfg = check_config();
        cfg.config = Some(config_path);
        cfg.profile = Some(togi::config::ResourceProfile::Cool);

        let resolved = resolve_config(cfg).expect("config should resolve");

        assert_eq!(resolved.config.test.jobs, 1);
        assert!(resolved.fail_fast);
    }

    #[test]
    fn resolve_config_keeps_explicit_jobs_over_profile() {
        let dir = tempfile::tempdir().expect("tempdir should be created");
        let config_path = dir.path().join("togi.toml");
        std::fs::write(
            &config_path,
            r#"
[test]
profile = "cool"
jobs = 4
"#,
        )
        .expect("config should be written");
        let mut cfg = check_config();
        cfg.config = Some(config_path);

        let resolved = resolve_config(cfg).expect("config should resolve");

        assert_eq!(resolved.profile, Some(togi::config::ResourceProfile::Cool));
        assert_eq!(resolved.config.test.jobs, 4);
        assert!(resolved.fail_fast);
    }

    #[test]
    fn resolve_config_cli_jobs_override_profile() {
        let dir = tempfile::tempdir().expect("tempdir should be created");
        let config_path = dir.path().join("togi.toml");
        std::fs::write(&config_path, "").expect("empty config should be written");
        let mut cfg = check_config();
        cfg.config = Some(config_path);
        cfg.profile = Some(togi::config::ResourceProfile::Ci);
        cfg.jobs = Some(3);

        let resolved = resolve_config(cfg).expect("config should resolve");

        assert_eq!(resolved.config.test.jobs, 3);
    }

    #[test]
    fn resolve_config_applies_timeout_calibration_options() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let config_path = dir.path().join("togi.toml");
        std::fs::write(&config_path, "")?;
        let mut cfg = check_config();
        cfg.config = Some(config_path);
        cfg.calibrate_timeout = true;
        cfg.timeout_multiplier = Some(2.5);
        cfg.timeout_slack = Some(6);

        let resolved = resolve_config(cfg)?;

        assert!(resolved.config.test.calibrate_timeout);
        assert_eq!(resolved.config.test.timeout_multiplier, 2.5);
        assert_eq!(resolved.config.test.timeout_slack, 6);
        Ok(())
    }

    #[test]
    fn resolve_config_skip_baseline_timing_disables_configured_calibration() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let config_path = dir.path().join("togi.toml");
        std::fs::write(&config_path, "[test]\ncalibrate_timeout = true\n")?;
        let mut cfg = check_config();
        cfg.config = Some(config_path);
        cfg.skip_baseline_timing = true;

        let resolved = resolve_config(cfg)?;

        assert!(!resolved.config.test.calibrate_timeout);
        Ok(())
    }

    #[test]
    fn resolve_config_cli_timeout_disables_calibration() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let config_path = dir.path().join("togi.toml");
        std::fs::write(&config_path, "[test]\ncalibrate_timeout = true\n")?;
        let mut cfg = check_config();
        cfg.config = Some(config_path);
        cfg.timeout = Some(12);

        let resolved = resolve_config(cfg)?;

        assert_eq!(resolved.config.test.timeout, 12);
        assert!(!resolved.config.test.calibrate_timeout);
        Ok(())
    }

    #[test]
    fn resolve_config_rejects_coverage_command_without_file() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let config_path = dir.path().join("togi.toml");
        std::fs::write(&config_path, "")?;
        let mut cfg = check_config();
        cfg.config = Some(config_path);
        cfg.coverage_cmd = Some("go test ./...".into());

        let err = resolve_config(cfg).unwrap_err();

        assert!(
            err.to_string()
                .contains("coverage collection command requires")
        );
        Ok(())
    }

    #[test]
    fn resolve_config_rejects_invalid_timeout_multiplier() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let config_path = dir.path().join("togi.toml");
        std::fs::write(&config_path, "")?;
        let mut cfg = check_config();
        cfg.config = Some(config_path);
        cfg.timeout_multiplier = Some(0.0);

        let err = match resolve_config(cfg) {
            Ok(_) => panic!("invalid multiplier should fail"),
            Err(err) => err,
        };

        assert!(err.to_string().contains("timeout_multiplier"));
        Ok(())
    }
    #[test]
    fn calibrated_timeout_uses_slowest_baseline_duration() {
        let timeout = calibrated_timeout_seconds(
            Some(Duration::from_millis(1_900)),
            Duration::from_millis(400),
            2.0,
            1,
        );

        assert_eq!(timeout, 5);
    }

    #[test]
    fn baseline_measurement_timeout_is_more_generous_than_mutation_timeout() {
        assert_eq!(baseline_measurement_timeout(1), Duration::from_secs(60));
        assert_eq!(baseline_measurement_timeout(30), Duration::from_secs(300));
    }

    #[test]
    fn cool_profile_sets_safe_runner_env_without_overriding_user_env() {
        let cargo = vec!["cargo".to_string(), "test".to_string()];
        let go = vec!["go".to_string(), "test".to_string(), "./...".to_string()];
        let pytest = vec!["pytest".to_string()];
        let commands: [&[String]; 3] = [&cargo, &go, &pytest];

        let env = resource_profile_env_for_commands(
            togi::config::ResourceProfile::Cool,
            &commands,
            |name| name == "GOMAXPROCS",
        );

        assert_eq!(env.get("CARGO_BUILD_JOBS").map(String::as_str), Some("1"));
        assert_eq!(env.get("RUST_TEST_THREADS").map(String::as_str), Some("1"));
        assert_eq!(
            env.get("PYTEST_XDIST_AUTO_NUM_WORKERS").map(String::as_str),
            Some("1")
        );
        assert!(!env.contains_key("GOMAXPROCS"));
    }

    #[test]
    fn parse_test_selection_json_accepts_file_line_test_map() {
        let root = Path::new("/repo");
        let json = r#"{
            "src/calc.go": {
                "12": ["TestAdd", "TestMax"]
            }
        }"#;

        assert!(parse_test_selection_json(json, root).is_ok());
    }

    #[test]
    fn parse_test_selection_json_accepts_timed_test_entries() {
        let root = Path::new("/repo");
        let json = r#"{
            "src/lib.rs": {
                "9": [
                    {"name": "math::fast_add", "duration_ms": 3},
                    {"name": "math::slow_add", "duration_ms": 30}
                ]
            }
        }"#;

        assert!(parse_test_selection_json(json, root).is_ok());
    }

    #[test]
    fn parse_test_selection_json_rejects_empty_test_name() {
        let root = Path::new("/repo");
        let json = r#"{
            "src/lib.rs": {
                "9": [{"name": ""}]
            }
        }"#;

        let err = match parse_test_selection_json(json, root) {
            Ok(_) => panic!("empty test name should be rejected"),
            Err(err) => err,
        };

        assert!(err.to_string().contains("invalid test selection entry"));
    }

    #[test]
    fn validate_diff_base_rejects_option_like_values() {
        let err = validate_diff_base("--output=/tmp/togi.diff").unwrap_err();

        assert!(err.to_string().contains("not an option"));
    }

    #[test]
    fn parse_test_selection_json_rejects_non_numeric_line() {
        let root = Path::new("/repo");
        let json = r#"{
            "src/calc.go": {
                "line": ["TestAdd"]
            }
        }"#;

        let err = parse_test_selection_json(json, root).unwrap_err();

        assert!(err.to_string().contains("invalid line number"));
    }

    #[test]
    fn parse_test_selection_json_rejects_zero_line() {
        let root = Path::new("/repo");
        let json = r#"{
            "src/calc.go": {
                "0": ["TestAdd"]
            }
        }"#;

        let err = parse_test_selection_json(json, root).unwrap_err();

        assert_eq!(err.to_string(), "invalid line number '0' for src/calc.go");
    }

    #[test]
    fn normalize_go_cover_file_strips_module_path() {
        assert_eq!(
            normalize_go_cover_file(
                Path::new("/repo/module"),
                Path::new("/repo/module"),
                "example.com/calc",
                "example.com/calc/sub/calc.go"
            ),
            "sub/calc.go"
        );
    }

    #[test]
    fn normalize_go_cover_file_returns_repo_relative_path_for_nested_module() {
        assert_eq!(
            normalize_go_cover_file(
                Path::new("/repo"),
                Path::new("/repo/services/api"),
                "example.com/api",
                "example.com/api/pkg/file.go"
            ),
            "services/api/pkg/file.go"
        );
    }

    #[test]
    fn unique_go_test_names_preserves_first_seen_order() {
        let names = vec![
            "TestAdd".to_string(),
            "TestMax".to_string(),
            "TestAdd".to_string(),
            "TestIsPositive".to_string(),
        ];

        assert_eq!(
            unique_go_test_names(names),
            vec!["TestAdd", "TestMax", "TestIsPositive"]
        );
    }

    #[test]
    fn go_coverage_selection_map_includes_only_covered_lines() {
        let profile = r#"mode: set
example.com/calc/calc.go:4.24,6.2 1 1
example.com/calc/calc.go:9.29,10.11 1 0
"#;
        let mut map = TestSelectionJson::new();

        add_go_coverage_to_selection_map(
            &mut map,
            Path::new("/repo/module"),
            Path::new("/repo/module"),
            "example.com/calc",
            profile,
            "TestAdd",
        )
        .unwrap();

        let file = map.get("calc.go").unwrap();
        assert_eq!(file.get("4").unwrap(), &vec!["TestAdd".to_string()]);
        assert_eq!(file.get("6").unwrap(), &vec!["TestAdd".to_string()]);
        assert!(!file.contains_key("9"));
    }
}
