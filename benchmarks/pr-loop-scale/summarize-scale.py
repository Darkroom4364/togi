#!/usr/bin/env python3
"""Observational PR-loop scale summary (issue #498, PR 2).

Parses exactly three normalized, primed schema-v3 scale-harness results and
emits a schema-1 scale summary: per-workload raw and median-of-three
wall/reported timings, the signed wall-minus-reported diagnostic, and the
four wall-ms ratio families as medians of paired per-sample ratios (never
ratios of medians), emitted as exact fractions.

This tool is parse-only and stdlib-only: it never shells out, never reads
the current checkout, and has no baseline, threshold, tolerance, gate, or
per-mutant attribution logic. The summary compares nothing; it is evidence
for the reader on one runner class only.

Exit codes:

* 0  summary emitted (JSON on stdout; with --output the identical bytes are
     written to a new path, created exclusively and never overwriting an
     existing file or a dangling symlink)
* 2  malformed, missing, duplicated, or incomparable input, or an output
     path that already exists
"""
import argparse
from fractions import Fraction
import hashlib
import json
import os
from pathlib import Path
import sys

EXPECTED_RESULT_KIND = "togi_pr_loop_benchmark_result"
EXPECTED_RESULT_SCHEMA = 3
EXPECTED_TIMING_POLICY = "observational-only"
EXPECTED_MANIFEST_IDENTITY = {
    "name": "togi-pr-loop-scale-benchmarks",
    "schema_version": 3,
    "path": "benchmarks/pr-loop-scale/manifest.json",
}
EXPECTED_CACHE_STATE = "primed"
EXPECTED_CACHE_POLICY = "job-private-explicit-gocache"
EXPECTED_SCENARIO = "scale-file"
EXPECTED_TOTAL = 98
EXPECTED_TEST_COMMAND = ["go", "test", "./..."]
EXPECTED_REPORT_KIND = "mutation_report"
EXPECTED_REPORT_SCHEMA = 1
SAMPLE_COUNT = 3
COMMON_ARGV_TAIL = (
    "check", "--base", "HEAD", "--timeout", "60", "--max-per-run", "500",
    "--test-cmd", "go test ./...", "--format", "json",
)
# The six telemetry workloads, in declaration order, with their runner mode,
# cache policy, exact invariant-name list, and exact command argv tail
# (everything after the binary path). Any drift in name, order, mode,
# policy, invariants, or argv is incomparable input: this summarizer is
# corpus-specific by design.
EXPECTED_WORKLOADS = (
    ("scale-regular-jobs1", "regular", "fresh", ("report-well-formed", "full-fresh-execution"),
     COMMON_ARGV_TAIL + ("--no-schemata", "--force-rerun", "--jobs", "1")),
    ("scale-warm-exact-cache", "regular", "reuse", ("report-well-formed", "full-exact-cache-reuse"),
     COMMON_ARGV_TAIL + ("--no-schemata", "--jobs", "1")),
    ("scale-regular-jobs4", "regular", "fresh", ("report-well-formed", "full-fresh-execution"),
     COMMON_ARGV_TAIL + ("--no-schemata", "--force-rerun", "--jobs", "4")),
    ("scale-schemata", "schemata", "fresh", ("report-well-formed", "schemata-fast-path-and-fallback"),
     COMMON_ARGV_TAIL + ("--schemata", "--force-rerun", "--jobs", "1")),
    ("scale-schemata-jobs4", "schemata", "fresh", ("report-well-formed", "schemata-fast-path-and-fallback"),
     COMMON_ARGV_TAIL + ("--schemata", "--force-rerun", "--jobs", "4")),
    ("scale-default", "default", "fresh", ("report-well-formed", "pr-diff-targeting"),
     COMMON_ARGV_TAIL),
)
RUNNER_CLASS_KEYS = ("runner_label", "os", "arch", "logical_cpu_count")
TOOLCHAIN_KEYS = ("go_version", "togi_version", "git_version", "kernel_release")
# Every provenance dimension that must hold for one three-sample primed
# acquisition; any drift is incomparable input, never a warning.
IDENTITY_PROVENANCE_KEYS = RUNNER_CLASS_KEYS + TOOLCHAIN_KEYS + (
    "fixture_source_dir",
    "go_build_cache_path",
)
# The four wall-ms ratio families. Each entry is (output key, numerator
# workload, denominator workload); every ratio is the median of the three
# paired per-sample ratios, never the ratio of the medians.
RATIO_FAMILIES = (
    ("regular_jobs4_over_jobs1", "scale-regular-jobs4", "scale-regular-jobs1"),
    ("schemata_over_regular_jobs1", "scale-schemata", "scale-regular-jobs1"),
    ("schemata_jobs4_over_schemata_jobs1", "scale-schemata-jobs4", "scale-schemata"),
    ("warm_over_cold", "scale-warm-exact-cache", "scale-regular-jobs1"),
)


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


