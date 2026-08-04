//! Deterministic end-to-end tests for the PR-loop benchmark harness (issue #487-A).
//!
//! These drive `benchmarks/pr-loop/run-pr-loop-benchmarks.sh` with a fake
//! `togi` binary and a stub `go`, so `cargo test` needs neither the Go
//! toolchain nor a built togi. The fake emits canned mutation reports keyed
//! off its argv and the presence of `.togi-cache`, and logs every invocation
//! so the tests can prove the harness runs `check --base HEAD` (never
//! `--all`) and manages the temp-project/cache lifecycle correctly.
//!
//! Tests skip when the harness's documented tool requirements (bash, git,
//! jq, python3) are unavailable, mirroring the toolchain gating of the
//! `#[ignore]`d fixture tests.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const FAKE_TOGI: &str = r#"#!/usr/bin/env bash
set -euo pipefail

if [ "${1:-}" = "--version" ]; then
  echo "togi 0.4.1-fake"
  exit 0
fi

log=${FAKE_TOGI_LOG:?FAKE_TOGI_LOG must be set}
cache_present=false
if [ -d .togi-cache ]; then
  cache_present=true
fi
argv_json=$(printf '%s\n' "$@" | jq -Rn '[inputs]')
jq -nc \
  --argjson argv "$argv_json" \
  --arg cwd "$(pwd)" \
  --argjson cache_present "$cache_present" \
  '{argv: $argv, cwd: $cwd, cache_present: $cache_present}' >> "$log"

force_rerun=false
schemata=false
for arg in "$@"; do
  case "$arg" in
    --force-rerun) force_rerun=true ;;
    --schemata) schemata=true ;;
  esac
done

total=${FAKE_TOGI_TOTAL:-4}
tested=$total
exact=0
state="executed"
if [ "$cache_present" = true ] && [ "$force_rerun" = false ]; then
  tested=0
  exact=$total
  state="exact_cache"
fi
mkdir -p .togi-cache
touch .togi-cache/seed

schemata_json="null"
if [ "$schemata" = true ]; then
  schemata_json='{"fast_path":1,"fallback":3,"fallback_reasons":[{"reason":"unsupported_operator","count":2},{"reason":"overlapping_range","count":1}]}'
fi

mutations_json=$(jq -nc --arg state "$state" --argjson count "${FAKE_TOGI_MUTATIONS:-4}" '
  ([
    {id: 1, file: "calc.go", line: 34, column: 7, operator: "gt_to_gte",
     original: ">", replacement: ">=", description: "fake",
     result: "survived", execution: {state: $state}, language: "go"},
    {id: 2, file: "calc.go", line: 35, column: 14, operator: "plus_to_minus",
     original: "+", replacement: "-", description: "fake",
     result: "survived", execution: {state: $state}, language: "go"},
    {id: 3, file: "calc.go", line: 34, column: 2, operator: "remove_if_body",
     description: "fake",
     result: "survived", execution: {state: $state}, language: "go"},
    {id: 4, file: "calc.go", line: 34, column: 6, operator: "negate_condition",
     description: "fake",
     result: "survived", execution: {state: $state}, language: "go"}
  ] | .[0:$count])')

jq -n \
  --arg state "$state" \
  --argjson total "$total" \
  --argjson tested "$tested" \
  --argjson exact "$exact" \
  --argjson schemata "$schemata_json" \
  --argjson mutations "$mutations_json" \
  '{
    kind: "mutation_report",
    schema_version: 1,
    generator: "togi/0.4.1-fake",
    total: $total,
    planned_total: $total,
    tested: $tested,
    killed: 0,
    survived: $total,
    timeout: 0,
    build_errors: 0,
    exact_cache_reused: $exact,
    incremental_history_reused: 0,
    partial: false,
    mutation_score: 0,
    duration_ms: 42,
    test_command: ["go", "test", "./..."],
    build_command: [],
    schemata: $schemata,
    build_error_groups: [],
    mutations: $mutations
  }'
exit 1
"#;

const FAKE_GO: &str = r#"#!/usr/bin/env bash
if [ "${1:-}" = "version" ]; then
  echo "go version go1.0.0-stub test/test"
  exit 0
fi
exit 0
"#;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script_path() -> PathBuf {
    repo_root().join("benchmarks/pr-loop/run-pr-loop-benchmarks.sh")
}

fn manifest_path() -> PathBuf {
    repo_root().join("benchmarks/pr-loop/manifest.json")
}

