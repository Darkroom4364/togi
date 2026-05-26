use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
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
            "false",
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

#[cfg(unix)]
#[test]
fn check_format_json_includes_build_error_diagnostics() {
    let dir = setup_git_repo();
    let build_cmd = format!(
        "{} not-a-togi-command",
        shell_quote(&assert_cmd::cargo::cargo_bin("togi"))
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
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "check failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "invalid JSON output: {e}\nstdout: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    });

    assert_eq!(value["build_errors"], 1);
    assert_eq!(value["mutations"][0]["result"], "build_error");
    assert!(value["mutations"][0]["build_error_fingerprint"].is_string());
    assert_eq!(value["build_error_groups"][0]["phase"], "build_command");
    assert_eq!(value["build_error_groups"][0]["runner"], "regular");
    assert!(
        value["build_error_groups"][0]["message"]
            .as_str()
            .is_some_and(|message| message.contains("not-a-togi-command"))
    );
    assert!(
        value["build_error_groups"][0]["command"]
            .as_array()
            .is_some_and(|command| command
                .iter()
                .any(|arg| arg.as_str() == Some("not-a-togi-command")))
    );
}

#[cfg(unix)]
#[test]
fn check_terminal_includes_build_error_diagnostics() {
    let dir = setup_git_repo();
    let build_cmd = format!(
        "{} not-a-togi-command",
        shell_quote(&assert_cmd::cargo::cargo_bin("togi"))
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
fn interrupted_check_exits_130_without_writing_side_effects() {
    let dir = setup_git_repo();
    let marker = dir.path().join("test-command-started");
    let release = dir.path().join("test-command-release");
    let stdout_path = dir.path().join("togi-stdout.log");
    let stderr_path = dir.path().join("togi-stderr.log");
    let pr_comment_path = dir.path().join("togi-pr-comment.md");
    let baseline_path = dir.path().join(".togi-baseline");
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
}

#[cfg(unix)]
fn shell_quote(path: &Path) -> String {
    let value = path.to_str().expect("test path should be utf-8");
    format!("'{}'", value.replace('\'', "'\\''"))
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
