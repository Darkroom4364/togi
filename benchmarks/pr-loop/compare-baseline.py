#!/usr/bin/env python3
"""Fail-closed PR-loop baseline comparator (issue #487-B).

Compares exactly three normalized, primed v2 harness results against the one
durable baseline produced by promote-baseline.py. Exit codes:

* 0  pass (observational drift may still be printed)
* 1  hard performance regression
* 2  malformed, missing, stale, or incomparable input

The comparator parses JSON data only; it never shells out or evaluates input.
Comparable identity is the semantic workload identity, per-scenario mutation
digest, runner class, measurement cache state/policy, and the recorded
manifest/patch/fixture digests. Volatile execution provenance (Go, Git,
kernel, image, togi version) is never comparable identity: drift there only
produces observational warnings.

Timing rule (fixed tolerance_policy v1): for every workload and both metrics
(wall_ms and reported_duration_ms), M is the integer middle of exactly three
measurements and B the verified stored baseline median; a workload/metric is
a hard regression iff ``2*M > 3*B + 2*floor`` (strict boundary), and any hard
regression exits 1. A single high raw sample that does not move the median
("one spike in three") is recorded and warned about observationally; it never
fails the comparison.
"""
import argparse
import hashlib
import json
from datetime import datetime
from pathlib import Path
import sys

EXPECTED_RESULT_KIND = "togi_pr_loop_benchmark_result"
EXPECTED_RESULT_SCHEMA = 2
BASELINE_KIND = "togi_pr_loop_baseline"
BASELINE_SCHEMA = 1
EXPECTED_CACHE_STATE = "primed"
EXPECTED_CACHE_POLICY = "job-private-explicit-gocache"
EXPECTED_TIMING_POLICY = "observational-only"
EXPECTED_MANIFEST_IDENTITY = {
    "name": "togi-pr-loop-benchmarks",
    "schema_version": 2,
    "path": "benchmarks/pr-loop/manifest.json",
}
RUNNER_MODES = ("regular", "schemata", "default")
METRICS = ("wall_ms", "reported_duration_ms")
SAMPLE_COUNT = 3
BASELINE_SAMPLE_COUNT = 5
RATIO_DRIFT_LIMIT_NUMERATOR = 1
RATIO_DRIFT_LIMIT_DENOMINATOR = 4
RATIO_EPSILON = 0.001
ROOT = Path(__file__).resolve().parents[2]
MANIFEST_PATH = ROOT / "benchmarks/pr-loop/manifest.json"
COMPARISON_POLICY = (
    "durable regression gate: median of three primed samples per workload/metric; "
    "hard failure when any workload/metric median exceeds the fixed tolerance_policy v1 "
    "(2*M > 3*B + 2*floor); raw sample spikes that do not move the median, ratio drift, "
    "and sign drift are observational only"
)
FIXED_TOLERANCE_POLICY = {
    "policy_version": 1,
    "repetitions": SAMPLE_COUNT,
    "aggregation": "median",
    "wall_ms": {"relative_numerator": 3, "relative_denominator": 2, "absolute_floor_ms": 250},
    "reported_duration_ms": {"relative_numerator": 3, "relative_denominator": 2, "absolute_floor_ms": 100},
}


def fail(message):
    raise ValueError(message)


def exact_int(value):
    return type(value) is int


def nonempty_string(value):
    return isinstance(value, str) and bool(value.strip())


def is_hex_digest(value):
    return isinstance(value, str) and len(value) == 64 and all(char in "0123456789abcdef" for char in value)


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
        fail(f"invalid JSON document {path}: {error}")
    return require(value, lambda item: isinstance(item, dict), f"document {path} is not a JSON object")


def median_of_three(values):
    ordered = sorted(values)
    return ordered[1]


def verify_stored_median(values, stored, message):
    if not exact_int(stored) or stored < 0:
        fail(message)
    if not all(exact_int(value) and value >= 0 for value in values):
        fail(message)
    ordered = sorted(values)
    middle = len(values) // 2
    if len(values) % 2 == 1:
        expected = ordered[middle]
    else:
        expected = (ordered[middle - 1] + ordered[middle]) / 2
    if stored != expected:
        fail(message)
    return expected


