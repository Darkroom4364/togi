#!/usr/bin/env python3
"""Fail-closed promoter turning a calibration artifact into a durable baseline.

Reads a downloaded PR-loop calibration artifact directory (the five measured
samples plus the collector's candidate JSON) and writes the deterministic
durable baseline consumed by compare-baseline.py and the PR-loop regression
gate. The tolerance policy is fixed (policy_version 1): the promoter never
accepts hand-authored thresholds. Promotion verifies the artifact end to end
instead of trusting it:

* every path under the artifact directory is a contained regular file
* the candidate's kind, schema, source commit, and sample digests hold
* current manifest, fixture, and patch digests match the candidate
* five distinct, valid, primed v2 result inputs back the candidate
* semantic, runner, and cache-policy identity match across the samples
* per-workload sample data is complete and no wall or reported-duration
  sample exceeds 3x its per-workload median

A durable baseline requires activation metadata (positive PR, non-empty
actor, RFC 3339 UTC) recorded from the reviewed activation flow. The
volatile Go build cache path is recorded as calibration evidence only;
it is never part of cross-run measurement identity.
"""
import argparse
import hashlib
import importlib.util
import json
import stat
import tempfile
import zipfile
from datetime import datetime
from pathlib import Path
import statistics
import sys

EXPECTED_RESULT_KIND = "togi_pr_loop_benchmark_result"
EXPECTED_RESULT_SCHEMA = 2
EXPECTED_CANDIDATE_KIND = "togi_pr_loop_calibration_candidate"
EXPECTED_CANDIDATE_SCHEMA = 2
BASELINE_KIND = "togi_pr_loop_baseline"
BASELINE_SCHEMA = 1
EXPECTED_CACHE_STATE = "primed"
EXPECTED_CACHE_POLICY = "job-private-explicit-gocache"
SAMPLE_COUNT = 5
MAX_SAMPLE_TO_MEDIAN = 3
COMPARISON_POLICY = (
    "durable regression gate: median of three primed samples per workload/metric; "
    "hard failure when any workload/metric median exceeds the fixed tolerance_policy v1 "
    "(2*M > 3*B + 2*floor); raw sample spikes that do not move the median, ratio drift, "
    "and sign drift are observational only"
)
FIXED_TOLERANCE_POLICY = {
    "policy_version": 1,
    "repetitions": 3,
    "aggregation": "median",
    "wall_ms": {"relative_numerator": 3, "relative_denominator": 2, "absolute_floor_ms": 250},
    "reported_duration_ms": {"relative_numerator": 3, "relative_denominator": 2, "absolute_floor_ms": 100},
}
ROOT = Path(__file__).resolve().parents[2]
MANIFEST_PATH = ROOT / "benchmarks/pr-loop/manifest.json"
CANDIDATE_NAME = "pr-loop-calibration-candidate.json"


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
        fail(f"invalid JSON document {path}: {error}")
    return require(value, lambda item: isinstance(item, dict), f"document {path} is not a JSON object")


def contained_regular_files(artifact_dir):
    files = {}
    for child in sorted(artifact_dir.rglob("*")):
        resolved = child.resolve()
        try:
            resolved.relative_to(artifact_dir)
        except ValueError:
            fail(f"artifact path escapes the artifact directory: {child}")
        if child.is_symlink():
            fail(f"artifact path is a symlink: {child}")
        if child.is_dir():
            continue
        if not child.is_file():
            fail(f"artifact path is not a regular file: {child}")
        files[child.relative_to(artifact_dir).as_posix()] = child
    return files


def archive_regular_files(archive_path):
    try:
        archive = zipfile.ZipFile(archive_path)
    except (OSError, zipfile.BadZipFile) as error:
        fail(f"invalid calibration artifact archive {archive_path}: {error}")
    files = {}
    with archive:
        for member in archive.infolist():
            name = member.filename
            parts = Path(name).parts
            if not name or name.startswith("/") or "\\" in name or any(part in ("", ".", "..") for part in parts):
                fail(f"unsafe archive member path: {name!r}")
            mode = member.external_attr >> 16
            if stat.S_ISLNK(mode) or (mode and not stat.S_ISREG(mode) and not stat.S_ISDIR(mode)):
                fail(f"archive member is not a regular file: {name}")
            if member.is_dir() or stat.S_ISDIR(mode):
                continue
            if name in files:
                fail(f"duplicate archive member: {name}")
            files[name] = archive.read(member)
    return files


