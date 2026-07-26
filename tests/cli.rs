use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
#[cfg(unix)]
use std::os::unix::{fs::PermissionsExt, process::CommandExt};
use std::path::Path;
#[cfg(unix)]
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn togi() -> Command {
    Command::cargo_bin("togi").unwrap()
}

fn bash_available() -> bool {
    std::process::Command::new("bash")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Set up a minimal git repo with a Go file and a diff to test against.
fn setup_git_repo() -> TempDir {
    let dir = TempDir::new().unwrap();
    let path = dir.path();

    // Init git repo
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(path)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(path)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(path)
        .output()
        .unwrap();

    // Create initial commit with a Go file
    let go_file = path.join("main.go");
    fs::write(
        &go_file,
        "package main\n\nfunc add(a, b int) int {\n\treturn a + b\n}\n",
    )
    .unwrap();
    fs::write(path.join("go.mod"), "module example.com/test\n\ngo 1.21\n").unwrap();

    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(path)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "initial"])
        .current_dir(path)
        .output()
        .unwrap();

    // Make a change (add a comparison)
    fs::write(
        &go_file,
        "package main\n\nfunc add(a, b int) int {\n\tif a > b {\n\t\treturn a\n\t}\n\treturn a + b\n}\n",
    )
    .unwrap();

    dir
}

#[test]
fn init_creates_config() {
    let dir = TempDir::new().unwrap();

    togi()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Created togi.toml"));

    assert!(dir.path().join("togi.toml").exists());
}

#[test]
fn init_fails_if_config_exists() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("togi.toml"), "").unwrap();

    togi()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("already exists"));
}