def normalized_workload(workload, path):
    require(workload, lambda item: isinstance(item, dict), f"{path}: workload is not an object")
    command = require(workload.get("command"), lambda item: isinstance(item, list) and item and all(nonempty_string(arg) for arg in item), f"{path}: workload command is invalid")
    semantics = require(workload.get("semantics"), lambda item: isinstance(item, dict), f"{path}: workload semantics are invalid")
    value = {key: item for key, item in workload.items() if key not in {"timing", "artifacts", "command"}}
    value["command_args"] = command[1:]
    value["semantics"] = {key: item for key, item in semantics.items() if key != "reported_duration_ms"}
    return value


def validate_fixture_scenarios(value, path):
    require(value, lambda item: isinstance(item, dict) and item, f"{path}: provenance.fixture_scenarios is invalid")
    for name, entry in value.items():
        require(entry, lambda item: isinstance(item, dict), f"{path}: fixture scenario {name} is invalid")
        for key in ("patch_file", "patch_sha256", "base_revision"):
            require(entry.get(key), nonempty_string, f"{path}: fixture scenario {name} has invalid {key}")
        require(entry["patch_sha256"], is_hex_digest, f"{path}: fixture scenario {name} patch digest is invalid")
    return value


def validate_rfc3339_utc(value, message):
    require(value, nonempty_string, message)
    try:
        datetime.strptime(value, "%Y-%m-%dT%H:%M:%SZ")
    except ValueError:
        fail(message)
    return value


def validate_baseline(path):
    baseline = load_json(path)
    require(baseline.get("kind"), lambda item: item == BASELINE_KIND, f"{path}: wrong baseline kind")
    require(baseline.get("schema_version"), lambda item: item == BASELINE_SCHEMA, f"{path}: wrong baseline schema")
    require(baseline.get("status"), lambda item: item == "durable", f"{path}: baseline is not a durable activated baseline")
    require(baseline.get("comparison_policy"), lambda item: item == COMPARISON_POLICY, f"{path}: baseline comparison policy mismatch")
    require(baseline.get("tolerance_policy"), lambda item: item == FIXED_TOLERANCE_POLICY, f"{path}: baseline tolerance policy must be the fixed v1 policy")
    activation = require(baseline.get("activation"), lambda item: isinstance(item, dict), f"{path}: baseline activation is invalid")
    require(activation.get("pr"), lambda item: exact_int(item) and item >= 1, f"{path}: baseline activation PR must be a positive integer")
    require(activation.get("actor"), nonempty_string, f"{path}: baseline activation actor must be non-empty")
    validate_rfc3339_utc(activation.get("utc"), f"{path}: baseline activation UTC must be RFC 3339 UTC (YYYY-MM-DDTHH:MM:SSZ)")
    source = require(baseline.get("source"), lambda item: isinstance(item, dict), f"{path}: baseline source is invalid")
    require(source.get("commit"), nonempty_string, f"{path}: baseline source commit is invalid")
    require(source.get("run"), nonempty_string, f"{path}: baseline source run is invalid")
    require(source.get("attempt"), lambda item: exact_int(item) and item >= 1, f"{path}: baseline source attempt must be a positive integer")
    validate_rfc3339_utc(source.get("utc"), f"{path}: baseline source UTC must be RFC 3339 UTC (YYYY-MM-DDTHH:MM:SSZ)")
    require(source.get("github_artifact_id"), lambda item: exact_int(item) and item >= 1, f"{path}: baseline source must preserve the GitHub artifact id")
    require(source.get("github_artifact_sha256"), is_hex_digest, f"{path}: baseline source must preserve the GitHub artifact digest")
    runner_class = validate_runner_class(baseline.get("runner_class"), f"{path}: baseline")
    measurement_identity = require(
        baseline.get("measurement_identity"),
        lambda item: item == {"go_build_cache_state": EXPECTED_CACHE_STATE, "go_build_cache_policy": EXPECTED_CACHE_POLICY},
        f"{path}: baseline measurement identity must be primed with the job-private explicit GOCACHE policy",
    )
    semantic_identity = require(baseline.get("semantic_identity"), lambda item: isinstance(item, dict), f"{path}: baseline semantic identity is invalid")
    require(semantic_identity.get("manifest"), lambda item: item == EXPECTED_MANIFEST_IDENTITY, f"{path}: baseline manifest identity is not v2")
    workload_names = validate_semantic_identity(semantic_identity, f"{path}: baseline")
    execution_provenance = require(baseline.get("execution_provenance"), lambda item: isinstance(item, dict), f"{path}: baseline execution provenance is invalid")
    for key in ("togi_version", "report_kind", "go_version", "git_version"):
        require(execution_provenance.get(key), nonempty_string, f"{path}: baseline execution provenance has invalid {key}")
    require(execution_provenance.get("report_schema_version"), exact_int, f"{path}: baseline execution provenance has invalid report_schema_version")
    digests = require(baseline.get("source_file_digests"), lambda item: isinstance(item, dict), f"{path}: baseline source_file_digests is invalid")
    require(digests, lambda item: item and all(is_hex_digest(digest) for digest in item.values()), f"{path}: baseline source_file_digests must map paths to SHA-256 digests")
    samples = require(baseline.get("samples"), lambda item: isinstance(item, dict) and item, f"{path}: baseline samples are invalid")
    require(sorted(samples), lambda item: item == sorted(workload_names), f"{path}: baseline samples must cover every semantic workload")
    for name, entry in samples.items():
        require(entry, lambda item: isinstance(item, dict), f"{path}: baseline samples for workload {name} are invalid")
        for metric in METRICS:
            values = require(
                entry.get(metric),
                lambda item: isinstance(item, list) and len(item) == 5 and all(exact_int(value) and value >= 0 for value in item),
                f"{path}: baseline workload {name} must carry exactly five raw non-negative integer {metric} values",
            )
            verify_stored_median(values, entry.get(f"{metric}_median"), f"{path}: baseline workload {name} stored {metric} median does not match the five raw values")
    return baseline


