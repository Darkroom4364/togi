#!/usr/bin/env python3
"""Fail-closed collector for five comparable PR-loop benchmark results."""
import argparse
import hashlib
import json
from datetime import datetime
from pathlib import Path
import statistics
import sys

EXPECTED_KIND = "togi_pr_loop_benchmark_result"
EXPECTED_SCHEMA = 2
CANDIDATE_KIND = "togi_pr_loop_calibration_candidate"
CANDIDATE_SCHEMA = 2
EXPECTED_CACHE_STATE = "primed"
EXPECTED_CACHE_POLICY = "job-private-explicit-gocache"
RUNNER_MODES = ("regular", "schemata", "default")
SAMPLE_COUNT = 5
ROOT = Path(__file__).resolve().parents[2]
MANIFEST_PATH = ROOT / "benchmarks/pr-loop/manifest.json"


def fail(message):
    raise ValueError(message)


def exact_int(value):
    return type(value) is int


def nonempty_string(value):
    return isinstance(value, str) and bool(value.strip())


def require(value, predicate, message):
    if not predicate(value):
        fail(message)
    return value


def digest_file(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def digest_tree(path):
    if not path.is_dir():
        fail(f"fixture source directory is unavailable: {path}")
    digest = hashlib.sha256()
    for child in sorted(path.rglob("*")):
        if child.is_file():
            digest.update(child.relative_to(path).as_posix().encode() + b"\0")
            digest.update(child.read_bytes())
    return digest.hexdigest()


def load_json(path):
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"invalid JSON result {path}: {error}")
    return require(value, lambda item: isinstance(item, dict), f"result {path} is not a JSON object")


def normalized_workload(workload, path):
    require(workload, lambda item: isinstance(item, dict), f"{path}: workload is not an object")
    command = require(workload.get("command"), lambda item: isinstance(item, list) and item and all(nonempty_string(arg) for arg in item), f"{path}: workload command is invalid")
    semantics = require(workload.get("semantics"), lambda item: isinstance(item, dict), f"{path}: workload semantics are invalid")
    value = {key: item for key, item in workload.items() if key not in {"timing", "artifacts", "command"}}
    value["command_args"] = command[1:]
    value["semantics"] = {key: item for key, item in semantics.items() if key != "reported_duration_ms"}
    return value


def validate_fixture_path(value, manifest_fixture_dir):
    require(value, lambda item: item == manifest_fixture_dir, "fixture source dir differs from manifest")
    path = Path(value)
    require(value, lambda item: not path.is_absolute() and ".." not in path.parts, "fixture source dir must be a repository-relative path")
    resolved = (ROOT / path).resolve()
    try:
        resolved.relative_to(ROOT.resolve())
    except ValueError:
        fail("fixture source dir escapes repository root")
    return resolved


def validate_fixture_scenarios(value, manifest_scenarios, path):
    require(value, lambda item: isinstance(item, dict), f"{path}: provenance.fixture_scenarios is invalid")
    require(sorted(value), lambda item: item == sorted(manifest_scenarios), f"{path}: provenance.fixture_scenarios keys differ from manifest scenarios")
    for name, entry in value.items():
        require(entry, lambda item: isinstance(item, dict), f"{path}: fixture scenario {name} is invalid")
        for key in ("patch_file", "patch_sha256", "base_revision"):
            require(entry.get(key), nonempty_string, f"{path}: fixture scenario {name} has invalid {key}")
        manifest_entry = manifest_scenarios[name]
        require(entry["patch_file"], lambda item: item == manifest_entry["patch_file"], f"{path}: fixture scenario {name} patch file differs from manifest")
    return value


