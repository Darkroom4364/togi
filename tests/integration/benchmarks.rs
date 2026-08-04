//! Deterministic end-to-end tests for the PR-loop benchmark harness (issue #487-A).
//!
//! These drive `benchmarks/pr-loop/run-pr-loop-benchmarks.sh` with a fake
//! `togi` binary and a stub `go`, so `cargo test` needs neither the Go
//! toolchain nor a built togi. The fake emits canned mutation reports keyed
//! off its argv, the applied scenario patch, and the presence of
//! `.togi-cache`, and logs every invocation so the tests can prove the
//! harness runs `check --base HEAD` (never `--all`) and manages the
//! per-scenario temp-project/cache lifecycle correctly.
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
# Tests may decouple schemata evidence from argv to prove the harness checks
# runner_mode against the report, not just the manifest.
if [ "${FAKE_TOGI_SCHEMATA:-}" = "on" ]; then
  schemata=true
elif [ "${FAKE_TOGI_SCHEMATA:-}" = "off" ]; then
  schemata=false
fi

# The multi-file scenario patch rewrites calc.go to call Sum; the
# single-file scenario leaves that line untouched.
scenario=single
if grep -q 'Sum(a, b)' calc.go; then
  scenario=multi
fi

total=${FAKE_TOGI_TOTAL:-4}
if [ "$scenario" = multi ]; then
  total=${FAKE_TOGI_TOTAL:-9}