#[test]
fn check_no_diff_exits_zero() {
    let dir = setup_git_repo();

    // Commit the working change so there's no unstaged diff against HEAD
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "second"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    // Diff HEAD against HEAD = no changes
    togi()
        .args(["check", "--base", "HEAD"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("No changes found"));
}

#[test]
fn check_dry_run_lists_mutations() {
    let dir = setup_git_repo();

    togi()
        .args(["check", "--base", "HEAD", "--dry-run", "--test-cmd", "true"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("mutations would be generated"));
}

/// Extend the fixture so mutants land on two lines: line 4 (the added `if`)
/// and line 7 (`return a - b`).
fn setup_two_line_mutation_repo() -> TempDir {
    let dir = setup_git_repo();
    fs::write(
        dir.path().join("main.go"),
        "package main\n\nfunc add(a, b int) int {\n\tif a > b {\n\t\treturn a\n\t}\n\treturn a - b\n}\n",
    )
    .unwrap();
    dir
}

#[cfg(unix)]
#[test]
fn check_classifies_zero_coverage_mutants_as_uncovered() {
    let dir = setup_two_line_mutation_repo();
    // Line 4 covered; line 7 tracked but never executed.
    fs::write(
        dir.path().join("lcov.info"),
        "SF:main.go\nDA:4,1\nDA:7,0\nend_of_record\n",
    )
    .unwrap();
    let expected = tempfile::NamedTempFile::new().unwrap();
    fs::write(
        expected.path(),
        fs::read(dir.path().join("main.go")).unwrap(),
    )
    .unwrap();
    let test_cmd = format!(
        "sh -c {}",
        shell_quote_text(&format!("cmp -s main.go {}", shell_quote(expected.path())))
    );

    let output = togi()
        .args([
            "check",
            "--base",
            "HEAD",
            "--format",
            "json",
            "--test-cmd",
            &test_cmd,
            "--coverage-file",
            "lcov.info",
            "--fail-under",
            "100",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();

    // The command passes for the unmutated file and kills each executed
    // mutant. The zero-coverage mutant must not count as a survivor, so the
    // run passes even with --fail-under 100.
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("reported as uncovered"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "invalid JSON output: {e}\nstdout: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    assert_eq!(value["killed"], 3);
    assert_eq!(value["survived"], 0);
    assert_eq!(value["uncovered"], 1);
    assert_eq!(value["mutation_score"], 100.0);

    let mutations = value["mutations"].as_array().unwrap();
    let uncovered: Vec<_> = mutations
        .iter()
        .filter(|m| m["result"] == "uncovered")
        .collect();
    assert_eq!(uncovered.len(), 1);
    assert_eq!(uncovered[0]["line"], 7);
    assert_eq!(uncovered[0]["file"], "main.go");
}

#[test]
fn check_all_mutants_uncovered_skips_execution() {
    let dir = setup_two_line_mutation_repo();
    // Both mutated lines have zero coverage: nothing should execute.
    fs::write(
        dir.path().join("lcov.info"),
        "SF:main.go\nDA:4,0\nDA:7,0\nend_of_record\n",
    )
    .unwrap();

    let output = togi()
        .args([
            "check",
            "--base",
            "HEAD",
            "--format",
            "json",
            "--test-cmd",
            "true",
            "--coverage-file",
            "lcov.info",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("Running"),
        "no mutations should execute: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "invalid JSON output: {e}\nstdout: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    assert_eq!(value["total"], 4);
    assert_eq!(value["killed"], 0);
    assert_eq!(value["survived"], 0);
    assert_eq!(value["uncovered"], 4);
    assert_eq!(value["mutation_score"], 100.0);
    assert!(
        value["mutations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["result"] == "uncovered")
    );
}

#[test]
fn check_all_mutants_uncovered_aborts_when_baseline_test_fails() {
    let dir = setup_two_line_mutation_repo();
    fs::write(
        dir.path().join("lcov.info"),
        "SF:main.go\nDA:4,0\nDA:7,0\nend_of_record\n",
    )
    .unwrap();

    let output = togi()
        .args([
            "check",
            "--base",
            "HEAD",
            "--format",
            "json",
            "--test-cmd",
            "false",
            "--coverage-file",
            "lcov.info",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("baseline test command failed (`false`)")
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("\"killed\""));
    assert!(!stdout.contains("mutation_score"));
    assert!(!dir.path().join(".togi-cache").exists());
}

#[test]
fn check_all_mutants_uncovered_aborts_when_baseline_build_fails() {
    let dir = setup_two_line_mutation_repo();
    fs::write(
        dir.path().join("lcov.info"),
        "SF:main.go\nDA:4,0\nDA:7,0\nend_of_record\n",
    )
    .unwrap();

    let output = togi()
        .args([
            "check",
            "--base",
            "HEAD",
            "--format",
            "json",
            "--test-cmd",
            "true",
            "--build-cmd",
            "false",
            "--coverage-file",
            "lcov.info",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("baseline build command failed (`false`)")
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("\"killed\""));
    assert!(!stdout.contains("mutation_score"));
    assert!(!dir.path().join(".togi-cache").exists());
}

#[test]
fn check_warns_when_fail_fast_is_ignored_for_custom_test_cmd() {
    let dir = setup_git_repo();

    togi()
        .args([
            "check",
            "--base",
            "HEAD",
            "--dry-run",
            "--test-cmd",
            "true",
            "--fail-fast",
        ])
        .current_dir(dir.path())
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "--fail-fast is ignored when --test-cmd is set",
        ));
}

#[test]
fn check_format_json_outputs_valid_json() {
    let dir = setup_git_repo();

    let output = togi()
        .args([
            "check",
            "--base",
            "HEAD",
            "--format",
            "json",
            "--test-cmd",
            "true",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "invalid JSON output: {e}\nstdout: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    assert!(value.get("total").is_some());
    assert!(value.get("mutations").is_some());
}

#[test]
fn check_aborts_before_mutations_when_baseline_test_fails() {
    let dir = setup_git_repo();

    let output = togi()
        .args([
            "check",
            "--all",
            "--path",
            "main.go",
            "--max-per-run",
            "1",
            "--no-schemata",
            "--test-cmd",
            "false",
            "--force-rerun",
            "--no-incremental-history",
            "--fail-under",
            "0",
            "--format",
            "json",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(2),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("baseline test command failed (`false`)"));
    assert!(!stderr.contains("Running 1 mutations"));
    assert!(!stdout.contains("\"killed\""));
    assert!(!stdout.contains("mutation_score"));
    assert!(
        !dir.path().join(".togi-cache").exists(),
        "baseline failure must not create mutation cache entries"
    );
}

#[cfg(unix)]
#[test]
fn check_schemata_cannot_bypass_a_source_sensitive_baseline_failure() {
    let dir = setup_git_repo();
    let test_cmd = format!(
        "sh -c {}",
        shell_quote_text("if grep -Fq 'if a > b' main.go; then exit 1; fi")
    );

    let output = togi()
        .args([
            "check",
            "--all",
            "--path",
            "main.go",
            "--max-per-run",
            "1",
            "--operators",
            "gt_to_gte",
            "--test-cmd",
            &test_cmd,
            "--force-rerun",
            "--no-incremental-history",
            "--fail-under",
            "0",
            "--format",
            "json",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("baseline test command failed"));
    assert!(!stderr.contains("Running 1 mutations"));
    assert!(!stdout.contains("\"killed\""));
    assert!(!stdout.contains("mutation_score"));
    assert!(!dir.path().join(".togi-cache").exists());
}

#[cfg(unix)]
#[test]
fn check_failing_baseline_does_not_accept_a_preseeded_exact_cache_verdict() {
    let dir = setup_git_repo();
    let log = dir.path().join("cache-baseline.log");
    let test_cmd = format!(
        "sh -c {}",
        shell_quote_text(
            "if [ \"$TOGI_BASELINE_MODE\" = fail ] && grep -Fq 'if a > b' main.go; then echo baseline-failed >> \"$TOGI_TEST_LOG\"; exit 1; fi; if grep -Fq 'if a > b' main.go; then echo baseline-pass >> \"$TOGI_TEST_LOG\"; else echo mutant >> \"$TOGI_TEST_LOG\"; fi"
        )
    );

    let seeded = togi()
        .args([
            "check",
            "--all",
            "--path",
            "main.go",
            "--max-per-run",
            "1",
            "--no-schemata",
            "--operators",
            "gt_to_gte",
            "--test-cmd",
            &test_cmd,
            "--force-rerun",
            "--no-incremental-history",
            "--fail-under",
            "0",
            "--format",
            "json",
        ])
        .env("TOGI_BASELINE_MODE", "pass")
        .env("TOGI_TEST_LOG", &log)
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        seeded.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&seeded.stdout),
        String::from_utf8_lossy(&seeded.stderr)
    );
    assert!(
        dir.path().join(".togi-cache").exists(),
        "the first run must seed an exact cache verdict"
    );

    let output = togi()
        .args([
            "check",
            "--all",
            "--path",
            "main.go",
            "--max-per-run",
            "1",
            "--no-schemata",
            "--operators",
            "gt_to_gte",
            "--test-cmd",
            &test_cmd,
            "--no-incremental-history",
            "--fail-under",
            "0",
            "--format",
            "json",
        ])
        .env("TOGI_BASELINE_MODE", "fail")
        .env("TOGI_TEST_LOG", &log)
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("baseline test command failed"));
    assert!(!stderr.contains("Running 1 mutations"));
    assert!(!stdout.contains("\"killed\""));
    assert!(!stdout.contains("mutation_score"));
    assert_eq!(
        fs::read_to_string(log).unwrap().lines().collect::<Vec<_>>(),
        vec!["baseline-pass", "mutant", "baseline-failed"]
    );
}

#[cfg(unix)]
#[test]
fn check_schemata_baselines_go_with_the_no_cache_argv() {
    let dir = setup_git_repo();
    let fake_bin = dir.path().join("fake-go-bin");
    let log = dir.path().join("fake-go.log");
    fs::write(
        dir.path().join("togi.toml"),
        r#"
[test]
command = ["go", "test", "./..."]
timeout = 5

[mutations]
schemata = true
"#,
    )
    .unwrap();

    let output = togi()
        .args([
            "check",
            "--all",
            "--path",
            "main.go",
            "--max-per-run",
            "1",
            "--operators",
            "gt_to_gte",
            "--force-rerun",
            "--no-incremental-history",
            "--fail-under",
            "0",
            "--format",
            "json",
        ])
        .env("PATH", fake_go_path_env(&fake_bin))
        .env("TOGI_GO_LOG", &log)
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("baseline test command failed"));
    assert!(stderr.contains("-count=1"));
    assert_eq!(fs::read_to_string(log).unwrap(), "no-cache");
    assert!(!stderr.contains("Running 1 mutations"));
    assert!(!stdout.contains("\"killed\""));
    assert!(!stdout.contains("mutation_score"));
    assert!(!dir.path().join(".togi-cache").exists());
}

#[cfg(unix)]
#[test]
fn check_no_schemata_keeps_the_raw_go_test_argv() {
    let dir = setup_git_repo();
    let fake_bin = dir.path().join("fake-go-bin");
    let log = dir.path().join("fake-go.log");
    fs::write(
        dir.path().join("togi.toml"),
        r#"
[test]
command = ["go", "test", "./..."]
timeout = 5
"#,
    )
    .unwrap();

    let output = togi()
        .args([
            "check",
            "--base",
            "HEAD",
            "--max-per-run",
            "1",
            "--no-schemata",
            "--operators",
            "gt_to_gte",
            "--force-rerun",
            "--no-incremental-history",
            "--fail-under",
            "0",
            "--format",
            "json",
        ])
        .env("PATH", fake_go_path_env(&fake_bin))
        .env("TOGI_GO_LOG", &log)
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["survived"], 1);
    assert_eq!(fs::read_to_string(log).unwrap(), "rawraw");
}

#[test]
fn check_runs_mutations_after_a_passing_baseline() {
    let dir = setup_git_repo();

    let output = togi()
        .args([
            "check",
            "--base",
            "HEAD",
            "--max-per-run",
            "1",
            "--no-schemata",
            "--test-cmd",
            "true",
            "--force-rerun",
            "--no-incremental-history",
            "--fail-under",
            "0",
            "--format",
            "json",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Checking baseline test suites..."));
    assert!(stderr.contains("Running 1 mutations..."));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["total"], 1);
    assert_eq!(value["survived"], 1);
    assert!(dir.path().join(".togi-cache").exists());
}

#[cfg(unix)]
#[test]
fn check_calibration_reuses_the_passing_baseline_run() {
    let dir = setup_git_repo();
    let log = dir.path().join("baseline-runs.log");
    let test_cmd = format!(
        "sh -c {}",
        shell_quote_text(
            "if grep -q 'if a > b' main.go; then echo baseline >> \"$TOGI_TEST_LOG\"; else echo mutant >> \"$TOGI_TEST_LOG\"; fi"
        )
    );

    let output = togi()
        .args([
            "check",
            "--all",
            "--path",
            "main.go",
            "--max-per-run",
            "1",
            "--no-schemata",
            "--operators",
            "gt_to_gte",
            "--test-cmd",
            &test_cmd,
            "--calibrate-timeout",
            "--force-rerun",
            "--no-incremental-history",
            "--fail-under",
            "0",
        ])
        .env("TOGI_TEST_LOG", &log)
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(log).unwrap().lines().collect::<Vec<_>>(),
        vec!["baseline", "mutant"]
    );
}

#[cfg(unix)]
#[test]
fn check_calibration_extends_a_slow_global_default_route() {
    let dir = setup_git_repo();
    fs::write(
        dir.path().join("togi.toml"),
        r#"
[test]
command = ["sh", "-c", "sleep 2"]
timeout = 1
calibrate_timeout = true
timeout_multiplier = 1.0
timeout_slack = 1
"#,
    )
    .unwrap();

    let output = togi()
        .args([
            "check",
            "--base",
            "HEAD",
            "--format",
            "json",
            "--max-per-run",
            "1",
            "--no-schemata",
            "--operators",
            "gt_to_gte",
            "--force-rerun",
            "--no-incremental-history",
            "--fail-under",
            "0",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["timeout"], 0);
    assert!(
        value["baseline_timing"]["calibrated_timeout_ms"]
            .as_u64()
            .is_some_and(|timeout| timeout >= 3_000)
    );
}

#[cfg(unix)]
#[test]
fn check_calibration_uses_the_slowest_default_route() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    for args in [
        &["init"][..],
        &["config", "user.email", "test@example.com"],
        &["config", "user.name", "Test"],
    ] {
        std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
    }
    fs::write(
        root.join("a.rs"),
        "pub fn compare(a: i32, b: i32) -> bool { a >= b }\n",
    )
    .unwrap();
    fs::write(
        root.join("z.go"),
        "package sample\n\nfunc compare(a, b int) bool { return a >= b }\n",
    )
    .unwrap();
    fs::write(
        root.join("togi.toml"),
        r#"
[test]
command = ["true"]
timeout = 1
calibrate_timeout = true
timeout_multiplier = 1.0
timeout_slack = 1

[test.languages.go]
command = ["sh", "-c", "sleep 2"]
"#,
    )
    .unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(root)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "initial"])
        .current_dir(root)
        .output()
        .unwrap();
    fs::write(
        root.join("a.rs"),
        "pub fn compare(a: i32, b: i32) -> bool { a > b }\n",
    )
    .unwrap();
    fs::write(
        root.join("z.go"),
        "package sample\n\nfunc compare(a, b int) bool { return a > b }\n",
    )
    .unwrap();

    let output = togi()
        .args([
            "check",
            "--base",
            "HEAD",
            "--format",
            "json",
            "--max-per-run",
            "2",
            "--no-schemata",
            "--operators",
            "gt_to_gte",
            "--force-rerun",
            "--no-incremental-history",
            "--fail-under",
            "0",
        ])
        .current_dir(root)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["timeout"], 0);
    assert!(
        value["baseline_timing"]["calibrated_timeout_ms"]
            .as_u64()
            .is_some_and(|timeout| timeout >= 3_000)
    );
}

#[cfg(unix)]
#[test]
fn check_calibration_uses_a_slow_project_route_without_an_explicit_timeout() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    for args in [
        &["init"][..],
        &["config", "user.email", "test@example.com"],
        &["config", "user.name", "Test"],
    ] {
        std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
    }
    fs::create_dir_all(root.join("zproject")).unwrap();
    fs::write(
        root.join("a.rs"),
        "pub fn compare(a: i32, b: i32) -> bool { a >= b }\n",
    )
    .unwrap();
    fs::write(
        root.join("zproject/main.go"),
        "package sample\n\nfunc compare(a, b int) bool { return a >= b }\n",
    )
    .unwrap();
    fs::write(
        root.join("togi.toml"),
        r#"
[test]
command = ["true"]
timeout = 1
calibrate_timeout = true
timeout_multiplier = 1.0
timeout_slack = 1

[projects.api]
path = "zproject"

[projects.api.test]
command = ["sh", "-c", "sleep 2"]
"#,
    )
    .unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(root)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "initial"])
        .current_dir(root)
        .output()
        .unwrap();
    fs::write(
        root.join("a.rs"),
        "pub fn compare(a: i32, b: i32) -> bool { return a > b }\n",
    )
    .unwrap();
    fs::write(
        root.join("zproject/main.go"),
        "package sample\n\nfunc compare(a, b int) bool { return a > b }\n",
    )
    .unwrap();

    let output = togi()
        .args([
            "check",
            "--base",
            "HEAD",
            "--format",
            "json",
            "--max-per-run",
            "2",
            "--no-schemata",
            "--operators",
            "gt_to_gte",
            "--force-rerun",
            "--no-incremental-history",
            "--fail-under",
            "0",
        ])
        .current_dir(root)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["timeout"], 0);
    assert!(
        value["baseline_timing"]["calibrated_timeout_ms"]
            .as_u64()
            .is_some_and(|timeout| timeout >= 3_000)
    );
}

