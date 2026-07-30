use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
#[cfg(unix)]
use std::os::unix::{fs::PermissionsExt, process::CommandExt};
use std::path::{Path, PathBuf};
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

fn setup_ambiguous_runtime_repo() -> TempDir {
    let dir = setup_git_repo();
    fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"example\"\n",
    )
    .unwrap();
    dir
}

#[test]
fn check_rejects_ambiguous_automatic_test_command() {
    let dir = setup_ambiguous_runtime_repo();

    togi()
        .args(["check", "--base", "not-a-valid-revision"])
        .current_dir(dir.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "multiple test runtimes detected (Cargo.toml, go.mod)",
        ))
        .stderr(predicate::str::contains("[test] command"))
        .stderr(predicate::str::contains("--test-cmd"));

    assert!(!dir.path().join(".togi.lock").exists());
}

#[test]
fn check_rejects_empty_language_route_before_baseline_or_cache() {
    let dir = setup_ambiguous_runtime_repo();
    fs::write(
        dir.path().join("togi.toml"),
        r#"
[test.languages.go]
command = []
"#,
    )
    .unwrap();

    togi()
        .args(["check", "--base", "not-a-valid-revision"])
        .current_dir(dir.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "multiple test runtimes detected (Cargo.toml, go.mod)",
        ))
        .stderr(predicate::str::contains("[test] command"));

    assert!(!dir.path().join(".togi.lock").exists());
    assert!(!dir.path().join(".togi-cache").exists());
}

#[test]
fn check_rejects_unrouted_path_when_ambiguity_is_deferred() {
    let dir = setup_ambiguous_runtime_repo();
    fs::write(
        dir.path().join("togi.toml"),
        r#"
[test.languages.rust]
command = ["true"]
"#,
    )
    .unwrap();

    togi()
        .args(["check", "--base", "HEAD"])
        .current_dir(dir.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "multiple test runtimes detected (Cargo.toml, go.mod)",
        ))
        .stderr(predicate::str::contains(
            "main.go has no explicit project or language test command",
        ));

    assert!(!dir.path().join(".togi-cache").exists());
}

#[cfg(unix)]
#[test]
fn check_rejects_empty_project_route_before_baseline_or_cache() {
    let dir = setup_ambiguous_runtime_repo();
    let log = dir.path().join("language-route.log");
    fs::write(
        dir.path().join("togi.toml"),
        r#"
[test.languages.go]
command = ["sh", "-c", 'echo language-route >> "$TOGI_TEST_LOG"']

[projects.main]
path = "main.go"

[projects.main.test]
command = []
"#,
    )
    .unwrap();

    togi()
        .args(["check", "--base", "HEAD"])
        .env("TOGI_TEST_LOG", &log)
        .current_dir(dir.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "multiple test runtimes detected (Cargo.toml, go.mod)",
        ))
        .stderr(predicate::str::contains(
            "main.go has no explicit project or language test command",
        ));

    assert!(!log.exists());
    assert!(!dir.path().join(".togi-cache").exists());
}