def validate_result(result, path, expected_workloads, manifest_fixture_dir, manifest_scenarios):
    require(result.get("kind"), lambda item: item == EXPECTED_KIND, f"{path}: wrong result kind")
    require(result.get("schema_version"), lambda item: item == EXPECTED_SCHEMA, f"{path}: wrong result schema")
    require(result.get("timing_policy"), lambda item: item == "observational-only", f"{path}: wrong timing policy")
    require(result.get("ok"), lambda item: item is True, f"{path}: result is not ok")
    require(result.get("failures"), lambda item: item == [], f"{path}: result has failures")
    require(result.get("manifest"), lambda item: item == {"name": "togi-pr-loop-benchmarks", "schema_version": 2, "path": "benchmarks/pr-loop/manifest.json"}, f"{path}: manifest identity mismatch")
    provenance = require(result.get("provenance"), lambda item: isinstance(item, dict), f"{path}: missing provenance")
    for key in ("runner_label", "os", "arch", "kernel_release", "fixture_source_dir", "togi_version", "report_kind", "go_version", "git_version"):
        require(provenance.get(key), nonempty_string, f"{path}: invalid provenance.{key}")
    require(provenance.get("logical_cpu_count"), lambda item: exact_int(item) and item > 0, f"{path}: invalid provenance.logical_cpu_count")
    require(provenance.get("report_schema_version"), exact_int, f"{path}: invalid provenance.report_schema_version")
    require(
        provenance.get("go_build_cache_state"),
        lambda item: item == EXPECTED_CACHE_STATE,
        f"{path}: go build cache state must be primed",
    )
    require(
        provenance.get("go_build_cache_policy"),
        lambda item: item == EXPECTED_CACHE_POLICY,
        f"{path}: go build cache policy must be {EXPECTED_CACHE_POLICY}",
    )
    require(provenance.get("go_build_cache_path"), nonempty_string, f"{path}: invalid provenance.go_build_cache_path")
    for key in ("image_os", "image_version"):
        require(provenance.get(key), lambda item: item is None or nonempty_string(item), f"{path}: invalid provenance.{key}")
    fixture_dir = validate_fixture_path(provenance["fixture_source_dir"], manifest_fixture_dir)
    validate_fixture_scenarios(provenance.get("fixture_scenarios"), manifest_scenarios, path)
    cross_workload = require(result.get("cross_workload"), lambda item: isinstance(item, dict), f"{path}: missing cross-workload evidence")
    scenario_identity = require(cross_workload.get("scenarios"), lambda item: isinstance(item, dict), f"{path}: missing per-scenario identity")
    require(sorted(scenario_identity), lambda item: item == sorted(manifest_scenarios), f"{path}: per-scenario identity keys differ from manifest scenarios")
    for name, entry in scenario_identity.items():
        require(entry, lambda item: isinstance(item, dict), f"{path}: scenario {name} identity is invalid")
        require(entry.get("mutation_identity_consistent"), lambda item: item is True, f"{path}: scenario {name} mutation identity is invalid")
        require(entry.get("mutation_identity_sha256"), nonempty_string, f"{path}: scenario {name} missing mutation identity digest")
    workloads = require(result.get("workloads"), lambda item: isinstance(item, list), f"{path}: missing workloads")
    names = [item.get("name") if isinstance(item, dict) else None for item in workloads]
    require(names, lambda item: item == [name for name, _ in expected_workloads], f"{path}: workload order mismatch")
    for workload, (expected_name, expected_scenario) in zip(workloads, expected_workloads):
        name = workload.get("name") if isinstance(workload, dict) else "unknown"
        require(workload, lambda item: isinstance(item, dict), f"{path}: workload {name} is malformed")
        require(workload.get("ok"), lambda item: item is True, f"{path}: workload {name} is not ok")
        require(workload.get("scenario"), lambda item: item == expected_scenario, f"{path}: workload {expected_name} has wrong scenario")
        require(workload.get("runner_mode"), lambda item: item in RUNNER_MODES, f"{path}: workload {expected_name} has invalid runner_mode")
        require(workload.get("invariants"), lambda item: isinstance(item, list) and item and all(isinstance(invariant, dict) and invariant.get("ok") is True for invariant in item), f"{path}: workload {name} has failed invariants")
        timing = require(workload.get("timing"), lambda item: isinstance(item, dict), f"{path}: workload {name} lacks timing")
        for key in ("wall_ms", "reported_duration_ms"):
            require(timing.get(key), lambda item: exact_int(item) and item >= 0, f"{path}: workload {name} has invalid {key}")
        semantics = require(workload.get("semantics"), lambda item: isinstance(item, dict), f"{path}: workload {name} lacks semantics")
        require(
            semantics.get("selected_test_command"),
            lambda item: isinstance(item, list) and item and all(nonempty_string(arg) for arg in item),
            f"{path}: workload {name} has invalid selected_test_command",
        )
        normalized_workload(workload, path)
    return provenance, workloads, fixture_dir, scenario_identity


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path, help="new candidate JSON path")
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--source-run", required=True)
    parser.add_argument("--source-attempt", required=True, type=int)
    parser.add_argument("--source-utc", required=True, help="UTC timestamp in RFC 3339 Z form")
    parser.add_argument("results", nargs="+", type=Path, help="exactly five normalized result files")
    args = parser.parse_args(argv)
    if len(args.results) != SAMPLE_COUNT:
        fail(f"expected exactly {SAMPLE_COUNT} results, received {len(args.results)}")
    resolved_results = [path.resolve() for path in args.results]
    if len(set(resolved_results)) != SAMPLE_COUNT:
        fail("result paths must name five independent files")
    try:
        result_digests = [digest_file(path) for path in resolved_results]
    except OSError as error:
        fail(f"cannot digest normalized result: {error}")
    if len(set(result_digests)) != SAMPLE_COUNT:
        fail("normalized results must have five distinct content digests")
    if args.source_attempt < 1 or not nonempty_string(args.source_commit) or not nonempty_string(args.source_run):
        fail("source commit, run, and positive attempt are required")
    try:
        datetime.strptime(args.source_utc, "%Y-%m-%dT%H:%M:%SZ")
    except ValueError:
        fail("source UTC must be RFC 3339 UTC (YYYY-MM-DDTHH:MM:SSZ)")
    if args.output.exists():
        fail(f"output already exists: {args.output}")
    manifest = load_json(MANIFEST_PATH)
    require(manifest.get("schema_version"), lambda item: item == EXPECTED_SCHEMA, "manifest schema is not v2")
    fixture = require(manifest.get("fixture"), lambda item: isinstance(item, dict), "manifest fixture is invalid")
    manifest_fixture_dir = require(fixture.get("source_dir"), lambda item: nonempty_string(item) and not Path(item).is_absolute() and ".." not in Path(item).parts, "manifest fixture source dir is invalid")
    fixture_dir = validate_fixture_path(manifest_fixture_dir, manifest_fixture_dir)
    scenarios = require(manifest.get("scenarios"), lambda item: isinstance(item, list) and item, "manifest scenarios are invalid")
    manifest_scenarios = {}
    for entry in scenarios:
        name = require(entry.get("name") if isinstance(entry, dict) else None, nonempty_string, "manifest scenario name is invalid")
        patch_file = require(entry.get("patch_file"), nonempty_string, f"manifest scenario {name} patch file is invalid")
        patch_path = Path(patch_file)
        require(patch_file, lambda item: not patch_path.is_absolute() and ".." not in patch_path.parts, f"manifest scenario {name} patch file must be repository-relative")
        require((ROOT / patch_path).is_file(), lambda item: item, f"manifest scenario {name} patch file is unavailable")
        manifest_scenarios[name] = {"patch_file": patch_file}
    expected_workloads = []
    for item in manifest.get("workloads", []):
        name = item.get("name") if isinstance(item, dict) else None
        scenario = item.get("scenario") if isinstance(item, dict) else None
        require(name, nonempty_string, "manifest has an invalid workload name")
        require(scenario, lambda value: value in manifest_scenarios, f"manifest workload {name} names an undeclared scenario")
        expected_workloads.append((name, scenario))
    require(expected_workloads, lambda item: bool(item), "manifest has no valid workloads")
    parsed = [(path, load_json(path)) for path in resolved_results]
    validated = [validate_result(value, path, expected_workloads, manifest_fixture_dir, manifest_scenarios) for path, value in parsed]
    first_provenance, first_workloads, _, first_scenario_identity = validated[0]
    identity = {
        "manifest": parsed[0][1]["manifest"],
        "fixture_source_dir": first_provenance["fixture_source_dir"],
        "fixture_scenarios": {
            name: {"patch_file": entry["patch_file"], "patch_sha256": entry["patch_sha256"]}
            for name, entry in first_provenance["fixture_scenarios"].items()
        },
        "scenario_mutation_identity": {
            name: entry["mutation_identity_sha256"] for name, entry in first_scenario_identity.items()
        },
        "workloads": [normalized_workload(item, parsed[0][0]) for item in first_workloads],
    }
    runner_class = {key: first_provenance[key] for key in ("runner_label", "os", "arch", "logical_cpu_count")}
    execution_provenance = {key: first_provenance[key] for key in ("togi_version", "report_kind", "report_schema_version", "go_version", "git_version")}
    measurement_identity = {
        "go_build_cache_state": first_provenance["go_build_cache_state"],
        "go_build_cache_policy": first_provenance["go_build_cache_policy"],
    }
    go_build_cache_path = first_provenance["go_build_cache_path"]
    diagnostics = []
    for index, ((path, result), (provenance, workloads, _, scenario_identity)) in enumerate(zip(parsed, validated), start=1):
        candidate_identity = {
            "manifest": result["manifest"],
            "fixture_source_dir": provenance["fixture_source_dir"],
            "fixture_scenarios": {
                name: {"patch_file": entry["patch_file"], "patch_sha256": entry["patch_sha256"]}
                for name, entry in provenance["fixture_scenarios"].items()
            },
            "scenario_mutation_identity": {
                name: entry["mutation_identity_sha256"] for name, entry in scenario_identity.items()
            },
            "workloads": [normalized_workload(item, path) for item in workloads],
        }
        if candidate_identity != identity:
            fail(f"{path}: semantic identity mismatch")
        if {key: provenance[key] for key in runner_class} != runner_class:
            fail(f"{path}: runner class mismatch")
        if {key: provenance[key] for key in execution_provenance} != execution_provenance:
            fail(f"{path}: execution provenance mismatch")
        if {key: provenance[key] for key in measurement_identity} != measurement_identity:
            fail(f"{path}: measurement identity mismatch")
        if provenance["go_build_cache_path"] != go_build_cache_path:
            fail(f"{path}: go build cache path differs across samples")
        diagnostics.append({"sample": f"sample-{index}", "kernel_release": provenance["kernel_release"], "image_os": provenance["image_os"], "image_version": provenance["image_version"], "go_build_cache_path": provenance["go_build_cache_path"]})
    for name, entry in identity["fixture_scenarios"].items():
        if entry["patch_sha256"] != digest_file(ROOT / entry["patch_file"]):
            fail(f"current fixture patch for scenario {name} does not match result provenance")
    samples = {}
    for index, (name, _) in enumerate(expected_workloads):
        wall = [workloads[index]["timing"]["wall_ms"] for _, workloads, _, _ in validated]
        duration = [workloads[index]["timing"]["reported_duration_ms"] for _, workloads, _, _ in validated]
        median = statistics.median(wall)
        samples[name] = {"wall_ms": wall, "duration_ms": duration, "wall_ms_median": median, "wall_ms_mad": statistics.median([abs(value - median) for value in wall])}
    candidate = {
        "kind": CANDIDATE_KIND,
        "schema_version": CANDIDATE_SCHEMA,
        "comparison_policy": "not-a-baseline; no comparator or gate is defined",
        "source": {"commit": args.source_commit, "run": args.source_run, "attempt": args.source_attempt, "utc": args.source_utc},
        "runner_class": runner_class,
        "runner_diagnostics": diagnostics,
        "execution_provenance": execution_provenance,
        "measurement_identity": measurement_identity,
        "semantic_identity": identity,
        "samples": samples,
        "source_file_digests": {
            "benchmarks/pr-loop/manifest.json": digest_file(MANIFEST_PATH),
            **{
                entry["patch_file"]: digest_file(ROOT / entry["patch_file"])
                for entry in manifest_scenarios.values()
            },
            f"{manifest_fixture_dir.rstrip('/')}/": digest_tree(fixture_dir),
            **{
                f"sample-{index}/pr-loop-benchmark-result.json": digest
                for index, digest in enumerate(result_digests, start=1)
            },
        },
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(candidate, indent=2, sort_keys=True) + "\n", encoding="utf-8")


if __name__ == "__main__":
    try:
        main(sys.argv[1:])
    except ValueError as error:
        print(f"calibration collection failed: {error}", file=sys.stderr)
        sys.exit(2)