#[cfg(unix)]
#[test]
fn check_baselines_global_language_and_project_routes() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    std::process::Command::new("git")
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("services/api")).unwrap();
    fs::write(
        root.join("src/lib.rs"),
        "pub fn compare(a: i32, b: i32) -> bool { a > b }\n",
    )
    .unwrap();
    fs::write(
        root.join("worker.go"),
        "package worker\n\nfunc compare(a, b int) bool { return a > b }\n",
    )
    .unwrap();
    fs::write(
        root.join("services/api/main.go"),
        "package api\n\nfunc compare(a, b int) bool { return a > b }\n",
    )
    .unwrap();
    fs::write(
        root.join("togi.toml"),
        r#"
[test]
command = ["sh", "-c", 'if grep -Fq "a > b" src/lib.rs; then echo global >> "$TOGI_TEST_LOG"; fi; exit 0']

[test.languages.go]
command = ["sh", "-c", 'if grep -Fq "a > b" worker.go; then echo language >> "$TOGI_TEST_LOG"; fi; exit 0']

[projects.api]
path = "services/api"

[projects.api.test]
command = ["sh", "-c", 'if grep -Fq "a > b" services/api/main.go; then echo project >> "$TOGI_TEST_LOG"; fi; exit 0']
"#,
    )
    .unwrap();
    let log = root.join("baseline-routes.log");

    let output = togi()
        .args([
            "check",
            "--all",
            "--max-per-run",
            "3",
            "--no-schemata",
            "--operators",
            "gt_to_gte",
            "--force-rerun",
            "--no-incremental-history",
            "--fail-under",
            "0",
        ])
        .env("TOGI_TEST_LOG", &log)
        .current_dir(root)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let mut baseline_routes = fs::read_to_string(log)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    baseline_routes.sort();
    assert_eq!(baseline_routes, vec!["global", "language", "project"]);
}