def verify_archive(archive_path, expected_digest, artifact_files):
    require(expected_digest, lambda item: isinstance(item, str) and len(item) == 64 and all(char in "0123456789abcdef" for char in item), "GitHub artifact SHA-256 must be normalized lowercase 64-hex")
    if digest_file(archive_path) != expected_digest:
        fail("downloaded artifact archive SHA-256 does not match the supplied GitHub digest")
    archive_files = archive_regular_files(archive_path)
    if set(archive_files) != set(artifact_files):
        fail("artifact archive member set does not match the extracted artifact directory")
    for name, path in artifact_files.items():
        if archive_files[name] != path.read_bytes():
            fail(f"artifact archive member content does not match extracted file: {name}")


def recollect_candidate(candidate, sample_paths):
    collector_path = ROOT / "benchmarks/pr-loop/collect-calibration.py"
    spec = importlib.util.spec_from_file_location("_togi_collect_calibration", collector_path)
    if spec is None or spec.loader is None:
        fail("checkout collector could not be loaded")
    collector = importlib.util.module_from_spec(spec)
    try:
        spec.loader.exec_module(collector)
    except (ImportError, OSError) as error:
        fail(f"checkout collector could not be imported: {error}")
    with tempfile.TemporaryDirectory(prefix="togi-pr-loop-recollect-") as temporary:
        output = Path(temporary) / "candidate.json"
        source = candidate["source"]
        arguments = [
            "--output", str(output),
            "--source-commit", source["commit"],
            "--source-run", source["run"],
            "--source-attempt", str(source["attempt"]),
            "--source-utc", source["utc"],
            *map(str, sample_paths),
        ]
        try:
            collector.main(arguments)
        except ValueError as error:
            fail(f"canonical candidate re-collection failed: {error}")
        return load_json(output)


def validate_rfc3339_utc(value, message):
    require(value, nonempty_string, message)
    try:
        datetime.strptime(value, "%Y-%m-%dT%H:%M:%SZ")
    except ValueError:
        fail(message)
    return value