def validate_runner_class(value, prefix):
    runner_class = require(value, lambda item: isinstance(item, dict), f"{prefix} runner class is invalid")
    for key in ("runner_label", "os", "arch"):
        require(runner_class.get(key), nonempty_string, f"{prefix} runner class has invalid {key}")
    require(runner_class.get("logical_cpu_count"), lambda item: exact_int(item) and item > 0, f"{prefix} runner class has invalid logical_cpu_count")
    return runner_class


def validate_semantic_identity(identity, prefix):
    workload_identities = require(identity.get("workloads"), lambda item: isinstance(item, list) and item, f"{prefix} semantic identity lacks workloads")
    require(identity.get("fixture_source_dir"), nonempty_string, f"{prefix} semantic identity lacks fixture_source_dir")
    fixture_scenarios = require(identity.get("fixture_scenarios"), lambda item: isinstance(item, dict) and item, f"{prefix} semantic identity lacks fixture_scenarios")
    for name, entry in fixture_scenarios.items():
        require(entry, lambda item: isinstance(item, dict), f"{prefix} semantic identity fixture scenario {name} is invalid")
        require(entry.get("patch_file"), nonempty_string, f"{prefix} semantic identity fixture scenario {name} patch file is invalid")
        require(entry.get("patch_sha256"), is_hex_digest, f"{prefix} semantic identity fixture scenario {name} patch digest is invalid")
    scenario_digests = require(identity.get("scenario_mutation_identity"), lambda item: isinstance(item, dict) and item, f"{prefix} semantic identity lacks scenario mutation identity")
    for name, digest in scenario_digests.items():
        require(digest, is_hex_digest, f"{prefix} semantic identity scenario {name} mutation digest is invalid")
    require(sorted(fixture_scenarios), lambda item: item == sorted(scenario_digests), f"{prefix} semantic identity fixture scenarios and mutation identities disagree")
    names = []
    for item in workload_identities:
        require(item, lambda entry: isinstance(entry, dict), f"{prefix} semantic identity has an invalid workload entry")
        name = require(item.get("name"), nonempty_string, f"{prefix} semantic identity has an invalid workload name")
        require(item.get("scenario"), lambda entry: entry in fixture_scenarios, f"{prefix} semantic identity workload {name} names an undeclared scenario")
        require(item.get("runner_mode"), lambda entry: entry in RUNNER_MODES, f"{prefix} semantic identity workload {name} has an invalid runner_mode")
        require(item.get("command_args"), lambda entry: isinstance(entry, list) and entry and all(nonempty_string(arg) for arg in entry), f"{prefix} semantic identity workload {name} has invalid command args")
        semantics = require(item.get("semantics"), lambda entry: isinstance(entry, dict), f"{prefix} semantic identity workload {name} lacks semantics")
        require(semantics.get("selected_test_command"), lambda entry: isinstance(entry, list) and entry and all(nonempty_string(arg) for arg in entry), f"{prefix} semantic identity workload {name} has an invalid selected test command")
        require(semantics.get("test_selection"), lambda entry: isinstance(entry, dict), f"{prefix} semantic identity workload {name} lacks test-selection identity")
        require(semantics.get("mutation_identity_sha256"), is_hex_digest, f"{prefix} semantic identity workload {name} lacks a mutation digest")
        names.append(name)
    require(names, lambda item: len(set(item)) == len(item), f"{prefix} semantic identity workload names must be unique")
    return names


