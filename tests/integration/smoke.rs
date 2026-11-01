//! End-to-end smoke fixtures for runtimes available on the standard Ubuntu CI
//! image. These prove more than mutation generation by running the full
//! mutation -> test-command -> outcome pipeline across several languages.
//!

use std::collections::HashMap;
#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(unix)]
use std::process::Output;
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

fn run_fixture(case: FixtureCase) -> togi::MutationReport {
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
            build_command_origin: togi::config::BuildCommandOrigin::None,
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
    report
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

/// Requires bash plus Node.js 24.14.1 for native TypeScript type stripping.
#[test]
#[ignore]
fn typescript_fixture_runs_end_to_end() {
    let report = run_fixture(FixtureCase {
        name: "typescript",
        dir: "typescript",
        file: "calc.ts",
    });
    assert!(
        report.killed > 0,
        "typescript fixture should kill at least one mutation"
    );
}

/// Requires bash plus the .NET 8 SDK.
#[test]
#[ignore]
fn csharp_fixture_runs_end_to_end() {
    let report = run_fixture(FixtureCase {
        name: "csharp",
        dir: "csharp",
        file: "Calc.cs",
    });
    assert!(
        report.killed > 0,
        "csharp fixture should kill at least one mutation"
    );
}

#[cfg(unix)]
fn demo_executable(path: &Path, source: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, source).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
#[test]
fn polyglot_demo_rejects_fatal_and_incomplete_results() {
    use serde_json::json;
    use std::fs;

    let sandbox = tempfile::tempdir().unwrap();
    let root = sandbox.path();
    for dir in [
        "examples",
        "tests/fixtures/polyglot",
        "target/debug",
        "scratch",
    ] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/polyglot-demo.sh"),
        root.join("examples/polyglot-demo.sh"),
    )
    .unwrap();
    fs::write(root.join("tests/fixtures/polyglot/fixture.txt"), "fixture").unwrap();
    demo_executable(
        &root.join("target/debug/togi"),
        r#"#!/bin/sh
printf '%s\n' "$1" >> "$DEMO_CALLS"
while [ "$#" -gt 0 ]; do
  if [ "$1" = --json-report ] && [ -f "$DEMO_REPORT" ]; then
    cp "$DEMO_REPORT" "$2"
  fi
  shift
done
exit "$DEMO_STATUS"
"#,
    );
    let valid = json!({
        "schema_version": 1, "kind": "mutation_report", "partial": false,
        "total": 3, "planned_total": 3, "tested": 3, "killed": 0, "survived": 3,
        "timeout": 0, "build_errors": 0,
        "mutations": (["go", "rust", "python"].map(|language| json!({
            "language": language, "result": "survived", "execution": {"state": "executed"}
        })))
    });
    let mut killed = valid.clone();
    killed["killed"] = json!(3);
    killed["survived"] = json!(0);
    for mutation in killed["mutations"].as_array_mut().unwrap() {
        mutation["result"] = json!("killed");
    }
    let mut cases = vec![
        ("survivors", 1, Some(valid.to_string()), 0),
        ("all killed", 0, Some(killed.to_string()), 0),
        ("fatal error", 2, Some(valid.to_string()), 2),
        ("interrupted", 130, None, 130),
        ("missing report", 1, None, 1),
        ("malformed report", 1, Some("{".into()), 1),
        ("survivors with exit zero", 0, Some(valid.to_string()), 1),
    ];
    for (field, value) in [
        ("timeout", json!(1)),
        ("build_errors", json!(1)),
        ("partial", json!(true)),
        ("total", json!(0)),
        ("planned_total", json!(4)),
        ("kind", json!("coverage_gate_report")),
    ] {
        let mut report = valid.clone();
        report[field] = value;
        cases.push((field, 1, Some(report.to_string()), 1));
    }
    for (name, status, report, expected) in cases {
        let report_path = root.join("report.json");
        if let Some(report) = report {
            fs::write(&report_path, report).unwrap();
        } else if report_path.exists() {
            fs::remove_file(&report_path).unwrap();
        }
        fs::write(root.join("calls"), "").unwrap();
        let output = Command::new("bash")
            .arg(root.join("examples/polyglot-demo.sh"))
            .env_remove("TOGI_BIN")
            .env("DEMO_REPORT", &report_path)
            .env("DEMO_STATUS", status.to_string())
            .env("DEMO_CALLS", root.join("calls"))
            .env("TMPDIR", root.join("scratch"))
            .env("GIT_CONFIG_GLOBAL", root.join("no-git-config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.code(),
            Some(expected),
            "{name}\n{stdout}\n{stderr}"
        );
        assert_eq!(
            stdout.contains("=== one run, one report, one gate"),
            expected == 0,
            "{name}\n{stdout}\n{stderr}"
        );
        assert_eq!(fs::read_to_string(root.join("calls")).unwrap(), "check\n");
        assert!(fs::read_dir(root.join("scratch")).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("togi-polyglot.")
        }));
    }
}