def exact_integer(value, expected, message):
    """Exact JSON integer equality: floats and booleans never pass."""
    if not (exact_int(value) and value == expected):
        fail(message)


def read_input(path):
    """Read one regular input once; return (raw bytes, parsed JSON object).

    The content digest and the JSON parse both cover exactly these bytes."""
    if not path.is_file():
        fail(f"{path}: input is not a regular file")
    try:
        data = path.read_bytes()
    except OSError as error:
        fail(f"cannot read result {path}: {error}")
    try:
        value = json.loads(data.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"invalid JSON document {path}: {error}")
    require(value, lambda item: isinstance(item, dict), f"document {path} is not a JSON object")
    return data, value


def positive_int(value):
    return exact_int(value) and value > 0


def validate_workload_semantics(path, name, mode, cache_policy, entry):
    """Validate the exact scale-corpus semantics of one workload, returning
    its (wall_ms, reported_duration_ms) timing pair."""
    require(entry.get("ok"), lambda item: item is True, f"{path}: workload {name} is not ok")
    require(entry.get("exit_status"), lambda item: exact_int(item) and item in (0, 1), f"{path}: workload {name} has an unexpected exit status")
    semantics = require(entry.get("semantics"), lambda item: isinstance(item, dict), f"{path}: workload {name} lacks semantics")
    for key in ("total", "planned_total", "mutation_count"):
        exact_integer(semantics.get(key), EXPECTED_TOTAL, f"{path}: workload {name} {key} must be {EXPECTED_TOTAL}")
    require(semantics.get("selected_test_command"), lambda item: item == EXPECTED_TEST_COMMAND, f"{path}: workload {name} test command mismatch")
    selection = require(semantics.get("test_selection"), lambda item: isinstance(item, dict), f"{path}: workload {name} lacks test selection evidence")
    require(selection.get("mode"), lambda item: item == "full-suite", f"{path}: workload {name} test selection must be full-suite")
    exact_integer(selection.get("full_suite_mutation_count"), EXPECTED_TOTAL, f"{path}: workload {name} full-suite count mismatch")
    exact_integer(selection.get("narrowed_mutation_count"), 0, f"{path}: workload {name} has narrowed mutations")
    timing = require(entry.get("timing"), lambda item: isinstance(item, dict), f"{path}: workload {name} lacks timing")
    wall = require(timing.get("wall_ms"), positive_int, f"{path}: workload {name} wall_ms must be a positive integer")
    reported = require(timing.get("reported_duration_ms"), positive_int, f"{path}: workload {name} reported_duration_ms must be a positive integer")
    exact_integer(semantics.get("reported_duration_ms"), reported, f"{path}: workload {name} reported duration disagrees with its timing")
    schemata = semantics.get("schemata")
    if mode == "schemata":
        require(schemata, lambda item: isinstance(item, dict), f"{path}: workload {name} lacks schemata evidence")
        fast_path = require(schemata.get("fast_path"), positive_int, f"{path}: workload {name} must have a schemata fast path")
        fallback = require(schemata.get("fallback"), positive_int, f"{path}: workload {name} must have schemata fallback mutants")
        require(fast_path + fallback, lambda item: item == EXPECTED_TOTAL, f"{path}: workload {name} schemata split must sum to {EXPECTED_TOTAL}")
    else:
        require(schemata, lambda item: item is None, f"{path}: workload {name} must report schemata null")
    if cache_policy == "reuse":
        exact_integer(semantics.get("tested"), 0, f"{path}: workload {name} must reuse, not re-execute")
        exact_integer(semantics.get("exact_cache_reused"), EXPECTED_TOTAL, f"{path}: workload {name} must come entirely from the exact cache")
    else:
        exact_integer(semantics.get("tested"), EXPECTED_TOTAL, f"{path}: workload {name} must execute every mutation")
        exact_integer(semantics.get("exact_cache_reused"), 0, f"{path}: workload {name} must not reuse the exact cache")
        exact_integer(semantics.get("incremental_history_reused"), 0, f"{path}: workload {name} must not reuse incremental history")
    return wall, reported