#[test]
fn check_cli_test_command_overrides_ambiguous_automatic_detection() {
    let dir = setup_ambiguous_runtime_repo();

    togi()
        .args(["check", "--base", "HEAD", "--dry-run", "--test-cmd", "true"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("mutations would be generated"));
}

#[test]
fn check_config_test_command_overrides_ambiguous_automatic_detection() {
    let dir = setup_ambiguous_runtime_repo();
    fs::write(
        dir.path().join("togi.toml"),
        r#"
[test]
command = ["true"]
"#,
    )
    .unwrap();

    togi()
        .args(["check", "--base", "HEAD", "--dry-run"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("mutations would be generated"));
}

#[cfg(unix)]
#[test]
fn check_uses_explicit_routes_in_ambiguous_mixed_runtime_repo() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    std::process::Command::new("git")
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    fs::create_dir_all(root.join("rust/src")).unwrap();
    fs::create_dir_all(root.join("go")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"mixed\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    fs::write(root.join("go.mod"), "module example.com/mixed\n\ngo 1.21\n").unwrap();
    fs::write(
        root.join("rust/src/lib.rs"),
        "pub fn compare(a: i32, b: i32) -> bool { a > b }\n",
    )
    .unwrap();
    fs::write(
        root.join("go/main.go"),
        "package sample\n\nfunc compare(a, b int) bool { return a > b }\n",
    )
    .unwrap();
    fs::write(
        root.join("togi.toml"),
        r#"
[test.languages.rust]
command = ["sh", "-c", 'echo rust-route >> "$TOGI_TEST_LOG"']

[projects.go]
path = "go"

[projects.go.test]
command = ["sh", "-c", 'echo project-route >> "$TOGI_TEST_LOG"']
"#,
    )
    .unwrap();
    let log = root.join("routes.log");

    let output = togi()
        .args([
            "check",
            "--all",
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
    let routes = fs::read_to_string(log).unwrap();
    assert!(routes.lines().any(|route| route == "rust-route"));
    assert!(routes.lines().any(|route| route == "project-route"));
}

#[cfg(unix)]
#[test]
fn check_uses_absolute_nested_project_route_in_ambiguous_mixed_runtime_repo() {
    let dir = setup_ambiguous_runtime_repo();
    let root = dir.path();
    let services = root.join("services");
    let api = services.join("api");
    fs::create_dir_all(&api).unwrap();
    fs::write(
        api.join("main.go"),
        "package api\n\nfunc compare(a, b int) bool { return a > b }\n",
    )
    .unwrap();
    fs::write(
        root.join("togi.toml"),
        format!(
            r#"
[test.languages.go]
command = ["sh", "-c", 'echo language-route >> "$TOGI_TEST_LOG"']

[projects.services]
path = "{}"

[projects.services.test]
command = ["sh", "-c", 'echo services-route >> "$TOGI_TEST_LOG"']

[projects.api]
path = "{}"

[projects.api.test]
command = ["sh", "-c", 'echo api-route >> "$TOGI_TEST_LOG"']
"#,
            services.display(),
            api.display()
        ),
    )
    .unwrap();
    let log = root.join("routes.log");

    let output = togi()
        .args([
            "check",
            "--all",
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
    let routes = fs::read_to_string(log).unwrap();
    assert!(routes.lines().any(|route| route == "language-route"));
    assert!(routes.lines().any(|route| route == "api-route"));
    assert!(!routes.lines().any(|route| route == "services-route"));
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
            "--test-cmd",
            "false",
            "--coverage-file",
            "lcov.info",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Test suite failure before mutation execution."));
    assert!(stderr.contains("Baseline phase: test"));
    assert!(stderr.contains("Outcome: failed"));
    assert!(stderr.contains("baseline test command failed (`false`)"));
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
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("baseline build command failed (`false`)"));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let failure: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("invalid suite-failure JSON: {error}\nstdout:\n{stdout}"));
    assert_eq!(failure["kind"], "suite_failure");
    assert_eq!(failure["phase"], "build");
    assert_eq!(failure["command"], serde_json::json!(["false"]));
    assert_eq!(failure["outcome"], "failed");
    assert!(failure.get("mutations").is_none());
    assert!(failure.get("mutation_score").is_none());
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
fn check_format_json_outputs_standalone_json_for_explain() {
    let dir = setup_git_repo();

    let output = togi()
        .args(["check", "--all", "--format", "json", "--test-cmd", "true"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Scanning all 1 supported files..."),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "invalid JSON output: {e}\nstdout: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    assert!(value.get("total").is_some());
    assert!(value.get("mutations").is_some());

    let report_path = dir.path().join("togi-report.json");
    fs::write(&report_path, &output.stdout).unwrap();
    let mutant_id = value["mutations"][0]["id"]
        .as_u64()
        .expect("report must contain a mutation")
        .to_string();

    togi()
        .args([
            "explain",
            &mutant_id,
            "--report",
            report_path.to_str().unwrap(),
        ])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(format!("Mutation #{mutant_id}")));
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
fn check_format_json_dry_run_outputs_a_single_preview_document() {
    let dir = setup_git_repo();

    let output = togi()
        .args([
            "check",
            "--all",
            "--format",
            "json",
            "--dry-run",
            "--test-cmd",
            "true",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "invalid dry-run JSON output: {e}\nstdout: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    assert_eq!(value["kind"], "dry_run");
    assert_eq!(value["dry_run"], true);
    assert_eq!(
        value["planned_total"],
        value["mutations"].as_array().unwrap().len()
    );
    assert!(value.get("mutation_score").is_none());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Scanning all 1 supported files..."),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!dir.path().join(".togi-cache").exists());
}

#[test]
fn check_format_json_no_mutations_outputs_an_empty_report() {
    let dir = setup_git_repo();
    assert_command_success(
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(dir.path())
            .output()
            .unwrap(),
        "commit no-diff setup",
    );
    assert_command_success(
        std::process::Command::new("git")
            .args(["commit", "-m", "second"])
            .current_dir(dir.path())
            .output()
            .unwrap(),
        "commit no-diff setup",
    );

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

    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "invalid empty-report JSON output: {e}\nstdout: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    assert_eq!(value["total"], 0);
    assert_eq!(value["planned_total"], 0);
    assert_eq!(value["mutation_score"], 100.0);
    assert_eq!(value["mutations"], serde_json::json!([]));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("No changes found"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn check_format_json_post_generation_no_mutations_outputs_empty_report() {
    let dir = setup_git_repo();
    let main_go = dir.path().join("main.go");
    fs::write(
        &main_go,
        "package main\n\nconst name = \"before\"\n\nfunc main() {}\n",
    )
    .unwrap();
    assert_command_success(
        std::process::Command::new("git")
            .args(["add", "main.go"])
            .current_dir(dir.path())
            .output()
            .unwrap(),
        "stage no-candidate baseline",
    );
    assert_command_success(
        std::process::Command::new("git")
            .args(["commit", "-m", "no-candidate baseline"])
            .current_dir(dir.path())
            .output()
            .unwrap(),
        "commit no-candidate baseline",
    );
    fs::write(
        &main_go,
        "package main\n\nconst name = \"after\"\n\nfunc main() {}\n",
    )
    .unwrap();

    let output = togi()
        .args([
            "check",
            "--base",
            "HEAD",
            "--format",
            "json",
            "--operators",
            "string_to_empty",
            "--test-cmd",
            "true",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "invalid post-generation empty-report JSON output: {e}\nstdout: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    assert_eq!(value["total"], 0);
    assert_eq!(value["planned_total"], 0);
    assert_eq!(value["mutations"], serde_json::json!([]));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("No mutations generated"),
        "stderr: {stderr}"
    );
    assert!(!stderr.contains("No changes found"), "stderr: {stderr}");
}

#[test]
fn check_all_no_supported_files_json_outputs_an_empty_report() {
    let dir = setup_git_repo();

    let output = togi()
        .args([
            "check",
            "--all",
            "--path",
            "go.mod",
            "--format",
            "json",
            "--test-cmd",
            "true",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "invalid no-supported-files JSON output: {e}\nstdout: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    assert_eq!(value["total"], 0);
    assert_eq!(value["mutations"], serde_json::json!([]));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("No supported source files found"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
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
    let failure: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("invalid suite-failure JSON: {error}\nstdout:\n{stdout}"));
    assert_eq!(failure["kind"], "suite_failure");
    assert_eq!(failure["phase"], "test");
    assert_eq!(failure["command"], serde_json::json!(["false"]));
    assert_eq!(failure["outcome"], "failed");
    assert!(stderr.contains("baseline test command failed (`false`)"));
    assert!(!stderr.contains("Running 1 mutations"));
    assert!(failure.get("mutations").is_none());
    assert!(failure.get("mutation_score").is_none());
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
    let survivors = value["mutations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|mutation| mutation["result"] == "survived")
        .collect::<Vec<_>>();
    assert!(!survivors.is_empty());
    assert!(
        survivors
            .iter()
            .all(|mutation| mutation["baseline_status"] == "non_comparable")
    );
}

#[test]
fn check_baseline_annotates_historic_survivors_in_a_single_json_document() {
    let dir = setup_git_repo();
    let common = [
        "check",
        "--base",
        "HEAD",
        "--format",
        "json",
        "--test-cmd",
        "true",
        "--no-schemata",
        "--operators",
        "gt_to_gte",
        "--max-per-run",
        "1",
        "--jobs",
        "1",
        "--force-rerun",
        "--no-incremental-history",
        "--fail-under",
        "0",
    ];

    let saved = togi()
        .args(common)
        .arg("--save-baseline")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        saved.status.success(),
        "baseline save failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&saved.stdout),
        String::from_utf8_lossy(&saved.stderr)
    );
    let baseline: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.path().join(".togi-baseline")).unwrap()).unwrap();
    assert_eq!(baseline["mutant_snapshot"]["version"], 1);

    let checked = togi()
        .args(common)
        .arg("--check-baseline")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        checked.status.success(),
        "baseline check failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&checked.stdout),
        String::from_utf8_lossy(&checked.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&checked.stdout).unwrap_or_else(|e| {
        panic!(
            "redirected check-baseline JSON was not one document: {e}\nstdout: {}",
            String::from_utf8_lossy(&checked.stdout)
        )
    });
    let survivors = report["mutations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|mutation| mutation["result"] == "survived")
        .collect::<Vec<_>>();
    assert_eq!(survivors.len(), 1);
    assert_eq!(survivors[0]["baseline_status"], "historic");
}

#[test]
fn check_baseline_annotates_historic_survivors_in_terminal() {
    let dir = setup_git_repo();
    let common = [
        "check",
        "--base",
        "HEAD",
        "--test-cmd",
        "true",
        "--no-schemata",
        "--operators",
        "gt_to_gte",
        "--max-per-run",
        "1",
        "--jobs",
        "1",
        "--force-rerun",
        "--no-incremental-history",
        "--fail-under",
        "0",
    ];

    let saved = togi()
        .args(common)
        .arg("--save-baseline")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        saved.status.success(),
        "baseline save failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&saved.stdout),
        String::from_utf8_lossy(&saved.stderr)
    );

    let checked = togi()
        .args(common)
        .arg("--check-baseline")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        checked.status.success(),
        "baseline check failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&checked.stdout),
        String::from_utf8_lossy(&checked.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&checked.stdout)
            .matches("Baseline: historic")
            .count(),
        1
    );
}

#[test]
fn check_baseline_annotations_reach_github_html_sarif_and_pr_comment() {
    let dir = setup_git_repo();
    let common = [
        "check",
        "--base",
        "HEAD",
        "--test-cmd",
        "true",
        "--no-schemata",
        "--operators",
        "gt_to_gte",
        "--max-per-run",
        "1",
        "--jobs",
        "1",
        "--force-rerun",
        "--no-incremental-history",
        "--fail-under",
        "0",
    ];

    let saved = togi()
        .args(common)
        .arg("--save-baseline")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        saved.status.success(),
        "baseline save failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&saved.stdout),
        String::from_utf8_lossy(&saved.stderr)
    );

    let github = togi()
        .args(common)
        .args(["--format", "github", "--check-baseline"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        github.status.success(),
        "GitHub report failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&github.stdout),
        String::from_utf8_lossy(&github.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&github.stdout)
            .matches("[baseline: historic]")
            .count(),
        1
    );

    let sarif = togi()
        .args(common)
        .args(["--format", "sarif", "--check-baseline"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        sarif.status.success(),
        "SARIF report failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&sarif.stdout),
        String::from_utf8_lossy(&sarif.stderr)
    );
    let sarif: serde_json::Value = serde_json::from_slice(&sarif.stdout).unwrap();
    assert_eq!(
        sarif["runs"][0]["results"][0]["properties"]["baseline_status"],
        "historic"
    );

    let html = togi()
        .args(common)
        .args(["--format", "html", "--check-baseline"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        html.status.success(),
        "HTML report failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&html.stdout),
        String::from_utf8_lossy(&html.stderr)
    );
    let html = fs::read_to_string(dir.path().join("togi-report.html")).unwrap();
    assert!(html.contains("<th>Baseline</th>"));
    assert!(html.contains("<td>historic</td>"));

    let pr_comment = dir.path().join("togi-pr-comment.md");
    let pr_comment_run = togi()
        .args(common)
        .arg("--check-baseline")
        .arg("--pr-comment")
        .arg(&pr_comment)
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        pr_comment_run.status.success(),
        "PR comment report failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&pr_comment_run.stdout),
        String::from_utf8_lossy(&pr_comment_run.stderr)
    );
    let pr_comment = fs::read_to_string(pr_comment).unwrap();
    assert!(pr_comment.contains("| File | Line | Operator | Description | Baseline |"));
    assert!(pr_comment.contains(" | historic |"));
}

#[test]
fn check_baseline_keeps_json_report_when_baseline_is_invalid() {
    let dir = setup_git_repo();
    fs::write(dir.path().join(".togi-baseline"), "not json").unwrap();

    let output = togi()
        .args([
            "check",
            "--base",
            "HEAD",
            "--format",
            "json",
            "--test-cmd",
            "true",
            "--no-schemata",
            "--operators",
            "gt_to_gte",
            "--max-per-run",
            "1",
            "--jobs",
            "1",
            "--force-rerun",
            "--no-incremental-history",
            "--check-baseline",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("invalid baseline"),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "invalid baseline must still leave one report document on stdout: {e}\nstdout: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    let survivors = report["mutations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|mutation| mutation["result"] == "survived")
        .collect::<Vec<_>>();
    assert_eq!(survivors.len(), 1);
    assert!(survivors[0].get("baseline_status").is_none());
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

#[test]
fn check_skips_baseline_actions_without_fresh_cache_or_history_evidence() {
    for (reuse_source, expected_state) in [
        ("exact cache", "exact_cache"),
        ("incremental history", "incremental_history"),
    ] {
        let dir = setup_git_repo();
        let baseline_path = dir.path().join(".togi-baseline");

        let fresh = togi()
            .args([
                "check",
                "--base",
                "HEAD",
                "--format",
                "json",
                "--test-cmd",
                "true",
                "--no-schemata",
                "--operators",
                "gt_to_gte",
                "--max-per-run",
                "1",
                "--jobs",
                "1",
                "--fail-under",
                "0",
                "--save-baseline",
            ])
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(
            fresh.status.success(),
            "fresh baseline run failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&fresh.stdout),
            String::from_utf8_lossy(&fresh.stderr)
        );
        let baseline_before = fs::read(&baseline_path).unwrap();
        let saved: serde_json::Value = serde_json::from_slice(&baseline_before).unwrap();
        assert_eq!(saved["total"], 1, "{reuse_source} fixture needs a baseline");

        for action in ["--save-baseline", "--check-baseline"] {
            if reuse_source == "incremental history" {
                let cache_dir = dir.path().join(".togi-cache");
                for entry in fs::read_dir(&cache_dir).unwrap() {
                    let entry = entry.unwrap();
                    if entry.file_type().unwrap().is_file()
                        && entry.file_name().to_string_lossy() != "history.json"
                    {
                        fs::remove_file(entry.path()).unwrap();
                    }
                }
                assert!(cache_dir.join("history.json").exists());
            }
            let reused = togi()
                .args([
                    "check",
                    "--base",
                    "HEAD",
                    "--format",
                    "json",
                    "--test-cmd",
                    "true",
                    "--no-schemata",
                    "--operators",
                    "gt_to_gte",
                    "--max-per-run",
                    "1",
                    "--jobs",
                    "1",
                ])
                .arg(action)
                .current_dir(dir.path())
                .output()
                .unwrap();
            let stderr = String::from_utf8_lossy(&reused.stderr);
            assert_eq!(
                reused.status.code(),
                Some(1),
                "{reuse_source} {action} verdict must retain normal no-baseline survivor behavior\nstdout:\n{}\nstderr:\n{stderr}",
                String::from_utf8_lossy(&reused.stdout),
            );
            assert_eq!(
                stderr
                    .matches("Report has no complete fresh execution evidence; skipping baseline save/check.")
                    .count(),
                1,
                "{reuse_source} {action}: {stderr}"
            );
            assert!(
                !stderr.contains("Baseline saved to .togi-baseline"),
                "{stderr}"
            );
            assert!(
                !stderr.contains("Mutation score regression detected!"),
                "{stderr}"
            );
            assert_eq!(fs::read(&baseline_path).unwrap(), baseline_before);

            let report: serde_json::Value = serde_json::from_slice(&reused.stdout).unwrap();
            assert_eq!(
                report["mutations"][0]["execution"]["state"], expected_state,
                "{reuse_source} {action}: {report}"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn check_skips_baseline_actions_for_mixed_fresh_and_reused_results() {
    for action in ["--save-baseline", "--check-baseline"] {
        let dir = setup_git_repo();
        fs::write(
            dir.path().join("history.go"),
            "package main\n\nfunc history(a, b int) bool {\n\treturn a > b\n}\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("fresh.go"),
            "package main\n\nfunc fresh(a, b int) bool {\n\treturn a > b\n}\n",
        )
        .unwrap();
        let state = tempfile::NamedTempFile::new().unwrap();
        fs::write(state.path(), "kill").unwrap();
        let test_cmd = format!(
            "sh -c {}",
            shell_quote_text(&format!(
                "if grep -Fq '>=' main.go history.go fresh.go; then test \"$(cat {})\" = survive; else exit 0; fi",
                shell_quote(state.path())
            ))
        );
        let baseline_path = dir.path().join(".togi-baseline");

        let initial = togi()
            .args([
                "check",
                "--all",
                "--format",
                "json",
                "--test-cmd",
                &test_cmd,
                "--no-schemata",
                "--operators",
                "gt_to_gte",
                "--max-per-run",
                "3",
                "--jobs",
                "1",
                "--fail-under",
                "0",
                "--save-baseline",
            ])
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(
            initial.status.success(),
            "initial baseline run failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&initial.stdout),
            String::from_utf8_lossy(&initial.stderr)
        );
        assert!(
            String::from_utf8_lossy(&initial.stderr).contains("Baseline saved to .togi-baseline")
        );
        let initial_report: serde_json::Value = serde_json::from_slice(&initial.stdout).unwrap();
        assert_eq!(initial_report["mutation_score"].as_f64(), Some(100.0));
        let baseline_before = fs::read(&baseline_path).unwrap();
        let baseline: serde_json::Value = serde_json::from_slice(&baseline_before).unwrap();
        assert_eq!(baseline["killed"], 3);
        assert_eq!(baseline["total"], 3);
        fs::remove_dir_all(dir.path().join(".togi-cache")).unwrap();

        fs::write(state.path(), "survive").unwrap();
        for path in ["history.go", "main.go"] {
            let seeded = togi()
                .args([
                    "check",
                    "--all",
                    "--path",
                    path,
                    "--format",
                    "json",
                    "--test-cmd",
                    &test_cmd,
                    "--no-schemata",
                    "--operators",
                    "gt_to_gte",
                    "--max-per-run",
                    "1",
                    "--jobs",
                    "1",
                    "--fail-under",
                    "0",
                ])
                .current_dir(dir.path())
                .output()
                .unwrap();
            assert!(
                seeded.status.success(),
                "seed {path} failed\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&seeded.stdout),
                String::from_utf8_lossy(&seeded.stderr)
            );

            if path == "history.go" {
                let cache_dir = dir.path().join(".togi-cache");
                for entry in fs::read_dir(&cache_dir).unwrap() {
                    let entry = entry.unwrap();
                    if entry.file_type().unwrap().is_file()
                        && entry.file_name().to_string_lossy() != "history.json"
                    {
                        fs::remove_file(entry.path()).unwrap();
                    }
                }
                assert!(cache_dir.join("history.json").exists());
            }
        }

        let mixed = togi()
            .args([
                "check",
                "--all",
                "--format",
                "json",
                "--test-cmd",
                &test_cmd,
                "--no-schemata",
                "--operators",
                "gt_to_gte",
                "--max-per-run",
                "3",
                "--jobs",
                "1",
            ])
            .arg(action)
            .current_dir(dir.path())
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&mixed.stderr);
        assert_eq!(
            mixed.status.code(),
            Some(1),
            "mixed {action} must retain normal no-baseline survivor behavior\nstdout:\n{}\nstderr:\n{stderr}",
            String::from_utf8_lossy(&mixed.stdout)
        );
        assert_eq!(
            stderr
                .matches(
                    "Report has no complete fresh execution evidence; skipping baseline save/check."
                )
                .count(),
            1,
            "{action}: {stderr}"
        );
        assert!(
            !stderr.contains("Baseline saved to .togi-baseline"),
            "{stderr}"
        );
        assert!(
            !stderr.contains("Mutation score regression detected!"),
            "{stderr}"
        );
        assert_eq!(fs::read(&baseline_path).unwrap(), baseline_before);

        let report: serde_json::Value = serde_json::from_slice(&mixed.stdout).unwrap();
        assert_eq!(report["tested"].as_u64(), Some(1));
        assert_eq!(report["exact_cache_reused"].as_u64(), Some(1));
        assert_eq!(report["incremental_history_reused"].as_u64(), Some(1));
        assert_eq!(report["killed"].as_u64(), Some(0));
        assert_eq!(report["survived"].as_u64(), Some(3));
        assert_eq!(report["mutation_score"].as_f64(), Some(0.0));
        for (file, expected_state) in [
            ("main.go", "exact_cache"),
            ("history.go", "incremental_history"),
            ("fresh.go", "executed"),
        ] {
            let mutation = report["mutations"]
                .as_array()
                .unwrap()
                .iter()
                .find(|mutation| mutation["file"].as_str() == Some(file))
                .unwrap_or_else(|| panic!("missing {file} mutation: {report}"));
            assert_eq!(
                mutation["execution"]["state"], expected_state,
                "{action}: {report}"
            );
        }
    }
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
fn check_help_lists_test_selection_confirmation_flags() {
    togi()
        .args(["check", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--test-selection-file"))
        .stdout(predicate::str::contains("--confirm-survivors"));
}

fn jq_available() -> bool {
    std::process::Command::new("jq")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

struct ActionHelperFixture {
    dir: TempDir,
    fake_togi: PathBuf,
    invocation_log: PathBuf,
    github_output: PathBuf,
    child_environment_log: PathBuf,
    report_path: PathBuf,
}

fn action_helper_fixture() -> ActionHelperFixture {
    let dir = TempDir::new().unwrap();
    let fake_togi = dir.path().join("fake-togi.sh");
    let invocation_log = dir.path().join("invocations.log");
    let github_output = dir.path().join("github-output");
    let report_path = dir.path().join("togi-report.json");
    let child_environment_log = dir.path().join("child-environment.log");
    fs::write(
        &fake_togi,
        r#"#!/usr/bin/env bash
set -u
if [[ -n "${FAKE_TOGI_ENV_LOG:-}" ]]; then
  printf 'TOGI_BASE=%s\n' "${TOGI_BASE-__unset__}" >> "$FAKE_TOGI_ENV_LOG"
  printf 'TOGI_TIMEOUT=%s\n' "${TOGI_TIMEOUT-__unset__}" >> "$FAKE_TOGI_ENV_LOG"
  printf 'TOGI_FORMAT=%s\n' "${TOGI_FORMAT-__unset__}" >> "$FAKE_TOGI_ENV_LOG"
  printf 'TOGI_TEST_CMD=%s\n' "${TOGI_TEST_CMD-__unset__}" >> "$FAKE_TOGI_ENV_LOG"
  printf 'TOGI_REPORT_PATH=%s\n' "${TOGI_REPORT_PATH-__unset__}" >> "$FAKE_TOGI_ENV_LOG"
  printf 'TOGI_BIN=%s\n' "${TOGI_BIN-__unset__}" >> "$FAKE_TOGI_ENV_LOG"
  printf '%s\n' '--' >> "$FAKE_TOGI_ENV_LOG"
fi

args=("$@")
format=terminal
for ((index = 0; index < ${#args[@]}; index++)); do
  if [[ "${args[index]}" == "--format" ]]; then
    format="${args[$((index + 1))]}"
    break
  fi
done
for arg in "${args[@]}"; do
  printf '<%s>\n' "$arg" >> "$FAKE_TOGI_LOG"
done
printf '%s\n' '--' >> "$FAKE_TOGI_LOG"
if [[ "$format" == "json" ]]; then
  printf '%s\n' "${FAKE_TOGI_JSON:?}"
  exit "${FAKE_TOGI_JSON_STATUS:-0}"
fi
printf 'review-format=%s\n' "$format"
exit "${FAKE_TOGI_REVIEW_STATUS:-0}"
"#,
    )
    .unwrap();
    let output = std::process::Command::new("bash")
        .args(["-c", "chmod +x \"$1\"", "--"])
        .arg(&fake_togi)
        .output()
        .unwrap();
    assert!(output.status.success());

    ActionHelperFixture {
        dir,
        fake_togi,
        invocation_log,
        github_output,
        child_environment_log,
        report_path,
    }
}

struct ActionHelperRun<'a> {
    base: Option<&'a str>,
    timeout: Option<&'a str>,
    format: Option<&'a str>,
    test_cmd: Option<&'a str>,
    review_status: i32,
    json_status: i32,
    json: &'a str,
}

fn run_action_helper(
    fixture: &ActionHelperFixture,
    run: ActionHelperRun<'_>,
) -> std::process::Output {
    let helper = Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/scripts/run-togi.sh");
    let mut command = std::process::Command::new("bash");
    command
        .arg(helper)
        .current_dir(fixture.dir.path())
        .env("TOGI_BIN", &fixture.fake_togi)
        .env("RUNNER_TEMP", fixture.dir.path())
        .env("GITHUB_OUTPUT", &fixture.github_output)
        .env("TOGI_REPORT_PATH", &fixture.report_path)
        .env("FAKE_TOGI_LOG", &fixture.invocation_log)
        .env("FAKE_TOGI_ENV_LOG", &fixture.child_environment_log)
        .env("FAKE_TOGI_REVIEW_STATUS", run.review_status.to_string())
        .env("FAKE_TOGI_JSON_STATUS", run.json_status.to_string())
        .env("FAKE_TOGI_JSON", run.json);
    for (name, value) in [
        ("TOGI_BASE", run.base),
        ("TOGI_TIMEOUT", run.timeout),
        ("TOGI_FORMAT", run.format),
        ("TOGI_TEST_CMD", run.test_cmd),
    ] {
        if let Some(value) = value {
            command.env(name, value);
        } else {
            command.env_remove(name);
        }
    }
    command.output().unwrap()
}

fn action_helper_invocations(fixture: &ActionHelperFixture) -> Vec<Vec<String>> {
    let mut invocations = Vec::new();
    let mut invocation = Vec::new();
    for line in fs::read_to_string(&fixture.invocation_log).unwrap().lines() {
        if line == "--" {
            invocations.push(std::mem::take(&mut invocation));
        } else {
            assert!(
                line.starts_with('<') && line.ends_with('>'),
                "unexpected fake invocation log line: {line}"
            );
            invocation.push(line[1..line.len() - 1].to_string());
        }
    }
    assert!(invocation.is_empty(), "unterminated fake invocation");
    invocations
}

fn action_args(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_string()).collect()
}

fn assert_action_outputs(fixture: &ActionHelperFixture) {
    assert_eq!(
        fs::read_to_string(&fixture.github_output).unwrap(),
        format!(
            "report-path={}\nmutation-score=75.5\nsurvivor-count=1\n",
            fixture.report_path.display()
        )
    );
}

const NORMAL_ACTION_REPORT: &str =
    r#"{"kind":"mutation_report","schema_version":1,"mutation_score":75.5,"survived":1}"#;

#[test]
fn github_action_run_helper_preserves_selected_format_and_exit_one() {
    if !bash_available() || !jq_available() {
        eprintln!("skipping action helper test because bash or jq is unavailable");
        return;
    }

    let fixture = action_helper_fixture();
    let output = run_action_helper(
        &fixture,
        ActionHelperRun {
            base: Some("HEAD~1"),
            timeout: Some("45"),
            format: Some("github"),
            test_cmd: Some("cargo test --workspace --all-features"),
            review_status: 1,
            json_status: 1,
            json: NORMAL_ACTION_REPORT,
        },
    );

    assert_eq!(
        output.status.code(),
        Some(1),
        "helper failed with stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "review-format=github\n"
    );
    assert_eq!(
        action_helper_invocations(&fixture),
        vec![
            action_args(&[
                "check",
                "--base",
                "HEAD~1",
                "--timeout",
                "45",
                "--format",
                "github",
                "--test-cmd",
                "cargo test --workspace --all-features",
            ]),
            action_args(&[
                "check",
                "--base",
                "HEAD~1",
                "--timeout",
                "45",
                "--format",
                "json",
                "--test-cmd",
                "cargo test --workspace --all-features",
            ]),
        ]
    );
    assert_eq!(
        fs::read_to_string(&fixture.report_path).unwrap(),
        format!("{NORMAL_ACTION_REPORT}\n")
    );
    assert_action_outputs(&fixture);
}

#[test]
fn github_action_run_helper_isolates_private_environment_from_child_togi() {
    if !bash_available() || !jq_available() {
        eprintln!("skipping action helper test because bash or jq is unavailable");
        return;
    }

    let fixture = action_helper_fixture();
    let output = run_action_helper(
        &fixture,
        ActionHelperRun {
            base: Some("origin/main"),
            timeout: Some("120"),
            format: Some("github"),
            test_cmd: Some("cargo test --locked"),
            review_status: 0,
            json_status: 0,
            json: NORMAL_ACTION_REPORT,
        },
    );

    assert!(output.status.success());
    assert_eq!(
        fs::read_to_string(&fixture.report_path).unwrap(),
        format!("{NORMAL_ACTION_REPORT}\n")
    );
    assert_action_outputs(&fixture);
    let isolated_child_environment = concat!(
        "TOGI_BASE=__unset__\n",
        "TOGI_TIMEOUT=__unset__\n",
        "TOGI_FORMAT=__unset__\n",
        "TOGI_TEST_CMD=__unset__\n",
        "TOGI_REPORT_PATH=__unset__\n",
        "TOGI_BIN=__unset__\n",
    );
    assert_eq!(
        fs::read_to_string(&fixture.child_environment_log).unwrap(),
        format!("{isolated_child_environment}--\n{isolated_child_environment}--\n")
    );
}

#[test]
fn github_action_run_helper_reuses_selected_json_stdout_once() {
    if !bash_available() || !jq_available() {
        eprintln!("skipping action helper test because bash or jq is unavailable");
        return;
    }

    let fixture = action_helper_fixture();
    let output = run_action_helper(
        &fixture,
        ActionHelperRun {
            base: Some("HEAD~1"),
            timeout: Some("45"),
            format: Some("json"),
            test_cmd: Some("go test ./..."),
            review_status: 1,
            json_status: 1,
            json: NORMAL_ACTION_REPORT,
        },
    );

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("{NORMAL_ACTION_REPORT}\n")
    );
    assert_eq!(
        action_helper_invocations(&fixture),
        vec![action_args(&[
            "check",
            "--base",
            "HEAD~1",
            "--timeout",
            "45",
            "--format",
            "json",
            "--test-cmd",
            "go test ./...",
        ])]
    );
    assert_eq!(
        fs::read_to_string(&fixture.report_path).unwrap(),
        format!("{NORMAL_ACTION_REPORT}\n")
    );
    assert_action_outputs(&fixture);
}

#[cfg(not(windows))]
#[test]
fn github_action_run_helper_keeps_native_report_output_path() {
    if !bash_available() || !jq_available() {
        eprintln!("skipping action helper test because bash or jq is unavailable");
        return;
    }

    let fixture = action_helper_fixture();
    let cygpath_dir = fixture.dir.path().join("fake-bin");
    fs::create_dir(&cygpath_dir).unwrap();
    let cygpath = cygpath_dir.join("cygpath");
    fs::write(
        &cygpath,
        "#!/usr/bin/env bash\n[[ \"$1\" == \"-u\" ]] || exit 2\nprintf '%s\\n' \"$FAKE_CYGPATH_RESULT\"\n",
    )
    .unwrap();
    let output = std::process::Command::new("bash")
        .args(["-c", "chmod +x \"$1\"", "--"])
        .arg(&cygpath)
        .output()
        .unwrap();
    assert!(output.status.success());
    let inherited_path = std::env::var("PATH").expect("PATH must be set");
    let path = format!("{}:{inherited_path}", cygpath_dir.display());
    let helper = Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/scripts/run-togi.sh");
    let output = std::process::Command::new("bash")
        .arg(helper)
        .current_dir(fixture.dir.path())
        .env("PATH", path)
        .env("TOGI_BIN", &fixture.fake_togi)
        .env("TOGI_REPORT_PATH", r"C:\runner\temp\togi-report.json")
        .env("RUNNER_TEMP", fixture.dir.path())
        .env("GITHUB_OUTPUT", &fixture.github_output)
        .env("FAKE_TOGI_LOG", &fixture.invocation_log)
        .env("FAKE_TOGI_REVIEW_STATUS", "0")
        .env("FAKE_TOGI_JSON_STATUS", "0")
        .env("FAKE_TOGI_JSON", NORMAL_ACTION_REPORT)
        .env("FAKE_CYGPATH_RESULT", &fixture.report_path)
        .env_remove("TOGI_BASE")
        .env_remove("TOGI_TIMEOUT")
        .env_remove("TOGI_TEST_CMD")
        .env("TOGI_FORMAT", "github")
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(fixture.report_path.exists());
    assert_eq!(
        action_helper_invocations(&fixture),
        vec![
            action_args(&["check", "--format", "github"]),
            action_args(&["check", "--format", "json"]),
        ]
    );
    assert_eq!(
        fs::read_to_string(&fixture.github_output).unwrap(),
        "report-path=C:\\runner\\temp\\togi-report.json\nmutation-score=75.5\nsurvivor-count=1\n"
    );
}

#[test]
fn github_action_run_helper_omits_unset_review_flags() {
    if !bash_available() || !jq_available() {
        eprintln!("skipping action helper test because bash or jq is unavailable");
        return;
    }

    let fixture = action_helper_fixture();
    let output = run_action_helper(
        &fixture,
        ActionHelperRun {
            base: None,
            timeout: None,
            format: None,
            test_cmd: None,
            review_status: 0,
            json_status: 0,
            json: NORMAL_ACTION_REPORT,
        },
    );

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "review-format=terminal\n"
    );
    assert_eq!(
        action_helper_invocations(&fixture),
        vec![
            action_args(&["check"]),
            action_args(&["check", "--format", "json"]),
        ]
    );
    assert_action_outputs(&fixture);
}

#[test]
fn github_action_run_helper_removes_invalid_or_fatal_sidecar_reports() {
    if !bash_available() || !jq_available() {
        eprintln!("skipping action helper test because bash or jq is unavailable");
        return;
    }

    for (json_status, json) in [(1, "not json"), (2, NORMAL_ACTION_REPORT)] {
        let fixture = action_helper_fixture();
        let output = run_action_helper(
            &fixture,
            ActionHelperRun {
                base: Some("HEAD~1"),
                timeout: None,
                format: Some("github"),
                test_cmd: None,
                review_status: 1,
                json_status,
                json,
            },
        );

        assert_eq!(
            output.status.code(),
            Some(2),
            "unexpected status for JSON sidecar status {json_status}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!fixture.report_path.exists());
        assert!(
            !fixture.github_output.exists()
                || fs::read_to_string(&fixture.github_output)
                    .unwrap()
                    .is_empty()
        );
    }
}

#[test]
fn github_action_run_helper_propagates_fatal_review_and_cleans_stale_report() {
    if !bash_available() || !jq_available() {
        eprintln!("skipping action helper test because bash or jq is unavailable");
        return;
    }

    let fixture = action_helper_fixture();
    fs::write(&fixture.report_path, "stale report").unwrap();
    let output = run_action_helper(
        &fixture,
        ActionHelperRun {
            base: Some("HEAD~1"),
            timeout: None,
            format: Some("github"),
            test_cmd: None,
            review_status: 2,
            json_status: 0,
            json: NORMAL_ACTION_REPORT,
        },
    );

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        action_helper_invocations(&fixture),
        vec![action_args(&[
            "check", "--base", "HEAD~1", "--format", "github",
        ])]
    );
    assert!(!fixture.report_path.exists());
    assert!(
        !fixture.github_output.exists()
            || fs::read_to_string(&fixture.github_output)
                .unwrap()
                .is_empty()
    );
}

#[test]
fn github_action_report_replays_a_direct_mutation() {
    if !bash_available() || !jq_available() {
        eprintln!("skipping action replay test because bash or jq is unavailable");
        return;
    }

    let repo = setup_git_repo();
    fs::write(
        repo.path().join("togi.toml"),
        "[mutations]\nschemata = false\nmax_per_run = 1\n",
    )
    .unwrap();
    fs::write(repo.path().join("test.sh"), "#!/bin/sh\nexit 0\n").unwrap();

    let report_dir = TempDir::new().unwrap();
    let report_path = report_dir.path().join("togi-report.json");
    let github_output = report_dir.path().join("github-output");
    let helper = Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/scripts/run-togi.sh");
    let output = std::process::Command::new("bash")
        .arg(helper)
        .current_dir(repo.path())
        .env("TOGI_BIN", assert_cmd::cargo::cargo_bin("togi"))
        .env("RUNNER_TEMP", report_dir.path())
        .env("GITHUB_OUTPUT", &github_output)
        .env("TOGI_BASE", "HEAD")
        .env("TOGI_FORMAT", "github")
        .env("TOGI_TEST_CMD", "sh test.sh")
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(1),
        "Action helper did not preserve the surviving review failure\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("::warning"));

    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(&report_path).unwrap()).unwrap();
    assert_eq!(report["kind"], "mutation_report");
    assert!(report["schema_version"].as_u64().is_some());
    let mutation = report["mutations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|mutation| mutation["replay"]["kind"] == "regular_direct")
        .expect("report should contain a directly replayable mutation");
    let mutation_id = mutation["id"].as_u64().unwrap().to_string();
    let output_values = fs::read_to_string(&github_output).unwrap();
    assert!(output_values.contains(&format!("report-path={}", report_path.display())));
    assert!(output_values.contains("mutation-score="));
    assert!(output_values.contains("survivor-count="));

    togi()
        .args([
            "replay",
            &mutation_id,
            "--report",
            report_path.to_str().unwrap(),
        ])
        .current_dir(repo.path())
        .assert()
        .success();
}

#[test]
fn github_action_inputs_have_no_baked_in_defaults() {
    let action_yml =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("action.yml")).unwrap();

    // Split the inputs: mapping into per-input blocks keyed by name.
    let mut blocks: Vec<(&str, Vec<&str>)> = Vec::new();
    let mut in_inputs = false;
    for line in action_yml.lines() {
        if line == "inputs:" {
            in_inputs = true;
            continue;
        }
        if in_inputs {
            if !line.starts_with("  ") {
                break;
            }
            if let Some(name) = line.strip_prefix("  ") {
                if !name.starts_with(' ') && name.ends_with(':') {
                    blocks.push((name.trim_end_matches(':'), Vec::new()));
                    continue;
                }
            }
            if let Some((_, body)) = blocks.last_mut() {
                body.push(line);
            }
        }
    }

    for name in ["base", "timeout", "format"] {
        let (_, body) = blocks
            .iter()
            .find(|(block, _)| *block == name)
            .unwrap_or_else(|| panic!("action.yml is missing the `{name}` input"));
        assert!(
            !body
                .iter()
                .any(|line| line.trim_start().starts_with("default:")),
            "action.yml input `{name}` must not bake in a default; unset inputs defer to togi.toml"
        );
    }

    for (name, default) in [
        ("version", "'latest'"),
        ("upload-report", "'true'"),
        ("report-retention-days", "'14'"),
        ("report-artifact-name", "'togi-report'"),
    ] {
        let (_, body) = blocks
            .iter()
            .find(|(block, _)| *block == name)
            .unwrap_or_else(|| panic!("action.yml is missing the `{name}` input"));
        assert!(
            body.iter()
                .any(|line| line.trim() == format!("default: {default}")),
            "action.yml input `{name}` must keep its {default} default"
        );
    }
}

#[test]
fn github_action_declares_replay_report_contract() {
    let action_yml = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("action.yml"))
        .unwrap()
        .replace("\r\n", "\n");

    for expected in [
        "outputs:\n  report-path:",
        "value: ${{ steps.run-togi.outputs.report-path }}",
        "value: ${{ steps.run-togi.outputs.mutation-score }}",
        "value: ${{ steps.run-togi.outputs.survivor-count }}",
        "id: run-togi",
        "TOGI_REPORT_PATH: ${{ runner.temp }}/togi-report.json",
        "report-artifact-name:",
        "if: ${{ always() && inputs.upload-report == 'true' && steps.run-togi.outputs.report-path != '' }}",
        "uses: actions/upload-artifact@v7",
        "name: ${{ inputs.report-artifact-name }}",
        "path: ${{ runner.temp }}/togi-report.json",
        "retention-days: ${{ inputs.report-retention-days }}",
        "if-no-files-found: warn",
    ] {
        assert!(
            action_yml.contains(expected),
            "action.yml is missing `{expected}`"
        );
    }
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
fn explain_reports_reused_cache_verdict_without_claiming_a_test_ran() {
    let dir = TempDir::new().unwrap();
    let report_path = dir.path().join("togi-report.json");
    fs::write(
        &report_path,
        r#"{
  "test_command": ["cargo", "test"],
  "mutations": [{
    "id": 1,
    "file": "src/main.go",
    "line": 3,
    "operator": "plus_to_minus",
    "description": "Replace + with -",
    "result": "survived",
    "execution": {"state": "exact_cache"}
  }]
}"#,
    )
    .unwrap();

    let output = togi()
        .args([
            "explain",
            "1",
            "--report",
            report_path.to_str().expect("report path should be utf-8"),
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "explain cached verdict failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Execution: reused from exact cache"));
    assert!(stdout.contains("Test command: not run."));
    assert!(!stdout.contains(r#"Test command: ["cargo","test"]"#));
}

#[test]
fn explain_reports_incremental_history_and_structured_nonexecution() {
    let dir = TempDir::new().unwrap();

    for (index, (label, result, state, reason, execution_text)) in [
        (
            "incremental history",
            "survived",
            "incremental_history",
            None,
            "reused from incremental history",
        ),
        (
            "build error",
            "build_error",
            "not_executed",
            Some("build_error"),
            "not executed (build_error)",
        ),
        (
            "uncovered",
            "uncovered",
            "not_executed",
            Some("uncovered"),
            "not executed (uncovered)",
        ),
        (
            "subsumed",
            "subsumed",
            "not_executed",
            Some("subsumed"),
            "not executed (subsumed)",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let mut execution = serde_json::json!({"state": state});
        if let Some(reason) = reason {
            execution["reason"] = serde_json::Value::String(reason.into());
        }
        let report_path = dir.path().join(format!("report-{index}.json"));
        fs::write(
            &report_path,
            serde_json::to_string(&serde_json::json!({
                "test_command": ["cargo", "test"],
                "mutations": [{
                    "id": 1,
                    "file": "src/main.go",
                    "line": 3,
                    "operator": "plus_to_minus",
                    "description": label,
                    "result": result,
                    "execution": execution,
                }],
            }))
            .unwrap(),
        )
        .unwrap();

        let output = togi()
            .args([
                "explain",
                "1",
                "--report",
                report_path.to_str().expect("report path should be utf-8"),
            ])
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "{label} explain failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains(&format!("Execution: {execution_text}")),
            "{label} stdout:\n{stdout}"
        );
        assert!(
            stdout.contains("Test command: not run."),
            "{label} stdout:\n{stdout}"
        );
        assert!(
            !stdout.contains(r#"Test command: ["cargo","test"]"#),
            "{label} stdout:\n{stdout}"
        );
    }
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
