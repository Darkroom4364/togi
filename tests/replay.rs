use assert_cmd::Command;
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn togi() -> Command {
    Command::cargo_bin("togi").unwrap()
}

fn git(root: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {} failed:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_status(root: &Path) -> Vec<u8> {
    std::process::Command::new("git")
        .args(["status", "--porcelain=v1", "-z"])
        .current_dir(root)
        .output()
        .unwrap()
        .stdout
}

fn snapshot_tree(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    fn visit(root: &Path, current: &Path, entries: &mut Vec<(PathBuf, Vec<u8>)>) {
        let Ok(read_dir) = fs::read_dir(current) else {
            return;
        };
        let mut paths = read_dir
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        paths.sort();
        for path in paths {
            if path.is_dir() {
                visit(root, &path, entries);
            } else if path.is_file() {
                entries.push((
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    fs::read(&path).unwrap(),
                ));
            }
        }
    }

    if !root.exists() {
        return Vec::new();
    }
    let mut entries = Vec::new();
    visit(root, root, &mut entries);
    entries
}

struct ReplayFixture {
    repo: TempDir,
    report_dir: TempDir,
    report_path: PathBuf,
    log_path: PathBuf,
    source_path: PathBuf,
    report: Value,
}

fn setup_replay_fixture() -> ReplayFixture {
    let repo = TempDir::new().unwrap();
    let root = repo.path();
    git(root, &["init"]);
    git(root, &["config", "user.email", "test@example.com"]);
    git(root, &["config", "user.name", "Togi Test"]);

    let source_path = root.join("main.go");
    fs::write(
        &source_path,
        "package main\n\nfunc add(a, b int) int {\n\treturn a + b\n}\n",
    )
    .unwrap();
    fs::write(
        root.join("go.mod"),
        "module example.com/replay\n\ngo 1.21\n",
    )
    .unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "initial"]);

    // Keep the target dirty: matching exact target bytes, rather than a clean
    // worktree, are the replay source identity contract.
    fs::write(
        &source_path,
        "package main\n\nfunc add(a, b int) int {\n\tif a > b {\n\t\treturn a\n\t}\n\treturn a + b\n}\n",
    )
    .unwrap();
    #[cfg(windows)]
    fs::write(
        root.join("test.cmd"),
        "@echo off\r\ngit rev-parse --is-inside-work-tree >nul || exit /b 1\r\n>>\"%TOGI_REPLAY_LOG%\" echo x\r\nexit /b 0\r\n",
    )
    .unwrap();
    #[cfg(not(windows))]
    fs::write(
        root.join("test.sh"),
        "#!/bin/sh\ntest \"$(git rev-parse --is-inside-work-tree)\" = true || exit 1\nprintf x >> \"$TOGI_REPLAY_LOG\"\nexit 0\n",
    )
    .unwrap();

    // Both report and command-log paths intentionally live outside the repo.
    let report_dir = TempDir::new().unwrap();
    let log_path = report_dir.path().join("invocations.log");
    let output = togi()
        .args([
            "check",
            "--all",
            "--format",
            "json",
            "--test-cmd",
            fixture_test_cmd(),
            "--no-schemata",
            "--max-per-run",
            "1",
        ])
        .current_dir(root)
        .env("TOGI_REPLAY_LOG", &log_path)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "surviving fixture should leave a JSON report\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "check did not emit one JSON report: {error}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert_eq!(report["kind"], "mutation_report");
    assert_eq!(report["schema_version"], 1);
    assert!(report["source_revision"].as_str().is_some());
    assert_eq!(report["mutations"][0]["source_path"], "main.go");
    assert!(
        report["mutations"][0]["source_fingerprint"]
            .as_str()
            .is_some_and(|fingerprint| fingerprint.starts_with("sha256:"))
    );
    assert_eq!(report["mutations"][0]["replay"]["kind"], "regular_direct");

    let report_path = report_dir.path().join("report.json");
    fs::write(&report_path, &output.stdout).unwrap();
    ReplayFixture {
        repo,
        report_dir,
        report_path,
        log_path,
        source_path,
        report,
    }
}

#[cfg(windows)]
fn fixture_test_cmd() -> &'static str {
    "cmd /C test.cmd"
}

#[cfg(not(windows))]
fn fixture_test_cmd() -> &'static str {
    "sh test.sh"
}

#[cfg(windows)]
fn fixture_effective_command() -> &'static str {
    "Effective command: [\"cmd\",\"/C\",\"test.cmd\"]"
}

