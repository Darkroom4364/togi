use anyhow::Context;
use clap::Parser;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Deserialize;
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
    test_selection_file: Option<PathBuf>,
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

struct ExecuteOptions {
    verbose: bool,
    show_output: bool,
    build_command_explicit: bool,
    force_default_command: bool,
    force_default_timeout: bool,
    cancelled: Arc<AtomicBool>,
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
            test_selection_file,
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
                test_selection_file,
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

    let (mut config, fail_fast, has_explicit_build_cmd, has_custom_test_cmd, has_cli_timeout) =
        resolve_config(cfg)?;
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
        ExecuteOptions {
            verbose,
            show_output,
            build_command_explicit: has_explicit_build_cmd,
            force_default_command: has_custom_test_cmd,
            force_default_timeout: has_cli_timeout,
            cancelled,
        },
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
    } else if report.survived > 0 && !check_baseline {
        drop(_lock);
        process::exit(1);
    }

    Ok(())
}

fn resolve_config(
    cfg: CheckConfig,
) -> anyhow::Result<(togi::config::Config, bool, bool, bool, bool)> {
    let mut config = togi::config::Config::load(cfg.config_path.as_deref())?;
    let has_custom_test_cmd = cfg.test_cmd.is_some();
    let has_cli_build_cmd = cfg.build_cmd.is_some();
    let has_cli_timeout = cfg.timeout.is_some();

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
    if let Some(path) = cfg.test_selection_file {
        config.mutations.test_selection_file = Some(path);
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
    Ok((
        config,
        fail_fast,
        has_explicit_build_cmd,
        has_custom_test_cmd,
        has_cli_timeout,
    ))
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
    options: ExecuteOptions,
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

    let runner = togi::runner::TestRunner {
        commands: togi::runner::CommandConfig {
            command: config.test.command,
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
        respect_workspace_ignores: config.mutations.respect_workspace_ignores,
        env: std::collections::HashMap::new(),
        cancelled: options.cancelled,
    };

    runner.run(mutations).await
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
    let raw: std::collections::HashMap<String, std::collections::HashMap<String, Vec<String>>> =
        serde_json::from_str(content).context("could not parse test selection JSON")?;
    let mut selection = togi::runner::TestSelectionConfig::new();

    for (file, lines) in raw {
        for (line, tests) in lines {
            let line = line
                .parse::<usize>()
                .with_context(|| format!("invalid line number '{line}' for {file}"))?;
            if line == 0 {
                anyhow::bail!("invalid line number '0' for {file}");
            }
            selection.insert(project_root, Path::new(&file), line, tests);
        }
    }

    Ok(selection)
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

#[cfg(test)]
mod tests {
    use super::*;

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