def validate_result(path, expected_names):
    result = load_json(path)
    require(result.get("kind"), lambda item: item == EXPECTED_RESULT_KIND, f"{path}: wrong result kind")
    require(result.get("schema_version"), lambda item: item == EXPECTED_RESULT_SCHEMA, f"{path}: wrong result schema")
    require(result.get("timing_policy"), lambda item: item == EXPECTED_TIMING_POLICY, f"{path}: wrong timing policy")
    require(result.get("ok"), lambda item: item is True, f"{path}: result is not ok")
    require(result.get("failures"), lambda item: item == [], f"{path}: result has failures")
    require(result.get("manifest"), lambda item: item == EXPECTED_MANIFEST_IDENTITY, f"{path}: manifest identity mismatch")
    provenance = require(result.get("provenance"), lambda item: isinstance(item, dict), f"{path}: missing provenance")
    for key in ("runner_label", "os", "arch", "kernel_release", "fixture_source_dir", "togi_version", "report_kind", "go_version", "git_version"):
        require(provenance.get(key), nonempty_string, f"{path}: invalid provenance.{key}")
    require(provenance.get("logical_cpu_count"), lambda item: exact_int(item) and item > 0, f"{path}: invalid provenance.logical_cpu_count")
    require(provenance.get("report_schema_version"), exact_int, f"{path}: invalid provenance.report_schema_version")
    require(provenance.get("go_build_cache_state"), lambda item: item == EXPECTED_CACHE_STATE, f"{path}: go build cache state must be primed")
    require(provenance.get("go_build_cache_policy"), lambda item: item == EXPECTED_CACHE_POLICY, f"{path}: go build cache policy must be {EXPECTED_CACHE_POLICY}")
    require(provenance.get("go_build_cache_path"), nonempty_string, f"{path}: invalid provenance.go_build_cache_path")
    for key in ("image_os", "image_version"):
        require(provenance.get(key), lambda item: item is None or nonempty_string(item), f"{path}: invalid provenance.{key}")
    validate_fixture_scenarios(provenance.get("fixture_scenarios"), path)
    cross_workload = require(result.get("cross_workload"), lambda item: isinstance(item, dict), f"{path}: missing cross-workload evidence")
    scenario_identity = require(cross_workload.get("scenarios"), lambda item: isinstance(item, dict) and item, f"{path}: missing per-scenario identity")
    for name, entry in scenario_identity.items():
        require(entry, lambda item: isinstance(item, dict), f"{path}: scenario {name} identity is invalid")
        require(entry.get("mutation_identity_consistent"), lambda item: item is True, f"{path}: scenario {name} mutation identity is invalid")
        require(entry.get("mutation_identity_sha256"), is_hex_digest, f"{path}: scenario {name} missing mutation identity digest")
    workloads = require(result.get("workloads"), lambda item: isinstance(item, list), f"{path}: missing workloads")
    names = [item.get("name") if isinstance(item, dict) else None for item in workloads]
    require(names, lambda item: item == expected_names, f"{path}: workload order/set mismatch")
    for workload in workloads:
        name = workload.get("name")
        require(workload.get("ok"), lambda item: item is True, f"{path}: workload {name} is not ok")
        require(workload.get("scenario"), nonempty_string, f"{path}: workload {name} has an invalid scenario")
        require(workload.get("runner_mode"), lambda item: item in RUNNER_MODES, f"{path}: workload {name} has an invalid runner_mode")
        require(workload.get("invariants"), lambda item: isinstance(item, list) and item and all(isinstance(invariant, dict) and invariant.get("ok") is True for invariant in item), f"{path}: workload {name} has failed invariants")
        timing = require(workload.get("timing"), lambda item: isinstance(item, dict), f"{path}: workload {name} lacks timing")
        for metric in METRICS:
            require(timing.get(metric), lambda item: exact_int(item) and item >= 0, f"{path}: workload {name} has invalid {metric}")
    return result


