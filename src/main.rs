use clap::Parser;
use std::path::PathBuf;
use std::process;
use std::time::Duration;

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
            build_cmd,
        } => {
            if let Err(e) = run_check(
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
                build_cmd,
            )
            .await
            {
                eprintln!("Error: {e}");
                let msg = e.to_string();
                if msg.contains("toml") || msg.contains("config") || msg.contains("togi.toml") {
                    eprintln!("Hint: run 'togi check --help' for usage information.");
                }
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

#[allow(clippy::too_many_arguments)]
async fn run_check(
    all: bool,
    base: String,
    config_path: Option<PathBuf>,
    format: String,
    jobs: Option<usize>,
    timeout: Option<u64>,
    dry_run: bool,
    verbose: bool,
    show_output: bool,
    test_cmd: Option<String>,
    build_cmd: Option<String>,
) -> anyhow::Result<()> {
    // 1. Load config
    let mut config = togi::config::Config::load(config_path.as_deref())?;

    // 2. Apply CLI overrides
    if base != "origin/main" {
        config.diff.base = base;
    }
    if let Some(j) = jobs {
        config.test.jobs = j;
    }
    if let Some(t) = timeout {
        config.test.timeout = t;
    }
    if let Some(cmd) = test_cmd {
        config.test.command = cmd.split_whitespace().map(String::from).collect();
    }
    if let Some(cmd) = build_cmd {
        config.test.build_command = cmd.split_whitespace().map(String::from).collect();
    }

    // Find project root (git toplevel)
    let project_root = get_project_root()?;

    // Acquire lock to prevent concurrent runs
    let _lock = togi::lock::acquire(&project_root)?;

    // Auto-detect test command if not explicitly configured
    config.resolve_test_command(&project_root);
    config.resolve_build_command(&project_root);

    // Warn about unrecognized language keys in [test.languages]
    let all_langs = togi::languages::all();
    let known: Vec<&str> = all_langs.iter().map(|l| l.name()).collect();
    config.warn_unknown_languages(&known);

    // 3. Build list of files to mutate
    let changed_files = if all {
        let files = togi::diff::collect_all_supported_files(&project_root)?;
        if files.is_empty() {
            println!("No supported source files found. Nothing to mutate.");
            return Ok(());
        }
        println!("Scanning all {} supported files...", files.len());
        files
    } else {
        let diff_output = get_git_diff(&config.diff.base)?;
        if diff_output.is_empty() {
            println!(
                "No changes found in diff against `{}`. Nothing to mutate.",
                config.diff.base
            );
            return Ok(());
        }
        let files = togi::diff::parse_diff(&diff_output);
        if files.is_empty() {
            println!("No added/modified lines found. Nothing to mutate.");
            return Ok(());
        }
        files
    };

    // 5. Generate mutations (over-generate when build check active to compensate for filtered mutations)
    let generation_limit = if config.test.build_command.is_empty() {
        config.mutations.max_per_run
    } else {
        config.mutations.max_per_run * 2
    };
    let mutations =
        togi::mutator::generate_mutations(&changed_files, &project_root, generation_limit)?;

    if mutations.len() >= config.mutations.max_per_run {
        eprintln!(
            "warning: mutation count capped at max_per_run ({}). Increase in togi.toml or use --dry-run to preview.",
            config.mutations.max_per_run
        );
    }

    if mutations.is_empty() {
        println!(
            "No mutations generated. This can happen if the changed files are in an unsupported language.\nSupported: Go (.go), Rust (.rs), Python (.py), TypeScript (.ts/.tsx)"
        );
        return Ok(());
    }

    // 6. Handle dry-run
    if dry_run {
        println!(
            "Dry run — {} mutations would be generated:",
            mutations.len()
        );
        for m in &mutations {
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
        return Ok(());
    }

    // 7. Run mutations
    eprintln!("Running {} mutations...", mutations.len());

    let language_commands: std::collections::HashMap<String, Vec<String>> = config
        .test
        .languages
        .iter()
        .map(|(k, v)| (k.clone(), v.command.clone()))
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

    let report = runner.run(mutations).await;

    // 8. Print report
    togi::report::print_report(&report, &format)?;

    // 9. Exit code
    if report.survived > 0 {
        process::exit(1);
    }

    Ok(())
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