def validate_result(path, result):
    """Validate one primed v3 scale result; return (workload timings, identity)."""
    exact_integer(result.get("schema_version"), EXPECTED_RESULT_SCHEMA, f"{path}: wrong result schema")
    require(result.get("kind"), lambda item: item == EXPECTED_RESULT_KIND, f"{path}: wrong result kind")
    require(result.get("timing_policy"), lambda item: item == EXPECTED_TIMING_POLICY, f"{path}: wrong timing policy")
    require(result.get("manifest"), lambda item: item == EXPECTED_MANIFEST_IDENTITY, f"{path}: manifest identity mismatch")
    manifest = result["manifest"]
    exact_integer(manifest.get("schema_version"), EXPECTED_RESULT_SCHEMA, f"{path}: manifest schema version must be an exact integer")
    require(result.get("ok"), lambda item: item is True, f"{path}: result is not ok")
    require(result.get("failures"), lambda item: item == [], f"{path}: result has failures")

    provenance = require(result.get("provenance"), lambda item: isinstance(item, dict), f"{path}: missing provenance")
    require(provenance.get("report_kind"), lambda item: item == EXPECTED_REPORT_KIND, f"{path}: invalid provenance.report_kind")
    exact_integer(provenance.get("report_schema_version"), EXPECTED_REPORT_SCHEMA, f"{path}: invalid provenance.report_schema_version")
    for key in IDENTITY_PROVENANCE_KEYS:
        require(provenance.get(key), nonempty_string if not key == "logical_cpu_count" else positive_int, f"{path}: invalid provenance.{key}")
    for key in ("image_os", "image_version"):
        require(provenance.get(key), lambda item: item is None or nonempty_string(item), f"{path}: invalid provenance.{key}")
    require(provenance.get("go_build_cache_state"), lambda item: item == EXPECTED_CACHE_STATE, f"{path}: go build cache state must be primed")
    require(provenance.get("go_build_cache_policy"), lambda item: item == EXPECTED_CACHE_POLICY, f"{path}: go build cache policy must be {EXPECTED_CACHE_POLICY}")
    require(provenance["go_build_cache_path"], lambda item: item.startswith("/"), f"{path}: go build cache path must be absolute")

    fixture_scenarios = require(provenance.get("fixture_scenarios"), lambda item: isinstance(item, dict), f"{path}: missing fixture scenario provenance")
    scale_scenario = require(fixture_scenarios.get(EXPECTED_SCENARIO), lambda item: isinstance(item, dict), f"{path}: missing {EXPECTED_SCENARIO} fixture provenance")
    patch_sha256 = require(scale_scenario.get("patch_sha256"), is_hex_digest, f"{path}: invalid {EXPECTED_SCENARIO} patch digest")

    cross_workload = require(result.get("cross_workload"), lambda item: isinstance(item, dict), f"{path}: missing cross-workload evidence")
    scenarios = require(cross_workload.get("scenarios"), lambda item: isinstance(item, dict), f"{path}: missing per-scenario identity")
    identity = require(scenarios.get(EXPECTED_SCENARIO), lambda item: isinstance(item, dict), f"{path}: missing {EXPECTED_SCENARIO} identity")
    require(identity.get("mutation_identity_consistent"), lambda item: item is True, f"{path}: {EXPECTED_SCENARIO} mutation identity is inconsistent")
    mutation_identity = require(identity.get("mutation_identity_sha256"), is_hex_digest, f"{path}: invalid {EXPECTED_SCENARIO} mutation digest")

    workloads = require(result.get("workloads"), lambda item: isinstance(item, list), f"{path}: workloads must be an array")
    require(workloads, lambda item: len(item) == len(EXPECTED_WORKLOADS), f"{path}: expected exactly {len(EXPECTED_WORKLOADS)} workloads")
    samples = {}
    for entry, (expected_name, expected_mode, expected_cache, expected_invariants, expected_tail) in zip(workloads, EXPECTED_WORKLOADS):
        require(entry, lambda item: isinstance(item, dict), f"{path}: workload entries must be objects")
        name = require(entry.get("name"), nonempty_string, f"{path}: workload lacks a name")
        require(name, lambda item: item == expected_name, f"{path}: expected workload {expected_name}, found {name}")
        require(entry.get("scenario"), lambda item: item == EXPECTED_SCENARIO, f"{path}: workload {name} scenario mismatch")
        require(entry.get("runner_mode"), lambda item: item == expected_mode, f"{path}: workload {name} runner mode mismatch")
        require(entry.get("cache_policy"), lambda item: item == expected_cache, f"{path}: workload {name} cache policy mismatch")
        invariants = require(entry.get("invariants"), lambda item: isinstance(item, list) and item, f"{path}: workload {name} lacks invariants")
        invariant_names = []
        for invariant in invariants:
            require(invariant, lambda item: isinstance(item, dict), f"{path}: workload {name} invariant entries must be objects")
            require(invariant.get("ok"), lambda item: item is True, f"{path}: workload {name} has a failed invariant")
            invariant_names.append(require(invariant.get("name"), nonempty_string, f"{path}: workload {name} invariant lacks a name"))
        require(invariant_names, lambda item: item == list(expected_invariants), f"{path}: workload {name} invariant names must be exactly {list(expected_invariants)}")
        command = require(entry.get("command"), lambda item: isinstance(item, list) and item and nonempty_string(item[0]) and all(isinstance(arg, str) for arg in item), f"{path}: workload {name} lacks a command argv")
        require(command[1:], lambda item: item == list(expected_tail), f"{path}: workload {name} command argv tail mismatch")
        semantics = require(entry.get("semantics"), lambda item: isinstance(item, dict), f"{path}: workload {name} lacks semantics")
        require(semantics.get("mutation_identity_sha256"), lambda item: item == mutation_identity, f"{path}: workload {name} mutation identity differs from the scenario identity")
        samples[name] = validate_workload_semantics(path, name, expected_mode, expected_cache, entry)

    comparable_identity = {
        "runner_class": {key: provenance[key] for key in RUNNER_CLASS_KEYS},
        "toolchain": {key: provenance[key] for key in TOOLCHAIN_KEYS},
        "corpus": {
            "fixture_source_dir": provenance["fixture_source_dir"],
            "scenario": EXPECTED_SCENARIO,
            "patch_sha256": patch_sha256,
            "mutation_identity_sha256": mutation_identity,
        },
        "provenance": {key: provenance[key] for key in IDENTITY_PROVENANCE_KEYS},
        "image": {"image_os": provenance["image_os"], "image_version": provenance["image_version"]},
    }
    return samples, comparable_identity


