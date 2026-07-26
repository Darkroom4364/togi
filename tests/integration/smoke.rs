//! End-to-end smoke fixtures for runtimes available on the standard Ubuntu CI
//! image. These prove more than mutation generation by running the full
//! mutation -> test-command -> outcome pipeline across several languages.
//!
//! TypeScript and C# still rely on parser/unit coverage today and need
//! dedicated runtime fixtures separately.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use togi::{ChangedFile, LineRange};

struct FixtureCase {
    name: &'static str,
    dir: &'static str,
    file: &'static str,
}

fn fixture_path(dir: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(dir)
}

fn source_line_count(path: &Path) -> usize {
    std::fs::read_to_string(path)
        .expect("fixture source should be readable")
        .lines()
        .count()
        .max(1)
}

fn run_fixture(case: FixtureCase) {
    let root = fixture_path(case.dir);
    togi::cache::clear(&root).expect("failed to clear togi cache");

    let changed = vec![ChangedFile {
        path: PathBuf::from(case.file),
        hunks: vec![LineRange {
            start: 1,
            end: source_line_count(&root.join(case.file)),
        }],
    }];

    let mutations = togi::mutator::generate_mutations(&changed, &root, 200, 0, &[])
        .expect("failed to generate mutations");
    assert!(
        !mutations.is_empty(),
        "{} fixture should generate at least one mutation",
        case.name
    );

    let baseline = Command::new("bash")
        .arg("run-tests.sh")
        .current_dir(&root)
        .output()
        .expect("failed to run baseline test command");
    assert!(
        baseline.status.success(),
        "{} baseline failed\nstdout:\n{}\nstderr:\n{}",
        case.name,
        String::from_utf8_lossy(&baseline.stdout),
        String::from_utf8_lossy(&baseline.stderr)
    );

    let runner = togi::runner::TestRunner {
        commands: togi::runner::CommandConfig {
            command: vec!["bash".into(), "run-tests.sh".into()],
            force_default_command: false,
            force_default_timeout: false,
            project_commands: vec![],
            language_commands: HashMap::new(),
            build_command: vec![],
            sandbox_command: vec![],
            build_command_explicit: false,
            timeout: Duration::from_secs(30),
            language_timeouts: HashMap::new(),
            test_selection: None,
        },
        parallelism: 1,
        project_root: root,
        verbose: false,
        show_output: false,
        max_tested: None,
        early_stop: Default::default(),
        respect_workspace_ignores: true,
        env: HashMap::new(),
        incremental_history: false,
        force_rerun: true,
        learned_selection: false,
        cancelled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };

    let report = runner.run(mutations).report;
    assert!(
        report.total > 0,
        "{} fixture should execute mutations",
        case.name
    );
    assert_eq!(
        report.timeout, 0,
        "{} fixture should not time out",
        case.name
    );
    assert_eq!(
        report.build_errors, 0,
        "{} fixture should not produce build errors",
        case.name
    );
    assert_eq!(
        report.total,
        report.killed + report.survived,
        "{} fixture should classify every mutation as killed or survived",
        case.name
    );

    println!(
        "{} fixture: {} total, {} killed, {} survived",
        case.name, report.total, report.killed, report.survived
    );
}

/// Requires `bash` plus the language toolchains bundled on the Ubuntu CI image.
#[test]
#[ignore]
fn rust_fixture_runs_end_to_end() {
    run_fixture(FixtureCase {
        name: "rust",
        dir: "rust",
        file: "src/lib.rs",
    });
}

/// Requires `bash` plus the language toolchains bundled on the Ubuntu CI image.
#[test]
#[ignore]
fn python_fixture_runs_end_to_end() {
    run_fixture(FixtureCase {
        name: "python",
        dir: "python",
        file: "calc.py",
    });
}

/// Requires `bash` plus the language toolchains bundled on the Ubuntu CI image.
#[test]
#[ignore]
fn java_fixture_runs_end_to_end() {
    run_fixture(FixtureCase {
        name: "java",
        dir: "java",
        file: "Calc.java",
    });
}

/// Requires `bash` plus the language toolchains bundled on the Ubuntu CI image.
#[test]
#[ignore]
fn c_fixture_runs_end_to_end() {
    run_fixture(FixtureCase {
        name: "c",
        dir: "c",
        file: "calc.c",
    });
}

/// Requires `bash` plus the language toolchains bundled on the Ubuntu CI image.
#[test]
#[ignore]
fn cpp_fixture_runs_end_to_end() {
    run_fixture(FixtureCase {
        name: "cpp",
        dir: "cpp",
        file: "calc.cpp",
    });
}

/// Requires `bash` plus the language toolchains bundled on the Ubuntu CI image.
#[test]
#[ignore]
fn ruby_fixture_runs_end_to_end() {
    run_fixture(FixtureCase {
        name: "ruby",
        dir: "ruby",
        file: "calc.rb",
    });
}

/// Proves the documented polyglot demo routes mutations to each language's
/// configured test suite, rather than applying the Rust default everywhere.
#[test]
#[ignore]
fn polyglot_demo_routes_mutations_to_each_language_test_suite() {
    // The demo creates commits, so it must not rely on a CI runner's identity.
    let clean_home = tempfile::tempdir().unwrap();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let output = Command::new("bash")
        .arg("examples/polyglot-demo.sh")
        .current_dir(&root)
        .env("HOME", clean_home.path())
        .env("XDG_CONFIG_HOME", clean_home.path().join("config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("failed to run polyglot demo");
    assert!(
        output.status.success(),
        "polyglot demo failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    for expected in [
        "✓ KILLED      calc.go:5 — zero_to_one",
        "✓ KILLED      calc.py:2 — increment_numeric",
        "✓ KILLED      src/lib.rs:2 — negate_condition",
    ] {
        assert!(
            stdout.contains(expected),
            "polyglot demo did not route {expected} to its language test suite\nstdout:\n{stdout}"
        );
    }
    let results_line = stdout
        .lines()
        .find(|line| line.starts_with("Results: "))
        .expect("polyglot demo should print a result summary");
    let has_clean_results = ["0 timeout", "0 build errors"]
        .into_iter()
        .all(|expected| results_line.split(", ").any(|field| field == expected));
    assert!(
        has_clean_results,
        "polyglot demo reported a timeout or build error\nstdout:\n{stdout}"
    );
}