fn tool_available(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Mirrors the harness's own `command -v` detection. Used for tools whose
/// `--version` handling differs between GNU and BSD (BSD sed rejects it),
/// where a version probe would report a present tool as missing.
fn tool_on_path(tool: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg("command -v \"$1\"")
        .arg("sh")
        .arg(tool)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// The harness requires bash, git, jq, sed, sha256sum or shasum, and
/// python3; the `go` prerequisite is satisfied by the stub in FakeTools.
fn harness_tools_available() -> bool {
    ["bash", "git", "jq", "python3"]
        .iter()
        .all(|tool| tool_available(tool))
        && tool_on_path("sed")
        && (tool_on_path("sha256sum") || tool_on_path("shasum"))
}

struct FakeTools {
    _dir: tempfile::TempDir,
    bin: PathBuf,
    togi: PathBuf,
    log: PathBuf,
}

fn install_fake_tools() -> FakeTools {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("tempdir for fake tools");
    let bin = dir.path().join("bin");
    fs::create_dir(&bin).expect("fake bin dir");
    let togi = bin.join("togi");
    let go = bin.join("go");
    fs::write(&togi, FAKE_TOGI).expect("write fake togi");
    fs::write(&go, FAKE_GO).expect("write stub go");
    fs::set_permissions(&togi, fs::Permissions::from_mode(0o755)).expect("chmod fake togi");
    fs::set_permissions(&go, fs::Permissions::from_mode(0o755)).expect("chmod stub go");
    let log = dir.path().join("invocations.jsonl");
    FakeTools {
        _dir: dir,
        bin,
        togi,
        log,
    }
}

fn run_harness(
    tools: &FakeTools,
    out_dir: &Path,
    manifest: Option<&Path>,
    extra_env: &[(&str, &str)],
) -> Output {
    let mut command = Command::new("bash");
    command
        .arg(script_path())
        .arg("--output")
        .arg(out_dir)
        .env("TOGI_BIN", &tools.togi)
        .env("FAKE_TOGI_LOG", &tools.log)
        .env(
            "PATH",
            format!(
                "{}:{}",
                tools.bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        );
    if let Some(manifest) = manifest {
        command.env("BENCH_MANIFEST", manifest);
    }
    for (key, value) in extra_env {
        command.env(key, value);
    }
    command.output().expect("spawn harness")
}

fn read_result(out_dir: &Path) -> serde_json::Value {
    let path = out_dir.join("pr-loop-benchmark-result.json");
    serde_json::from_str(
        &fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display())),
    )
    .expect("normalized result is valid JSON")
}

fn argv_strings(run: &serde_json::Value) -> Vec<&str> {
    run["argv"]
        .as_array()
        .expect("argv array")
        .iter()
        .map(|arg| arg.as_str().expect("argv string"))
        .collect()
}

fn command_strings(workload: &serde_json::Value) -> Vec<&str> {
    workload["command"]
        .as_array()
        .expect("command array")
        .iter()
        .map(|arg| arg.as_str().expect("command string"))
        .collect()
}

/// Assert an argv/command vector invokes `check --base HEAD` with pinned
/// `--jobs 1` and never falls back to `--all`. Works both for the result
/// JSON's command (which includes the binary path) and the fake binary's
/// logged argv (which does not).
fn assert_pr_diff_command(args: &[&str]) {
    let check_pos = args
        .iter()
        .position(|arg| *arg == "check")
        .expect("command must invoke `check`");
    let base_pos = args
        .iter()
        .position(|arg| *arg == "--base")
        .expect("command must pass --base");
    assert!(
        check_pos < base_pos,
        "`check` must precede --base: {args:?}"
    );
    assert_eq!(
        args[base_pos + 1],
        "HEAD",
        "diff base must be HEAD (PR diff), got: {args:?}"
    );
    assert!(
        !args.contains(&"--all"),
        "benchmarks must never use --all: {args:?}"
    );
    let jobs_pos = args
        .iter()
        .position(|arg| *arg == "--jobs")
        .expect("command must pin --jobs");
    assert_eq!(args[jobs_pos + 1], "1", "jobs must be pinned to 1");
}

#[test]
fn pr_loop_harness_runs_all_four_workloads_against_a_pr_diff() {
    if !harness_tools_available() {
        eprintln!("skipping: harness tools (bash/git/jq/sed/sha256sum|shasum/python3) unavailable");
        return;
    }
    let tools = install_fake_tools();
    let out_dir = tempfile::tempdir().expect("output tempdir");

    let output = run_harness(&tools, out_dir.path(), None, &[]);
    assert!(
        output.status.success(),
        "harness must succeed with well-formed fake reports\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let result = read_result(out_dir.path());
    assert_eq!(result["ok"], true);
    assert_eq!(result["failures"], serde_json::json!([]));
    assert_eq!(
        result["cross_workload"]["mutation_identity_consistent"],
        true
    );
    assert!(result["cross_workload"]["mutation_identity_sha256"].is_string());

    let workloads = result["workloads"].as_array().expect("workloads array");
    let names: Vec<&str> = workloads
        .iter()
        .map(|workload| workload["name"].as_str().expect("workload name"))
        .collect();
    assert_eq!(
        names,
        [
            "cold-regular",
            "warm-exact-cache",
            "cold-schemata",
            "pr-diff-default"
        ]
    );

    for workload in workloads {
        assert_eq!(workload["ok"], true, "workload {} failed", workload["name"]);
        for invariant in workload["invariants"].as_array().expect("invariants") {
            assert_eq!(
                invariant["ok"], true,
                "invariant {} failed for workload {}",
                invariant["name"], workload["name"]
            );
        }
        assert_pr_diff_command(&command_strings(workload));
        assert!(
            workload["timing"]["wall_ms"]
                .as_u64()
                .expect("wall_ms number")
                > 0,
            "monotonic clock must record nonzero wall_ms for {}",
            workload["name"]
        );
    }

    // Workload-specific semantic evidence.
    assert_eq!(workloads[0]["semantics"]["tested"], 4);
    assert_eq!(workloads[0]["semantics"]["exact_cache_reused"], 0);
    assert_eq!(workloads[1]["semantics"]["tested"], 0);
    assert_eq!(workloads[1]["semantics"]["exact_cache_reused"], 4);
    assert!(
        workloads[2]["semantics"]["schemata"]["fast_path"]
            .as_u64()
            .expect("fast_path")
            >= 1
    );
    assert!(
        workloads[2]["semantics"]["schemata"]["fallback"]
            .as_u64()
            .expect("fallback")
            >= 1
    );

    // Raw artifacts persist for every workload.
    for name in &names {
        assert!(
            out_dir
                .path()
                .join("raw")
                .join(format!("{name}.report.json"))
                .exists(),
            "raw report missing for {name}"
        );
    }

    // Invocation log: one temp project, `check --base HEAD`, cache lifecycle.
    let log = fs::read_to_string(&tools.log).expect("invocation log");
    let runs: Vec<serde_json::Value> = log
        .lines()
        .map(|line| serde_json::from_str(line).expect("invocation log line"))
        .collect();
    assert_eq!(runs.len(), 4, "expected exactly four togi invocations");

    let project_dir = runs[0]["cwd"].as_str().expect("invocation cwd");
    assert_ne!(
        Path::new(project_dir),
        repo_root().join("tests/fixtures/go"),
        "harness must copy the fixture into a disposable project"
    );
    let cache_sequence: Vec<bool> = runs
        .iter()
        .map(|run| run["cache_present"].as_bool().expect("cache flag"))
        .collect();
    assert_eq!(
        cache_sequence,
        [false, true, false, false],
        "cold runs must start cacheless and the warm run must reuse the seeded cache"
    );
    let force_rerun_sequence: Vec<bool> = runs
        .iter()
        .map(|run| argv_strings(run).contains(&"--force-rerun"))
        .collect();
    assert_eq!(force_rerun_sequence, [true, false, true, false]);

    for run in &runs {
        assert_eq!(
            run["cwd"].as_str().expect("cwd"),
            project_dir,
            "all workloads must share the single disposable project"
        );
        assert_pr_diff_command(&argv_strings(run));
    }
    assert!(
        !Path::new(project_dir).exists(),
        "disposable project must be removed after the run: {project_dir}"
    );
}

#[test]
fn pr_loop_harness_rejects_malformed_manifests() {
    if !harness_tools_available() {
        eprintln!("skipping: harness tools (bash/git/jq/sed/sha256sum|shasum/python3) unavailable");
        return;
    }
    let tools = install_fake_tools();
    let base: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(manifest_path()).expect("read repo manifest"))
            .expect("repo manifest is valid JSON");

    let mut missing_workloads = base.clone();
    missing_workloads
        .as_object_mut()
        .unwrap()
        .remove("workloads");

    let mut empty_workloads = base.clone();
    empty_workloads["workloads"] = serde_json::json!([]);

    let mut object_workloads = base.clone();
    object_workloads["workloads"] = serde_json::json!({});

    let mut three_workloads = base.clone();
    three_workloads["workloads"]
        .as_array_mut()
        .unwrap()
        .truncate(3);

    let mut wrong_order = base.clone();
    wrong_order["workloads"].as_array_mut().unwrap().reverse();

    let mut warm_not_reuse = base.clone();
    warm_not_reuse["workloads"][1]["cache"] = serde_json::json!("fresh");

    let mut missing_count = base.clone();
    missing_count["fixture"]
        .as_object_mut()
        .unwrap()
        .remove("expected_mutation_count");

    let mut zero_count = base.clone();
    zero_count["fixture"]["expected_mutation_count"] = serde_json::json!(0);

    let mut bad_patch_digest = base.clone();
    bad_patch_digest["fixture"]["patch_sha256"] = serde_json::json!("not-a-digest");

    let mut unknown_invariant = base.clone();
    unknown_invariant["workloads"][0]["invariants"] = serde_json::json!(["made-up-invariant"]);

    let mut future_schema = base.clone();
    future_schema["schema_version"] = serde_json::json!(2);

    let mut all_flag = base.clone();
    all_flag["togi"]["common_args"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!("--all"));

    let mut missing_base = base.clone();
    missing_base["togi"]["common_args"]
        .as_array_mut()
        .unwrap()
        .retain(|arg| arg != "--base" && arg != "HEAD");

    let mut wrong_base_value = base.clone();
    wrong_base_value["togi"]["common_args"][2] = serde_json::json!("HEAD~1");

    let mut base_ref_mismatch = base.clone();
    base_ref_mismatch["fixture"]["base_ref"] = serde_json::json!("origin/main");

    let mut wrong_subcommand = base.clone();
    wrong_subcommand["togi"]["common_args"][0] = serde_json::json!("list-operators");

    let mut duplicate_base = base.clone();
    duplicate_base["togi"]["common_args"]
        .as_array_mut()
        .unwrap()
        .extend([serde_json::json!("--base"), serde_json::json!("HEAD")]);

    let mut inline_base_form = base.clone();
    {
        let args = inline_base_form["togi"]["common_args"]
            .as_array_mut()
            .unwrap();
        let pos = args.iter().position(|arg| arg == "--base").unwrap();
        args[pos] = serde_json::json!("--base=HEAD");
        args.remove(pos + 1);
    }

    let mut missing_well_formed = base.clone();
    missing_well_formed["workloads"][2]["invariants"] =
        serde_json::json!(["schemata-fast-path-and-fallback"]);

    // base_ref and --base moved together: a consistent pair is still a
    // contract violation because the benchmark only measures HEAD diffs.
    let mut base_ref_not_head = base.clone();
    base_ref_not_head["fixture"]["base_ref"] = serde_json::json!("HEAD~1");
    base_ref_not_head["togi"]["common_args"][2] = serde_json::json!("HEAD~1");

    let mut extra_args_all = base.clone();
    extra_args_all["workloads"][0]["extra_args"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!("--all"));

    let mut extra_args_wrong_base = base.clone();
    extra_args_wrong_base["workloads"][2]["extra_args"]
        .as_array_mut()
        .unwrap()
        .extend([serde_json::json!("--base"), serde_json::json!("HEAD~1")]);

    let mut extra_args_inline_base = base.clone();
    extra_args_inline_base["workloads"][3]["extra_args"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!("--base=HEAD"));

    let mut extra_args_duplicate_base = base.clone();
    extra_args_duplicate_base["workloads"][1]["extra_args"]
        .as_array_mut()
        .unwrap()
        .extend([serde_json::json!("--base"), serde_json::json!("HEAD")]);

    let cases: Vec<(&str, serde_json::Value)> = vec![
        ("missing workloads field", missing_workloads),
        ("empty workloads array", empty_workloads),
        ("non-array workloads", object_workloads),
        ("only three workloads", three_workloads),
        ("workloads in wrong order", wrong_order),
        ("warm workload not reusing cache", warm_not_reuse),
        ("missing expected_mutation_count", missing_count),
        ("zero expected_mutation_count", zero_count),
        ("malformed patch digest", bad_patch_digest),
        ("unknown invariant name", unknown_invariant),
        ("unsupported schema_version", future_schema),
        ("--all in common_args", all_flag),
        ("missing --base in common_args", missing_base),
        (
            "--base value not matching fixture.base_ref",
            wrong_base_value,
        ),
        ("fixture.base_ref not matching --base", base_ref_mismatch),
        ("wrong subcommand in common_args", wrong_subcommand),
        ("duplicate --base in common_args", duplicate_base),
        ("inline --base= form in common_args", inline_base_form),
        ("workload missing report-well-formed", missing_well_formed),
        ("base_ref moved to HEAD~1 with --base", base_ref_not_head),
        ("--all injected via extra_args", extra_args_all),
        (
            "wrong split --base injected via extra_args",
            extra_args_wrong_base,
        ),
        (
            "inline --base= injected via extra_args",
            extra_args_inline_base,
        ),
        (
            "duplicate --base injected via extra_args",
            extra_args_duplicate_base,
        ),
    ];

    for (label, manifest) in cases {
        let dir = tempfile::tempdir().expect("case tempdir");
        let manifest_file = dir.path().join("manifest.json");
        fs::write(
            &manifest_file,
            serde_json::to_string_pretty(&manifest).expect("serialize manifest"),
        )
        .expect("write case manifest");
        let out_dir = dir.path().join("out");

        let output = run_harness(&tools, &out_dir, Some(&manifest_file), &[]);
        assert!(
            !output.status.success(),
            "harness must reject {label}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let result_path = out_dir.join("pr-loop-benchmark-result.json");
        if result_path.exists() {
            let result = read_result(&out_dir);
            assert_ne!(
                result["ok"], true,
                "harness must never emit success for {label}"
            );
        }
    }
}

#[test]
fn pr_loop_harness_fails_when_mutation_records_are_truncated() {
    if !harness_tools_available() {
        eprintln!("skipping: harness tools (bash/git/jq/sed/sha256sum|shasum/python3) unavailable");
        return;
    }
    // A report whose `mutations` array is missing records (or empty) must
    // never satisfy invariants vacuously: report-well-formed requires the
    // array to exist and to hold exactly `total` entries.
    for emitted in ["2", "0"] {
        let tools = install_fake_tools();
        let out_dir = tempfile::tempdir().expect("output tempdir");
        let output = run_harness(
            &tools,
            out_dir.path(),
            None,
            &[("FAKE_TOGI_MUTATIONS", emitted)],
        );
        assert!(
            !output.status.success(),
            "harness must fail with {emitted} of 4 mutation records\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let result = read_result(out_dir.path());
        assert_eq!(result["ok"], false, "emitted={emitted}");
        let failures: Vec<&str> = result["failures"]
            .as_array()
            .expect("failures array")
            .iter()
            .map(|failure| failure.as_str().expect("failure string"))
            .collect();
        assert!(
            failures.contains(&"cold-regular:report-well-formed"),
            "emitted={emitted}: expected cold-regular:report-well-formed in {failures:?}"
        );
    }
}

#[test]
fn pr_loop_harness_rejects_output_flag_without_value() {
    if !tool_available("bash") {
        eprintln!("skipping: bash unavailable");
        return;
    }
    for args in [&["--output"][..], &["--output", "--keep-workspace"][..]] {
        let output = Command::new("bash")
            .arg(script_path())
            .args(args)
            .output()
            .expect("spawn harness");
        assert_eq!(
            output.status.code(),
            Some(2),
            "harness must exit 2 for args {args:?}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("--output requires a directory argument"),
            "expected specific arity error for args {args:?}, got stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn pr_loop_harness_fails_when_report_semantics_drift() {
    if !harness_tools_available() {
        eprintln!("skipping: harness tools (bash/git/jq/sed/sha256sum|shasum/python3) unavailable");
        return;
    }
    let tools = install_fake_tools();
    let out_dir = tempfile::tempdir().expect("output tempdir");

    // The fake reports five mutations while the manifest pins four: the
    // report-well-formed invariant must fail the harness, never pass silently.
    let output = run_harness(&tools, out_dir.path(), None, &[("FAKE_TOGI_TOTAL", "5")]);

    assert!(
        !output.status.success(),
        "harness must fail on semantic drift\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let result = read_result(out_dir.path());
    assert_eq!(result["ok"], false);
    let failures: Vec<&str> = result["failures"]
        .as_array()
        .expect("failures array")
        .iter()
        .map(|failure| failure.as_str().expect("failure string"))
        .collect();
    assert!(
        failures.contains(&"cold-regular:report-well-formed"),
        "expected cold-regular:report-well-formed in {failures:?}"
    );
}

const APPROVED_BENCHMARK_PREREQUISITES: &str = r#"set -euo pipefail
packages=()
for tool in bash git go jq sed sha256sum python3; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    case "$tool" in
      bash) packages+=(bash) ;;
      git) packages+=(git) ;;
      go) packages+=(golang-go) ;;
      jq) packages+=(jq) ;;
      sed) packages+=(sed) ;;
      sha256sum) packages+=(coreutils) ;;
      python3) packages+=(python3) ;;
    esac
  fi