#[cfg(unix)]
#[test]
fn check_aborts_before_mutation_work_when_a_later_project_baseline_fails() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    std::process::Command::new("git")
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    fs::create_dir_all(root.join("zproject")).unwrap();
    fs::write(
        root.join("a.rs"),
        "pub fn compare(a: i32, b: i32) -> bool { a > b }\n",
    )
    .unwrap();
    fs::write(
        root.join("m.go"),
        "package worker\n\nfunc compare(a, b int) bool { return a > b }\n",
    )
    .unwrap();
    fs::write(
        root.join("zproject/main.go"),
        "package api\n\nfunc compare(a, b int) bool { return a > b }\n",
    )
    .unwrap();
    fs::write(
        root.join("togi.toml"),
        r#"
[test]
command = ["sh", "-c", 'if grep -Fq "a > b" a.rs; then echo global >> "$TOGI_TEST_LOG"; fi; exit 0']

[test.languages.go]
command = ["sh", "-c", 'if grep -Fq "a > b" m.go; then echo language >> "$TOGI_TEST_LOG"; fi; exit 0']

[projects.api]
path = "zproject"

[projects.api.test]
command = ["sh", "-c", 'if grep -Fq "a > b" zproject/main.go; then echo project-failed >> "$TOGI_TEST_LOG"; exit 1; fi; exit 0']
"#,
    )
    .unwrap();
    let log = root.join("baseline-routes.log");

    let output = togi()
        .args([
            "check",
            "--all",
            "--max-per-run",
            "3",
            "--no-schemata",
            "--operators",
            "gt_to_gte",
            "--force-rerun",
            "--no-incremental-history",
            "--fail-under",
            "0",
            "--format",
            "json",
        ])
        .env("TOGI_TEST_LOG", &log)
        .current_dir(root)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("baseline test command failed"), "{stderr}");
    assert!(!stderr.contains("Running 3 mutations"));
    assert!(!stdout.contains("\"killed\""));
    assert!(!stdout.contains("mutation_score"));
    assert!(!root.join(".togi-cache").exists());
    assert_eq!(
        fs::read_to_string(log).unwrap().lines().collect::<Vec<_>>(),
        vec!["global", "language", "project-failed"]
    );
}