/// Proves the documented polyglot demo routes mutations to each language's
/// configured test suite, rather than applying the Rust default everywhere.
#[test]
#[ignore]
#[cfg(unix)]
fn polyglot_demo_routes_mutations_to_each_language_test_suite() {
    // The demo creates commits, so it must not rely on a CI runner's identity.
    let clean_home = tempfile::tempdir().unwrap();
    let cargo_target_dir = tempfile::tempdir().unwrap();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let output = Command::new("bash")
        .arg("examples/polyglot-demo.sh")
        .current_dir(&root)
        .env("HOME", clean_home.path())
        .env("XDG_CONFIG_HOME", clean_home.path().join("config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("CARGO_TARGET_DIR", cargo_target_dir.path())
        .env_remove("TOGI_BIN")
        .output()
        .expect("failed to run polyglot demo");
    assert!(
        output.status.success(),
        "polyglot demo failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        cargo_target_dir.path().join("debug/togi").is_file(),
        "polyglot demo did not build to the external target directory"
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
    assert_eq!(stdout.matches("Results: ").count(), 1);
    assert_eq!(
        std::fs::read_to_string(scratch.join("calls")).unwrap(),
        "check\n"
    );
    assert_eq!(
        std::fs::read_to_string(scratch.join("statuses")).unwrap(),
        "1\n"
    );
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(scratch.join("report.json")).unwrap()).unwrap();
    assert_eq!(report["kind"], "mutation_report");
    assert_eq!(report["partial"], false);
    assert_eq!(report["total"], report["planned_total"]);
    assert_eq!(report["timeout"], 0);
    assert_eq!(report["build_errors"], 0);
    assert!(report["survived"].as_u64().unwrap() > 0);
    for language in ["go", "rust", "python"] {
        assert!(
            report["mutations"]
                .as_array()
                .unwrap()
                .iter()
                .any(|mutation| {
                    mutation["language"] == language && mutation["execution"]["state"] == "executed"
                })
        );
    }

    // A genuine engine error must survive the script, without its success banner.
    let failure = run(true);
    assert_eq!(failure.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&failure.stderr)
            .contains("could not read missing-demo-config.toml")
    );
    assert!(
        !String::from_utf8_lossy(&failure.stdout).contains("=== one run, one report, one gate")
    );
    assert_eq!(
        std::fs::read_to_string(scratch.join("calls")).unwrap(),
        "check\ncheck\n"
    );
    assert_eq!(
        std::fs::read_to_string(scratch.join("statuses")).unwrap(),
        "1\n2\n"
    );
}

#[cfg(unix)]
fn demo_test_tools_available() -> bool {
    Command::new("bash")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
        && Command::new("git")
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
}

#[cfg(unix)]
struct DemoRun {
    output: Output,
    fake_togi: PathBuf,
    invocations: String,
}

#[cfg(unix)]
fn run_demo_with_fake_togi(script: &str, status: i32, report: &str) -> DemoRun {
    let fixture = tempfile::tempdir().unwrap();
    let target_dir = fixture.path().join("external-target");
    let fake_togi = target_dir.join("debug/togi");
    fs::create_dir_all(fake_togi.parent().unwrap()).unwrap();
    fs::write(
        &fake_togi,
        r#"#!/usr/bin/env bash
set -eu
report=
while (($#)); do
  case "$1" in
    --json-report)
      report="${2:?missing report path}"
      shift 2
      ;;
    *)
      shift
      ;;
  esac
done
[[ -n "$report" ]]
printf '%s\n' "$0" >> "$FAKE_TOGI_LOG"
printf '%s\n' "$FAKE_TOGI_REPORT" > "$report"
exit "$FAKE_TOGI_STATUS"
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&fake_togi).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_togi, permissions).unwrap();

    let home = fixture.path().join("home");
    fs::create_dir(&home).unwrap();
    let invocation_log = fixture.path().join("fake-togi.log");
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let output = Command::new("bash")
        .arg(root.join(script))
        .current_dir(&root)
        .env("CARGO_TARGET_DIR", &target_dir)
        .env_remove("TOGI_BIN")
        .env("FAKE_TOGI_LOG", &invocation_log)
        .env("FAKE_TOGI_STATUS", status.to_string())
        .env("FAKE_TOGI_REPORT", report)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Togi Test")
        .env("GIT_AUTHOR_EMAIL", "togi-test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Togi Test")
        .env("GIT_COMMITTER_EMAIL", "togi-test@example.invalid")
        .output()
        .expect("failed to run demo");

    DemoRun {
        output,
        fake_togi,
        invocations: fs::read_to_string(invocation_log).expect("fake togi was not invoked"),
    }
}