def median_of_three(values):
    return sorted(values)[1]


def build_summary(per_sample):
    """Assemble the schema-1 summary from three validated samples."""
    identities = [identity for _, identity in per_sample]
    first = identities[0]
    for key in ("runner_class", "toolchain", "corpus", "provenance", "image"):
        for other in identities[1:]:
            require(other[key], lambda item: item == first[key], f"samples disagree on {key}; not one primed acquisition on one runner class")

    workload_summaries = []
    for name, mode, cache_policy, _, _ in EXPECTED_WORKLOADS:
        walls = [samples[name][0] for samples, _ in per_sample]
        reported = [samples[name][1] for samples, _ in per_sample]
        median_wall = median_of_three(walls)
        median_reported = median_of_three(reported)
        workload_summaries.append({
            "name": name,
            "scenario": EXPECTED_SCENARIO,
            "runner_mode": mode,
            "cache_policy": cache_policy,
            "wall_ms": walls,
            "reported_duration_ms": reported,
            "median_wall_ms": median_wall,
            "median_reported_duration_ms": median_reported,
            "diagnostic_wall_minus_reported_ms": median_wall - median_reported,
        })

    ratios = {}
    for key, numerator, denominator in RATIO_FAMILIES:
        pairs = []
        paired = []
        for samples, _ in per_sample:
            num = samples[numerator][0]
            den = samples[denominator][0]
            pairs.append([num, den])
            paired.append(Fraction(num, den))
        median_ratio = median_of_three(paired)
        ratios[key] = {
            "metric": "wall_ms",
            "numerator_workload": numerator,
            "denominator_workload": denominator,
            "sample_pairs_ms": pairs,
            "median_fraction": {
                "numerator": median_ratio.numerator,
                "denominator": median_ratio.denominator,
            },
        }

    return {
        "kind": "togi_pr_loop_scale_summary",
        "schema_version": 1,
        "timing_policy": EXPECTED_TIMING_POLICY,
        "sample_count": SAMPLE_COUNT,
        "aggregation": {
            "workload_timings": "median_of_three",
            "ratios": "median_of_paired_sample_ratios",
            "ratio_metric": "wall_ms",
        },
        "identity": {
            "manifest": EXPECTED_MANIFEST_IDENTITY,
            "runner_class": first["runner_class"],
            "toolchain": first["toolchain"],
            "corpus": first["corpus"],
            "image": first["image"],
            "measurement": {
                "go_build_cache_state": EXPECTED_CACHE_STATE,
                "go_build_cache_policy": EXPECTED_CACHE_POLICY,
                "go_build_cache_path": first["provenance"]["go_build_cache_path"],
            },
        },
        "workloads": workload_summaries,
        "ratios": ratios,
    }