#[test]
fn check_format_sarif_outputs_valid_sarif() {
    let dir = setup_git_repo();

    // `true` lets every mutant survive so the SARIF report has results.
    let output = togi()
        .args([
            "check",
            "--base",
            "HEAD",
            "--format",
            "sarif",
            "--test-cmd",
            "true",
            "--no-schemata",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "invalid SARIF output: {e}\nstdout: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    assert_eq!(value["version"], "2.1.0");
    let run = &value["runs"][0];
    assert_eq!(run["tool"]["driver"]["name"], "togi");
    assert!(run["invocations"][0]["properties"]["mutation_score"].is_number());

    let results = run["results"].as_array().unwrap();
    assert!(!results.is_empty(), "expected surviving mutant results");
    assert_eq!(results[0]["level"], "warning");
    let location = &results[0]["locations"][0]["physicalLocation"];
    assert_eq!(location["artifactLocation"]["uri"], "main.go");
    assert!(location["region"]["startLine"].as_u64().unwrap() >= 1);
}

#[cfg(unix)]
#[test]
fn check_aborts_before_report_when_baseline_build_fails() {
    let dir = setup_git_repo();

    let output = togi()
        .args([
            "check",
            "--base",
            "HEAD",
            "--format",
            "json",
            "--test-cmd",
            "true",
            "--build-cmd",
            "false",
            "--no-schemata",
            "--operators",
            "gt_to_gte",
            "--max-per-run",
            "1",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(2),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("baseline build command failed (`false`)"));
    assert!(!stderr.contains("Running 1 mutations"));
    assert!(!stdout.contains("\"killed\""));
    assert!(!stdout.contains("mutation_score"));
    assert!(!dir.path().join(".togi-cache").exists());
}

#[cfg(unix)]
#[test]
fn check_terminal_includes_build_error_diagnostics() {
    let dir = setup_git_repo();
    let expected = tempfile::NamedTempFile::new().unwrap();
    fs::write(
        expected.path(),
        fs::read(dir.path().join("main.go")).unwrap(),
    )
    .unwrap();
    let failing_command = format!(
        "{} not-a-togi-command",
        shell_quote(&assert_cmd::cargo::cargo_bin("togi"))
    );
    let build_cmd = format!(
        "sh -c {}",
        shell_quote_text(&format!(
            "if cmp -s main.go {}; then exit 0; else exec {failing_command}; fi",
            shell_quote(expected.path())
        ))
    );

    togi()
        .args([
            "check",
            "--base",
            "HEAD",
            "--test-cmd",
            "true",
            "--build-cmd",
            &build_cmd,
            "--no-schemata",
            "--operators",
            "gt_to_gte",
            "--max-per-run",
            "1",
            "--jobs",
            "1",
        ])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Build error diagnostics:"))
        .stdout(predicate::str::contains("build_command"))
        .stdout(predicate::str::contains("not-a-togi-command"));
}

fn write_baseline(dir: &Path, killed: usize, total: usize) {
    fs::write(
        dir.join(".togi-baseline"),
        format!(
            r#"{{
  "files": {{
    "main.go": {{
      "killed": {killed},
      "total": {total}
    }}
  }},
  "killed": {killed},
  "total": {total}
}}"#
        ),
    )
    .unwrap();
}

#[test]
fn check_baseline_allows_existing_survivors() {
    let dir = setup_git_repo();
    write_baseline(dir.path(), 0, 1);

    let output = togi()
        .args([
            "check",
            "--base",
            "HEAD",
            "--format",
            "json",
            "--test-cmd",
            "true",
            "--check-baseline",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "baseline check failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "invalid JSON output: {e}\nstdout: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    assert!(value["survived"].as_u64().unwrap() > 0);
}

#[test]
fn check_baseline_fails_on_regression() {
    let dir = setup_git_repo();
    write_baseline(dir.path(), 1, 1);

    togi()
        .args([
            "check",
            "--base",
            "HEAD",
            "--test-cmd",
            "true",
            "--check-baseline",
        ])
        .current_dir(dir.path())
        .assert()
        .code(1)
        .stderr(predicate::str::contains(
            "Mutation score regression detected",
        ));
}

#[test]
fn check_baseline_still_honors_fail_under() {
    let dir = setup_git_repo();
    write_baseline(dir.path(), 0, 1);

    togi()
        .args([
            "check",
            "--base",
            "HEAD",
            "--test-cmd",
            "true",
            "--check-baseline",
            "--fail-under",
            "1",
        ])
        .current_dir(dir.path())
        .assert()
        .code(1)
        .stderr(predicate::str::contains("below --fail-under threshold"));
}

#[cfg(unix)]
#[test]
fn interrupted_mutation_exits_130_without_a_report_or_side_effects() {
    let dir = setup_git_repo();
    let baseline_marker = dir.path().join("baseline-marker");
    let mutation_marker = dir.path().join("mutation-marker");
    let release = dir.path().join("mutation-release");
    let stdout_path = dir.path().join("togi-stdout.log");
    let stderr_path = dir.path().join("togi-stderr.log");
    let pr_comment_path = dir.path().join("togi-pr-comment.md");
    let baseline_path = dir.path().join(".togi-baseline");
    let cache_path = dir.path().join(".togi-cache");
    let test_script = dir.path().join("block-mutant.sh");
    fs::write(
        &test_script,
        format!(
            "#!/bin/sh\n\
             baseline={}\n\
             mutation={}\n\
             release={}\n\
             if grep -Fq 'if a > b' main.go; then\n\
             \tprintf baseline > \"$baseline\"\n\
             \texit 0\n\
             fi\n\
             printf mutation > \"$mutation\"\n\
             while [ ! -f \"$release\" ]; do\n\
             \tsleep 0.05\n\
             done\n",
            shell_quote(&baseline_marker),
            shell_quote(&mutation_marker),
            shell_quote(&release),
        ),
    )
    .unwrap();

    let stdout = fs::File::create(&stdout_path).unwrap();
    let stderr = fs::File::create(&stderr_path).unwrap();
    let test_command = format!("sh {}", shell_quote(&test_script));
    // foxguard: ignore[rs/no-command-injection]
    // The test launches the just-built `togi` binary from Cargo metadata.
    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("togi"))
        .args([
            "check",
            "--all",
            "--path",
            "main.go",
            "--max-per-run",
            "1",
            "--no-schemata",
            "--operators",
            "gt_to_gte",
            "--test-cmd",
            &test_command,
            "--timeout",
            "5",
            "--force-rerun",
            "--no-incremental-history",
            "--fail-under",
            "0",
            "--format",
            "json",
            "--save-baseline",
            "--pr-comment",
            pr_comment_path
                .to_str()
                .expect("PR comment path should be utf-8"),
        ])
        .current_dir(dir.path())
        .process_group(0)
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .unwrap();

    if !wait_for_path(&baseline_marker, Duration::from_secs(10))
        || !wait_for_path(&mutation_marker, Duration::from_secs(10))
    {
        send_signal_to_process_group(child.id(), libc::SIGKILL);
        let _ = child.wait();
        panic!(
            "baseline or mutation command did not start\nstdout:\n{}\nstderr:\n{}",
            read_log(&stdout_path),
            read_log(&stderr_path)
        );
    }

    send_signal_to_process_group(child.id(), libc::SIGINT);
    fs::write(&release, "").unwrap();
    let status = wait_for_child(&mut child, Duration::from_secs(10)).unwrap_or_else(|message| {
        send_signal_to_process_group(child.id(), libc::SIGKILL);
        let _ = child.wait();
        panic!(
            "{message}\nstdout:\n{}\nstderr:\n{}",
            read_log(&stdout_path),
            read_log(&stderr_path)
        );
    });

    assert_eq!(
        status.code(),
        Some(130),
        "interrupted mutation should exit 130\nstatus: {status:?}\nstdout:\n{}\nstderr:\n{}",
        read_log(&stdout_path),
        read_log(&stderr_path)
    );
    let stdout = read_log(&stdout_path);
    assert!(!stdout.contains("mutation_score"));
    assert!(!stdout.contains("\"total\""));
    assert!(!stdout.contains("\"killed\""));
    assert!(!cache_path.exists(), "interrupted mutation wrote a cache");
    assert!(
        !baseline_path.exists(),
        "interrupted mutation wrote a baseline"
    );
    assert!(
        !pr_comment_path.exists(),
        "interrupted mutation wrote a PR comment"
    );
}

#[cfg(unix)]
#[test]
fn interrupted_check_exits_130_without_writing_side_effects() {
    let dir = setup_git_repo();
    let marker = dir.path().join("test-command-started");
    let release = dir.path().join("test-command-release");
    let stdout_path = dir.path().join("togi-stdout.log");
    let stderr_path = dir.path().join("togi-stderr.log");
    let pr_comment_path = dir.path().join("togi-pr-comment.md");
    let baseline_path = dir.path().join(".togi-baseline");
    let cache_path = dir.path().join(".togi-cache");
    let slow_test = dir.path().join("slow-test.sh");

    fs::write(
        &slow_test,
        format!(
            "#!/bin/sh\n\
             marker={}\n\
             release={}\n\
             printf started > \"$marker\"\n\
             while [ ! -f \"$release\" ]; do\n\
             \tsleep 0.05\n\
             done\n",
            shell_quote(&marker),
            shell_quote(&release)
        ),
    )
    .unwrap();

    let stdout = fs::File::create(&stdout_path).unwrap();
    let stderr = fs::File::create(&stderr_path).unwrap();
    // foxguard: ignore[rs/no-command-injection]
    // The test launches the just-built `togi` binary from Cargo metadata.
    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("togi"))
        .args([
            "check",
            "--base",
            "HEAD",
            "--test-cmd",
            &format!("sh {}", shell_quote(&slow_test)),
            "--timeout",
            "5",
            "--jobs",
            "1",
            "--save-baseline",
            "--pr-comment",
            pr_comment_path
                .to_str()
                .expect("PR comment path should be utf-8"),
        ])
        .current_dir(dir.path())
        .process_group(0)
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .unwrap();

    if !wait_for_path(&marker, Duration::from_secs(10)) {
        send_signal_to_process_group(child.id(), libc::SIGKILL);
        let _ = child.wait();
        panic!(
            "test command did not start\nstdout:\n{}\nstderr:\n{}",
            read_log(&stdout_path),
            read_log(&stderr_path)
        );
    }

    send_signal_to_process_group(child.id(), libc::SIGINT);
    let _ = wait_for_file_contains(&stderr_path, "Interrupted", Duration::from_secs(1));
    fs::write(&release, "").unwrap();

    let status = wait_for_child(&mut child, Duration::from_secs(10)).unwrap_or_else(|message| {
        send_signal_to_process_group(child.id(), libc::SIGKILL);
        let _ = child.wait();
        panic!(
            "{message}\nstdout:\n{}\nstderr:\n{}",
            read_log(&stdout_path),
            read_log(&stderr_path)
        );
    });

    assert_eq!(
        status.code(),
        Some(130),
        "interrupted check should exit 130\nstatus: {status:?}\nstdout:\n{}\nstderr:\n{}",
        read_log(&stdout_path),
        read_log(&stderr_path)
    );
    assert!(
        !baseline_path.exists(),
        "interrupted check wrote {}",
        baseline_path.display()
    );
    assert!(
        !pr_comment_path.exists(),
        "interrupted check wrote {}",
        pr_comment_path.display()
    );
    assert!(
        !cache_path.exists(),
        "interrupted check wrote {}",
        cache_path.display()
    );
    let stdout = read_log(&stdout_path);
    assert!(!stdout.contains("mutation_score"));
    assert!(!stdout.contains("\"killed\""));
}

#[cfg(unix)]
fn shell_quote(path: &Path) -> String {
    let value = path.to_str().expect("test path should be utf-8");
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(unix)]
fn shell_quote_text(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(unix)]
fn fake_go_path_env(fake_bin: &Path) -> std::ffi::OsString {
    fs::create_dir_all(fake_bin).unwrap();
    let fake_go = fake_bin.join("go");
    fs::write(
        &fake_go,
        r#"#!/bin/sh
if [ "$1" = "test" ] && [ "$2" = "-count=1" ]; then
    printf no-cache >> "$TOGI_GO_LOG"
    exit 1
fi
printf raw >> "$TOGI_GO_LOG"
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&fake_go).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_go, permissions).unwrap();

    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![fake_bin.to_path_buf()];
    paths.extend(std::env::split_paths(&inherited));
    std::env::join_paths(paths).unwrap()
}