#[cfg(unix)]
fn demo_report(survived: usize, timeout: usize, build_errors: usize) -> String {
    format!(r#"{{"survived":{survived},"timeout":{timeout},"build_errors":{build_errors}}}"#)
}

#[cfg(unix)]
#[test]
fn demo_scripts_honor_external_target_dir_for_clean_survivors() {
    if !demo_test_tools_available() {
        eprintln!("skipping demo script test: bash and git are required");
        return;
    }

    for script in ["examples/demo.sh", "examples/polyglot-demo.sh"] {
        for (status, report) in [(0, demo_report(1, 0, 0)), (1, demo_report(1, 0, 0))] {
            let run = run_demo_with_fake_togi(script, status, &report);
            assert!(
                run.output.status.success(),
                "{script} should accept clean exit {status}\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&run.output.stdout),
                String::from_utf8_lossy(&run.output.stderr)
            );
            assert_eq!(
                run.invocations,
                format!("{}\n", run.fake_togi.display()),
                "{script} did not use the binary in CARGO_TARGET_DIR"
            );
            if script == "examples/polyglot-demo.sh" {
                assert!(
                    String::from_utf8_lossy(&run.output.stdout).contains(
                        "=== one run, one report, one gate — no per-language glue required ==="
                    ),
                    "polyglot demo should complete after clean exit {status}"
                );
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn demo_scripts_reject_invalid_reports_and_fatal_exit_codes() {
    if !demo_test_tools_available() {
        eprintln!("skipping demo script test: bash and git are required");
        return;
    }

    for script in ["examples/demo.sh", "examples/polyglot-demo.sh"] {
        for (kind, status, report, expected_status, expects_diagnostic) in [
            ("missing survivor report", 0, demo_report(0, 0, 0), 1, true),
            ("build-error report", 1, demo_report(1, 0, 1), 1, true),
            ("timeout report", 1, demo_report(1, 1, 0), 1, true),
            ("command error", 2, demo_report(1, 0, 0), 2, false),
            ("interrupt", 130, demo_report(1, 0, 0), 130, false),
            ("unexpected exit", 42, demo_report(1, 0, 0), 42, false),
        ] {
            let run = run_demo_with_fake_togi(script, status, &report);
            assert_eq!(
                run.output.status.code(),
                Some(expected_status),
                "{script} should propagate {kind}\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&run.output.stdout),
                String::from_utf8_lossy(&run.output.stderr)
            );
            assert_eq!(
                run.invocations,
                format!("{}\n", run.fake_togi.display()),
                "{script} did not use the binary in CARGO_TARGET_DIR"
            );
            if expects_diagnostic {
                assert!(
                    String::from_utf8_lossy(&run.output.stderr).contains(
                        "Expected a JSON report with survived > 0, timeout == 0, and build_errors == 0."
                    ),
                    "{script} should diagnose {kind}"
                );
            }
            if script == "examples/polyglot-demo.sh" {
                assert!(
                    !String::from_utf8_lossy(&run.output.stdout).contains(
                        "=== one run, one report, one gate — no per-language glue required ==="
                    ),
                    "polyglot demo printed its success banner after {kind}"
                );
            }
        }
    }
}