fi
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
  schemata_json=$(jq -nc --argjson total "$total" '
    {fast_path: 1, fallback: ($total - 1),
     fallback_reasons: [{reason: "unsupported_operator", count: ($total - 1)}]}')
fi

if [ "$scenario" = multi ]; then
  mutations_json=$(jq -nc --arg state "$state" --argjson count "${FAKE_TOGI_MUTATIONS:-9}" '
    ([
      {id: 1, file: "calc.go", line: 5, column: 22, operator: "zero_to_one",
       original: "0", replacement: "1", description: "fake",
       result: "killed", execution: {state: $state}, language: "go"},
      {id: 2, file: "calc.go", line: 5, column: 22, operator: "increment_numeric",
       original: "0", replacement: "1", description: "fake",
       result: "killed", execution: {state: $state}, language: "go"},
      {id: 3, file: "calc.go", line: 5, column: 22, operator: "decrement_numeric",
       original: "0", replacement: "-1", description: "fake",
       result: "killed", execution: {state: $state}, language: "go"},
      {id: 4, file: "calc.go", line: 5, column: 20, operator: "plus_to_minus",
       original: "+", replacement: "-", description: "fake",
       result: "survived", execution: {state: $state}, language: "go"},
      {id: 5, file: "numbers.go", line: 5, column: 12, operator: "zero_to_one",
       original: "0", replacement: "1", description: "fake",
       result: "killed", execution: {state: $state}, language: "go"},
      {id: 6, file: "numbers.go", line: 5, column: 12, operator: "increment_numeric",
       original: "0", replacement: "1", description: "fake",
       result: "killed", execution: {state: $state}, language: "go"},
      {id: 7, file: "numbers.go", line: 5, column: 12, operator: "decrement_numeric",
       original: "0", replacement: "-1", description: "fake",
       result: "killed", execution: {state: $state}, language: "go"},
      {id: 8, file: "numbers.go", line: 5, column: 10, operator: "plus_to_minus",
       original: "+", replacement: "-", description: "fake",
       result: "survived", execution: {state: $state}, language: "go"},
      {id: 9, file: "numbers.go", line: 5, column: 16, operator: "plus_to_minus",
       original: "+", replacement: "-", description: "fake",
       result: "killed", execution: {state: $state}, language: "go"}
    ] | .[0:$count])')
else
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
fi

if [ "$scenario" = multi ] && [ "${FAKE_TOGI_MULTI_ONE_FILE:-}" = "1" ]; then
  mutations_json=$(jq 'map(.file = "calc.go" | .line = 5)' <<<"$mutations_json")
fi

jq -n \
  --arg state "$state" \
  --argjson total "$total" \
  --argjson tested "$tested" \
  --argjson exact "$exact" \
  --argjson schemata "$schemata_json" \
  --arg selection "${FAKE_TOGI_TEST_SELECTION:-full}" \
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
    mutations: ($mutations | map(if $selection == "narrowed" then . + {test_selection: {mode: "narrowed"}} else . end))
  }'
exit 1
"#;

const FAKE_GO: &str = r#"#!/usr/bin/env bash
if [ "${1:-}" = "version" ]; then
  echo "go version go1.0.0-stub test/test"
  exit 0
fi
if [ "${1:-}" = "env" ] && [ "${2:-}" = "GOCACHE" ]; then
  if [ -n "${FAKE_GO_ENV_GOCACHE:-}" ]; then
    printf '%s\n' "$FAKE_GO_ENV_GOCACHE"
  elif [ -n "${GOCACHE:-}" ]; then
    printf '%s\n' "$GOCACHE"
  else
    printf '%s\n' "/fake-go-build-cache"
  fi
  exit 0
fi
exit 0
"#;

const WORKLOAD_NAMES: [&str; 6] = [
    "cold-regular",
    "warm-exact-cache",
    "cold-schemata",
    "pr-diff-default",
    "multi-file-regular",
    "multi-file-default",
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script_path() -> PathBuf {
    repo_root().join("benchmarks/pr-loop/run-pr-loop-benchmarks.sh")
}

fn manifest_path() -> PathBuf {
    repo_root().join("benchmarks/pr-loop/manifest.json")
}

fn command_available(mut command: Command) -> bool {
    command
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
    command_available(Command::new("bash"))
        && command_available(Command::new("git"))
        && command_available(Command::new("jq"))
        && command_available(Command::new("python3"))
        && tool_on_path("sed")
        && (tool_on_path("sha256sum") || tool_on_path("shasum"))
}

/// Content digest helper mirroring the harness's sha256sum/shasum fallback,
/// used to re-digest artifact files after deliberate test tampering.
fn sha256_file(path: &Path) -> String {
    let output = if tool_on_path("sha256sum") {
        Command::new("sha256sum")
            .arg(path)
            .output()
            .expect("spawn sha256sum")
    } else {
        Command::new("shasum")
            .args(["-a", "256"])
            .arg(path)
            .output()
            .expect("spawn shasum")
    };
    assert!(output.status.success(), "sha256 tool failed for {path:?}");
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .expect("sha256 output")
        .to_string()
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
        // Cache provenance must come from the test, never the host env.
        .env_remove("GOCACHE")
        .env_remove("BENCH_GO_BUILD_CACHE_STATE")
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

fn invocation_log(tools: &FakeTools) -> Vec<serde_json::Value> {
    fs::read_to_string(&tools.log)
        .expect("invocation log")
        .lines()
        .map(|line| serde_json::from_str(line).expect("invocation log line"))
        .collect()
}

#[test]
fn pr_loop_harness_runs_all_six_workloads_across_two_scenarios() {
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
    assert_eq!(result["schema_version"], 2);
    assert_eq!(result["ok"], true);
    assert_eq!(result["failures"], serde_json::json!([]));
    assert_eq!(result["provenance"]["go_build_cache_state"], "unclassified");
    assert_eq!(result["provenance"]["go_build_cache_policy"], "unenforced");
    assert_eq!(
        result["provenance"]["go_build_cache_path"],
        serde_json::Value::Null
    );
    let mut fixture_scenarios: Vec<&str> = result["provenance"]["fixture_scenarios"]
        .as_object()
        .expect("fixture scenarios")
        .keys()
        .map(String::as_str)
        .collect();
    fixture_scenarios.sort_unstable();
    assert_eq!(fixture_scenarios, ["multi-file", "single-file"]);

    let scenarios = result["cross_workload"]["scenarios"]
        .as_object()
        .expect("per-scenario identity");
    for name in ["single-file", "multi-file"] {
        assert_eq!(
            scenarios[name]["mutation_identity_consistent"], true,
            "scenario {name} must have consistent mutation identity"
        );
        assert!(scenarios[name]["mutation_identity_sha256"].is_string());
    }
    assert_ne!(
        scenarios["single-file"]["mutation_identity_sha256"],
        scenarios["multi-file"]["mutation_identity_sha256"],
        "scenarios legitimately produce different mutation identities"
    );

    let workloads = result["workloads"].as_array().expect("workloads array");
    let names: Vec<&str> = workloads
        .iter()
        .map(|workload| workload["name"].as_str().expect("workload name"))
        .collect();
    assert_eq!(names, WORKLOAD_NAMES);
    let scenario_of: Vec<&str> = workloads
        .iter()
        .map(|workload| workload["scenario"].as_str().expect("workload scenario"))
        .collect();
    assert_eq!(
        scenario_of,
        [
            "single-file",
            "single-file",
            "single-file",
            "single-file",
            "multi-file",
            "multi-file"
        ]
    );
    let runner_modes: Vec<&str> = workloads
        .iter()
        .map(|workload| workload["runner_mode"].as_str().expect("runner mode"))
        .collect();
    assert_eq!(
        runner_modes,
        [
            "regular", "regular", "schemata", "default", "regular", "default"
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
        assert_eq!(
            workload["semantics"]["selected_test_command"],
            serde_json::json!(["go", "test", "./..."]),
            "workload {} must expose the resolved test command",
            workload["name"]
        );
        assert_eq!(
            workload["semantics"]["test_selection"]["mode"],
            "full-suite"
        );
        assert_eq!(
            workload["semantics"]["test_selection"]["full_suite_mutation_count"],
            workload["semantics"]["mutation_count"]
        );
        assert_eq!(
            workload["semantics"]["test_selection"]["narrowed_mutation_count"],
            0
        );
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
    // Multi-file coverage: 9 mutations spanning both changed files.
    assert_eq!(workloads[4]["semantics"]["total"], 9);
    assert_eq!(workloads[4]["semantics"]["tested"], 9);
    assert_eq!(workloads[5]["semantics"]["total"], 9);

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

    // Invocation log: one temp project per scenario, `check --base HEAD`,
    // per-scenario cache lifecycle.
    let runs = invocation_log(&tools);
    assert_eq!(runs.len(), 6, "expected exactly six togi invocations");

    let single_project = runs[0]["cwd"].as_str().expect("invocation cwd");
    let multi_project = runs[4]["cwd"].as_str().expect("invocation cwd");
    assert_ne!(
        Path::new(single_project),
        repo_root().join("tests/fixtures/go"),
        "harness must copy the fixture into disposable projects"
    );
    assert_ne!(
        single_project, multi_project,
        "each scenario must get its own disposable project"
    );
    for (index, run) in runs.iter().enumerate() {
        let expected = if index < 4 {
            single_project
        } else {
            multi_project
        };
        assert_eq!(
            run["cwd"].as_str().expect("cwd"),
            expected,
            "workloads of one scenario must share that scenario's project"
        );
        assert_pr_diff_command(&argv_strings(run));
    }
    let cache_sequence: Vec<bool> = runs
        .iter()
        .map(|run| run["cache_present"].as_bool().expect("cache flag"))
        .collect();
    assert_eq!(
        cache_sequence,
        [false, true, false, false, false, false],
        "cold runs must start cacheless and the warm run must reuse the seeded cache"
    );
    let force_rerun_sequence: Vec<bool> = runs
        .iter()
        .map(|run| argv_strings(run).contains(&"--force-rerun"))
        .collect();
    assert_eq!(
        force_rerun_sequence,
        [true, false, true, false, true, false]
    );

    assert!(
        !Path::new(single_project).exists() && !Path::new(multi_project).exists(),
        "disposable projects must be removed after the run"
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

    let mut five_workloads = base.clone();
    five_workloads["workloads"]
        .as_array_mut()
        .unwrap()
        .truncate(5);

    let mut wrong_order = base.clone();
    wrong_order["workloads"].as_array_mut().unwrap().reverse();

    let mut warm_not_reuse = base.clone();
    warm_not_reuse["workloads"][1]["cache"] = serde_json::json!("fresh");

    let mut missing_count = base.clone();
    missing_count["scenarios"][0]
        .as_object_mut()
        .unwrap()
        .remove("expected_mutation_count");

    let mut zero_count = base.clone();
    zero_count["scenarios"][0]["expected_mutation_count"] = serde_json::json!(0);

    let mut bad_patch_digest = base.clone();
    bad_patch_digest["scenarios"][1]["patch_sha256"] = serde_json::json!("not-a-digest");

    let mut empty_changed_files = base.clone();
    empty_changed_files["scenarios"][1]["changed_files"] = serde_json::json!([]);

    let mut inverted_line_range = base.clone();
    inverted_line_range["scenarios"][1]["changed_files"][0]["line_range"] =
        serde_json::json!([5, 4]);

    let mut missing_scenario_field = base.clone();
    missing_scenario_field["workloads"][0]
        .as_object_mut()
        .unwrap()
        .remove("scenario");

    let mut undeclared_scenario = base.clone();
    undeclared_scenario["workloads"][0]["scenario"] = serde_json::json!("no-such-scenario");

    let mut missing_scenarios = base.clone();
    missing_scenarios
        .as_object_mut()
        .unwrap()
        .remove("scenarios");

    let mut dropped_multi_file_scenario = base.clone();
    dropped_multi_file_scenario["scenarios"]
        .as_array_mut()
        .unwrap()
        .truncate(1);

    let mut interleaved_scenarios = base.clone();
    {
        let workloads = interleaved_scenarios["workloads"].as_array_mut().unwrap();
        let moved = workloads.remove(4);
        workloads.insert(1, moved);
    }

    let mut cross_scenario_dependency = base.clone();
    cross_scenario_dependency["workloads"][1]["expects_cache_from"] =
        serde_json::json!("multi-file-regular");

    let mut dependency_without_seed = base.clone();
    dependency_without_seed["workloads"][2]["expects_cache_from"] =
        serde_json::json!("pr-diff-default");

    let mut forward_dependency = base.clone();
    forward_dependency["workloads"][0]["expects_cache_from"] =
        serde_json::json!("warm-exact-cache");

    let mut unknown_invariant = base.clone();
    unknown_invariant["workloads"][0]["invariants"] = serde_json::json!(["made-up-invariant"]);

    let mut previous_schema = base.clone();
    previous_schema["schema_version"] = serde_json::json!(1);

    let mut future_schema = base.clone();
    future_schema["schema_version"] = serde_json::json!(3);

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

    let mut runner_mode_mismatch = base.clone();
    runner_mode_mismatch["workloads"][0]["runner_mode"] = serde_json::json!("schemata");

    let mut default_with_schemata_flag = base.clone();
    default_with_schemata_flag["workloads"][3]["extra_args"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!("--schemata"));

    let mut regular_without_flag = base.clone();
    regular_without_flag["workloads"][0]["extra_args"] = serde_json::json!([]);

    let mut unknown_runner_mode = base.clone();
    unknown_runner_mode["workloads"][0]["runner_mode"] = serde_json::json!("turbo");

    let mut duplicate_test_cmd = base.clone();
    duplicate_test_cmd["workloads"][0]["extra_args"]
        .as_array_mut()
        .unwrap()
        .extend([
            serde_json::json!("--test-cmd"),
            serde_json::json!("go test ./..."),
        ]);

    let cases: Vec<(&str, serde_json::Value)> = vec![
        ("missing workloads field", missing_workloads),
        ("empty workloads array", empty_workloads),
        ("non-array workloads", object_workloads),
        ("only five workloads", five_workloads),
        ("workloads in wrong order", wrong_order),
        ("warm workload not reusing cache", warm_not_reuse),
        ("missing expected_mutation_count", missing_count),
        ("zero expected_mutation_count", zero_count),
        ("malformed patch digest", bad_patch_digest),
        ("empty changed_files", empty_changed_files),
        ("inverted line range", inverted_line_range),
        ("workload missing scenario", missing_scenario_field),
        ("undeclared scenario reference", undeclared_scenario),
        ("missing scenarios field", missing_scenarios),
        ("dropped multi-file scenario", dropped_multi_file_scenario),
        ("interleaved scenario workloads", interleaved_scenarios),
        ("cross-scenario cache dependency", cross_scenario_dependency),
        (
            "cache dependency without seeds_cache",
            dependency_without_seed,
        ),
        ("forward cache dependency", forward_dependency),
        ("unknown invariant name", unknown_invariant),
        ("previous schema_version", previous_schema),
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
        ("runner_mode contradicting argv", runner_mode_mismatch),
        (
            "default workload with --schemata",
            default_with_schemata_flag,
        ),
        (
            "regular workload without --no-schemata",
            regular_without_flag,
        ),
        ("unknown runner_mode", unknown_runner_mode),
        ("duplicate --test-cmd in extra_args", duplicate_test_cmd),
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
fn pr_loop_harness_rejects_scenario_patch_digest_drift() {
    if !harness_tools_available() {
        eprintln!("skipping: harness tools (bash/git/jq/sed/sha256sum|shasum/python3) unavailable");
        return;
    }
    let tools = install_fake_tools();
    let mut manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(manifest_path()).expect("read repo manifest"))
            .expect("repo manifest is valid JSON");
    // A well-formed 64-hex digest that does not match the multi-file patch.
    manifest["scenarios"][1]["patch_sha256"] = serde_json::json!("0".repeat(64));
    let dir = tempfile::tempdir().expect("case tempdir");
    let manifest_file = dir.path().join("manifest.json");
    fs::write(
        &manifest_file,
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .expect("write case manifest");
    let output = run_harness(&tools, &dir.path().join("out"), Some(&manifest_file), &[]);
    assert!(!output.status.success(), "patch digest drift must fail");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("digest mismatch"),
        "expected digest mismatch error, got: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn pr_loop_harness_enforces_go_build_cache_provenance() {
    if !harness_tools_available() {
        eprintln!("skipping: harness tools (bash/git/jq/sed/sha256sum|shasum/python3) unavailable");
        return;
    }

    // warmup and primed runs bind and record the job-private explicit cache.
    for state in ["warmup", "primed"] {
        let tools = install_fake_tools();
        let gocache = tempfile::tempdir().expect("gocache tempdir");
        let out_dir = tempfile::tempdir().expect("output tempdir");
        let cache_path = gocache.path().to_str().expect("gocache path").to_string();
        let output = run_harness(
            &tools,
            out_dir.path(),
            None,
            &[
                ("BENCH_GO_BUILD_CACHE_STATE", state),
                ("GOCACHE", &cache_path),
            ],
        );
        assert!(
            output.status.success(),
            "{state} run with a valid GOCACHE must succeed\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result = read_result(out_dir.path());
        assert_eq!(result["provenance"]["go_build_cache_state"], state);
        assert_eq!(
            result["provenance"]["go_build_cache_policy"],
            "job-private-explicit-gocache"
        );
        assert_eq!(
            result["provenance"]["go_build_cache_path"],
            serde_json::Value::String(cache_path.clone())
        );
    }

    // Rejection conditions: missing, relative, nonexistent, mismatched, or
    // unknown cache state must never produce measured evidence.
    let tools = install_fake_tools();
    let gocache = tempfile::tempdir().expect("gocache tempdir");
    let cache_path = gocache.path().to_str().expect("gocache path").to_string();
    let missing_dir = gocache.path().join("does-not-exist");
    let cases: Vec<(&str, Vec<(&str, &str)>)> = vec![
        (
            "primed without GOCACHE",
            vec![("BENCH_GO_BUILD_CACHE_STATE", "primed")],
        ),
        (
            "warmup without GOCACHE",
            vec![("BENCH_GO_BUILD_CACHE_STATE", "warmup")],
        ),
        (
            "relative GOCACHE",
            vec![
                ("BENCH_GO_BUILD_CACHE_STATE", "primed"),
                ("GOCACHE", "relative/cache"),
            ],
        ),
        (
            "nonexistent GOCACHE directory",
            vec![
                ("BENCH_GO_BUILD_CACHE_STATE", "primed"),
                ("GOCACHE", missing_dir.to_str().expect("missing path")),
            ],
        ),
        (
            "GOCACHE disagreeing with go env",
            vec![
                ("BENCH_GO_BUILD_CACHE_STATE", "primed"),
                ("GOCACHE", &cache_path),
                ("FAKE_GO_ENV_GOCACHE", "/somewhere/else"),
            ],
        ),
        (
            "unknown cache state",
            vec![("BENCH_GO_BUILD_CACHE_STATE", "prime")],
        ),
    ];
    for (label, env) in cases {
        let out_dir = tempfile::tempdir().expect("output tempdir");
        let output = run_harness(&tools, out_dir.path(), None, &env);
        assert_eq!(
            output.status.code(),
            Some(2),
            "harness must exit 2 for {label}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !out_dir
                .path()
                .join("pr-loop-benchmark-result.json")
                .exists(),
            "{label} must never produce a result"
        );
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
            "harness must fail with {emitted} mutation records\nstdout: {}\nstderr: {}",
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
    if !command_available(Command::new("bash")) {
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

    // The fake reports five mutations while the single-file scenario pins
    // four: the report-well-formed invariant must fail the harness, never
    // pass silently.
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

#[test]
fn pr_loop_harness_fails_when_runner_mode_report_disagrees() {
    if !harness_tools_available() {
        eprintln!("skipping: harness tools (bash/git/jq/sed/sha256sum|shasum/python3) unavailable");
        return;
    }
    // A regular workload whose report carries schemata evidence (or a
    // schemata workload without it) contradicts the declared runner_mode.
    // The fake's FAKE_TOGI_SCHEMATA override decouples its schemata evidence
    // from argv so the manifest stays valid.
    for (name, override_value) in [("cold-regular", "on"), ("cold-schemata", "off")] {
        let tools = install_fake_tools();
        let out_dir = tempfile::tempdir().expect("output tempdir");
        let output = run_harness(
            &tools,
            out_dir.path(),
            None,
            &[("FAKE_TOGI_SCHEMATA", override_value)],
        );
        assert!(
            !output.status.success(),
            "runner_mode/report disagreement must fail for {name}\nstderr: {}",
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
        let expected = format!("{name}:runner-mode-consistency");
        assert!(
            failures.contains(&expected.as_str()),
            "expected {expected} in {failures:?}"
        );
    }
}

#[test]
fn pr_loop_harness_rejects_narrowed_selection_and_single_file_multi_evidence() {
    if !harness_tools_available() {
        eprintln!("skipping: harness tools unavailable");
        return;
    }
    for (environment, expected) in [
        (
            ("FAKE_TOGI_TEST_SELECTION", "narrowed"),
            "cold-regular:report-well-formed",
        ),
        (
            ("FAKE_TOGI_MULTI_ONE_FILE", "1"),
            "multi-file-regular:pr-diff-targeting",
        ),
    ] {
        let tools = install_fake_tools();
        let output_dir = tempfile::tempdir().expect("output tempdir");
        let output = run_harness(&tools, output_dir.path(), None, &[environment]);
        assert!(!output.status.success(), "invalid evidence must fail");
        let result = read_result(output_dir.path());
        let failures: Vec<&str> = result["failures"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item.as_str().unwrap())
            .collect();
        assert!(
            failures.contains(&expected),
            "expected {expected} in {failures:?}"
        );
    }
}

const APPROVED_GO_CACHE_CREATION: &str = r#"set -euo pipefail
if [ -e "$BENCH_GOCACHE" ] && [ -n "$(ls -A "$BENCH_GOCACHE" 2>/dev/null)" ]; then
  echo "job-private Go build cache $BENCH_GOCACHE must start empty" >&2
  exit 1
fi
mkdir -p "$BENCH_GOCACHE"
test -d "$BENCH_GOCACHE"
test -z "$(ls -A "$BENCH_GOCACHE")"
"#;

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
       == ["cold-regular", "warm-exact-cache", "cold-schemata", "pr-diff-default",
           "multi-file-regular", "multi-file-default"])
' "$result" >/dev/null
for workload in cold-regular warm-exact-cache cold-schemata pr-diff-default multi-file-regular multi-file-default; do
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
    if job.get("env").is_some() {
        return Err("benchmark job must not define job-level environment".to_string());
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
        ("name", "Create job-private Go build cache"),
        ("name", "Check benchmark prerequisites"),
        ("name", "Build release binary"),
        ("name", "Warm Go build cache for PR-loop benchmark evidence"),
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
        if index == 11 {
            if condition != Some("${{ always() }}") {
                return Err("artifact upload must be the sole unconditional step".to_string());
            }
        } else if condition.is_some() {
            return Err(format!("benchmark step {index} must not be conditional"));
        }
    }
    for index in [0, 1, 3, 4, 11] {
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
    if steps[4]
        .get("with")
        .and_then(|with| with.get("go-version"))
        .and_then(|value| value.as_str())
        != Some("1.26.5")
    {
        return Err("benchmark job must pin Go to exactly 1.26.5".to_string());
    }
    if steps[5].get("shell").and_then(|value| value.as_str()) != Some("bash")
        || steps[5].get("run").and_then(|value| value.as_str()) != Some(APPROVED_GO_CACHE_CREATION)
        || steps[5]
            .get("env")
            .and_then(|env| env.get("BENCH_GOCACHE"))
            .and_then(|value| value.as_str())
            != Some("${{ runner.temp }}/togi-pr-loop-gocache")
    {
        return Err(
            "cache creation step must bind and create the job-private GOCACHE fail-closed"
                .to_string(),
        );
    }
    if steps[6].get("shell").and_then(|value| value.as_str()) != Some("bash")
        || steps[6].get("run").and_then(|value| value.as_str())
            != Some(APPROVED_BENCHMARK_PREREQUISITES)
    {
        return Err(
            "prerequisite step must contain only the approved tool install/check command"
                .to_string(),
        );
    }
    if steps[7].get("run").and_then(|value| value.as_str())
        != Some("cargo build --locked --release")
    {
        return Err("only the named release-build step may build togi".to_string());
    }
    for index in [8, 9] {
        if steps[index]
            .get("run")
            .and_then(|value| value.as_str())
            .map(str::trim)
            != Some(
                "bash benchmarks/pr-loop/run-pr-loop-benchmarks.sh --output \"$BENCHMARK_OUTPUT\"",
            )
        {
            return Err(
                "only the named warmup and harness steps may run the benchmark harness".to_string(),
            );
        }
        if steps[index]
            .get("env")
            .and_then(|env| env.get("GOCACHE"))
            .and_then(|value| value.as_str())
            != Some("${{ runner.temp }}/togi-pr-loop-gocache")
        {
            return Err(
                "warmup and measured steps must bind the identical explicit GOCACHE".to_string(),
            );
        }
    }
    if steps[8]
        .get("env")
        .and_then(|env| env.get("BENCH_GO_BUILD_CACHE_STATE"))
        .and_then(|value| value.as_str())
        != Some("warmup")
        || steps[9]
            .get("env")
            .and_then(|env| env.get("BENCH_GO_BUILD_CACHE_STATE"))
            .and_then(|value| value.as_str())
            != Some("primed")
    {
        return Err("warmup must precede primed measured evidence".to_string());
    }
    if steps[10].get("shell").and_then(|value| value.as_str()) != Some("bash")
        || steps[10]
            .get("env")
            .and_then(|env| env.get("BENCHMARK_OUTPUT"))
            .and_then(|value| value.as_str())
            != Some("${{ runner.temp }}/togi-pr-loop-benchmarks/measured")
        || steps[10].get("run").and_then(|value| value.as_str())
            != Some(APPROVED_BENCHMARK_EVIDENCE_VALIDATION)
    {
        return Err(
            "only the named validation step may verify measured benchmark evidence".to_string(),
        );
    }
    if steps[11].get("if").and_then(|value| value.as_str()) != Some("${{ always() }}") {
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

    let create_cache = step("Create job-private Go build cache");
    let warmup = step("Warm Go build cache for PR-loop benchmark evidence");
    let create_index = steps
        .iter()
        .position(|step| std::ptr::eq(step, create_cache))
        .expect("cache creation step index");
    let warmup_index = steps
        .iter()
        .position(|step| std::ptr::eq(step, warmup))
        .expect("warmup step index");
    assert!(
        create_index < warmup_index,
        "job-private cache creation must precede warmup"
    );

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
        Some("${{ runner.temp }}/togi-pr-loop-benchmarks/measured")
    );
    assert_eq!(
        harness
            .get("env")
            .and_then(|env| env.get("BENCH_GO_BUILD_CACHE_STATE"))
            .and_then(|value| value.as_str()),
        Some("primed")
    );
    assert_eq!(
        harness
            .get("env")
            .and_then(|env| env.get("GOCACHE"))
            .and_then(|value| value.as_str()),
        Some("${{ runner.temp }}/togi-pr-loop-gocache")
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
    let prerequisite = steps[6]
        .get("run")
        .and_then(|run| run.as_str())
        .expect("prerequisite step must have a run command")
        .to_string();
    steps[6].as_mapping_mut().unwrap().insert(
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
            .expect("benchmark job must have mutable steps")[8]
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
fn ci_pr_loop_benchmark_contract_rejects_go_version_and_cache_drift() {
    let ci: serde_yaml::Value = serde_yaml::from_str(
        &fs::read_to_string(repo_root().join(".github/workflows/ci.yml")).unwrap(),
    )
    .expect("ci.yml must parse as YAML");
    let base = ci
        .get("jobs")
        .and_then(|jobs| jobs.get("pr-loop-benchmarks"))
        .expect("benchmark job must exist");

    let mut unpinned = base.clone();
    unpinned
        .get_mut("steps")
        .and_then(|steps| steps.as_sequence_mut())
        .expect("benchmark job must have mutable steps")[4]
        .get_mut("with")
        .and_then(|with| with.as_mapping_mut())
        .expect("setup-go must have inputs")
        .insert(
            serde_yaml::Value::String("go-version".to_string()),
            serde_yaml::Value::String("stable".to_string()),
        );
    assert!(
        validate_pr_loop_benchmark_step_set(&unpinned).is_err(),
        "an unpinned Go toolchain must be rejected"
    );

    let mut missing_create = base.clone();
    missing_create
        .get_mut("steps")
        .and_then(|steps| steps.as_sequence_mut())
        .expect("benchmark job must have mutable steps")
        .remove(5);
    assert!(
        validate_pr_loop_benchmark_step_set(&missing_create).is_err(),
        "dropping the cache creation step must be rejected"
    );

    let mut divergent_gocache = base.clone();
    divergent_gocache
        .get_mut("steps")
        .and_then(|steps| steps.as_sequence_mut())
        .expect("benchmark job must have mutable steps")[9]
        .get_mut("env")
        .and_then(|env| env.as_mapping_mut())
        .expect("harness step must have env")
        .insert(
            serde_yaml::Value::String("GOCACHE".to_string()),
            serde_yaml::Value::String("/tmp/other-cache".to_string()),
        );
    assert!(
        validate_pr_loop_benchmark_step_set(&divergent_gocache).is_err(),
        "a measured GOCACHE diverging from warmup must be rejected"
    );
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
        .remove(9);
    assert!(
        validate_pr_loop_benchmark_step_set(&missing).is_err(),
        "a successful harness must be followed by the required evidence validation"
    );

    let mut wrong_path = base.clone();
    wrong_path
        .get_mut("steps")
        .and_then(|steps| steps.as_sequence_mut())
        .expect("benchmark job must have mutable steps")[10]
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

fn collector_path() -> PathBuf {
    repo_root().join("benchmarks/pr-loop/collect-calibration.py")
}

fn run_collector(output: &Path, results: &[PathBuf]) -> Output {
    let mut command = Command::new("python3");
    command
        .arg(collector_path())
        .arg("--output")
        .arg(output)
        .arg("--source-commit")
        .arg("test-commit")
        .arg("--source-run")
        .arg("test-run")
        .arg("--source-attempt")
        .arg("1")
        .arg("--source-utc")
        .arg("2026-08-04T00:00:00Z");
    for result in results {
        command.arg(result);
    }
    command.output().expect("spawn collector")
}

/// Runs five primed samples bound to one explicit GOCACHE, mirroring the
/// calibration workflow's measured acquisition.
fn run_primed_samples(tools: &FakeTools, samples_dir: &Path, gocache: &Path) -> Vec<PathBuf> {
    let cache_path = gocache.to_str().expect("gocache path");
    let mut results = Vec::new();
    for sample in 1..=5 {
        let output = samples_dir.join(format!("sample-{sample}"));
        let run = run_harness(
            tools,
            &output,
            None,
            &[
                ("BENCH_GO_BUILD_CACHE_STATE", "primed"),
                ("GOCACHE", cache_path),
            ],
        );
        assert!(
            run.status.success(),
            "sample {sample} harness run failed\nstderr: {}",
            String::from_utf8_lossy(&run.stderr)
        );
        results.push(output.join("pr-loop-benchmark-result.json"));
    }
    results
}

#[test]
fn pr_loop_calibration_collector_fails_closed_and_preserves_samples() {
    if !harness_tools_available() || !tool_on_path("python3") {
        eprintln!("skipping: harness or Python tools unavailable");
        return;
    }
    let tools = install_fake_tools();
    let gocache = tempfile::tempdir().expect("gocache tempdir");
    let samples = tempfile::tempdir().expect("sample tempdir");
    let results = run_primed_samples(&tools, samples.path(), gocache.path());
    let candidate = samples.path().join("candidate.json");
    let output = run_collector(&candidate, &results);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&candidate).expect("candidate")).expect("JSON");
    assert_eq!(value["kind"], "togi_pr_loop_calibration_candidate");
    assert_eq!(value["schema_version"], 2);
    assert_eq!(value["source"]["commit"], "test-commit");
    for name in WORKLOAD_NAMES {
        assert_eq!(
            value["samples"][name]["wall_ms"].as_array().unwrap().len(),
            5,
            "workload {name} must retain five wall samples"
        );
    }
    assert!(value["samples"]["cold-regular"]["wall_ms_median"].is_number());
    assert!(value["samples"]["cold-regular"]["wall_ms_mad"].is_number());
    assert_eq!(
        value["measurement_identity"]["go_build_cache_state"],
        "primed"
    );
    assert_eq!(
        value["measurement_identity"]["go_build_cache_policy"],
        "job-private-explicit-gocache"
    );
    assert!(
        value["measurement_identity"]
            .get("go_build_cache_path")
            .is_none(),
        "the volatile cache path must not be measurement identity"
    );
    assert_eq!(
        value["runner_diagnostics"][0]["go_build_cache_path"],
        serde_json::Value::String(gocache.path().to_str().expect("gocache path").to_string()),
        "the cache path is kept as per-sample diagnostic evidence"
    );
    let mut identity_scenarios: Vec<&str> =
        value["semantic_identity"]["scenario_mutation_identity"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
    identity_scenarios.sort_unstable();
    assert_eq!(identity_scenarios, ["multi-file", "single-file"]);

    let wrong_count = run_collector(&samples.path().join("wrong-count.json"), &results[..4]);
    assert!(!wrong_count.status.success());
    let duplicate_candidate = samples.path().join("duplicate.json");
    let duplicates = vec![results[0].clone(); 5];
    assert!(
        !run_collector(&duplicate_candidate, &duplicates)
            .status
            .success()
    );
    assert!(
        !duplicate_candidate.exists(),
        "duplicate inputs must not create a candidate"
    );
    let copied_duplicate_dir = samples.path().join("copied-duplicates");
    fs::create_dir(&copied_duplicate_dir).unwrap();
    let copied_duplicates: Vec<PathBuf> = (1..=5)
        .map(|index| {
            let copied = copied_duplicate_dir.join(format!("copy-{index}.json"));
            fs::copy(&results[0], &copied).unwrap();
            copied
        })
        .collect();
    let copied_duplicate_candidate = samples.path().join("copied-duplicate.json");
    let copied_duplicate = run_collector(&copied_duplicate_candidate, &copied_duplicates);
    assert_eq!(copied_duplicate.status.code(), Some(2));
    assert!(
        !copied_duplicate_candidate.exists(),
        "byte-identical copied inputs must not create a candidate"
    );

    // A sample measured against a different cache path is not comparable.
    let other_gocache = tempfile::tempdir().expect("other gocache");
    let other_sample_dir = samples.path().join("other-cache-sample");
    assert!(
        run_harness(
            &tools,
            &other_sample_dir,
            None,
            &[
                ("BENCH_GO_BUILD_CACHE_STATE", "primed"),
                (
                    "GOCACHE",
                    other_gocache.path().to_str().expect("other gocache path")
                ),
            ],
        )
        .status
        .success()
    );
    let mut divergent_cache = results.clone();
    divergent_cache[4] = other_sample_dir.join("pr-loop-benchmark-result.json");
    let divergent_candidate = samples.path().join("divergent-cache.json");
    let divergent = run_collector(&divergent_candidate, &divergent_cache);
    assert_eq!(divergent.status.code(), Some(2));
    assert!(
        !divergent_candidate.exists(),
        "divergent cache paths must not create a candidate"
    );

    let portable_dir = samples.path().join("different-leading-location");
    fs::create_dir(&portable_dir).unwrap();
    let portable_results: Vec<PathBuf> = results
        .iter()
        .enumerate()
        .map(|(index, result)| {
            let copied = portable_dir.join(format!("result-{index}.json"));
            fs::copy(result, &copied).unwrap();
            copied
        })
        .collect();
    let portable_candidate = samples.path().join("portable.json");
    assert!(
        run_collector(&portable_candidate, &portable_results)
            .status
            .success()
    );
    let portable: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(portable_candidate).unwrap()).unwrap();
    assert_eq!(portable["semantic_identity"], value["semantic_identity"]);
    assert!(
        portable["source_file_digests"]
            .as_object()
            .unwrap()
            .keys()
            .all(|key| !key.starts_with('/'))
    );

    for (field, replacement, output_name) in [
        (
            "/provenance/logical_cpu_count",
            serde_json::Value::Bool(true),
            "boolean-cpu",
        ),
        (
            "/workloads/0/timing/wall_ms",
            serde_json::Value::Bool(false),
            "boolean-timing",
        ),
        (
            "/provenance/fixture_source_dir",
            serde_json::Value::String("../outside".to_string()),
            "fixture-escape",
        ),
        (
            "/provenance/togi_version",
            serde_json::Value::String("different-togi".to_string()),
            "execution",
        ),
        (
            "/provenance/go_build_cache_state",
            serde_json::Value::String("warmup".to_string()),
            "go-cache-state",
        ),
        (
            "/provenance/go_build_cache_policy",
            serde_json::Value::String("unenforced".to_string()),
            "go-cache-policy",
        ),
        (
            "/provenance/go_build_cache_path",
            serde_json::Value::String("/elsewhere/go-cache".to_string()),
            "go-cache-path",
        ),
        (
            "/workloads/4/scenario",
            serde_json::Value::String("single-file".to_string()),
            "workload-scenario",
        ),
        (
            "/workloads/0/runner_mode",
            serde_json::Value::String("turbo".to_string()),
            "runner-mode",
        ),
    ] {
        let altered = samples.path().join(format!("{output_name}.json"));
        let mut result: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&results[4]).unwrap()).unwrap();
        *result.pointer_mut(field).unwrap() = replacement;
        fs::write(&altered, serde_json::to_vec(&result).unwrap()).unwrap();
        let mut mismatched = results.clone();
        mismatched[4] = altered;
        let rejected = run_collector(
            &samples.path().join(format!("{output_name}-out.json")),
            &mismatched,
        );
        assert!(!rejected.status.success());
        assert!(
            !String::from_utf8_lossy(&rejected.stderr).contains("Traceback"),
            "malformed input must have a collector error, not a traceback"
        );
    }
    for (replacement, output_name) in [
        (
            serde_json::Value::String("unclassified".to_string()),
            "unclassified",
        ),
        (serde_json::Value::Null, "missing-state"),
    ] {
        let altered = samples.path().join(format!("{output_name}.json"));
        let mut result: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&results[4]).unwrap()).unwrap();
        if replacement.is_null() {
            result["provenance"]
                .as_object_mut()
                .unwrap()
                .remove("go_build_cache_state");
        } else {
            result["provenance"]["go_build_cache_state"] = replacement;
        }
        fs::write(&altered, serde_json::to_vec(&result).unwrap()).unwrap();
        let mut mismatched = results.clone();
        mismatched[4] = altered;
        let candidate = samples.path().join(format!("{output_name}-out.json"));
        assert_eq!(
            run_collector(&candidate, &mismatched).status.code(),
            Some(2)
        );
        assert!(!candidate.exists());
    }

    let diagnostic = samples.path().join("diagnostic.json");
    let mut diagnostic_result: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&results[4]).unwrap()).unwrap();
    *diagnostic_result
        .pointer_mut("/provenance/kernel_release")
        .unwrap() = serde_json::Value::String("different-kernel".to_string());
    let mut diagnostic_results = results.clone();
    fs::write(&diagnostic, serde_json::to_vec(&diagnostic_result).unwrap()).unwrap();
    diagnostic_results[4] = diagnostic;
    let diagnostic_candidate = samples.path().join("diagnostic-out.json");
    assert!(
        run_collector(&diagnostic_candidate, &diagnostic_results)
            .status
            .success()
    );
    let diagnostics: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(diagnostic_candidate).unwrap()).unwrap();
    assert_eq!(
        diagnostics["runner_diagnostics"][4]["kernel_release"],
        "different-kernel"
    );

    let malformed = samples.path().join("malformed.json");
    fs::write(&malformed, "{").expect("malformed result");
    let mut malformed_results = results.clone();
    malformed_results[4] = malformed;
    assert!(
        !run_collector(
            &samples.path().join("malformed-out.json"),
            &malformed_results
        )
        .status
        .success()
    );

    for (field, output_name) in [
        ("/workloads/0/cache_policy", "semantic"),
        ("/provenance/runner_label", "runner"),
    ] {
        let altered = samples.path().join(format!("{output_name}.json"));
        let mut result: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&results[4]).unwrap()).unwrap();
        *result.pointer_mut(field).unwrap() = serde_json::Value::String("mismatch".to_string());
        fs::write(&altered, serde_json::to_vec(&result).unwrap()).unwrap();
        let mut mismatched = results.clone();
        mismatched[4] = altered;
        assert!(
            !run_collector(
                &samples.path().join(format!("{output_name}-out.json")),
                &mismatched
            )
            .status
            .success()
        );
    }
}

#[test]
fn pr_loop_calibration_workflow_is_manual_read_only_and_retained() {
    let workflow =
        fs::read_to_string(repo_root().join(".github/workflows/pr-loop-calibration.yml"))
            .expect("calibration workflow");
    let parsed: serde_yaml::Value = serde_yaml::from_str(&workflow).expect("workflow YAML");
    let job = &parsed["jobs"]["calibrate"];
    assert!(
        job.get("env").is_none(),
        "calibration job must not use job-level runner context"
    );
    let steps = job["steps"]
        .as_sequence()
        .expect("calibration must have steps");
    let step = |name| {
        steps
            .iter()
            .position(|item| item["name"].as_str() == Some(name))
            .map(|index| (index, &steps[index]))
            .unwrap_or_else(|| panic!("missing calibration step {name}"))
    };
    let setup_go = steps
        .iter()
        .find(|item| item["uses"].as_str() == Some("actions/setup-go@v7"))
        .expect("calibration must set up Go");
    assert_eq!(
        setup_go["with"]["go-version"].as_str(),
        Some("1.26.5"),
        "calibration must pin Go to exactly 1.26.5"
    );
    let (create_index, create) = step("Create job-private Go build cache");
    let (warmup_index, warmup) = step("Warm Go build cache");
    let (acquisition_index, acquisition) = step("Acquire five independent samples");
    assert_eq!(create["shell"].as_str(), Some("bash"));
    assert_eq!(create["run"].as_str(), Some(APPROVED_GO_CACHE_CREATION));
    assert_eq!(
        create["env"]["BENCH_GOCACHE"].as_str(),
        Some("${{ runner.temp }}/togi-pr-loop-gocache")
    );
    assert!(create_index < warmup_index);
    assert!(warmup_index < acquisition_index);
    assert_eq!(
        warmup["env"]["BENCH_GO_BUILD_CACHE_STATE"].as_str(),
        Some("warmup")
    );
    assert_eq!(
        warmup["env"]["GOCACHE"].as_str(),
        Some("${{ runner.temp }}/togi-pr-loop-gocache")
    );
    assert_eq!(
        warmup["env"]["CALIBRATION_OUTPUT"].as_str(),
        Some("${{ runner.temp }}/togi-pr-loop-calibration/warmup")
    );
    assert_eq!(
        warmup["run"].as_str(),
        Some("bash benchmarks/pr-loop/run-pr-loop-benchmarks.sh --output \"$CALIBRATION_OUTPUT\"")
    );
    assert_eq!(
        acquisition["env"]["BENCH_GO_BUILD_CACHE_STATE"].as_str(),
        Some("primed")
    );
    assert_eq!(
        acquisition["env"]["GOCACHE"].as_str(),
        Some("${{ runner.temp }}/togi-pr-loop-gocache"),
        "warmup and measured acquisition must bind the identical explicit GOCACHE"
    );
    assert_eq!(
        acquisition["env"]["CALIBRATION_OUTPUT"].as_str(),
        Some("${{ runner.temp }}/togi-pr-loop-calibration")
    );
    let collect = step("Collect candidate calibration").1["run"]
        .as_str()
        .expect("collector command");
    let result_arguments: Vec<&str> = collect
        .lines()
        .map(str::trim)
        .filter(|line| line.contains("pr-loop-benchmark-result.json"))
        .map(|line| line.trim_end_matches('\\').trim())
        .collect();
    let expected_arguments: Vec<String> = (1..=5)
        .map(|sample| {
            format!("\"$CALIBRATION_OUTPUT/sample-{sample}/pr-loop-benchmark-result.json\"")
        })
        .collect();
    assert_eq!(
        result_arguments,
        expected_arguments
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
    );
    let upload = step("Upload calibration evidence").1;
    assert_eq!(
        upload["with"]["path"].as_str(),
        Some("${{ runner.temp }}/togi-pr-loop-calibration")
    );
    assert!(workflow.contains("workflow_dispatch:"));
    assert!(workflow.contains("if: github.ref == 'refs/heads/main'"));
    assert!(workflow.contains("retention-days: 14"));
    assert!(!workflow.contains("pull_request:"));
    assert!(!workflow.contains("permissions: write"));
}

fn promoter_path() -> PathBuf {
    repo_root().join("benchmarks/pr-loop/promote-baseline.py")
}

fn archive_artifact(artifact_dir: &Path, archive: &Path) {
    let script = r#"import pathlib, sys, zipfile
root, output = map(pathlib.Path, sys.argv[1:])
with zipfile.ZipFile(output, "w", zipfile.ZIP_DEFLATED) as archive:
    for path in sorted(root.rglob("*")):
        if path.is_file() and not path.is_symlink():
            archive.write(path, path.relative_to(root).as_posix())
"#;
    let output = Command::new("python3")
        .arg("-c")
        .arg(script)
        .arg(artifact_dir)
        .arg(archive)
        .output()
        .expect("create artifact archive");
    assert!(
        output.status.success(),
        "archive failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

const PROMOTER_ACTIVATION: [&str; 6] = [
    "--activation-pr",
    "487",
    "--activation-actor",
    "octocat",
    "--activation-utc",
    "2026-08-04T00:00:00Z",
];

fn run_promoter(
    artifact_dir: &Path,
    output: &Path,
    expected_commit: &str,
    extra: &[&str],
) -> Output {
    run_promoter_with(
        artifact_dir,
        output,
        expected_commit,
        &PROMOTER_ACTIVATION,
        extra,
    )
}

fn run_promoter_with(
    artifact_dir: &Path,
    output: &Path,
    expected_commit: &str,
    activation: &[&str],
    extra: &[&str],
) -> Output {
    let archive = output.with_extension("artifact.zip");
    if !archive.exists() {
        archive_artifact(artifact_dir, &archive);
    }
    let mut command = Command::new("python3");
    command
        .arg(promoter_path())
        .arg("--artifact-dir")
        .arg(artifact_dir)
        .arg("--artifact-archive")
        .arg(&archive)
        .arg("--github-artifact-id")
        .arg("123")
        .arg("--github-artifact-sha256")
        .arg(sha256_file(&archive))
        .arg("--output")
        .arg(output)
        .arg("--expected-source-commit")
        .arg(expected_commit);
    for arg in activation {
        command.arg(arg);
    }
    for arg in extra {
        command.arg(arg);
    }
    command.output().expect("spawn promoter")
}

/// Builds a calibration-shaped artifact directory: five primed samples plus
/// the collector's candidate JSON, mirroring the uploaded CI artifact.
fn build_calibration_artifact(tools: &FakeTools, artifact: &Path, gocache: &Path) -> Vec<PathBuf> {
    let results = run_primed_samples(tools, artifact, gocache);
    let candidate = artifact.join("pr-loop-calibration-candidate.json");
    let collected = run_collector(&candidate, &results);
    assert!(
        collected.status.success(),
        "artifact candidate collection failed\nstderr: {}",
        String::from_utf8_lossy(&collected.stderr)
    );
    results
}

fn read_json(path: &Path) -> serde_json::Value {
    serde_json::from_str(&fs::read_to_string(path).expect("read JSON")).expect("valid JSON")
}

#[test]
fn pr_loop_promote_baseline_happy_path_is_deterministic_and_never_overwrites() {
    if !harness_tools_available() || !tool_on_path("python3") {
        eprintln!("skipping: harness or Python tools unavailable");
        return;
    }
    let tools = install_fake_tools();
    let gocache = tempfile::tempdir().expect("gocache tempdir");
    let workspace = tempfile::tempdir().expect("workspace");
    let artifact = workspace.path().join("artifact");
    fs::create_dir(&artifact).unwrap();
    build_calibration_artifact(&tools, &artifact, gocache.path());

    let baseline = workspace.path().join("baseline.json");
    let promoted = run_promoter(&artifact, &baseline, "test-commit", &[]);
    assert!(
        promoted.status.success(),
        "promotion must succeed\nstderr: {}",
        String::from_utf8_lossy(&promoted.stderr)
    );
    let value = read_json(&baseline);
    assert_eq!(value["kind"], "togi_pr_loop_baseline");
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["status"], "durable");
    assert_eq!(
        value["tolerance_policy"],
        serde_json::json!({
            "policy_version": 1,
            "repetitions": 3,
            "aggregation": "median",
            "wall_ms": {"relative_numerator": 3, "relative_denominator": 2, "absolute_floor_ms": 250},
            "reported_duration_ms": {"relative_numerator": 3, "relative_denominator": 2, "absolute_floor_ms": 100}
        }),
        "the promoter must pin the fixed tolerance policy, never hand-authored thresholds"
    );
    assert!(
        value["comparison_policy"]
            .as_str()
            .expect("comparison policy")
            .contains("median of three"),
        "the durable baseline must describe the fixed comparison policy"
    );
    assert_eq!(value["activation"]["pr"], 487);
    assert_eq!(value["activation"]["actor"], "octocat");
    assert_eq!(value["activation"]["utc"], "2026-08-04T00:00:00Z");
    assert_eq!(value["source"]["commit"], "test-commit");
    assert_eq!(
        value["measurement_identity"],
        serde_json::json!({
            "go_build_cache_state": "primed",
            "go_build_cache_policy": "job-private-explicit-gocache"
        }),
        "measurement identity must not carry the volatile cache path"
    );
    assert_eq!(
        value["calibration_evidence"]["go_build_cache_path"],
        serde_json::Value::String(gocache.path().to_str().unwrap().to_string()),
        "the volatile cache path survives only as calibration evidence"
    );
    assert_eq!(value["semantic_identity"]["manifest"]["schema_version"], 2);
    for name in WORKLOAD_NAMES {
        assert_eq!(
            value["samples"][name]["wall_ms"].as_array().unwrap().len(),
            5,
            "baseline must carry five wall samples for {name}"
        );
        assert_eq!(
            value["samples"][name]["reported_duration_ms"]
                .as_array()
                .unwrap()
                .len(),
            5,
            "baseline must carry five reported-duration samples for {name}"
        );
        assert_eq!(
            value["samples"][name]["reported_duration_ms_median"], 42,
            "the fake report pins duration_ms to 42 for {name}"
        );
    }

    // Deterministic: identical inputs and metadata produce identical bytes.
    let second = workspace.path().join("baseline-second.json");
    assert!(
        run_promoter(&artifact, &second, "test-commit", &[])
            .status
            .success()
    );
    assert_eq!(
        fs::read(&baseline).unwrap(),
        fs::read(&second).unwrap(),
        "promotion must be deterministic"
    );

    // No overwrite by default; explicit --overwrite opts in.
    let third = run_promoter(&artifact, &baseline, "test-commit", &[]);
    assert_eq!(third.status.code(), Some(2));
    let overwritten = run_promoter(&artifact, &baseline, "test-commit", &["--overwrite"]);
    assert!(overwritten.status.success());
    let durable = read_json(&baseline);
    assert_eq!(durable["status"], "durable");
    assert_eq!(durable["activation"]["pr"], 487);
}

#[test]
fn pr_loop_promote_baseline_rejects_wall_outlier() {
    if !harness_tools_available() || !tool_on_path("python3") {
        eprintln!("skipping: harness or Python tools unavailable");
        return;
    }
    let tools = install_fake_tools();
    let gocache = tempfile::tempdir().expect("gocache tempdir");
    let workspace = tempfile::tempdir().expect("workspace");
    let artifact = workspace.path().join("artifact");
    fs::create_dir(&artifact).unwrap();
    let results = run_primed_samples(&tools, &artifact, gocache.path());

    // Poison the last sample's cold-regular wall time to 12x the median of
    // the others: the collector accepts it, the promoter must not.
    let mut walls: Vec<u64> = results[..4]
        .iter()
        .map(|path| {
            read_json(path)["workloads"][0]["timing"]["wall_ms"]
                .as_u64()
                .unwrap()
        })
        .collect();
    walls.sort_unstable();
    let median = walls[walls.len() / 2].max(1);
    let poisoned = artifact.join("sample-5/pr-loop-benchmark-result.json");
    let mut result = read_json(&poisoned);
    result["workloads"][0]["timing"]["wall_ms"] = serde_json::json!(12 * median);
    fs::write(&poisoned, serde_json::to_vec_pretty(&result).unwrap()).unwrap();

    let candidate = artifact.join("pr-loop-calibration-candidate.json");
    let collected = run_collector(&candidate, &results);
    assert!(
        collected.status.success(),
        "collector has no outlier policy and must still collect\nstderr: {}",
        String::from_utf8_lossy(&collected.stderr)
    );
    let promoted = run_promoter(
        &artifact,
        &workspace.path().join("baseline.json"),
        "test-commit",
        &[],
    );
    assert_eq!(promoted.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&promoted.stderr).contains("median"),
        "expected an outlier rejection, got: {}",
        String::from_utf8_lossy(&promoted.stderr)
    );
}

#[test]
fn pr_loop_promote_baseline_fails_closed_on_artifact_tampering() {
    if !harness_tools_available() || !tool_on_path("python3") {
        eprintln!("skipping: harness or Python tools unavailable");
        return;
    }
    let tools = install_fake_tools();
    let gocache = tempfile::tempdir().expect("gocache tempdir");
    let workspace = tempfile::tempdir().expect("workspace");
    let artifact = workspace.path().join("artifact");
    fs::create_dir(&artifact).unwrap();
    build_calibration_artifact(&tools, &artifact, gocache.path());
    let candidate = artifact.join("pr-loop-calibration-candidate.json");
    let sample5 = artifact.join("sample-5/pr-loop-benchmark-result.json");

    // Wrong expected source commit.
    let rejected = run_promoter(
        &artifact,
        &workspace.path().join("wrong-commit.json"),
        "different-commit",
        &[],
    );
    assert_eq!(rejected.status.code(), Some(2));

    // Artifact directory without the candidate.
    let no_candidate = workspace.path().join("no-candidate");
    fs::create_dir(&no_candidate).unwrap();
    let rejected = run_promoter(
        &no_candidate,
        &workspace.path().join("no-candidate-out.json"),
        "test-commit",
        &[],
    );
    assert_eq!(rejected.status.code(), Some(2));

    // Symlink anywhere in the artifact tree is rejected.
    let symlinked = workspace.path().join("symlinked");
    copy_dir(&artifact, &symlinked);
    std::os::unix::fs::symlink("/etc/hostname", symlinked.join("evil-link")).unwrap();
    let rejected = run_promoter(
        &symlinked,
        &workspace.path().join("symlinked-out.json"),
        "test-commit",
        &[],
    );
    assert_eq!(rejected.status.code(), Some(2));

    // Missing sample file named by the candidate.
    let missing_sample = workspace.path().join("missing-sample");
    copy_dir(&artifact, &missing_sample);
    fs::remove_file(missing_sample.join("sample-5/pr-loop-benchmark-result.json")).unwrap();
    let rejected = run_promoter(
        &missing_sample,
        &workspace.path().join("missing-sample-out.json"),
        "test-commit",
        &[],
    );
    assert_eq!(rejected.status.code(), Some(2));

    // Sample content drift after collection breaks the recorded digest.
    let drifted = workspace.path().join("drifted");
    copy_dir(&artifact, &drifted);
    let drifted_sample = drifted.join("sample-5/pr-loop-benchmark-result.json");
    let mut result = read_json(&drifted_sample);
    result["workloads"][0]["timing"]["wall_ms"] = serde_json::json!(1);
    fs::write(&drifted_sample, serde_json::to_vec_pretty(&result).unwrap()).unwrap();

    // Candidate identity is derived from all five raw reports, never trusted.
    for (pointer, replacement, name) in [
        (
            "/semantic_identity/workloads/0/runner_mode",
            serde_json::json!("forged"),
            "runner-mode",
        ),
        (
            "/semantic_identity/workloads/0/semantics/selected_test_command/2",
            serde_json::json!("./only"),
            "test-command",
        ),
        (
            "/semantic_identity/scenario_mutation_identity/multi-file",
            serde_json::json!("0".repeat(64)),
            "scenario-identity",
        ),
        (
            "/runner_class/runner_label",
            serde_json::json!("forged-runner"),
            "runner-class",
        ),
        (
            "/execution_provenance/go_version",
            serde_json::json!("forged-go"),
            "execution-provenance",
        ),
    ] {
        let forged = workspace.path().join(format!("forged-{name}"));
        copy_dir(&artifact, &forged);
        let candidate_path = forged.join("pr-loop-calibration-candidate.json");
        let mut forged_candidate = read_json(&candidate_path);
        *forged_candidate
            .pointer_mut(pointer)
            .expect("candidate field") = replacement;
        fs::write(
            &candidate_path,
            serde_json::to_vec_pretty(&forged_candidate).unwrap(),
        )
        .unwrap();
        let output = workspace.path().join(format!("forged-{name}.json"));
        let rejected = run_promoter(&forged, &output, "test-commit", &[]);
        assert_eq!(rejected.status.code(), Some(2), "{name} must fail closed");
        assert!(!output.exists(), "{name} must not write a baseline");
    }
    let rejected = run_promoter(
        &drifted,
        &workspace.path().join("drifted-out.json"),
        "test-commit",
        &[],
    );
    assert_eq!(rejected.status.code(), Some(2));

    // Candidate sample data disagreeing with the artifact results.
    let tampered_samples = workspace.path().join("tampered-samples");
    copy_dir(&artifact, &tampered_samples);
    let tampered_candidate = tampered_samples.join("pr-loop-calibration-candidate.json");
    let mut value = read_json(&tampered_candidate);
    value["samples"]["cold-regular"]["wall_ms"][0] = serde_json::json!(1);
    fs::write(
        &tampered_candidate,
        serde_json::to_vec_pretty(&value).unwrap(),
    )
    .unwrap();

    // GitHub archive custody is mandatory: positive id, normalized digest,
    // and an exact regular-file archive/extraction match.
    let rejected = run_promoter(
        &artifact,
        &workspace.path().join("bad-artifact-id.json"),
        "test-commit",
        &["--github-artifact-id", "0"],
    );
    assert_eq!(rejected.status.code(), Some(2));
    let rejected = run_promoter(
        &artifact,
        &workspace.path().join("bad-artifact-digest.json"),
        "test-commit",
        &["--github-artifact-sha256", &"0".repeat(64)],
    );
    assert_eq!(rejected.status.code(), Some(2));
    let archive_output = workspace.path().join("archive-extra.json");
    let archive = archive_output.with_extension("artifact.zip");
    archive_artifact(&artifact, &archive);
    let added = Command::new("python3")
        .args([
            "-c",
            "import sys, zipfile; zipfile.ZipFile(sys.argv[1], 'a').writestr('extra', b'x')",
        ])
        .arg(&archive)
        .output()
        .expect("append archive member");
    assert!(added.status.success());
    let rejected = run_promoter(&artifact, &archive_output, "test-commit", &[]);
    assert_eq!(rejected.status.code(), Some(2));
    let rejected = run_promoter(
        &tampered_samples,
        &workspace.path().join("tampered-samples-out.json"),
        "test-commit",
        &[],
    );
    assert_eq!(rejected.status.code(), Some(2));

    // Candidate pinned to a different manifest/fixture digest generation.
    let stale = workspace.path().join("stale");
    copy_dir(&artifact, &stale);
    let stale_candidate = stale.join("pr-loop-calibration-candidate.json");
    let mut value = read_json(&stale_candidate);
    value["source_file_digests"]["benchmarks/pr-loop/manifest.json"] =
        serde_json::json!("0".repeat(64));
    fs::write(&stale_candidate, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    let rejected = run_promoter(
        &stale,
        &workspace.path().join("stale-out.json"),
        "test-commit",
        &[],
    );
    assert_eq!(rejected.status.code(), Some(2));

    // A non-primed input, even with a re-digested candidate entry.
    let unprimed = workspace.path().join("unprimed");
    copy_dir(&artifact, &unprimed);
    let unprimed_sample = unprimed.join("sample-5/pr-loop-benchmark-result.json");
    let mut result = read_json(&unprimed_sample);
    result["provenance"]["go_build_cache_state"] = serde_json::json!("unclassified");
    fs::write(
        &unprimed_sample,
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .unwrap();
    let unprimed_candidate = unprimed.join("pr-loop-calibration-candidate.json");
    let mut value = read_json(&unprimed_candidate);
    value["source_file_digests"]["sample-5/pr-loop-benchmark-result.json"] =
        serde_json::json!(sha256_file(&unprimed_sample));
    fs::write(
        &unprimed_candidate,
        serde_json::to_vec_pretty(&value).unwrap(),
    )
    .unwrap();
    let rejected = run_promoter(
        &unprimed,
        &workspace.path().join("unprimed-out.json"),
        "test-commit",
        &[],
    );
    assert_eq!(rejected.status.code(), Some(2));

    // The pristine artifact still promotes after all tampering variants.
    assert!(candidate.exists() && sample5.exists());
    let promoted = run_promoter(
        &artifact,
        &workspace.path().join("pristine.json"),
        "test-commit",
        &[],
    );
    assert!(
        promoted.status.success(),
        "pristine artifact must still promote\nstderr: {}",
        String::from_utf8_lossy(&promoted.stderr)
    );
}

/// Recursively copies a directory tree of regular files (test artifacts only).
fn copy_dir(source: &Path, target: &Path) {
    fs::create_dir_all(target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let destination = target.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &destination);
        } else {
            fs::copy(&path, &destination).unwrap();
        }
    }
}

#[test]
fn pr_loop_promote_baseline_requires_durable_activation_metadata() {
    if !harness_tools_available() || !tool_on_path("python3") {
        eprintln!("skipping: harness or Python tools unavailable");
        return;
    }
    let tools = install_fake_tools();
    let gocache = tempfile::tempdir().expect("gocache tempdir");
    let workspace = tempfile::tempdir().expect("workspace");
    let artifact = workspace.path().join("artifact");
    fs::create_dir(&artifact).unwrap();
    build_calibration_artifact(&tools, &artifact, gocache.path());

    // No activation metadata at all: a durable baseline is never written.
    let missing = workspace.path().join("missing-activation.json");
    let rejected = run_promoter_with(&artifact, &missing, "test-commit", &[], &[]);
    assert_eq!(rejected.status.code(), Some(2));
    assert!(!missing.exists());

    // Invalid activation metadata (extra args override the defaults).
    for (name, override_args) in [
        ("zero-pr", vec!["--activation-pr", "0"]),
        ("blank-actor", vec!["--activation-actor", "   "]),
        ("not-utc", vec!["--activation-utc", "yesterday"]),
        (
            "offset-utc",
            vec!["--activation-utc", "2026-08-04T00:00:00+00:00"],
        ),
    ] {
        let output = workspace.path().join(format!("{name}.json"));
        let rejected = run_promoter_with(
            &artifact,
            &output,
            "test-commit",
            &PROMOTER_ACTIVATION,
            &override_args,
        );
        assert_eq!(
            rejected.status.code(),
            Some(2),
            "{name} must fail closed\nstderr: {}",
            String::from_utf8_lossy(&rejected.stderr)
        );
        assert!(!output.exists(), "{name} must not write a baseline");
    }

    // The fixed tolerance policy is not a CLI surface: there is no way to
    // pass hand-authored thresholds through promotion.
    let help = Command::new("python3")
        .arg(promoter_path())
        .arg("--help")
        .output()
        .expect("promoter help");
    let help_text = String::from_utf8_lossy(&help.stdout);
    for flag in ["--tolerance", "--threshold", "--floor", "--policy"] {
        assert!(
            !help_text.contains(flag),
            "promoter must not accept hand-authored policy flag {flag}"
        );
    }
}

#[test]
fn pr_loop_promote_baseline_rejects_reported_duration_outlier() {
    if !harness_tools_available() || !tool_on_path("python3") {
        eprintln!("skipping: harness or Python tools unavailable");
        return;
    }
    let tools = install_fake_tools();
    let gocache = tempfile::tempdir().expect("gocache tempdir");
    let workspace = tempfile::tempdir().expect("workspace");
    let artifact = workspace.path().join("artifact");
    fs::create_dir(&artifact).unwrap();
    let results = run_primed_samples(&tools, &artifact, gocache.path());

    // Poison the last sample's cold-regular reported duration to 12x the
    // median of the others: the collector accepts it, the promoter must not.
    let poisoned = artifact.join("sample-5/pr-loop-benchmark-result.json");
    let mut result = read_json(&poisoned);
    let median = result["workloads"][0]["timing"]["reported_duration_ms"]
        .as_u64()
        .unwrap()
        .max(1);
    result["workloads"][0]["timing"]["reported_duration_ms"] = serde_json::json!(12 * median);
    fs::write(&poisoned, serde_json::to_vec_pretty(&result).unwrap()).unwrap();

    let candidate = artifact.join("pr-loop-calibration-candidate.json");
    let collected = run_collector(&candidate, &results);
    assert!(
        collected.status.success(),
        "collector has no outlier policy and must still collect\nstderr: {}",
        String::from_utf8_lossy(&collected.stderr)
    );
    let promoted = run_promoter(
        &artifact,
        &workspace.path().join("baseline.json"),
        "test-commit",
        &[],
    );
    assert_eq!(promoted.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&promoted.stderr).contains("median"),
        "expected an outlier rejection, got: {}",
        String::from_utf8_lossy(&promoted.stderr)
    );
}

fn comparator_path() -> PathBuf {
    repo_root().join("benchmarks/pr-loop/compare-baseline.py")
}

fn run_comparator(baseline: &Path, results: &[PathBuf], output: Option<&Path>) -> Output {
    let mut command = Command::new("python3");
    command
        .arg(comparator_path())
        .arg("--baseline")
        .arg(baseline);
    if let Some(output) = output {
        command.arg("--output").arg(output);
    }
    for result in results {
        command.arg(result);
    }
    command.output().expect("spawn comparator")
}

/// Runs `count` primed samples bound to one explicit GOCACHE, mirroring the
/// regression gate's measured acquisition.
fn run_gate_samples(
    tools: &FakeTools,
    samples_dir: &Path,
    gocache: &Path,
    count: usize,
) -> Vec<PathBuf> {
    let cache_path = gocache.to_str().expect("gocache path");
    let mut results = Vec::new();
    for sample in 1..=count {
        let output = samples_dir.join(format!("sample-{sample}"));
        let run = run_harness(
            tools,
            &output,
            None,
            &[
                ("BENCH_GO_BUILD_CACHE_STATE", "primed"),
                ("GOCACHE", cache_path),
            ],
        );
        assert!(
            run.status.success(),
            "sample {sample} harness run failed\nstderr: {}",
            String::from_utf8_lossy(&run.stderr)
        );
        results.push(output.join("pr-loop-benchmark-result.json"));
    }
    results
}

struct GateFixture {
    _workspace: tempfile::TempDir,
    baseline: PathBuf,
    samples_dir: PathBuf,
    results: Vec<PathBuf>,
}

/// Builds the durable baseline end to end (harness -> collector -> promoter)
/// plus three fresh primed comparison samples on the same runner class.
fn build_gate_fixture() -> (FakeTools, GateFixture) {
    let tools = install_fake_tools();
    let gocache = tempfile::tempdir().expect("gocache tempdir");
    let workspace = tempfile::tempdir().expect("workspace");
    let artifact = workspace.path().join("artifact");
    fs::create_dir(&artifact).unwrap();
    build_calibration_artifact(&tools, &artifact, gocache.path());
    let baseline = workspace.path().join("baseline.json");
    let promoted = run_promoter(&artifact, &baseline, "test-commit", &[]);
    assert!(
        promoted.status.success(),
        "gate fixture promotion failed\nstderr: {}",
        String::from_utf8_lossy(&promoted.stderr)
    );
    let samples_dir = workspace.path().join("gate-samples");
    fs::create_dir(&samples_dir).unwrap();
    let results = run_gate_samples(&tools, &samples_dir, gocache.path(), 3);
    (
        tools,
        GateFixture {
            _workspace: workspace,
            baseline,
            samples_dir,
            results,
        },
    )
}

/// Overwrites one workload/metric timing value in each of the three result
/// files; values map to samples in order and the middle value after sorting
/// is the median M seen by the comparator.
fn set_result_metric(results: &[PathBuf], workload: &str, metric: &str, values: [u64; 3]) {
    assert_eq!(results.len(), 3);
    for (path, value) in results.iter().zip(values) {
        let mut result = read_json(path);
        let mut found = false;
        for item in result["workloads"].as_array_mut().unwrap() {
            if item["name"] == workload {
                item["timing"][metric] = serde_json::json!(value);
                found = true;
            }
        }
        assert!(found, "workload {workload} missing from {path:?}");
        fs::write(path, serde_json::to_vec_pretty(&result).unwrap()).unwrap();
    }
}

/// Rewrites one baseline workload/metric with five raw values and the
/// matching stored median (B) so the comparator's internal check holds.
fn set_baseline_metric(baseline: &Path, workload: &str, metric: &str, values: [u64; 5]) {
    let mut sorted = values;
    sorted.sort_unstable();
    let median = sorted[2];
    let mut doc = read_json(baseline);
    doc["samples"][workload][metric] = serde_json::json!(values);
    doc["samples"][workload][format!("{metric}_median")] = serde_json::json!(median);
    fs::write(baseline, serde_json::to_vec_pretty(&doc).unwrap()).unwrap();
}

#[test]
fn pr_loop_comparator_passes_fresh_samples_and_writes_summary() {
    if !harness_tools_available() || !tool_on_path("python3") {
        eprintln!("skipping: harness or Python tools unavailable");
        return;
    }
    let (_tools, fixture) = build_gate_fixture();
    let summary_path = fixture.samples_dir.join("comparison.json");
    let output = run_comparator(&fixture.baseline, &fixture.results, Some(&summary_path));
    assert_eq!(
        output.status.code(),
        Some(0),
        "fresh comparable samples must pass\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("comparison result: PASS"));
    let summary = read_json(&summary_path);
    assert_eq!(summary["kind"], "togi_pr_loop_comparison");
    assert_eq!(summary["result"], "pass");
    assert_eq!(summary["regressions"], serde_json::json!([]));
    for name in WORKLOAD_NAMES {
        for metric in ["wall_ms", "reported_duration_ms"] {
            assert_eq!(
                summary["workloads"][name][metric]["exceeds_tolerance"], false,
                "{name}/{metric} must be inside tolerance"
            );
        }
    }
    // The machine-readable summary is the stdout JSON document.
    let written = fs::read_to_string(&summary_path).unwrap();
    assert!(stdout.contains(written.trim_end()));

    // The comparator refuses to clobber an existing summary.
    let rerun = run_comparator(&fixture.baseline, &fixture.results, Some(&summary_path));
    assert_eq!(rerun.status.code(), Some(2));
}

#[test]
fn pr_loop_comparator_exact_boundary_passes_and_plus_one_fails() {
    if !harness_tools_available() || !tool_on_path("python3") {
        eprintln!("skipping: harness or Python tools unavailable");
        return;
    }
    let (_tools, fixture) = build_gate_fixture();
    // Wall: cap = (3*B + 2*250)/2 with B=1000 -> 1750ms exactly.
    // 2*M > 3*B + 2*floor is strict: M=1750 passes, M=1751 exceeds.
    set_baseline_metric(&fixture.baseline, "cold-regular", "wall_ms", [1000; 5]);
    set_baseline_metric(&fixture.baseline, "pr-diff-default", "wall_ms", [1000; 5]);

    let boundary_dir = fixture.samples_dir.join("boundary");
    fs::create_dir(&boundary_dir).unwrap();
    let boundary_results: Vec<PathBuf> = fixture
        .results
        .iter()
        .enumerate()
        .map(|(index, result)| {
            let copied = boundary_dir.join(format!("result-{index}.json"));
            fs::copy(result, &copied).unwrap();
            copied
        })
        .collect();
    set_result_metric(&boundary_results, "cold-regular", "wall_ms", [1750; 3]);
    set_result_metric(&boundary_results, "pr-diff-default", "wall_ms", [1750; 3]);
    let boundary = run_comparator(&fixture.baseline, &boundary_results, None);
    assert_eq!(
        boundary.status.code(),
        Some(0),
        "median exactly at the tolerance cap must pass\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&boundary.stdout),
        String::from_utf8_lossy(&boundary.stderr)
    );

    let plus_one_dir = fixture.samples_dir.join("plus-one");
    fs::create_dir(&plus_one_dir).unwrap();
    let plus_one_results: Vec<PathBuf> = fixture
        .results
        .iter()
        .enumerate()
        .map(|(index, result)| {
            let copied = plus_one_dir.join(format!("result-{index}.json"));
            fs::copy(result, &copied).unwrap();
            copied
        })
        .collect();
    set_result_metric(&plus_one_results, "cold-regular", "wall_ms", [1751; 3]);
    set_result_metric(&plus_one_results, "pr-diff-default", "wall_ms", [1751; 3]);
    let plus_one = run_comparator(&fixture.baseline, &plus_one_results, None);
    assert_eq!(
        plus_one.status.code(),
        Some(1),
        "one millisecond over the cap on two workload/metrics must fail\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&plus_one.stdout),
        String::from_utf8_lossy(&plus_one.stderr)
    );
    assert!(String::from_utf8_lossy(&plus_one.stdout).contains("comparison result: FAIL"));
}

#[test]
fn pr_loop_comparator_sample_spikes_warn_and_any_median_over_fails() {
    if !harness_tools_available() || !tool_on_path("python3") {
        eprintln!("skipping: harness or Python tools unavailable");
        return;
    }
    let (_tools, fixture) = build_gate_fixture();
    // Wall: cap = (3*B + 2*250)/2 with B=1000 -> 1750ms. Reported duration:
    // cap = (3*B + 2*100)/2 with B=100 -> 250ms.
    set_baseline_metric(&fixture.baseline, "cold-regular", "wall_ms", [1000; 5]);
    set_baseline_metric(
        &fixture.baseline,
        "cold-regular",
        "reported_duration_ms",
        [100; 5],
    );

    let copy_results = |name: &str| {
        let dir = fixture.samples_dir.join(name);
        fs::create_dir(&dir).unwrap();
        fixture
            .results
            .iter()
            .enumerate()
            .map(|(index, result)| {
                let copied = dir.join(format!("result-{index}.json"));
                fs::copy(result, &copied).unwrap();
                copied
            })
            .collect::<Vec<_>>()
    };

    // (a) One high raw wall sample: the median stays under the cap, so the
    // spike is recorded and warned observationally but passes.
    let spike = copy_results("wall-single-spike");
    set_result_metric(&spike, "cold-regular", "wall_ms", [1751, 1000, 1000]);
    let output = run_comparator(&fixture.baseline, &spike, None);
    assert_eq!(
        output.status.code(),
        Some(0),
        "one raw wall spike must pass observationally\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("sample-level exceedance (observational): cold-regular wall_ms"),
        "expected a wall sample-exceedance warning: {stdout}"
    );
    assert!(stdout.contains("comparison result: PASS"));

    // (b) Two high raw wall samples push the median over the cap: any median
    // over the cap is a hard regression.
    let spikes = copy_results("wall-two-spikes");
    set_result_metric(&spikes, "cold-regular", "wall_ms", [1751, 1751, 1000]);
    let output = run_comparator(&fixture.baseline, &spikes, None);
    assert_eq!(
        output.status.code(),
        Some(1),
        "two raw wall spikes move the median over the cap and must fail\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // (c) One high raw reported-duration sample: observational pass.
    let spike = copy_results("reported-single-spike");
    set_result_metric(
        &spike,
        "cold-regular",
        "reported_duration_ms",
        [300, 100, 100],
    );
    let output = run_comparator(&fixture.baseline, &spike, None);
    assert_eq!(
        output.status.code(),
        Some(0),
        "one raw reported-duration spike must pass observationally\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout
            .contains("sample-level exceedance (observational): cold-regular reported_duration_ms"),
        "expected a reported-duration sample-exceedance warning: {stdout}"
    );

    // (d) Two high raw reported-duration samples: the median crosses, exit 1.
    let spikes = copy_results("reported-two-spikes");
    set_result_metric(
        &spikes,
        "cold-regular",
        "reported_duration_ms",
        [300, 300, 100],
    );
    let output = run_comparator(&fixture.baseline, &spikes, None);
    assert_eq!(
        output.status.code(),
        Some(1),
        "two raw reported-duration spikes move the median over the cap and must fail\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // (e) A single median-over metric fails even though the other metric is
    // normal: "one spike passes" never makes a median regression advisory.
    let median_over = copy_results("wall-median-over");
    set_result_metric(&median_over, "cold-regular", "wall_ms", [1751; 3]);
    let output = run_comparator(&fixture.baseline, &median_over, None);
    assert_eq!(
        output.status.code(),
        Some(1),
        "a single median-over metric must fail\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("cold-regular reported_duration_ms: median 42ms"));
    assert!(stdout.contains("comparison result: FAIL"));
}

#[test]
fn pr_loop_comparator_rejects_wrong_count_missing_and_malformed_inputs() {
    if !harness_tools_available() || !tool_on_path("python3") {
        eprintln!("skipping: harness or Python tools unavailable");
        return;
    }
    let (_tools, fixture) = build_gate_fixture();
    for (label, count) in [("two", 2), ("four", 4)] {
        let mut results = fixture.results.clone();
        while results.len() < count {
            results.push(fixture.results[0].clone());
        }
        results.truncate(count);
        let rejected = run_comparator(&fixture.baseline, &results, None);
        assert_eq!(
            rejected.status.code(),
            Some(2),
            "{label} results must fail closed\nstderr: {}",
            String::from_utf8_lossy(&rejected.stderr)
        );
    }
    // Duplicate paths are not three independent measurements.
    let duplicates = vec![fixture.results[0].clone(); 3];
    let rejected = run_comparator(&fixture.baseline, &duplicates, None);
    assert_eq!(rejected.status.code(), Some(2));

    // Missing baseline file.
    let missing = fixture.samples_dir.join("no-such-baseline.json");
    let rejected = run_comparator(&missing, &fixture.results, None);
    assert_eq!(rejected.status.code(), Some(2));

    // Malformed baseline JSON.
    let malformed = fixture.samples_dir.join("malformed-baseline.json");
    fs::write(&malformed, "{").unwrap();
    let rejected = run_comparator(&malformed, &fixture.results, None);
    assert_eq!(rejected.status.code(), Some(2));

    // Malformed result JSON.
    let malformed_result = fixture.samples_dir.join("malformed-result.json");
    fs::write(&malformed_result, "{").unwrap();
    let mut results = fixture.results.clone();
    results[2] = malformed_result;
    let rejected = run_comparator(&fixture.baseline, &results, None);
    assert_eq!(rejected.status.code(), Some(2));

    // A directory in place of the baseline document is missing input, not a
    // baseline.
    let directory = fixture.samples_dir.join("baseline-dir");
    fs::create_dir(&directory).unwrap();
    let rejected = run_comparator(&directory, &fixture.results, None);
    assert_eq!(rejected.status.code(), Some(2));
}

#[test]
fn pr_loop_comparator_rejects_stale_or_hand_authored_baselines() {
    if !harness_tools_available() || !tool_on_path("python3") {
        eprintln!("skipping: harness or Python tools unavailable");
        return;
    }
    let (_tools, fixture) = build_gate_fixture();
    let rewrite_baseline = |name: &str, mutate: &dyn Fn(&mut serde_json::Value)| {
        let mut doc = read_json(&fixture.baseline);
        mutate(&mut doc);
        let path = fixture.samples_dir.join(format!("{name}.json"));
        fs::write(&path, serde_json::to_vec_pretty(&doc).unwrap()).unwrap();
        path
    };

    // A non-durable (pending-activation) document is not a baseline.
    let pending = rewrite_baseline("pending", &|doc| {
        doc["status"] = serde_json::json!("pending-activation");
    });
    assert_eq!(
        run_comparator(&pending, &fixture.results, None)
            .status
            .code(),
        Some(2)
    );

    // Hand-authored tolerance thresholds never replace the fixed policy.
    let custom_policy = rewrite_baseline("custom-policy", &|doc| {
        doc["tolerance_policy"]["wall_ms"]["absolute_floor_ms"] = serde_json::json!(10_000);
    });
    assert_eq!(
        run_comparator(&custom_policy, &fixture.results, None)
            .status
            .code(),
        Some(2)
    );

    // A stored median disagreeing with the five raw values is corrupt.
    let bad_median = rewrite_baseline("bad-median", &|doc| {
        doc["samples"]["cold-regular"]["wall_ms_median"] = serde_json::json!(1);
    });
    let rejected = run_comparator(&bad_median, &fixture.results, None);
    assert_eq!(rejected.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("median"),
        "expected a median verification error: {}",
        String::from_utf8_lossy(&rejected.stderr)
    );

    // Dropped raw reported-duration samples are malformed.
    let dropped = rewrite_baseline("dropped-duration", &|doc| {
        doc["samples"]["cold-regular"]
            .as_object_mut()
            .unwrap()
            .remove("reported_duration_ms");
    });
    assert_eq!(
        run_comparator(&dropped, &fixture.results, None)
            .status
            .code(),
        Some(2)
    );

    // A baseline pinned to a different corpus generation is stale.
    let stale_digests = rewrite_baseline("stale-digests", &|doc| {
        doc["source_file_digests"]["benchmarks/pr-loop/manifest.json"] =
            serde_json::json!("0".repeat(64));
    });
    let rejected = run_comparator(&stale_digests, &fixture.results, None);
    assert_eq!(rejected.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("digest"),
        "expected a digest mismatch: {}",
        String::from_utf8_lossy(&rejected.stderr)
    );

    // The digest key set must be complete and exact: deleting any required
    // key (manifest, scenario patch, fixture tree, or one of the five sample
    // results) or adding any extra key is incomparable before values matter.
    for (name, key) in [
        ("deleted-manifest-key", "benchmarks/pr-loop/manifest.json"),
        (
            "deleted-patch-key",
            "benchmarks/pr-loop/fixture-change-multi.patch",
        ),
        ("deleted-fixture-key", "tests/fixtures/go/"),
        (
            "deleted-sample-key",
            "sample-5/pr-loop-benchmark-result.json",
        ),
    ] {
        let deleted = rewrite_baseline(name, &|doc| {
            doc["source_file_digests"]
                .as_object_mut()
                .unwrap()
                .remove(key);
        });
        let rejected = run_comparator(&deleted, &fixture.results, None);
        assert_eq!(
            rejected.status.code(),
            Some(2),
            "{name} must fail closed\nstderr: {}",
            String::from_utf8_lossy(&rejected.stderr)
        );
    }
    let extra = rewrite_baseline("extra-digest-key", &|doc| {
        doc["source_file_digests"]["benchmarks/pr-loop/extra.json"] =
            serde_json::json!("0".repeat(64));
    });
    let rejected = run_comparator(&extra, &fixture.results, None);
    assert_eq!(
        rejected.status.code(),
        Some(2),
        "an extra digest key must fail closed\nstderr: {}",
        String::from_utf8_lossy(&rejected.stderr)
    );
}

#[test]
fn pr_loop_comparator_rejects_non_integer_or_negative_median_data() {
    if !harness_tools_available() || !tool_on_path("python3") {
        eprintln!("skipping: harness or Python tools unavailable");
        return;
    }
    let (_tools, fixture) = build_gate_fixture();
    let rewrite_baseline = |name: &str, mutate: &dyn Fn(&mut serde_json::Value)| {
        let mut doc = read_json(&fixture.baseline);
        mutate(&mut doc);
        let path = fixture.samples_dir.join(format!("{name}.json"));
        fs::write(&path, serde_json::to_vec_pretty(&doc).unwrap()).unwrap();
        path
    };

    // A stored median that is a float, bool, or negative is corrupt even
    // when it would compare equal to the integer median numerically.
    let true_median = read_json(&fixture.baseline)["samples"]["cold-regular"]["wall_ms_median"]
        .as_u64()
        .expect("integer median");
    for (name, replacement) in [
        ("float-median", serde_json::json!(true_median as f64)),
        ("bool-median", serde_json::json!(true)),
        ("negative-median", serde_json::json!(-1)),
    ] {
        let corrupt = rewrite_baseline(name, &|doc| {
            doc["samples"]["cold-regular"]["wall_ms_median"] = replacement.clone();
        });
        let rejected = run_comparator(&corrupt, &fixture.results, None);
        assert_eq!(
            rejected.status.code(),
            Some(2),
            "{name} must fail closed\nstderr: {}",
            String::from_utf8_lossy(&rejected.stderr)
        );
        assert!(
            String::from_utf8_lossy(&rejected.stderr).contains("median"),
            "{name} must report the median verification: {}",
            String::from_utf8_lossy(&rejected.stderr)
        );
    }

    // Float, bool, and negative raw sample values are corrupt too.
    for (name, replacement) in [
        ("float-raw", serde_json::json!(true_median as f64 + 0.5)),
        ("bool-raw", serde_json::json!(false)),
        ("negative-raw", serde_json::json!(-5)),
    ] {
        let corrupt = rewrite_baseline(name, &|doc| {
            doc["samples"]["cold-regular"]["wall_ms"][0] = replacement.clone();
        });
        let rejected = run_comparator(&corrupt, &fixture.results, None);
        assert_eq!(
            rejected.status.code(),
            Some(2),
            "{name} must fail closed\nstderr: {}",
            String::from_utf8_lossy(&rejected.stderr)
        );
    }

    // A stored float median equal to the true integer median still fails:
    // JSON 1000.0 is not the integer 1000 even though Python `==` would say so.
    let float_equal = rewrite_baseline("float-equal-median", &|doc| {
        doc["samples"]["cold-regular"]["wall_ms"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .for_each(|value| *value = serde_json::json!(true_median));
        doc["samples"]["cold-regular"]["wall_ms_median"] = serde_json::json!(true_median as f64);
    });
    let rejected = run_comparator(&float_equal, &fixture.results, None);
    assert_eq!(
        rejected.status.code(),
        Some(2),
        "a float stored median must fail even when numerically equal\nstderr: {}",
        String::from_utf8_lossy(&rejected.stderr)
    );
}

#[test]
fn pr_loop_comparator_rejects_invalid_baseline_provenance() {
    if !harness_tools_available() || !tool_on_path("python3") {
        eprintln!("skipping: harness or Python tools unavailable");
        return;
    }
    let (_tools, fixture) = build_gate_fixture();
    let rewrite_baseline = |name: &str, mutate: &dyn Fn(&mut serde_json::Value)| {
        let mut doc = read_json(&fixture.baseline);
        mutate(&mut doc);
        let path = fixture.samples_dir.join(format!("{name}.json"));
        fs::write(&path, serde_json::to_vec_pretty(&doc).unwrap()).unwrap();
        path
    };

    // Activation UTC must be strict RFC 3339 Z, mirroring the promoter.
    for (name, utc) in [
        ("activation-utc-garbage", "yesterday"),
        ("activation-utc-offset", "2026-08-04T00:00:00+00:00"),
        ("activation-utc-date-only", "2026-08-04"),
    ] {
        let corrupt = rewrite_baseline(name, &|doc| {
            doc["activation"]["utc"] = serde_json::json!(utc);
        });
        let rejected = run_comparator(&corrupt, &fixture.results, None);
        assert_eq!(
            rejected.status.code(),
            Some(2),
            "{name} must fail closed\nstderr: {}",
            String::from_utf8_lossy(&rejected.stderr)
        );
    }
    let missing_activation_utc = rewrite_baseline("activation-utc-missing", &|doc| {
        doc["activation"].as_object_mut().unwrap().remove("utc");
    });
    assert_eq!(
        run_comparator(&missing_activation_utc, &fixture.results, None)
            .status
            .code(),
        Some(2)
    );

    // Source run must be a non-empty string.
    for (name, run) in [
        ("source-run-blank", serde_json::json!("   ")),
        ("source-run-nonstring", serde_json::json!(30928964359u64)),
    ] {
        let corrupt = rewrite_baseline(name, &|doc| {
            doc["source"]["run"] = run.clone();
        });
        let rejected = run_comparator(&corrupt, &fixture.results, None);
        assert_eq!(
            rejected.status.code(),
            Some(2),
            "{name} must fail closed\nstderr: {}",
            String::from_utf8_lossy(&rejected.stderr)
        );
    }
    let missing_run = rewrite_baseline("source-run-missing", &|doc| {
        doc["source"].as_object_mut().unwrap().remove("run");
    });
    assert_eq!(
        run_comparator(&missing_run, &fixture.results, None)
            .status
            .code(),
        Some(2)
    );

    // Source attempt must be a positive exact integer: zero, bool, and float
    // are all corrupt.
    for (name, attempt) in [
        ("source-attempt-zero", serde_json::json!(0)),
        ("source-attempt-bool", serde_json::json!(true)),
        ("source-attempt-float", serde_json::json!(1.0)),
    ] {
        let corrupt = rewrite_baseline(name, &|doc| {
            doc["source"]["attempt"] = attempt.clone();
        });
        let rejected = run_comparator(&corrupt, &fixture.results, None);
        assert_eq!(
            rejected.status.code(),
            Some(2),
            "{name} must fail closed\nstderr: {}",
            String::from_utf8_lossy(&rejected.stderr)
        );
    }

    // Source UTC must exist and be strict RFC 3339 Z.
    let missing_source_utc = rewrite_baseline("source-utc-missing", &|doc| {
        doc["source"].as_object_mut().unwrap().remove("utc");
    });
    assert_eq!(
        run_comparator(&missing_source_utc, &fixture.results, None)
            .status
            .code(),
        Some(2)
    );
    let invalid_source_utc = rewrite_baseline("source-utc-invalid", &|doc| {
        doc["source"]["utc"] = serde_json::json!("2026-08-04 16:26:18");
    });
    assert_eq!(
        run_comparator(&invalid_source_utc, &fixture.results, None)
            .status
            .code(),
        Some(2)
    );

    // The untouched durable baseline still validates after all corruptions.
    let accepted = run_comparator(&fixture.baseline, &fixture.results, None);
    assert_eq!(
        accepted.status.code(),
        Some(0),
        "pristine baseline must still compare\nstderr: {}",
        String::from_utf8_lossy(&accepted.stderr)
    );
}

#[test]
fn pr_loop_comparator_fails_closed_on_identity_runner_and_cache_mismatch() {
    if !harness_tools_available() || !tool_on_path("python3") {
        eprintln!("skipping: harness or Python tools unavailable");
        return;
    }
    let (_tools, fixture) = build_gate_fixture();
    let tampered_results = |name: &str, pointer: &str, replacement: serde_json::Value| {
        let dir = fixture.samples_dir.join(name);
        fs::create_dir(&dir).unwrap();
        fixture
            .results
            .iter()
            .enumerate()
            .map(|(index, result)| {
                let copied = dir.join(format!("result-{index}.json"));
                if index == 0 {
                    let mut doc = read_json(result);
                    *doc.pointer_mut(pointer).expect("tamper field") = replacement.clone();
                    fs::write(&copied, serde_json::to_vec_pretty(&doc).unwrap()).unwrap();
                } else {
                    fs::copy(result, &copied).unwrap();
                }
                copied
            })
            .collect::<Vec<_>>()
    };

    for (label, pointer, replacement) in [
        // Semantic identity: normalized workload command args.
        (
            "command-args",
            "/workloads/0/command/1",
            serde_json::json!("mutate"),
        ),
        // Semantic identity: selected test command.
        (
            "test-command",
            "/workloads/0/semantics/selected_test_command/2",
            serde_json::json!("./only"),
        ),
        // Semantic identity: test selection.
        (
            "test-selection",
            "/workloads/0/semantics/test_selection/mode",
            serde_json::json!("narrowed"),
        ),
        // Per-scenario mutation digest.
        (
            "mutation-digest",
            "/cross_workload/scenarios/single-file/mutation_identity_sha256",
            serde_json::json!("0".repeat(64)),
        ),
        // Runner class.
        (
            "runner-label",
            "/provenance/runner_label",
            serde_json::json!("other-runner"),
        ),
        (
            "logical-cpu",
            "/provenance/logical_cpu_count",
            serde_json::json!(1),
        ),
        // Measurement cache state/policy.
        (
            "cache-state",
            "/provenance/go_build_cache_state",
            serde_json::json!("warmup"),
        ),
        (
            "cache-policy",
            "/provenance/go_build_cache_policy",
            serde_json::json!("unenforced"),
        ),
        // Non-successful results are never comparable evidence.
        ("not-ok", "/ok", serde_json::json!(false)),
        (
            "failed-invariant",
            "/workloads/0/invariants/0/ok",
            serde_json::json!(false),
        ),
    ] {
        let results = tampered_results(label, pointer, replacement);
        let rejected = run_comparator(&fixture.baseline, &results, None);
        assert_eq!(
            rejected.status.code(),
            Some(2),
            "{label} must fail closed\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&rejected.stdout),
            String::from_utf8_lossy(&rejected.stderr)
        );
    }

    // Baseline-side semantic drift is incomparable too.
    let mut doc = read_json(&fixture.baseline);
    doc["semantic_identity"]["workloads"][0]["runner_mode"] = serde_json::json!("forged");
    let forged = fixture.samples_dir.join("forged-identity-baseline.json");
    fs::write(&forged, serde_json::to_vec_pretty(&doc).unwrap()).unwrap();
    let rejected = run_comparator(&forged, &fixture.results, None);
    assert_eq!(rejected.status.code(), Some(2));
}

#[test]
fn pr_loop_comparator_provenance_drift_warns_without_failing() {
    if !harness_tools_available() || !tool_on_path("python3") {
        eprintln!("skipping: harness or Python tools unavailable");
        return;
    }
    let (_tools, fixture) = build_gate_fixture();
    // Volatile execution provenance (Git/kernel/togi versions) is
    // observational: drift warns but never changes the exit code. The pinned
    // Go toolchain is NOT volatile and is covered by a separate hard check.
    let dir = fixture.samples_dir.join("provenance-drift");
    fs::create_dir(&dir).unwrap();
    let results: Vec<PathBuf> = fixture
        .results
        .iter()
        .enumerate()
        .map(|(index, result)| {
            let copied = dir.join(format!("result-{index}.json"));
            if index == 1 {
                let mut doc = read_json(result);
                doc["provenance"]["git_version"] = serde_json::json!("git version 9.9.9");
                doc["provenance"]["togi_version"] = serde_json::json!("togi 0.0.0-drift");
                fs::write(&copied, serde_json::to_vec_pretty(&doc).unwrap()).unwrap();
            } else {
                fs::copy(result, &copied).unwrap();
            }
            copied
        })
        .collect();
    let output = run_comparator(&fixture.baseline, &results, None);
    assert_eq!(
        output.status.code(),
        Some(0),
        "provenance drift must not fail the gate\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("volatile execution provenance drift: git_version"),
        "expected git_version drift warning: {stdout}"
    );
    assert!(
        stdout.contains("volatile execution provenance drift: togi_version"),
        "expected togi_version drift warning: {stdout}"
    );
}

#[test]
fn pr_loop_comparator_pins_the_go_toolchain() {
    if !harness_tools_available() || !tool_on_path("python3") {
        eprintln!("skipping: harness or Python tools unavailable");
        return;
    }
    let (_tools, fixture) = build_gate_fixture();
    // Go is a pinned comparable dimension: any result measured under a Go
    // toolchain different from the baseline's is incomparable, never a
    // warning.
    let dir = fixture.samples_dir.join("go-version-drift");
    fs::create_dir(&dir).unwrap();
    let results: Vec<PathBuf> = fixture
        .results
        .iter()
        .enumerate()
        .map(|(index, result)| {
            let copied = dir.join(format!("result-{index}.json"));
            if index == 2 {
                let mut doc = read_json(result);
                doc["provenance"]["go_version"] =
                    serde_json::json!("go version go1.25.0 linux/amd64");
                fs::write(&copied, serde_json::to_vec_pretty(&doc).unwrap()).unwrap();
            } else {
                fs::copy(result, &copied).unwrap();
            }
            copied
        })
        .collect();
    let rejected = run_comparator(&fixture.baseline, &results, None);
    assert_eq!(
        rejected.status.code(),
        Some(2),
        "a Go toolchain mismatch must fail closed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&rejected.stdout),
        String::from_utf8_lossy(&rejected.stderr)
    );
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("go version"),
        "expected a Go toolchain mismatch error: {}",
        String::from_utf8_lossy(&rejected.stderr)
    );
}

#[test]
fn pr_loop_comparator_ratio_and_sign_drift_are_observational_only() {
    if !harness_tools_available() || !tool_on_path("python3") {
        eprintln!("skipping: harness or Python tools unavailable");
        return;
    }
    let (_tools, fixture) = build_gate_fixture();
    let copy_results = |name: &str| {
        let dir = fixture.samples_dir.join(name);
        fs::create_dir(&dir).unwrap();
        fixture
            .results
            .iter()
            .enumerate()
            .map(|(index, result)| {
                let copied = dir.join(format!("result-{index}.json"));
                fs::copy(result, &copied).unwrap();
                copied
            })
            .collect::<Vec<_>>()
    };

    // Warm/cold wall ratio drifting more than 25% below the recorded
    // baseline ratio warns but still exits 0.
    set_baseline_metric(&fixture.baseline, "cold-regular", "wall_ms", [1000; 5]);
    set_baseline_metric(&fixture.baseline, "warm-exact-cache", "wall_ms", [1000; 5]);
    let ratio = copy_results("ratio-drift");
    set_result_metric(&ratio, "cold-regular", "wall_ms", [1000; 3]);
    set_result_metric(&ratio, "warm-exact-cache", "wall_ms", [100; 3]);
    let output = run_comparator(&fixture.baseline, &ratio, None);
    assert_eq!(
        output.status.code(),
        Some(0),
        "ratio drift must stay observational\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("wall ratio drifted"),
        "expected ratio drift warning: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    // Schemata minus cold-regular wall delta changing sign warns but exits 0.
    set_baseline_metric(&fixture.baseline, "cold-schemata", "wall_ms", [900; 5]);
    let sign = copy_results("sign-drift");
    set_result_metric(&sign, "cold-regular", "wall_ms", [1000; 3]);
    set_result_metric(&sign, "cold-schemata", "wall_ms", [1100; 3]);
    let output = run_comparator(&fixture.baseline, &sign, None);
    assert_eq!(
        output.status.code(),
        Some(0),
        "sign drift must stay observational\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("changed sign"),
        "expected sign-change warning: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// SHA-256 hex of one file, mirroring the Python tooling's digest_file.
fn digest_file_hex(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    digest.update(fs::read(path).expect("read file to digest"));
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

/// SHA-256 hex of a fixture tree, mirroring the Python tooling's digest_tree
/// (sorted relative POSIX paths, each followed by a NUL and the content).
fn digest_tree_hex(root: &Path) -> String {
    use sha2::{Digest, Sha256};
    fn collect(root: &Path, dir: &Path, files: &mut Vec<String>) {
        for entry in fs::read_dir(dir).expect("list fixture tree") {
            let path = entry.expect("fixture entry").path();
            if path.is_dir() {
                collect(root, &path, files);
            } else if path.is_file() {
                files.push(
                    path.strip_prefix(root)
                        .expect("fixture path prefix")
                        .to_str()
                        .expect("UTF-8 fixture path")
                        .replace('\\', "/"),
                );
            }
        }
    }
    let mut files = Vec::new();
    collect(root, root, &mut files);
    files.sort_unstable();
    let mut digest = Sha256::new();
    for relative in files {
        digest.update(relative.as_bytes());
        digest.update(b"\0");
        digest.update(fs::read(root.join(&relative)).expect("read fixture file"));
    }
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

#[test]
fn pr_loop_checked_in_baseline_is_a_valid_durable_promoted_artifact() {
    let baseline = read_json(&repo_root().join("benchmarks/pr-loop/baseline.json"));
    assert_eq!(baseline["kind"], "togi_pr_loop_baseline");
    assert_eq!(baseline["schema_version"], 1);
    assert_eq!(baseline["status"], "durable");
    assert!(
        baseline["comparison_policy"]
            .as_str()
            .expect("comparison policy")
            .contains("median of three")
    );
    assert_eq!(
        baseline["tolerance_policy"],
        serde_json::json!({
            "policy_version": 1,
            "repetitions": 3,
            "aggregation": "median",
            "wall_ms": {"relative_numerator": 3, "relative_denominator": 2, "absolute_floor_ms": 250},
            "reported_duration_ms": {"relative_numerator": 3, "relative_denominator": 2, "absolute_floor_ms": 100}
        }),
        "the checked-in baseline must pin the fixed tolerance policy"
    );
    let activation = &baseline["activation"];
    assert!(activation["pr"].as_u64().expect("activation PR") >= 1);
    assert!(
        !activation["actor"]
            .as_str()
            .expect("actor")
            .trim()
            .is_empty()
    );
    let utc = activation["utc"].as_str().expect("activation UTC");
    assert!(
        utc.len() == 20 && utc.ends_with('Z') && &utc[10..11] == "T",
        "activation UTC must be RFC 3339 Z form, got {utc}"
    );
    let source = &baseline["source"];
    assert!(!source["commit"].as_str().expect("source commit").is_empty());
    assert!(source["github_artifact_id"].as_u64().expect("artifact id") >= 1);
    let artifact_digest = source["github_artifact_sha256"]
        .as_str()
        .expect("artifact digest");
    assert!(
        artifact_digest.len() == 64
            && artifact_digest
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "artifact digest must be normalized lowercase 64-hex"
    );
    let runner_class = &baseline["runner_class"];
    for key in ["runner_label", "os", "arch"] {
        assert!(!runner_class[key].as_str().expect(key).is_empty());
    }
    assert!(runner_class["logical_cpu_count"].as_u64().expect("cpus") >= 1);
    assert_eq!(
        baseline["measurement_identity"],
        serde_json::json!({
            "go_build_cache_state": "primed",
            "go_build_cache_policy": "job-private-explicit-gocache"
        })
    );

    // Samples: every declared workload, five raw values per metric, stored
    // medians internally correct, promoter's 3x outlier bound intact.
    let samples = baseline["samples"].as_object().expect("samples");
    let mut sample_names: Vec<&str> = samples.keys().map(String::as_str).collect();
    sample_names.sort_unstable();
    let mut expected_names = WORKLOAD_NAMES;
    expected_names.sort_unstable();
    assert_eq!(sample_names, expected_names);
    for name in WORKLOAD_NAMES {
        for metric in ["wall_ms", "reported_duration_ms"] {
            let raw: Vec<u64> = baseline["samples"][name][metric]
                .as_array()
                .expect("raw metric array")
                .iter()
                .map(|value| value.as_u64().expect("integer raw value"))
                .collect();
            assert_eq!(raw.len(), 5, "{name}/{metric} must carry five raw values");
            let mut sorted = raw.clone();
            sorted.sort_unstable();
            let stored = baseline["samples"][name][format!("{metric}_median")]
                .as_u64()
                .expect("integer stored median");
            assert_eq!(stored, sorted[2], "{name}/{metric} stored median mismatch");
            assert!(stored > 0, "{name}/{metric} median must be positive");
            for value in &raw {
                assert!(
                    *value <= 3 * stored,
                    "{name}/{metric} raw value {value} exceeds the promoter's 3x bound"
                );
            }
        }
    }

    // Digest completeness: exactly the manifest, every declared scenario
    // patch, the fixture tree, and the five sample result digests — and the
    // recorded digests match this checkout.
    let manifest = read_json(&repo_root().join("benchmarks/pr-loop/manifest.json"));
    let mut expected_keys: Vec<String> = vec!["benchmarks/pr-loop/manifest.json".to_string()];
    for scenario in manifest["scenarios"].as_array().expect("scenarios") {
        expected_keys.push(
            scenario["patch_file"]
                .as_str()
                .expect("patch file")
                .to_string(),
        );
    }
    let fixture_dir = manifest["fixture"]["source_dir"]
        .as_str()
        .expect("fixture source dir");
    expected_keys.push(format!("{}/", fixture_dir.trim_end_matches('/')));
    for index in 1..=5 {
        expected_keys.push(format!("sample-{index}/pr-loop-benchmark-result.json"));
    }
    expected_keys.sort_unstable();
    expected_keys.dedup();
    let digests = baseline["source_file_digests"]
        .as_object()
        .expect("source file digests");
    let mut actual_keys: Vec<&str> = digests.keys().map(String::as_str).collect();
    actual_keys.sort_unstable();
    assert_eq!(
        actual_keys,
        expected_keys.iter().map(String::as_str).collect::<Vec<_>>(),
        "baseline digest key set must exactly match the current corpus"
    );
    for (key, value) in digests {
        let recorded = value.as_str().expect("digest string");
        assert!(
            recorded.len() == 64
                && recorded
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "{key} digest must be normalized lowercase 64-hex"
        );
        if key.starts_with("sample-") {
            continue;
        }
        let actual = if let Some(dir) = key.strip_suffix('/') {
            digest_tree_hex(&repo_root().join(dir))
        } else {
            digest_file_hex(&repo_root().join(key))
        };
        assert_eq!(
            actual, recorded,
            "current {key} digest must match the baseline"
        );
    }

    // Semantic identity pins the v2 manifest and the exact workload order.
    assert_eq!(
        baseline["semantic_identity"]["manifest"],
        serde_json::json!({
            "name": "togi-pr-loop-benchmarks",
            "schema_version": 2,
            "path": "benchmarks/pr-loop/manifest.json"
        })
    );
    let identity_names: Vec<&str> = baseline["semantic_identity"]["workloads"]
        .as_array()
        .expect("identity workloads")
        .iter()
        .map(|workload| workload["name"].as_str().expect("workload name"))
        .collect();
    assert_eq!(identity_names, WORKLOAD_NAMES);
}

#[test]
fn codeowners_protects_the_gate_corpus_workflows_and_itself() {
    let contents =
        fs::read_to_string(repo_root().join(".github/CODEOWNERS")).expect("CODEOWNERS must exist");
    for pattern in [
        "/benchmarks/pr-loop/",
        "/.github/workflows/pr-loop-regression-gate.yml",
        "/.github/workflows/pr-loop-calibration.yml",
        "/.github/CODEOWNERS",
    ] {
        let line = contents
            .lines()
            .filter(|line| !line.trim_start().starts_with('#'))
            .find(|line| line.split_whitespace().next() == Some(pattern))
            .unwrap_or_else(|| panic!("CODEOWNERS must cover {pattern}"));
        let owners: Vec<&str> = line.split_whitespace().skip(1).collect();
        assert_eq!(
            owners,
            ["@Darkroom4364"],
            "{pattern} must be owned exactly by @Darkroom4364"
        );
    }
}

#[test]
fn docs_and_readme_record_the_gate_enforcement_contract() {
    // Observable identifiers only: job/workflow names, the workflow path,
    // and the ruleset id — not prose.
    let docs =
        fs::read_to_string(repo_root().join("docs/COMPATIBILITY.md")).expect("compatibility docs");
    assert!(
        docs.contains("PR-loop Benchmark Evidence"),
        "COMPATIBILITY must keep documenting the telemetry job"
    );
    assert!(
        docs.contains("telemetry only"),
        "COMPATIBILITY must mark the evidence job as telemetry only"
    );
    assert!(
        docs.contains("PR-loop Regression Gate"),
        "COMPATIBILITY must document the regression gate"
    );
    assert!(
        docs.contains(".github/workflows/pr-loop-regression-gate.yml"),
        "COMPATIBILITY must link the gate workflow"
    );
    assert!(
        !docs.contains("no\n  current baseline, threshold, or merge gate"),
        "COMPATIBILITY must not claim the gate does not exist"
    );
    let readme = fs::read_to_string(repo_root().join("README.md")).expect("README");
    assert!(
        readme.contains("15308939"),
        "README must name the main ruleset id used for required-check activation"
    );
    assert!(
        readme.contains("`PR-loop Regression Gate`"),
        "README must name the exact required check context"
    );
}

#[test]
fn pr_loop_regression_gate_workflow_is_fail_closed_and_comparator_driven() {
    let workflow =
        fs::read_to_string(repo_root().join(".github/workflows/pr-loop-regression-gate.yml"))
            .expect("regression gate workflow");
    let parsed: serde_yaml::Value = serde_yaml::from_str(&workflow).expect("gate workflow YAML");
    assert_eq!(parsed["name"].as_str(), Some("PR-loop Regression Gate"));
    assert_eq!(
        parsed["permissions"]["contents"].as_str(),
        Some("read"),
        "gate must be read-only"
    );

    // Triggers: every PR and every push to main, with no paths filters,
    // so the gate can never be bypassed by file selection.
    assert!(workflow.contains("pull_request:"));
    assert!(workflow.contains("push:"));
    assert!(workflow.contains("branches: [main]"));
    assert!(!workflow.contains("paths:"), "gate must not filter paths");
    assert!(!workflow.contains("workflow_dispatch:"));

    let job = &parsed["jobs"]["regression-gate"];
    assert_eq!(job["name"].as_str(), Some("PR-loop Regression Gate"));
    assert_eq!(
        parsed["name"].as_str(),
        job["name"].as_str(),
        "workflow and job names must coincide so the required check context is unambiguous"
    );
    assert_eq!(job["runs-on"].as_str(), Some("ubuntu-24.04"));
    assert_eq!(job["permissions"]["contents"].as_str(), Some("read"));
    for field in ["if", "needs", "continue-on-error"] {
        assert!(
            job.get(field).is_none(),
            "gate job must not define `{field}`: the gate can never be skipped or masked"
        );
    }
    // runner.temp is not available in job-level env; the gate must bind the
    // job-private GOCACHE with step-level literals only.
    let job_env = job.get("env").and_then(|env| env.as_mapping());
    if let Some(env) = job_env {
        for (key, value) in env {
            assert!(
                !value.as_str().unwrap_or_default().contains("runner.temp"),
                "job-level env {key:?} must not reference runner.temp"
            );
        }
    }

    let steps = job["steps"].as_sequence().expect("gate must have steps");
    let step = |name| {
        steps
            .iter()
            .position(|item| item["name"].as_str() == Some(name))
            .map(|index| (index, &steps[index]))
            .unwrap_or_else(|| panic!("missing gate step {name}"))
    };

    let checkout = steps
        .iter()
        .find(|item| item["uses"].as_str() == Some("actions/checkout@v7"))
        .expect("gate must check out the repository");
    assert_eq!(
        checkout["with"]["persist-credentials"].as_bool(),
        Some(false)
    );
    assert_eq!(
        checkout["with"]["fetch-depth"].as_u64(),
        Some(0),
        "gate must fetch full history so the trusted base commit is available"
    );
    let toolchain = steps
        .iter()
        .find(|item| {
            item["uses"].as_str()
                == Some("dtolnay/rust-toolchain@29eef336d9b2848a0b548edc03f92a220660cdb8")
        })
        .expect("gate must pin the Rust toolchain action");
    assert_eq!(toolchain["with"]["toolchain"].as_str(), Some("stable"));
    assert!(
        steps.iter().any(|item| {
            item["uses"].as_str()
                == Some("Swatinem/rust-cache@e18b497796c12c097a38f9edb9d0641fb99eee32")
        }),
        "gate must use the pinned rust cache"
    );
    let native = step("Assert native Linux x86_64 runner").1;
    assert_eq!(
        native["run"].as_str(),
        Some("bash ./.github/scripts/assert-native-target.sh")
    );
    for (key, expected) in [
        ("TOGI_EXPECTED_TARGET", "x86_64-unknown-linux-gnu"),
        ("TOGI_EXPECTED_ARCH", "x86_64"),
    ] {
        assert_eq!(native["env"][key].as_str(), Some(expected));
    }
    let setup_go = steps
        .iter()
        .find(|item| item["uses"].as_str() == Some("actions/setup-go@v7"))
        .expect("gate must set up Go");
    assert_eq!(
        setup_go["with"]["go-version"].as_str(),
        Some("1.26.5"),
        "gate must pin Go to exactly 1.26.5"
    );

    let (create_index, create) = step("Create job-private Go build cache");
    assert_eq!(create["shell"].as_str(), Some("bash"));
    assert_eq!(create["run"].as_str(), Some(APPROVED_GO_CACHE_CREATION));
    assert_eq!(
        create["env"]["BENCH_GOCACHE"].as_str(),
        Some("${{ runner.temp }}/togi-pr-loop-gocache"),
        "cache creation must bind the step-level runner.temp GOCACHE literal"
    );
    let (_, prerequisites) = step("Check benchmark prerequisites");
    let prerequisite_run = prerequisites["run"].as_str().expect("prerequisite run");
    for tool in ["bash", "git", "go", "jq", "sed", "sha256sum", "python3"] {
        assert!(
            prerequisite_run.contains(tool),
            "gate prerequisites must check {tool}"
        );
    }
    let (_, build) = step("Build release binary");
    assert_eq!(
        build["run"].as_str(),
        Some("cargo build --locked --release")
    );

    let (warmup_index, warmup) = step("Warm Go build cache");
    let (acquisition_index, acquisition) = step("Acquire three primed samples");
    let (select_index, select) = step("Select trusted baseline");
    let (compare_index, compare) = step("Compare against durable baseline");
    let (upload_index, upload) = step("Upload regression gate evidence");
    assert!(create_index < warmup_index);
    assert!(warmup_index < acquisition_index);
    assert!(acquisition_index < select_index);
    assert!(select_index < compare_index);
    assert!(compare_index < upload_index);
    for (name, item) in [("warmup", warmup), ("acquisition", acquisition)] {
        assert_eq!(
            item["env"]["GOCACHE"].as_str(),
            Some("${{ runner.temp }}/togi-pr-loop-gocache"),
            "{name} must bind the identical step-level runner.temp GOCACHE literal"
        );
        assert_eq!(
            item["env"]["TOGI_BIN"].as_str(),
            Some("${{ github.workspace }}/target/release/togi")
        );
        assert_eq!(
            item["env"]["BENCH_RUNNER_LABEL"].as_str(),
            Some("github-actions-ubuntu-24.04-linux-x86_64"),
            "{name} must record the gate runner class"
        );
    }
    assert_eq!(
        warmup["env"]["BENCH_GO_BUILD_CACHE_STATE"].as_str(),
        Some("warmup")
    );
    assert_eq!(
        acquisition["env"]["BENCH_GO_BUILD_CACHE_STATE"].as_str(),
        Some("primed")
    );
    let acquisition_run = acquisition["run"].as_str().expect("acquisition run");
    assert!(
        acquisition_run.contains("for sample in 1 2 3; do"),
        "the gate must acquire exactly three primed samples in one loop"
    );
    assert_eq!(
        acquisition_run.matches("run-pr-loop-benchmarks.sh").count(),
        1,
        "the acquisition loop must contain exactly one harness invocation"
    );
    assert!(
        acquisition_run.contains("$GATE_OUTPUT/sample-$sample"),
        "acquisition must emit one output directory per sample"
    );

    // Trusted-base selection: on pull_request the comparator must run
    // against the base SHA's baseline (never the PR head's copy); the only
    // head fallback is the explicit one-time bootstrap when the base
    // genuinely carries no baseline. Invalid or unavailable base SHAs fail
    // closed with no permissive fallback.
    assert_eq!(select["shell"].as_str(), Some("bash"));
    assert_eq!(
        select["env"]["EVENT_NAME"].as_str(),
        Some("${{ github.event_name }}")
    );
    assert_eq!(
        select["env"]["PR_BASE_SHA"].as_str(),
        Some("${{ github.event.pull_request.base.sha }}")
    );
    let select_run = select["run"].as_str().expect("select run");
    for required in [
        "=~ ^[0-9a-f]{40}$",
        "git cat-file -e \"$BASE_SHA^{commit}\"",
        "git cat-file -e \"$BASE_SHA:benchmarks/pr-loop/baseline.json\"",
        "git show \"$BASE_SHA:benchmarks/pr-loop/baseline.json\" > \"$GATE_OUTPUT/baseline.json\"",
        "one-time bootstrap",
        "cp benchmarks/pr-loop/baseline.json \"$GATE_OUTPUT/baseline.json\"",
        "test -s \"$GATE_OUTPUT/baseline.json\"",
    ] {
        assert!(
            select_run.contains(required),
            "baseline selection must contain `{required}`"
        );
    }
    for forbidden in ["|| true", "continue"] {
        assert!(
            !select_run.contains(forbidden),
            "baseline selection must not contain permissive fallback `{forbidden}`"
        );
    }

    // The comparison step alone performs the timing exit behavior: it must
    // invoke the comparator against the selected trusted baseline.
    let compare_run = compare["run"].as_str().expect("compare run");
    assert!(compare_run.contains("benchmarks/pr-loop/compare-baseline.py"));
    assert!(compare_run.contains("--baseline \"$GATE_OUTPUT/baseline.json\""));
    assert!(
        !compare_run.contains("--baseline benchmarks/pr-loop/baseline.json"),
        "compare step must not read the PR head's baseline directly"
    );
    assert!(compare_run.contains("--output \"$GATE_OUTPUT/pr-loop-regression-comparison.json\""));
    for sample in 1..=3 {
        assert!(
            compare_run.contains(&format!("sample-{sample}/pr-loop-benchmark-result.json")),
            "compare step must pass sample-{sample}"
        );
    }
    assert_eq!(compare["shell"].as_str(), Some("bash"));

    assert_eq!(upload["if"].as_str(), Some("${{ always() }}"));
    assert_eq!(
        upload["uses"].as_str(),
        Some("actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02")
    );
    assert_eq!(
        upload["with"]["name"].as_str(),
        Some("pr-loop-regression-gate-${{ github.run_id }}-${{ github.run_attempt }}")
    );
    assert_eq!(
        upload["with"]["path"].as_str(),
        Some("${{ runner.temp }}/togi-pr-loop-gate")
    );
    assert_eq!(upload["with"]["if-no-files-found"].as_str(), Some("warn"));
    assert_eq!(upload["with"]["retention-days"].as_u64(), Some(14));

    // No step other than the artifact upload may be conditional or masked:
    // the gate blocks when the baseline is missing or incomparable.
    for (index, item) in steps.iter().enumerate() {
        assert!(
            item.get("continue-on-error").is_none(),
            "gate step {index} must not mask failure"
        );
        if index != upload_index {
            assert!(
                item.get("if").is_none(),
                "gate step {index} must not be conditional"
            );
        }
    }
}