#[cfg(not(windows))]
fn fixture_effective_command() -> &'static str {
    "Effective command: [\"sh\",\"test.sh\"]"
}

fn write_json(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

fn assert_rejected_without_invocation(fixture: &ReplayFixture, contents: &[u8], id: &str) {
    fs::write(&fixture.report_path, contents).unwrap();
    fs::write(&fixture.log_path, []).unwrap();
    let output = togi()
        .args([
            "replay",
            id,
            "--report",
            fixture.report_path.to_str().unwrap(),
        ])
        .current_dir(fixture.repo.path())
        .env("TOGI_REPLAY_LOG", &fixture.log_path)
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "malformed/non-replayable report unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        fs::read(&fixture.log_path).unwrap().is_empty(),
        "replay spawned the test command before rejecting the report"
    );
}

#[cfg(windows)]
fn non_git_fixture_test_cmd() -> &'static str {
    "cmd /C test.cmd"
}

#[cfg(not(windows))]
fn non_git_fixture_test_cmd() -> &'static str {
    "sh test.sh"
}

#[test]
fn non_git_all_schema_v1_report_is_valid_but_not_replayable() {
    let project = TempDir::new().unwrap();
    let root = project.path();
    let report_dir = TempDir::new().unwrap();
    let report_path = report_dir.path().join("report.json");
    let log_path = report_dir.path().join("invocations.log");

    fs::write(
        root.join("main.go"),
        "package main\n\nfunc add(a, b int) int {\n\treturn a + b\n}\n",
    )
    .unwrap();
    #[cfg(windows)]
    fs::write(
        root.join("test.cmd"),
        "@echo off\r\n>>\"%TOGI_REPLAY_LOG%\" echo x\r\nexit /b 0\r\n",
    )
    .unwrap();
    #[cfg(not(windows))]
    fs::write(
        root.join("test.sh"),
        "#!/bin/sh\nprintf x >> \"$TOGI_REPLAY_LOG\"\nexit 0\n",
    )
    .unwrap();

    let check = togi()
        .args([
            "check",
            "--all",
            "--format",
            "json",
            "--test-cmd",
            non_git_fixture_test_cmd(),
            "--no-schemata",
            "--max-per-run",
            "1",
        ])
        .current_dir(root)
        .env("TOGI_REPLAY_LOG", &log_path)
        .output()
        .unwrap();
    assert_eq!(
        check.status.code(),
        Some(1),
        "surviving fixture should emit a JSON report\nstderr:\n{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let report: Value = serde_json::from_slice(&check.stdout).unwrap_or_else(|error| {
        panic!(
            "check did not emit one JSON report: {error}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&check.stdout),
            String::from_utf8_lossy(&check.stderr)
        )
    });
    assert_eq!(report["kind"], "mutation_report");
    assert_eq!(report["schema_version"], 1);
    assert!(
        report["generator"]
            .as_str()
            .is_some_and(|generator| !generator.is_empty())
    );
    assert!(report.get("source_revision").is_none());
    let id = report["mutations"][0]["id"].as_u64().unwrap().to_string();
    fs::write(&report_path, &check.stdout).unwrap();
    fs::write(&log_path, []).unwrap();

    let replay = togi()
        .args(["replay", &id, "--report", report_path.to_str().unwrap()])
        .current_dir(root)
        .env("TOGI_REPLAY_LOG", &log_path)
        .output()
        .unwrap();
    assert!(!replay.status.success());
    let stderr = String::from_utf8_lossy(&replay.stderr);
    assert!(
        stderr.contains("generated without a Git source revision"),
        "{stderr}"
    );
    assert!(
        stderr.contains("rerun `togi check` from a Git worktree"),
        "{stderr}"
    );
    assert!(
        fs::read(&log_path).unwrap().is_empty(),
        "replay spawned the test command for a non-replayable report"
    );
}