#[cfg(unix)]
fn wait_for_path(path: &Path, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

#[cfg(unix)]
fn wait_for_file_contains(path: &Path, needle: &str, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if fs::read_to_string(path).is_ok_and(|content| content.contains(needle)) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

#[cfg(unix)]
fn wait_for_child(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Result<std::process::ExitStatus, String> {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return Ok(status);
        }
        if start.elapsed() >= timeout {
            return Err(format!("togi did not exit within {timeout:?}"));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(unix)]
fn send_signal_to_process_group(pid: u32, signal: libc::c_int) {
    unsafe {
        let _ = libc::killpg(pid as libc::pid_t, signal);
    }
}

#[cfg(unix)]
fn read_log(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

#[test]
fn check_help_lists_test_selection_file_flag() {
    togi()
        .args(["check", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--test-selection-file"));
}

#[test]
fn github_action_run_helper_preserves_multiword_test_cmd() {
    if !bash_available() {
        eprintln!("skipping action helper test because bash is unavailable");
        return;
    }

    assert_action_helper_test_cmd("go test ./...");
    assert_action_helper_test_cmd("cargo test --workspace --all-features");
}

fn assert_action_helper_test_cmd(test_cmd: &str) {
    let dir = TempDir::new().unwrap();
    let fake_togi = dir.path().join("fake-togi.sh");
    fs::write(&fake_togi, "#!/usr/bin/env bash\nprintf '<%s>\\n' \"$@\"\n").unwrap();
    std::process::Command::new("bash")
        .args(["-c", "chmod +x \"$1\"", "--"])
        .arg(&fake_togi)
        .output()
        .unwrap();

    let helper = Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/scripts/run-togi.sh");
    let output = std::process::Command::new("bash")
        .arg(helper)
        .env("TOGI_BIN", &fake_togi)
        .env("TOGI_BASE", "HEAD~1")
        .env("TOGI_TIMEOUT", "45")
        .env("TOGI_FORMAT", "json")
        .env("TOGI_TEST_CMD", test_cmd)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "helper failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let args: Vec<String> = stdout.lines().map(String::from).collect();
    assert_eq!(
        args,
        vec![
            "<check>".to_string(),
            "<--base>".to_string(),
            "<HEAD~1>".to_string(),
            "<--timeout>".to_string(),
            "<45>".to_string(),
            "<--format>".to_string(),
            "<json>".to_string(),
            "<--test-cmd>".to_string(),
            format!("<{test_cmd}>"),
        ]
    );
}

#[test]
fn github_action_asset_resolver_matches_release_assets() {
    if !bash_available() {
        eprintln!("skipping action asset resolver test because bash is unavailable");
        return;
    }

    assert_eq!(
        resolve_action_asset("Linux", "x86_64"),
        action_asset("togi-linux-x86_64.tar.gz", "togi")
    );
    assert_eq!(
        resolve_action_asset("Darwin", "arm64"),
        action_asset("togi-macos-arm64.tar.gz", "togi")
    );
    assert_eq!(
        resolve_action_asset("Darwin", "x86_64"),
        action_asset("togi-macos-x86_64.tar.gz", "togi")
    );
    assert_eq!(
        resolve_action_asset("MINGW64_NT-10.0", "AMD64"),
        action_asset("togi-windows-x86_64.zip", "togi.exe")
    );
}

#[test]
fn github_action_install_steps_place_binary_and_update_github_path() {
    if !bash_available() {
        eprintln!("skipping action install test because bash is unavailable");
        return;
    }

    assert_action_installs_asset(&resolve_action_asset("Linux", "x86_64"), false);
    assert_action_installs_asset(
        &resolve_action_asset("MINGW64_NT-10.0", "AMD64"),
        !cfg!(windows),
    );
}

#[derive(Debug, PartialEq, Eq)]
struct ActionAsset {
    archive: String,
    binary: String,
}

fn action_asset(archive: &str, binary: &str) -> ActionAsset {
    ActionAsset {
        archive: archive.to_string(),
        binary: binary.to_string(),
    }
}

fn resolve_action_asset(os: &str, arch: &str) -> ActionAsset {
    let helper =
        Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/scripts/resolve-togi-asset.sh");
    let output = std::process::Command::new("bash")
        .arg(helper)
        .env("TOGI_OS", os)
        .env("TOGI_ARCH", arch)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "resolver failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut archive = None;
    let mut binary = None;
    for line in stdout.lines() {
        if let Some(value) = line.strip_prefix("TOGI_ARCHIVE=") {
            archive = Some(value.to_string());
        } else if let Some(value) = line.strip_prefix("TOGI_BINARY=") {
            binary = Some(value.to_string());
        } else {
            panic!("unexpected resolver output line: {line}");
        }
    }

    ActionAsset {
        archive: archive.expect("resolver did not emit TOGI_ARCHIVE"),
        binary: binary.expect("resolver did not emit TOGI_BINARY"),
    }
}

fn assert_action_installs_asset(asset: &ActionAsset, fake_cygpath: bool) {
    let dir = TempDir::new().unwrap();
    let payload_dir = dir.path().join("payload");
    fs::create_dir_all(&payload_dir).unwrap();
    fs::write(
        payload_dir.join(&asset.binary),
        "#!/usr/bin/env bash\nexit 0\n",
    )
    .unwrap();
    create_action_archive(asset, &payload_dir, &dir.path().join(&asset.archive));

    let github_path = dir.path().join("github_path");
    fs::write(&github_path, "").unwrap();

    let mut command = std::process::Command::new("bash");
    command
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/scripts/install-togi-archive.sh"))
        .env("RUNNER_TEMP", dir.path())
        .env("TOGI_ARCHIVE", &asset.archive)
        .env("TOGI_BINARY", &asset.binary)
        .env("GITHUB_PATH", &github_path);

    if fake_cygpath {
        let fake_bin = dir.path().join("fake-bin");
        fs::create_dir_all(&fake_bin).unwrap();
        let fake_cygpath_path = fake_bin.join("cygpath");
        fs::write(
            &fake_cygpath_path,
            "#!/usr/bin/env bash\ncase \"$1\" in\n  -u) printf '%s\\n' \"$2\" ;;\n  -w) printf 'C:\\\\togi\\\\%s\\n' \"$(basename \"$2\")\" ;;\n  *) exit 1 ;;\nesac\n",
        )
        .unwrap();
        chmod_executable(&fake_cygpath_path);

        let mut paths = vec![fake_bin];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        command.env("PATH", std::env::join_paths(paths).unwrap());
    }

    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "install helper failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let install_dir = dir.path().join("togi-bin");
    let installed = install_dir.join(&asset.binary);
    assert!(
        installed.exists(),
        "missing installed binary: {installed:?}"
    );
    assert_bash_test_x(&installed);

    let github_path_entry = fs::read_to_string(&github_path).unwrap();
    let github_path_entry = github_path_entry.trim();
    if fake_cygpath {
        assert_eq!(github_path_entry, "C:\\togi\\togi-bin");
    } else if cfg!(windows) {
        assert!(
            github_path_entry.contains("\\togi-bin") && !github_path_entry.starts_with('/'),
            "expected Windows GITHUB_PATH entry, got {github_path_entry}"
        );
    } else {
        assert_eq!(github_path_entry, install_dir.display().to_string());
    }
}

fn create_action_archive(asset: &ActionAsset, payload_dir: &Path, archive_path: &Path) {
    if asset.archive.ends_with(".tar.gz") {
        assert_command_success(
            std::process::Command::new("tar")
                .arg("czf")
                .arg(archive_path)
                .arg("-C")
                .arg(payload_dir)
                .arg(&asset.binary)
                .output()
                .unwrap(),
            "tar archive creation",
        );
    } else {
        create_zip_archive(payload_dir, archive_path, &asset.binary);
    }
}

fn create_zip_archive(payload_dir: &Path, archive_path: &Path, binary: &str) {
    if cfg!(windows) {
        assert_command_success(
            std::process::Command::new("powershell")
                .args([
                    "-NoProfile",
                    "-Command",
                    "Compress-Archive -LiteralPath $env:TOGI_TEST_BINARY -DestinationPath $env:TOGI_TEST_ARCHIVE",
                ])
                .env("TOGI_TEST_BINARY", payload_dir.join(binary))
                .env("TOGI_TEST_ARCHIVE", archive_path)
                .output()
                .unwrap(),
            "zip archive creation",
        );
    } else {
        assert_command_success(
            std::process::Command::new("zip")
                .arg("-q")
                .arg(archive_path)
                .arg(binary)
                .current_dir(payload_dir)
                .output()
                .unwrap(),
            "zip archive creation",
        );
    }
}

fn chmod_executable(path: &Path) {
    assert_command_success(
        std::process::Command::new("bash")
            .arg("-c")
            .arg("chmod +x \"$1\"")
            .arg("chmod")
            .arg(path)
            .output()
            .unwrap(),
        "chmod +x",
    );
}

fn assert_bash_test_x(path: &Path) {
    assert_command_success(
        std::process::Command::new("bash")
            .arg("-c")
            .arg("path=$1; if command -v cygpath >/dev/null 2>&1; then path=$(cygpath -u \"$path\"); fi; test -x \"$path\"")
            .arg("test")
            .arg(path)
            .output()
            .unwrap(),
        "test -x",
    );
}

fn assert_command_success(output: std::process::Output, context: &str) {
    assert!(
        output.status.success(),
        "{context} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn explain_reads_json_report() {
    let dir = TempDir::new().unwrap();
    let report = r#"{
  "total": 2,
  "tested": 2,
  "killed": 1,
  "survived": 1,
  "timeout": 0,
  "build_errors": 0,
  "mutation_score": 0.0,
  "duration_ms": 10,
  "test_command": ["cargo", "test"],
  "build_command": ["cargo", "check"],
  "mutations": [
    {
      "id": 1,
      "file": "src/main.go",
      "line": 3,
      "operator": "plus_to_minus",
      "description": "Replace + with -",
      "result": "killed",
      "original": "+",
      "replacement": "-"
    },
    {
      "id": 2,
      "file": "src/main.go",
      "line": 4,
      "operator": "gt_to_gte",
      "description": "Replace > with >=",
      "result": "survived",
      "original": ">",
      "replacement": ">=",
      "diff": "--- a/src/main.go\n+++ b/src/main.go\n@@ -1 +1 @@\n-a > b\n+a >= b\n"
    }
  ]
}"#;
    let report_path = dir.path().join("togi-report.json");
    fs::write(&report_path, report).unwrap();

    togi()
        .args([
            "explain",
            "2",
            "--report",
            report_path.to_str().expect("report path should be utf-8"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Mutation #2"))
        .stdout(predicate::str::contains("src/main.go:4"))
        .stdout(predicate::str::contains(
            r#"Test command: ["cargo","test"]"#,
        ))
        .stdout(predicate::str::contains(
            r#"Build check: ["cargo","check"]"#,
        ))
        .stdout(predicate::str::contains("Why it survived"));

    togi()
        .args([
            "explain",
            "1",
            "--report",
            report_path.to_str().expect("report path should be utf-8"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Change: + -> -"))
        .stdout(predicate::str::contains("Why it was killed"));
}

#[test]
fn check_invalid_config_exits_two() {
    let dir = setup_git_repo();
    fs::write(dir.path().join("togi.toml"), "invalid {{{{ toml").unwrap();

    togi()
        .args(["check", "--base", "HEAD"])
        .current_dir(dir.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("Error"));
}

#[test]
fn clean_removes_cache_dir() {
    let dir = setup_git_repo();
    let cache_dir = dir.path().join(".togi-cache");
    fs::create_dir_all(&cache_dir).unwrap();
    fs::write(cache_dir.join("entry-1"), "{}").unwrap();
    fs::write(cache_dir.join("entry-2"), "{}").unwrap();
    assert!(cache_dir.exists());

    togi()
        .arg("clean")
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Cache cleared"));

    assert!(!cache_dir.exists(), ".togi-cache should be removed");
}

#[test]
fn clean_succeeds_when_cache_missing() {
    let dir = setup_git_repo();
    assert!(!dir.path().join(".togi-cache").exists());

    togi()
        .arg("clean")
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Cache cleared"));
}

#[test]
fn list_operators_prints_known_ids_and_categories() {
    let output = togi().arg("list-operators").assert().success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();

    // Known operator IDs (one per category) should appear in output.
    for id in [
        "lt_to_lte",        // binary
        "true_to_false",    // literal
        "plus_to_minus",    // boundary
        "remove_if_body",   // removal
        "remove_unary_not", // unary
        "remove_break",     // loop
        "negate_condition", // negate
        "return_empty",     // return
    ] {
        assert!(
            stdout.contains(id),
            "expected list-operators output to contain '{id}', got:\n{stdout}"
        );
    }

    // All expected category headers should appear.
    for cat in [
        "binary:",
        "literal:",
        "boundary:",
        "removal:",
        "unary:",
        "loop:",
        "negate:",
        "return:",
    ] {
        assert!(
            stdout.contains(cat),
            "expected list-operators output to contain category '{cat}', got:\n{stdout}"
        );
    }
}
