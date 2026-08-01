# External dogfood protocol

This protocol can create reproducible evidence that a **released** Togi binary
ran one explicitly authorized, bounded mutation-testing case outside this
repository. It is not an adoption claim, a comparative performance claim, or a
recommendation about the target project. PR A adds only this protocol; it
contains no run result and no versioned case directory.

## Selection and permission boundary

A case is eligible only when its public owner has explicitly authorized that
exact no-secret, read-only run. Ownership, a public repository, or a compatible
license alone are not permission. The runner is purpose-built for the only
currently approved case and refuses any changed target, revision, base, mutation
scope, commands, release, or approval provenance:

- approval: [Mitigrid PR #125 comment 5150807894](https://github.com/Darkroom4364/Mitigrid/pull/125#issuecomment-5150807894), authored by `Darkroom4364`;
- released binary: Togi `v0.4.1`, `togi-linux-x86_64.tar.gz`, SHA-256 `6be7bf55d3c84a539cdaa4e60e5b5ef212ddb0e2575cd6b85ceae50218abce5c`;
- target: `https://github.com/Darkroom4364/Mitigrid.git` at `f5f3f57c92fdb3405b92eca7c9b6a6d3d704c1e8`, whose direct parent is `16e7c9e49f353fd7f4254276b3a7ece99c6dedf6`;
- direct-parent diff: exactly `.togi-baseline`, `crates/opencem-cli/src/commands/pack.rs`, `docs/governance/mutation-baseline-v0.1.md`, `docs/governance/public-readiness.md`, and `docs/governance/release-policy.md`; mutation scope: `crates/opencem-cli/src/commands/pack.rs`;
- preflight/test command: `cargo test --locked --workspace`; build command: `cargo check --locked --workspace`.

The workflow fetches the public approval comment and requires its id, author,
URL, and complete body to match before it clones the target. It uses no user
inputs for a target or command, no secrets, no cache reuse, and a fresh target
clone. The target must be clean before and after the run; after `togi clean`,
the runner deletes only `.togi.lock` and requires both it and `.togi-cache` to
be absent before recording final cleanliness.

## Dispatch and reproducibility

`external-dogfood.yml` is `workflow_dispatch` only. Its job runs only when
`github.ref` is the repository default branch; currently it requires
`refs/heads/main`. Dispatch reviewed `main` with `expected_workflow_sha` equal
to the full commit SHA being run. The runner requires the workflow-supplied
default-branch ref to equal `GITHUB_REF`, and the SHA to equal both `GITHUB_SHA`
and the checked-out Togi revision. A top-level `external-dogfood` concurrency
group queues rather than cancels runs. It runs only on Linux x86_64
(`ubuntu-24.04`) with a 90-minute job deadline and a 2,100-second outer
execution deadline.

The runner downloads the published archive and `checksums.txt`, requires the
archive's named manifest entry and its calculated SHA-256 to equal the fixed
release checksum, safely extracts it, and requires `togi --version` to be
exactly `togi 0.4.1`. It never builds or executes Togi from the workflow source.

Every network or expensive target phase has a declared bound recorded in both
`case.json` and `commands.txt`: each of the approval/archive/checksum downloads
uses a 15-second connect timeout and a 120-second total curl timeout; target
clone and checkout use 300 and 120 seconds; locked dependency fetch, preflight,
and dry run use 480, 600, and 300 seconds; the actual execution uses 2,100
seconds; and cleanup uses 120 seconds. The three download maxima plus those
seven bounded phases total 4,380 seconds (73 minutes), leaving 17 minutes of
the 90-minute job deadline for checkout/setup, release inspection, validation,
and upload. Target commands retain their approved arguments; the
`timeout --preserve-status` wrappers are protocol bounds.

The target uses a new `HOME` and `CARGO_HOME`. It fetches
locked dependencies once, then uses an allowlisted `env -i` environment with an
isolated home/Cargo home, installed Rust toolchain location, `PATH`,
`CARGO_NET_OFFLINE=true`, `TZ=UTC`, and `LC_ALL=C` for preflight, dry run, and
execution. It records only the allowlisted environment facts: runner image and
OS, uname/architecture, CPU count, tool versions, locale/timezone, target
lockfile and config digests, and protocol identities/limits. It does not dump
the environment, home directory, credentials, or tokens.

Before execution, the full workspace preflight must pass. Togi v0.4.1 cannot
combine its diff `--base` mode with `--path`; the runner therefore uses the
approved explicit base without `--path`. Before its dry run it proves and
records the full direct-parent diff's exact five-path boundary in
`target-changed-paths.txt`, preserves that full binary diff, and requires every
dry-run and report mutation to name the approved mutation-scope path. The other
four changed files are non-mutable to released v0.4.1: `.togi-baseline` and the
three Markdown governance files have unsupported file types, which the mutator
reports only on stderr. JSON stdout remains the single dry-run or report
document. The target's
checked-in `togi.toml` must set `max_per_run = 0`. The generated count—not a
truncation cap—must be between 1 and 20 inclusive and equal the dry-run mutation
array length. The actual run repeats the explicit base and test/build command
under `timeout --preserve-status 2100s`, with a per-mutation timeout of 120
seconds, `--jobs 2 --force-rerun --no-incremental-history`, and no
`--max-per-run`. A timeout or other nonzero status in any prior phase aborts the
run and is nonpublishable. For execution, only exit status 0 or 1 reaches
validation; a timeout status is nonpublishable, and a survivor is an observed
result, not a protocol failure.

## Evidence artifact and validation

The named workflow artifact contains:

- immutable case, approval, release verification, environment, command, full
  target diff, exact changed-path list/config, and clean-worktree metadata;
- raw stdout/stderr and status files for dependency fetch, preflight, dry run,
  and execution;
- `dry-run.json` and `report.json`, each a byte-for-byte copy of its raw stdout;
- `metrics.json`, generated deterministically from the machine-readable
  `report.json`, not transcribed into prose;
- `validation.txt` and a final `SHA256SUMS` covering every expected artifact
  except itself.

The offline verifier first validates the exact authorization, default-branch
provenance, identities, commands, full direct-parent diff boundary, complete
dry-run plan and mutation scope, report schema/count invariants, and clean/full
execution. It permits survivors but rejects timeouts, build errors, uncovered
or subsumed mutations, exact-cache/history reuse, partial or early-stopped
reports, or a non-fresh execution. `--generate` derives canonical metrics for
an initial artifact; `--verify` recomputes them and validates all checksums
without network access.

```bash
bash .github/scripts/verify-external-dogfood-evidence.sh --verify CASE_DIRECTORY
```

For a rerun, dispatch reviewed default-branch `main` again with its exact SHA,
preserve the new artifact, and run the offline verifier on both artifacts.
Compare the recorded workflow revision/ref, release digest, target/base/direct
diff boundary/mutation scope, lockfile/config digests, runner
image/OS/architecture, CPU, tool versions, locale/timezone, dry-run count, and
generated metrics before treating outcomes as comparable. Changed environment
or dependency resolution is recorded drift, not proof of a Togi
regression or improvement.

## Publication sequence

1. Review and merge PR A, then dispatch the protocol from reviewed `main`.
2. Retain the uploaded artifact only if its offline verification passes and its
   report is complete and fresh.
3. Put that evidence in a separate, evidence-only PR B. Only then may a
   reviewed document link the versioned evidence and its workflow artifact.

A protocol, a successful command, or one result cannot prove target adoption,
general performance, broad compatibility, or reproducibility on a different
runner image or dependency set.