def validate_result(path, expected_workload_names):
    result = load_json(path)
    require(result.get("kind"), lambda item: item == EXPECTED_RESULT_KIND, f"{path}: wrong result kind")
    require(result.get("schema_version"), lambda item: item == EXPECTED_RESULT_SCHEMA, f"{path}: wrong result schema")
    require(result.get("ok"), lambda item: item is True, f"{path}: result is not ok")
    require(result.get("failures"), lambda item: item == [], f"{path}: result has failures")
    provenance = require(result.get("provenance"), lambda item: isinstance(item, dict), f"{path}: missing provenance")
    require(provenance.get("go_build_cache_state"), lambda item: item == EXPECTED_CACHE_STATE, f"{path}: go build cache state must be primed")
    require(provenance.get("go_build_cache_policy"), lambda item: item == EXPECTED_CACHE_POLICY, f"{path}: go build cache policy must be {EXPECTED_CACHE_POLICY}")
    require(provenance.get("go_build_cache_path"), nonempty_string, f"{path}: invalid go_build_cache_path")
    workloads = require(result.get("workloads"), lambda item: isinstance(item, list), f"{path}: missing workloads")
    names = [item.get("name") if isinstance(item, dict) else None for item in workloads]
    require(names, lambda item: item == expected_workload_names, f"{path}: workload order mismatch")
    wall = {}
    for workload in workloads:
        name = workload.get("name")
        require(workload.get("ok"), lambda item: item is True, f"{path}: workload {name} is not ok")
        timing = require(workload.get("timing"), lambda item: isinstance(item, dict), f"{path}: workload {name} lacks timing")
        for key in ("wall_ms", "reported_duration_ms"):
            require(timing.get(key), lambda item: exact_int(item) and item >= 0, f"{path}: workload {name} has invalid {key}")
        wall[name] = (timing["wall_ms"], timing["reported_duration_ms"])
    return result, wall


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifact-dir", required=True, type=Path, help="downloaded calibration artifact directory")
    parser.add_argument("--artifact-archive", required=True, type=Path, help="downloaded GitHub artifact ZIP archive")
    parser.add_argument("--github-artifact-id", required=True, type=int, help="positive GitHub artifact id")
    parser.add_argument("--github-artifact-sha256", required=True, help="normalized SHA-256 of the downloaded GitHub artifact ZIP")
    parser.add_argument("--output", required=True, type=Path, help="new baseline JSON path")
    parser.add_argument("--expected-source-commit", required=True, help="source commit the artifact must calibrate")
    parser.add_argument("--activation-pr", type=int, required=True, help="positive activation PR number from the reviewed activation flow")
    parser.add_argument("--activation-actor", required=True, help="non-empty activation actor from the reviewed activation flow")
    parser.add_argument("--activation-utc", required=True, help="activation time (RFC 3339 UTC) from the reviewed activation flow")
    parser.add_argument("--overwrite", action="store_true", help="replace an existing output document")
    args = parser.parse_args(argv)

    require(args.expected_source_commit, nonempty_string, "expected source commit is required")
    require(args.github_artifact_id, lambda item: exact_int(item) and item > 0, "GitHub artifact id must be a positive integer")
    archive_path = args.artifact_archive.resolve()
    if not archive_path.is_file():
        fail(f"artifact archive is unavailable: {args.artifact_archive}")
    artifact_dir = args.artifact_dir.resolve()
    if not artifact_dir.is_dir():
        fail(f"artifact directory is unavailable: {args.artifact_dir}")
    files = contained_regular_files(artifact_dir)
    verify_archive(archive_path, args.github_artifact_sha256, files)
    candidate_path = files.get(CANDIDATE_NAME)
    if candidate_path is None:
        fail(f"artifact directory lacks {CANDIDATE_NAME}")
    candidate = load_json(candidate_path)
    require(candidate.get("kind"), lambda item: item == EXPECTED_CANDIDATE_KIND, "candidate kind mismatch")
    require(candidate.get("schema_version"), lambda item: item == EXPECTED_CANDIDATE_SCHEMA, "candidate schema mismatch")
    source = require(candidate.get("source"), lambda item: isinstance(item, dict), "candidate source is invalid")
    require(source.get("commit"), lambda item: item == args.expected_source_commit, "candidate source commit does not match the expected source commit")
    require(source.get("run"), nonempty_string, "candidate source run is invalid")
    require(source.get("attempt"), lambda item: exact_int(item) and item >= 1, "candidate source attempt is invalid")
    validate_rfc3339_utc(source.get("utc"), "candidate source UTC must be RFC 3339 UTC (YYYY-MM-DDTHH:MM:SSZ)")

    digests = require(candidate.get("source_file_digests"), lambda item: isinstance(item, dict), "candidate source_file_digests is invalid")
    sample_keys = sorted(key for key in digests if key.startswith("sample-"))
    require(sample_keys, lambda item: len(item) == SAMPLE_COUNT and item == [f"sample-{index}/pr-loop-benchmark-result.json" for index in range(1, SAMPLE_COUNT + 1)], "candidate does not name exactly five sample results")
    for key in sample_keys:
        require(digests[key], lambda item: isinstance(item, str) and len(item) == 64 and all(char in "0123456789abcdef" for char in item), f"candidate digest for {key} is invalid")
        path = files.get(key)
        if path is None:
            fail(f"artifact directory lacks {key}")
        if digest_file(path) != digests[key]:
            fail(f"artifact digest mismatch for {key}")
    if len({digests[key] for key in sample_keys}) != SAMPLE_COUNT:
        fail("candidate sample digests are not five distinct values")
    canonical_candidate = recollect_candidate(candidate, [files[key] for key in sample_keys])
    if candidate != canonical_candidate:
        fail("candidate does not exactly match canonical re-collection from artifact samples")

    manifest = load_json(MANIFEST_PATH)
    require(manifest.get("schema_version"), lambda item: item == EXPECTED_RESULT_SCHEMA, "current manifest schema is not v2")
    scenarios = require(manifest.get("scenarios"), lambda item: isinstance(item, list) and item, "current manifest scenarios are invalid")
    patch_files = []
    for entry in scenarios:
        patch_file = require(entry.get("patch_file") if isinstance(entry, dict) else None, nonempty_string, "current manifest scenario patch file is invalid")
        if patch_file not in patch_files:
            patch_files.append(patch_file)
    fixture = require(manifest.get("fixture"), lambda item: isinstance(item, dict), "current manifest fixture is invalid")
    fixture_dir = require(fixture.get("source_dir"), nonempty_string, "current manifest fixture source dir is invalid")
    current_digests = {"benchmarks/pr-loop/manifest.json": digest_file(MANIFEST_PATH)}
    for patch_file in patch_files:
        current_digests[patch_file] = digest_file(ROOT / patch_file)
    current_digests[f"{fixture_dir.rstrip('/')}/"] = digest_tree(ROOT / fixture_dir)
    for key, digest in current_digests.items():
        if digests.get(key) != digest:
            fail(f"current {key} digest does not match the candidate; recalibrate against this checkout")

    semantic_identity = require(candidate.get("semantic_identity"), lambda item: isinstance(item, dict), "candidate semantic identity is invalid")
    require(
        semantic_identity.get("manifest"),
        lambda item: item == {"name": "togi-pr-loop-benchmarks", "schema_version": 2, "path": "benchmarks/pr-loop/manifest.json"},
        "candidate semantic identity is not v2",
    )
    identity_workloads = require(semantic_identity.get("workloads"), lambda item: isinstance(item, list) and item, "candidate semantic identity lacks workloads")
    workload_names = []
    for item in identity_workloads:
        name = item.get("name") if isinstance(item, dict) else None
        require(name, nonempty_string, "candidate semantic identity has an invalid workload name")
        workload_names.append(name)
    runner_class = require(candidate.get("runner_class"), lambda item: isinstance(item, dict), "candidate runner class is invalid")
    for key in ("runner_label", "os", "arch"):
        require(runner_class.get(key), nonempty_string, f"candidate runner class has invalid {key}")
    require(runner_class.get("logical_cpu_count"), lambda item: exact_int(item) and item > 0, "candidate runner class has invalid logical_cpu_count")
    measurement_identity = require(
        candidate.get("measurement_identity"),
        lambda item: item == {"go_build_cache_state": EXPECTED_CACHE_STATE, "go_build_cache_policy": EXPECTED_CACHE_POLICY},
        "candidate measurement identity must be primed with the job-private explicit GOCACHE policy",
    )

    results = []
    walls = []
    for key in sample_keys:
        result, wall = validate_result(files[key], workload_names)
        results.append(result)
        walls.append(wall)
    cache_path = results[0]["provenance"]["go_build_cache_path"]
    for result in results[1:]:
        if result["provenance"]["go_build_cache_path"] != cache_path:
            fail("go build cache path differs across samples")

    candidate_samples = require(candidate.get("samples"), lambda item: isinstance(item, dict), "candidate samples are invalid")
    require(sorted(candidate_samples), lambda item: item == sorted(workload_names), "candidate samples do not cover every workload")
    samples = {}
    for name in workload_names:
        wall = [entry[name][0] for entry in walls]
        duration = [entry[name][1] for entry in walls]
        median = statistics.median(wall)
        duration_median = statistics.median(duration)
        recomputed = {
            "wall_ms": wall,
            "duration_ms": duration,
            "wall_ms_median": median,
            "wall_ms_mad": statistics.median([abs(value - median) for value in wall]),
        }
        if candidate_samples.get(name) != recomputed:
            fail(f"candidate samples for workload {name} do not match the artifact results")
        if median <= 0:
            fail(f"workload {name} has a non-positive wall median")
        if duration_median <= 0:
            fail(f"workload {name} has a non-positive reported-duration median")
        for value in wall:
            if value > MAX_SAMPLE_TO_MEDIAN * median:
                fail(f"workload {name} has a wall sample above {MAX_SAMPLE_TO_MEDIAN}x its median")
        for value in duration:
            if value > MAX_SAMPLE_TO_MEDIAN * duration_median:
                fail(f"workload {name} has a reported-duration sample above {MAX_SAMPLE_TO_MEDIAN}x its median")
        samples[name] = {
            "wall_ms": wall,
            "reported_duration_ms": duration,
            "wall_ms_median": median,
            "reported_duration_ms_median": duration_median,
        }

    require(args.activation_pr, lambda item: exact_int(item) and item >= 1, "activation PR must be a positive integer")
    require(args.activation_actor, nonempty_string, "activation actor must be non-empty")
    validate_rfc3339_utc(args.activation_utc, "activation UTC must be RFC 3339 UTC (YYYY-MM-DDTHH:MM:SSZ)")

    baseline = {
        "kind": BASELINE_KIND,
        "schema_version": BASELINE_SCHEMA,
        "status": "durable",
        "comparison_policy": COMPARISON_POLICY,
        "tolerance_policy": FIXED_TOLERANCE_POLICY,
        "activation": {
            "pr": args.activation_pr,
            "actor": args.activation_actor,
            "utc": args.activation_utc,
        },
        "source": {
            **source,
            "github_artifact_id": args.github_artifact_id,
            "github_artifact_sha256": args.github_artifact_sha256,
        },
        "runner_class": runner_class,
        "execution_provenance": require(candidate.get("execution_provenance"), lambda item: isinstance(item, dict), "candidate execution provenance is invalid"),
        "measurement_identity": measurement_identity,
        "semantic_identity": semantic_identity,
        "samples": samples,
        "calibration_evidence": {
            "note": "go_build_cache_path is volatile per-run evidence, never cross-run measurement identity",
            "go_build_cache_path": cache_path,
            "runner_diagnostics": require(candidate.get("runner_diagnostics"), lambda item: isinstance(item, list) and len(item) == SAMPLE_COUNT, "candidate runner diagnostics are invalid"),
        },
        "source_file_digests": digests,
    }
    if args.output.exists() and not args.overwrite:
        fail(f"output already exists: {args.output} (pass --overwrite to replace it)")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(baseline, indent=2, sort_keys=True) + "\n", encoding="utf-8")

if __name__ == "__main__":
    try:
        main(sys.argv[1:])
    except ValueError as error:
        print(f"baseline promotion failed: {error}", file=sys.stderr)
        sys.exit(2)