def result_semantic_identity(result, path):
    provenance = result["provenance"]
    return {
        "manifest": result["manifest"],
        "fixture_source_dir": provenance["fixture_source_dir"],
        "fixture_scenarios": {
            name: {"patch_file": entry["patch_file"], "patch_sha256": entry["patch_sha256"]}
            for name, entry in provenance["fixture_scenarios"].items()
        },
        "scenario_mutation_identity": {
            name: entry["mutation_identity_sha256"] for name, entry in result["cross_workload"]["scenarios"].items()
        },
        "workloads": [normalized_workload(item, path) for item in result["workloads"]],
    }


def result_runner_class(result):
    provenance = result["provenance"]
    return {key: provenance[key] for key in ("runner_label", "os", "arch", "logical_cpu_count")}


def ratio(value, baseline):
    if baseline == 0:
        return None
    return value / baseline


def fmt_ratio(value):
    return "n/a (zero baseline)" if value is None else f"{value:.3f}"


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", required=True, type=Path, help="durable baseline JSON produced by promote-baseline.py")
    parser.add_argument("--output", type=Path, default=None, help="write the machine-readable JSON comparison summary to this new path")
    parser.add_argument("results", nargs="*", type=Path, help="exactly three normalized primed v2 harness results")
    args = parser.parse_args(argv)
    if len(args.results) != SAMPLE_COUNT:
        fail(f"expected exactly {SAMPLE_COUNT} results, received {len(args.results)}")
    if args.output is not None and args.output.exists():
        fail(f"output already exists: {args.output}")

    baseline = validate_baseline(args.baseline)
    expected_names = validate_semantic_identity(baseline["semantic_identity"], "baseline")

    resolved_results = [path.resolve() for path in args.results]
    if len(set(resolved_results)) != SAMPLE_COUNT:
        fail("result paths must name three independent files")
    try:
        result_digests = [digest_file(path) for path in resolved_results]
    except OSError as error:
        fail(f"cannot digest normalized result: {error}")
    if len(set(result_digests)) != SAMPLE_COUNT:
        fail("normalized results must have three distinct content digests")
    results = [validate_result(path, expected_names) for path in resolved_results]

    # Current manifest, scenario patches, and fixture tree must match the
    # digests recorded when the baseline was promoted; otherwise the checked
    # out corpus has drifted and the comparison is not meaningful.
    # The recorded digest set must be complete: exactly the current manifest,
    # every scenario patch it declares, the fixture tree, and the five sample
    # result digests preserved by the promoter. Deletions or extra keys are
    # incomparable before any value is checked.
    digests = baseline["source_file_digests"]
    manifest = load_json(MANIFEST_PATH)
    require(manifest.get("schema_version"), lambda item: item == EXPECTED_RESULT_SCHEMA, "current manifest schema is not v2")
    manifest_scenarios = require(manifest.get("scenarios"), lambda item: isinstance(item, list) and item, "current manifest scenarios are invalid")
    patch_files = []
    for entry in manifest_scenarios:
        patch_file = require(entry.get("patch_file") if isinstance(entry, dict) else None, nonempty_string, "current manifest scenario patch file is invalid")
        if patch_file not in patch_files:
            patch_files.append(patch_file)
    manifest_fixture = require(manifest.get("fixture"), lambda item: isinstance(item, dict), "current manifest fixture is invalid")
    fixture_source_dir = require(manifest_fixture.get("source_dir"), nonempty_string, "current manifest fixture source dir is invalid")
    expected_digest_keys = {"benchmarks/pr-loop/manifest.json", *patch_files, f"{fixture_source_dir.rstrip('/')}/"}
    expected_digest_keys.update(f"sample-{index}/pr-loop-benchmark-result.json" for index in range(1, BASELINE_SAMPLE_COUNT + 1))
    if set(digests) != expected_digest_keys:
        fail("baseline source_file_digests must name exactly the current manifest, every scenario patch, the fixture tree, and the five sample results")
    for key, expected in sorted(digests.items()):
        if key.startswith("sample-"):
            continue
        path = require(Path(key), lambda item: not item.is_absolute() and ".." not in item.parts, f"baseline digest path is not repository-relative: {key}")
        if key.endswith("/"):
            actual = digest_tree(ROOT / key.rstrip("/"))
        else:
            full = ROOT / path
            if not full.is_file():
                fail(f"baseline digest path is unavailable: {key}")
            actual = digest_file(full)
        if actual != expected:
            fail(f"current {key} digest does not match the baseline; the corpus drifted since calibration")

    # Comparable identity: semantic workload identity, runner class, and
    # measurement cache state/policy must all match the baseline exactly.
    first_identity = result_semantic_identity(results[0], resolved_results[0])
    if first_identity != baseline["semantic_identity"]:
        fail(f"{resolved_results[0]}: semantic identity mismatch with the baseline")
    for path, result in zip(resolved_results[1:], results[1:]):
        if result_semantic_identity(result, path) != baseline["semantic_identity"]:
            fail(f"{path}: semantic identity mismatch with the baseline")
    for path, result in zip(resolved_results, results):
        if result_runner_class(result) != baseline["runner_class"]:
            fail(f"{path}: runner class mismatch with the baseline")

    warnings = []
    for path, result in zip(resolved_results, results):
        provenance = result["provenance"]
        for key, expected in baseline["execution_provenance"].items():
            if provenance.get(key) != expected:
                warnings.append(f"{path.name}: volatile execution provenance drift: {key} is {provenance.get(key)!r}, baseline recorded {expected!r}")

    workload_results = {}
    regressions = []
    sample_exceedances = []
    for index, name in enumerate(expected_names):
        workload_results[name] = {}
        for metric in METRICS:
            measurements = [result["workloads"][index]["timing"][metric] for result in results]
            median = median_of_three(measurements)
            stored = baseline["samples"][name][f"{metric}_median"]
            policy = FIXED_TOLERANCE_POLICY[metric]
            numerator = policy["relative_numerator"]
            denominator = policy["relative_denominator"]
            floor = policy["absolute_floor_ms"]
            cap_numerator = numerator * stored + denominator * floor
            exceeds = denominator * median > cap_numerator
            raw_over = [index for index, value in enumerate(measurements, start=1) if denominator * value > cap_numerator]
            workload_results[name][metric] = {
                "median_ms": median,
                "baseline_median_ms": stored,
                "absolute_floor_ms": floor,
                "cap_ms": cap_numerator / denominator,
                "ratio_to_baseline": ratio(median, stored),
                "exceeds_tolerance": exceeds,
                "raw_samples_over_cap": raw_over,
            }
            if exceeds:
                regressions.append({"workload": name, "metric": metric, "median_ms": median, "baseline_median_ms": stored, "cap_ms": cap_numerator / denominator})
            elif raw_over:
                sample_exceedances.append({"workload": name, "metric": metric, "samples_over_cap": raw_over})

    # Observational-only signals, never timing failures: raw sample spikes
    # whose median stays under the cap, warm/cold wall-ratio drift beyond
    # 25%, and schemata minus cold-regular wall sign changes.
    for exceedance in sample_exceedances:
        warnings.append(
            f"sample-level exceedance (observational): {exceedance['workload']} {exceedance['metric']} "
            f"over the cap in {len(exceedance['samples_over_cap'])} of {SAMPLE_COUNT} raw samples; "
            "the median remains under the cap"
        )

    observed = {}
    for pair in (("warm-exact-cache", "cold-regular"), ("cold-schemata", "cold-regular")):
        if all(name in workload_results for name in pair):
            baseline_ratio = None
            baseline_numerator = baseline["samples"][pair[0]]["wall_ms_median"]
            baseline_denominator = baseline["samples"][pair[1]]["wall_ms_median"]
            if baseline_denominator > 0:
                baseline_ratio = baseline_numerator / baseline_denominator
            observed_median = workload_results[pair[0]]["wall_ms"]["median_ms"]
            cold_median = workload_results[pair[1]]["wall_ms"]["median_ms"]
            observed_ratio = ratio(observed_median, cold_median)
            observed[f"{pair[0]}-vs-{pair[1]}-wall-ratio"] = {"baseline": baseline_ratio, "observed": observed_ratio}
            if baseline_ratio is not None and observed_ratio is not None:
                drift = abs(observed_ratio - baseline_ratio)
                if drift > (RATIO_DRIFT_LIMIT_NUMERATOR / RATIO_DRIFT_LIMIT_DENOMINATOR) * baseline_ratio + RATIO_EPSILON:
                    warnings.append(f"{pair[0]}/{pair[1]} wall ratio drifted from {baseline_ratio:.3f} to {observed_ratio:.3f} (>25% drift, observational)")
    baseline_delta = None
    observed_delta = None
    sign_changed = False
    if "cold-schemata" in workload_results and "cold-regular" in workload_results:
        baseline_delta = baseline["samples"]["cold-schemata"]["wall_ms_median"] - baseline["samples"]["cold-regular"]["wall_ms_median"]
        observed_delta = workload_results["cold-schemata"]["wall_ms"]["median_ms"] - workload_results["cold-regular"]["wall_ms"]["median_ms"]
        sign_changed = (baseline_delta > 0 and observed_delta < 0) or (baseline_delta < 0 and observed_delta > 0)
    observed["schemata-vs-cold-regular-wall-delta"] = {"baseline_ms": baseline_delta, "observed_ms": observed_delta, "sign_changed": sign_changed}
    if sign_changed:
        warnings.append(f"schemata - cold-regular wall delta changed sign ({baseline_delta}ms at baseline, {observed_delta}ms now, observational)")

    # Timing rule: any workload/metric median over the cap is a hard
    # regression; sample-level spikes that do not move the median only warn.
    passed = not regressions
    summary = {
        "kind": "togi_pr_loop_comparison",
        "schema_version": 1,
        "baseline": {
            "path": str(args.baseline),
            "source_commit": baseline["source"]["commit"],
            "activation": baseline["activation"],
        },
        "workloads": workload_results,
        "observed": observed,
        "sample_exceedances": sample_exceedances,
        "regressions": regressions,
        "warnings": warnings,
        "result": "pass" if passed else "fail",
    }
    text = json.dumps(summary, indent=2, sort_keys=True)
    if args.output is not None:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(text + "\n", encoding="utf-8")
        print(text)
    else:
        print(text)

    for name in expected_names:
        for metric in METRICS:
            entry = workload_results[name][metric]
            status = "OVER" if entry["exceeds_tolerance"] else "ok"
            print(f"{name} {metric}: median {entry['median_ms']}ms vs baseline {entry['baseline_median_ms']}ms (cap {entry['cap_ms']:.1f}ms, ratio {fmt_ratio(entry['ratio_to_baseline'])}) {status}")
    for warning in warnings:
        print(f"warning: {warning}")
    if not passed:
        print(f"comparison result: FAIL ({len(regressions)} workload/metric regressions over tolerance)")
        return 1
    print("comparison result: PASS")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except ValueError as error:
        print(f"baseline comparison failed: {error}", file=sys.stderr)
        sys.exit(2)