#[test]
fn replay_forces_a_real_direct_execution_without_source_or_cache_residue() {
    let fixture = setup_replay_fixture();
    let id = fixture.report["mutations"][0]["id"]
        .as_u64()
        .unwrap()
        .to_string();
    let source_before = fs::read(&fixture.source_path).unwrap();
    let status_before = git_status(fixture.repo.path());
    let git_worktrees_before = snapshot_tree(&fixture.repo.path().join(".git/worktrees"));
    let cache_before = snapshot_tree(&fixture.repo.path().join(".togi-cache"));
    let lock_before = fs::read(fixture.repo.path().join(".togi.lock")).unwrap_or_default();
    let log_before = fs::read(&fixture.log_path).unwrap();

    let output = togi()
        .args([
            "replay",
            &id,
            "--report",
            fixture.report_path.to_str().unwrap(),
        ])
        .current_dir(fixture.repo.path())
        .env("TOGI_REPLAY_LOG", &fixture.log_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "replay failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Expected historical result: survived"));
    assert!(stdout.contains("Fresh result: survived"));
    assert!(stdout.contains("forced fresh direct execution"));
    assert!(stdout.contains(fixture_effective_command()));
    assert_ne!(fs::read(&fixture.log_path).unwrap(), log_before);
    assert_eq!(fs::read(&fixture.source_path).unwrap(), source_before);
    assert_eq!(git_status(fixture.repo.path()), status_before);
    assert_eq!(
        snapshot_tree(&fixture.repo.path().join(".git/worktrees")),
        git_worktrees_before
    );
    assert_eq!(
        snapshot_tree(&fixture.repo.path().join(".togi-cache")),
        cache_before
    );
    assert_eq!(
        fs::read(fixture.repo.path().join(".togi.lock")).unwrap_or_default(),
        lock_before
    );
    assert!(fixture.report_dir.path().exists());
}

#[test]
fn replay_rejects_invalid_or_mismatched_reports_before_running_tests() {
    let fixture = setup_replay_fixture();
    let id = fixture.report["mutations"][0]["id"]
        .as_u64()
        .unwrap()
        .to_string();

    assert_rejected_without_invocation(&fixture, b"{not json", &id);
    assert_rejected_without_invocation(&fixture, br#"{"mutations":[]}"#, &id);

    let mut unsupported_version = fixture.report.clone();
    unsupported_version["schema_version"] = json!(2);
    write_json(&fixture.report_path, &unsupported_version);
    assert_rejected_without_invocation(&fixture, &fs::read(&fixture.report_path).unwrap(), &id);

    let mut wrong_kind = fixture.report.clone();
    wrong_kind["kind"] = json!("dry_run");
    write_json(&fixture.report_path, &wrong_kind);
    assert_rejected_without_invocation(&fixture, &fs::read(&fixture.report_path).unwrap(), &id);

    assert_rejected_without_invocation(
        &fixture,
        &serde_json::to_vec(&fixture.report).unwrap(),
        "999",
    );

    for replay in [
        json!({"kind": "unavailable", "reason": "schemata"}),
        json!({"kind": "unavailable", "reason": "not_executed"}),
    ] {
        let mut unavailable = fixture.report.clone();
        unavailable["mutations"][0]["replay"] = replay;
        write_json(&fixture.report_path, &unavailable);
        assert_rejected_without_invocation(&fixture, &fs::read(&fixture.report_path).unwrap(), &id);
    }

    let mut non_executed_result = fixture.report.clone();
    non_executed_result["mutations"][0]["result"] = json!("build_error");
    write_json(&fixture.report_path, &non_executed_result);
    assert_rejected_without_invocation(&fixture, &fs::read(&fixture.report_path).unwrap(), &id);

    let mut not_executed = fixture.report.clone();
    not_executed["mutations"][0]["execution"] =
        json!({"state": "not_executed", "reason": "build_error"});
    write_json(&fixture.report_path, &not_executed);
    assert_rejected_without_invocation(&fixture, &fs::read(&fixture.report_path).unwrap(), &id);

    let mut mismatched_origin = fixture.report.clone();
    mismatched_origin["mutations"][0]["replay"]["origin"] = json!("exact_cache");
    write_json(&fixture.report_path, &mismatched_origin);
    assert_rejected_without_invocation(&fixture, &fs::read(&fixture.report_path).unwrap(), &id);

    let mut missing_execution = fixture.report.clone();
    missing_execution["mutations"][0]
        .as_object_mut()
        .unwrap()
        .remove("execution");
    write_json(&fixture.report_path, &missing_execution);
    assert_rejected_without_invocation(&fixture, &fs::read(&fixture.report_path).unwrap(), &id);

    let mut control_path = fixture.report.clone();
    control_path["mutations"][0]["source_path"] = json!(".git/config");
    write_json(&fixture.report_path, &control_path);
    assert_rejected_without_invocation(&fixture, &fs::read(&fixture.report_path).unwrap(), &id);

    let mut uppercase_control_path = fixture.report.clone();
    uppercase_control_path["mutations"][0]["source_path"] = json!(".GIT/config");
    write_json(&fixture.report_path, &uppercase_control_path);
    assert_rejected_without_invocation(&fixture, &fs::read(&fixture.report_path).unwrap(), &id);

    let tampered_values = [
        ("source_path", json!("../test.sh")),
        ("byte_start", json!(999_999)),
        ("original", json!("tampered")),
        (
            "source_fingerprint",
            json!("sha256:0000000000000000000000000000000000000000000000000000000000000000"),
        ),
    ];
    for (field, value) in tampered_values {
        let mut tampered = fixture.report.clone();
        tampered["mutations"][0][field] = value;
        write_json(&fixture.report_path, &tampered);
        assert_rejected_without_invocation(&fixture, &fs::read(&fixture.report_path).unwrap(), &id);
    }

    let mut empty_command = fixture.report.clone();
    empty_command["mutations"][0]["replay"]["test_command"] = json!([]);
    write_json(&fixture.report_path, &empty_command);
    assert_rejected_without_invocation(&fixture, &fs::read(&fixture.report_path).unwrap(), &id);

    let source_before = fs::read(&fixture.source_path).unwrap();
    fs::write(&fixture.source_path, b"package main\n// target changed\n").unwrap();
    assert_rejected_without_invocation(
        &fixture,
        &serde_json::to_vec(&fixture.report).unwrap(),
        &id,
    );
    fs::write(&fixture.source_path, source_before).unwrap();

    git(fixture.repo.path(), &["add", "main.go"]);
    git(fixture.repo.path(), &["commit", "-m", "different head"]);
    assert_rejected_without_invocation(
        &fixture,
        &serde_json::to_vec(&fixture.report).unwrap(),
        &id,
    );
}

#[cfg(unix)]
#[test]
fn replay_rejects_symlink_alias_to_control_path_before_spawning() {
    let fixture = setup_replay_fixture();
    let id = fixture.report["mutations"][0]["id"]
        .as_u64()
        .unwrap()
        .to_string();
    let cached_source = fixture.repo.path().join(".togi-cache/alias-target.go");
    let source_bytes = fs::read(&fixture.source_path).unwrap();
    fs::create_dir_all(cached_source.parent().unwrap()).unwrap();
    fs::write(&cached_source, &source_bytes).unwrap();
    std::os::unix::fs::symlink(&cached_source, fixture.repo.path().join("alias.go")).unwrap();

    let mut alias_report = fixture.report.clone();
    alias_report["mutations"][0]["source_path"] = json!("alias.go");
    write_json(&fixture.report_path, &alias_report);
    fs::write(&fixture.log_path, []).unwrap();
    let output = togi()
        .args([
            "replay",
            &id,
            "--report",
            fixture.report_path.to_str().unwrap(),
        ])
        .current_dir(fixture.repo.path())
        .env("TOGI_REPLAY_LOG", &fixture.log_path)
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("resolved replay source path targets a Togi or Git control path")
    );
    assert!(fs::read(&fixture.log_path).unwrap().is_empty());
    assert_eq!(fs::read(&cached_source).unwrap(), source_bytes);
}