def write_output_exclusive(path, text):
    """Create path exclusively (never overwrite, never follow a dangling
    symlink) and write text atomically with respect to other creators."""
    try:
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except FileExistsError:
        fail(f"output already exists: {path}")
    except OSError as error:
        fail(f"cannot create output {path}: {error}")
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            handle.write(text)
    except OSError as error:
        fail(f"cannot write output {path}: {error}")


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=None, help="write the summary JSON to this new path (exclusive create; never overwrites)")
    parser.add_argument("results", nargs="*", type=Path, help="exactly three normalized primed v3 scale-harness results")
    args = parser.parse_args(argv)
    if len(args.results) != SAMPLE_COUNT:
        fail(f"expected exactly {SAMPLE_COUNT} results, received {len(args.results)}")

    for path in args.results:
        if path.is_symlink():
            fail(f"{path}: input must be a regular file, not a symlink")
    resolved = [path.resolve() for path in args.results]
    if len(set(resolved)) != SAMPLE_COUNT:
        fail("result paths must name three independent files")
    digests = []
    per_sample = []
    for resolved_path in resolved:
        data, result = read_input(resolved_path)
        digests.append(hashlib.sha256(data).hexdigest())
        per_sample.append(validate_result(resolved_path, result))
    if len(set(digests)) != SAMPLE_COUNT:
        fail("results must have three distinct content digests")

    summary = build_summary(per_sample)
    summary["identity"]["input_sha256"] = digests
    text = json.dumps(summary, indent=2, sort_keys=True) + "\n"
    if args.output is not None:
        write_output_exclusive(args.output, text)
    sys.stdout.write(text)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except (ValueError, OverflowError) as error:
        print(f"scale summary failed: {error}", file=sys.stderr)
        sys.exit(2)