done
if ((${#packages[@]})); then
  sudo apt-get update
  sudo apt-get install --yes --no-install-recommends "${packages[@]}"
fi
for tool in bash git go jq sed sha256sum python3; do
  command -v "$tool" >/dev/null 2>&1 || {
    echo "missing benchmark prerequisite: $tool" >&2
    exit 1
  }
done
"#;

const APPROVED_BENCHMARK_EVIDENCE_VALIDATION: &str = r#"set -euo pipefail
result="$BENCHMARK_OUTPUT/pr-loop-benchmark-result.json"
raw="$BENCHMARK_OUTPUT/raw"
test -f "$result"
test -d "$raw"
jq -e '
  .ok == true
  and ([.workloads[].name]
       == ["cold-regular", "warm-exact-cache", "cold-schemata", "pr-diff-default"])
' "$result" >/dev/null
for workload in cold-regular warm-exact-cache cold-schemata pr-diff-default; do
  for suffix in report.json stdout stderr; do
    test -f "$raw/$workload.$suffix"
  done
done
"#;

fn validate_pr_loop_benchmark_step_set(job: &serde_yaml::Value) -> Result<(), String> {
    let mapping = job
        .as_mapping()
        .ok_or_else(|| "benchmark job must be a YAML mapping".to_string())?;
    let mut keys: Vec<&str> = mapping
        .keys()
        .map(|key| {
            key.as_str()
                .ok_or_else(|| "benchmark job keys must be strings".to_string())
        })
        .collect::<Result<_, _>>()?;
    keys.sort_unstable();
    if keys != ["name", "permissions", "runs-on", "steps"] {
        return Err(format!(
            "benchmark job keys must be exactly name, runs-on, permissions, and steps; got {keys:?}"
        ));
    }
    if job.get("name").and_then(|value| value.as_str()) != Some("PR-loop Benchmark Evidence")
        || job.get("runs-on").and_then(|value| value.as_str()) != Some("ubuntu-24.04")
        || job
            .get("permissions")
            .and_then(|value| value.as_mapping())
            .is_none_or(|permissions| {
                permissions.len() != 1
                    || permissions
                        .get(serde_yaml::Value::String("contents".to_string()))
                        .and_then(|value| value.as_str())
                        != Some("read")
            })
    {
        return Err("benchmark job top-level values must match the approved contract".to_string());
    }

    let steps = job
        .get("steps")
        .and_then(|value| value.as_sequence())
        .ok_or_else(|| "benchmark job has no step sequence".to_string())?;
    let expected = [
        ("uses", "actions/checkout@v7"),
        (
            "uses",
            "dtolnay/rust-toolchain@29eef336d9b2848a0b548edc03f92a220660cdb8",
        ),
        ("name", "Assert runner matches the required target"),
        (
            "uses",
            "Swatinem/rust-cache@e18b497796c12c097a38f9edb9d0641fb99eee32",
        ),
        ("uses", "actions/setup-go@v7"),
        ("name", "Check benchmark prerequisites"),
        ("name", "Build release binary"),
        ("name", "Run PR-loop benchmark harness"),
        ("name", "Validate PR-loop benchmark evidence"),
        ("name", "Upload PR-loop benchmark evidence"),
    ];
    if steps.len() != expected.len() {
        return Err(format!(
            "benchmark job must contain exactly the intended {} steps, found {}",
            expected.len(),
            steps.len()
        ));
    }
    for (index, ((field, expected), step)) in expected.iter().zip(steps).enumerate() {
        if step.get(*field).and_then(|value| value.as_str()) != Some(*expected) {
            return Err(format!(
                "benchmark step {index} must be {field} `{expected}`, got {step:?}"
            ));
        }
    }
    for (index, step) in steps.iter().enumerate() {
        if step.get("continue-on-error").is_some() {
            return Err(format!("benchmark step {index} must not mask failure"));
        }
        let condition = step.get("if").and_then(|value| value.as_str());
        if index == 9 {
            if condition != Some("${{ always() }}") {
                return Err("artifact upload must be the sole unconditional step".to_string());
            }
        } else if condition.is_some() {
            return Err(format!("benchmark step {index} must not be conditional"));
        }
    }
    for index in [0, 1, 3, 4, 9] {
        if steps[index].get("run").is_some() {
            return Err(format!(
                "non-executable benchmark setup/upload step {index} must not contain `run`"
            ));
        }
    }
    if steps[2].get("run").and_then(|value| value.as_str())
        != Some("bash ./.github/scripts/assert-native-target.sh")
    {
        return Err("native assertion step must be the exact assertion script".to_string());
    }
    if steps[5].get("shell").and_then(|value| value.as_str()) != Some("bash")
        || steps[5].get("run").and_then(|value| value.as_str())
            != Some(APPROVED_BENCHMARK_PREREQUISITES)
    {
        return Err(
            "prerequisite step must contain only the approved tool install/check command"
                .to_string(),
        );
    }
    if steps[6].get("run").and_then(|value| value.as_str())
        != Some("cargo build --locked --release")
    {
        return Err("only the named release-build step may build togi".to_string());
    }
    if steps[7]
        .get("run")
        .and_then(|value| value.as_str())
        .map(str::trim)
        != Some("bash benchmarks/pr-loop/run-pr-loop-benchmarks.sh --output \"$BENCHMARK_OUTPUT\"")
    {
        return Err("only the named harness step may run the benchmark harness".to_string());
    }
    if steps[8].get("shell").and_then(|value| value.as_str()) != Some("bash")
        || steps[8]
            .get("env")
            .and_then(|env| env.get("BENCHMARK_OUTPUT"))
            .and_then(|value| value.as_str())
            != Some("${{ runner.temp }}/togi-pr-loop-benchmarks")
        || steps[8].get("run").and_then(|value| value.as_str())
            != Some(APPROVED_BENCHMARK_EVIDENCE_VALIDATION)
    {
        return Err("only the named validation step may verify benchmark evidence".to_string());
    }
    if steps[9].get("if").and_then(|value| value.as_str()) != Some("${{ always() }}") {
        return Err("only the named upload step must be unconditional".to_string());
    }
    Ok(())
}

#[test]
fn ci_pr_loop_benchmark_contract_is_structural() {
    let ci: serde_yaml::Value = serde_yaml::from_str(
        &fs::read_to_string(repo_root().join(".github/workflows/ci.yml")).unwrap(),
    )
    .expect("ci.yml must parse as YAML");
    let job = ci
        .get("jobs")
        .and_then(|jobs| jobs.get("pr-loop-benchmarks"))
        .expect("normal cargo test must cover the pr-loop benchmark CI job");
    assert_eq!(
        job.get("runs-on").and_then(|value| value.as_str()),
        Some("ubuntu-24.04")
    );
    assert_eq!(
        job.get("permissions")
            .and_then(|permissions| permissions.get("contents"))
            .and_then(|value| value.as_str()),
        Some("read")
    );
    assert!(
        job.get("continue-on-error").is_none(),
        "harness failure must fail the job"
    );
    validate_pr_loop_benchmark_step_set(job)
        .unwrap_or_else(|error| panic!("benchmark CI step contract failed: {error}"));
    let steps = job
        .get("steps")
        .and_then(|value| value.as_sequence())
        .expect("benchmark job must have structured steps");
    let step = |name: &str| {
        steps
            .iter()
            .find(|step| step.get("name").and_then(|value| value.as_str()) == Some(name))
            .unwrap_or_else(|| panic!("benchmark job missing `{name}`"))
    };
    let checkout = steps
        .iter()
        .find(|step| {
            step.get("uses").and_then(|value| value.as_str()) == Some("actions/checkout@v7")
        })
        .expect("benchmark job must check out the repository");
    assert_eq!(
        checkout
            .get("with")
            .and_then(|with| with.get("persist-credentials"))
            .and_then(|value| value.as_bool()),
        Some(false)
    );
    let native = step("Assert runner matches the required target");
    assert_eq!(
        native.get("run").and_then(|value| value.as_str()),
        Some("bash ./.github/scripts/assert-native-target.sh")
    );
    for (key, expected) in [
        ("TOGI_EXPECTED_TARGET", "x86_64-unknown-linux-gnu"),
        ("TOGI_EXPECTED_ARCH", "x86_64"),
    ] {
        assert_eq!(
            native
                .get("env")
                .and_then(|env| env.get(key))
                .and_then(|value| value.as_str()),
            Some(expected),
            "native assertion must bind {key}"
        );
    }

    let build = step("Build release binary");
    assert_eq!(
        build.get("run").and_then(|value| value.as_str()),
        Some("cargo build --locked --release")
    );
    let harness = step("Run PR-loop benchmark harness");
    assert_eq!(
        harness
            .get("env")
            .and_then(|env| env.get("TOGI_BIN"))
            .and_then(|value| value.as_str()),
        Some("${{ github.workspace }}/target/release/togi")
    );
    assert_eq!(
        harness
            .get("env")
            .and_then(|env| env.get("BENCHMARK_OUTPUT"))
            .and_then(|value| value.as_str()),
        Some("${{ runner.temp }}/togi-pr-loop-benchmarks")
    );
    assert_eq!(
        harness
            .get("run")
            .and_then(|value| value.as_str())
            .map(str::trim),
        Some("bash benchmarks/pr-loop/run-pr-loop-benchmarks.sh --output \"$BENCHMARK_OUTPUT\"")
    );

    let upload = step("Upload PR-loop benchmark evidence");
    assert_eq!(
        upload.get("if").and_then(|value| value.as_str()),
        Some("${{ always() }}")
    );
    assert_eq!(
        upload.get("uses").and_then(|value| value.as_str()),
        Some("actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02")
    );
    for (key, expected) in [
        ("path", "${{ runner.temp }}/togi-pr-loop-benchmarks"),
        ("if-no-files-found", "warn"),
    ] {
        assert_eq!(
            upload
                .get("with")
                .and_then(|with| with.get(key))
                .and_then(|value| value.as_str()),
            Some(expected),
            "artifact upload must preserve `{key}`"
        );
    }
    assert_eq!(
        upload
            .get("with")
            .and_then(|with| with.get("name"))
            .and_then(|value| value.as_str()),
        Some("pr-loop-benchmarks-${{ github.run_id }}-${{ github.run_attempt }}")
    );
}

#[test]
fn ci_pr_loop_benchmark_contract_rejects_injected_evaluator_step() {
    let ci: serde_yaml::Value = serde_yaml::from_str(
        &fs::read_to_string(repo_root().join(".github/workflows/ci.yml")).unwrap(),
    )
    .expect("ci.yml must parse as YAML");
    let mut job = ci
        .get("jobs")
        .and_then(|jobs| jobs.get("pr-loop-benchmarks"))
        .expect("benchmark job must exist")
        .clone();
    let evaluator: serde_yaml::Value =
        serde_yaml::from_str("name: Collect observations\nrun: python3 evaluator.py\n").unwrap();
    job.get_mut("steps")
        .and_then(|steps| steps.as_sequence_mut())
        .expect("benchmark job must have mutable steps")
        .push(evaluator);
    assert!(
        validate_pr_loop_benchmark_step_set(&job).is_err(),
        "a neutral-name executable evaluator/gate step must be rejected"
    );
}

#[test]
fn ci_pr_loop_benchmark_contract_rejects_evaluator_in_prerequisite_step() {
    let ci: serde_yaml::Value = serde_yaml::from_str(
        &fs::read_to_string(repo_root().join(".github/workflows/ci.yml")).unwrap(),
    )
    .expect("ci.yml must parse as YAML");
    let mut job = ci
        .get("jobs")
        .and_then(|jobs| jobs.get("pr-loop-benchmarks"))
        .expect("benchmark job must exist")
        .clone();
    let steps = job
        .get_mut("steps")
        .and_then(|steps| steps.as_sequence_mut())
        .expect("benchmark job must have mutable steps");
    let prerequisite = steps[5]
        .get("run")
        .and_then(|run| run.as_str())
        .expect("prerequisite step must have a run command")
        .to_string();
    steps[5].as_mapping_mut().unwrap().insert(
        serde_yaml::Value::String("run".to_string()),
        serde_yaml::Value::String(format!("{prerequisite}\npython3 evaluator.py")),
    );
    assert!(
        validate_pr_loop_benchmark_step_set(&job).is_err(),
        "an evaluator appended to the prerequisite step must be rejected"
    );
}

#[test]
fn ci_pr_loop_benchmark_contract_rejects_harness_control_flow() {
    let ci: serde_yaml::Value = serde_yaml::from_str(
        &fs::read_to_string(repo_root().join(".github/workflows/ci.yml")).unwrap(),
    )
    .expect("ci.yml must parse as YAML");
    let base = ci
        .get("jobs")
        .and_then(|jobs| jobs.get("pr-loop-benchmarks"))
        .expect("benchmark job must exist");
    for (field, value) in [
        ("continue-on-error", serde_yaml::Value::Bool(true)),
        ("if", serde_yaml::Value::String("${{ false }}".to_string())),
    ] {
        let mut job = base.clone();
        job.get_mut("steps")
            .and_then(|steps| steps.as_sequence_mut())
            .expect("benchmark job must have mutable steps")[7]
            .as_mapping_mut()
            .unwrap()
            .insert(serde_yaml::Value::String(field.to_string()), value);
        assert!(
            validate_pr_loop_benchmark_step_set(&job).is_err(),
            "harness `{field}` control flow must be rejected"
        );
    }
}

#[test]
fn ci_pr_loop_benchmark_contract_rejects_job_needs() {
    let ci: serde_yaml::Value = serde_yaml::from_str(
        &fs::read_to_string(repo_root().join(".github/workflows/ci.yml")).unwrap(),
    )
    .expect("ci.yml must parse as YAML");
    let mut job = ci
        .get("jobs")
        .and_then(|jobs| jobs.get("pr-loop-benchmarks"))
        .expect("benchmark job must exist")
        .clone();
    job.as_mapping_mut().unwrap().insert(
        serde_yaml::Value::String("needs".to_string()),
        serde_yaml::from_str("[check]").unwrap(),
    );
    assert!(
        validate_pr_loop_benchmark_step_set(&job).is_err(),
        "`needs` must be rejected because it can skip benchmark evidence"
    );
}

#[test]
fn ci_pr_loop_benchmark_contract_rejects_missing_or_wrong_evidence_validation() {
    let ci: serde_yaml::Value = serde_yaml::from_str(
        &fs::read_to_string(repo_root().join(".github/workflows/ci.yml")).unwrap(),
    )
    .expect("ci.yml must parse as YAML");
    let base = ci
        .get("jobs")
        .and_then(|jobs| jobs.get("pr-loop-benchmarks"))
        .expect("benchmark job must exist");

    let mut missing = base.clone();
    missing
        .get_mut("steps")
        .and_then(|steps| steps.as_sequence_mut())
        .expect("benchmark job must have mutable steps")
        .remove(8);
    assert!(
        validate_pr_loop_benchmark_step_set(&missing).is_err(),
        "a successful harness must be followed by the required evidence validation"
    );

    let mut wrong_path = base.clone();
    wrong_path
        .get_mut("steps")
        .and_then(|steps| steps.as_sequence_mut())
        .expect("benchmark job must have mutable steps")[8]
        .as_mapping_mut()
        .unwrap()
        .insert(
            serde_yaml::Value::String("run".to_string()),
            serde_yaml::Value::String(
                "test -f \"$BENCHMARK_OUTPUT/wrong-result.json\"".to_string(),
            ),
        );
    assert!(
        validate_pr_loop_benchmark_step_set(&wrong_path).is_err(),
        "the validation step must use the approved normalized-result and raw paths"
    );
}