#[cfg(windows)]
#[test]
fn replay_rejects_junction_alias_to_control_path_before_spawning() {
    let fixture = setup_replay_fixture();
    let id = fixture.report["mutations"][0]["id"]
        .as_u64()
        .unwrap()
        .to_string();
    let cache_dir = fixture.repo.path().join(".togi-cache");
    let cached_source = cache_dir.join("alias-target.go");
    let source_bytes = fs::read(&fixture.source_path).unwrap();
    fs::create_dir_all(&cache_dir).unwrap();
    fs::write(&cached_source, &source_bytes).unwrap();
    // Junctions need no special privilege and must be treated like symlinks:
    // the alias resolves into Togi control state and is rejected pre-spawn.
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(fixture.repo.path().join("alias.d"))
        .arg(&cache_dir)
        .status()
        .unwrap();
    assert!(status.success(), "mklink /J failed");

    let mut alias_report = fixture.report.clone();
    alias_report["mutations"][0]["source_path"] = json!("alias.d/alias-target.go");
    write_json(&fixture.report_path, &alias_report);
    fs::write(&fixture.log_path, []).unwrap();
    let output = togi()
        .args([
            "replay",
            &id,
            "--report",
            fixture.report_path.to_str().unwrap(),
        ])
        .current_dir(fixture.repo.path())
        .env("TOGI_REPLAY_LOG", &fixture.log_path)
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("resolved replay source path targets a Togi or Git control path")
    );
    assert!(fs::read(&fixture.log_path).unwrap().is_empty());
    assert_eq!(fs::read(&cached_source).unwrap(), source_bytes);
}
