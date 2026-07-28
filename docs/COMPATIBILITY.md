# Compatibility Contract

This document is the authoritative compatibility contract for togi. It defines
supported languages and test runners, operating system and architecture coverage,
the Minimum Supported Rust Version (MSRV), and the stability guarantees for
versioned JSON reports and CLI flags.

Every supported tier maps to an actual CI leg that exercises it. Nothing here
is aspirational or forward-looking — if it is not verified in CI today, it is
not claimed as supported.

**This contract is versioned.** When behavior changes (new language support,
OS/arch additions, MSRV bumps, stability-policy updates), this document must be
updated in the same pull request that changes the behavior.

---

## Guarantee Tiers

| Tier | Meaning |
|------|---------|
| **Tier 1 — CI-verified** | End-to-end mutation testing exercises this configuration in CI. Regressions block the PR. |
| **Tier 2 — Build-verified** | The binary compiles and passes its unit test suite in CI, but end-to-end language mutation tests do not run on this platform. |
| **Not supported** | No CI coverage. May work incidentally but is not guaranteed. |

---

## Language and Test-Runner Support

Nine languages are supported. All nine have end-to-end integration tests that
run the full mutation→test-command→outcome pipeline on **Linux x86_64** (Tier 1).

Auto-detected test commands are listed below. Any test command can be overridden
via `togi.toml` or `--test-cmd`; the auto-detected commands are defaults only.

| Language | Extensions | Auto-detected test commands | Schemata | CI leg |
|----------|------------|-----------------------------|----------|--------|
| Go | `.go` | `go test ./...` | Yes | Integration Tests |
| Rust | `.rs` | `cargo test` | Yes | Integration Tests, Dogfood |
| Python | `.py` | `pytest` | No | Integration Tests |
| TypeScript | `.ts`, `.tsx` | `npm test`, `pnpm test`, `yarn test`, `bun test` | No | Integration Tests |
| Java | `.java` | `mvn test`, `./gradlew test` | Yes | Integration Tests |
| C | `.c`, `.h` | `ctest` | Yes | Integration Tests |
| C++ | `.cpp`, `.cc`, `.cxx`, `.hpp`, `.hxx` | `ctest` | Yes | Integration Tests |
| Ruby | `.rb` | `bundle exec rspec` | No | Integration Tests |
| C# | `.cs` | `dotnet test` | No | Integration Tests |

**Schemata** (the `TOGI_MUTANT` batch-execution mode) are supported for Go,
Rust, Java, C, and C++. For unsupported languages or operators, togi
automatically falls back to one-mutant-at-a-time execution. Use `--no-schemata`
to force the regular runner for all mutations.

### CI evidence

- **Integration Tests** ([`ci.yml`](../.github/workflows/ci.yml) `integration`
  job, `ubuntu-latest`): runs `cargo test --locked -- --ignored`, which
  executes the end-to-end fixture for every language listed above. The job
  provisions Go (stable), Node.js 24.14.1, and .NET 8.0.x; Python, Ruby, Java,
  C, and C++ toolchains are provided by the standard Ubuntu runner image.
- **Dogfood** ([`ci.yml`](../.github/workflows/ci.yml) `dogfood` job,
  `ubuntu-latest`): runs togi on togi itself, exercising Rust end-to-end on
  every push to `main` and every PR.

### Auto-detection fallback

When no language-specific marker file is found, togi falls back to `make test`.
This is a best-effort default; projects without a `Makefile` that defines a
`test` target should configure an explicit test command via `togi init` or
`togi.toml`.

---

## Operating System and Architecture Matrix

| OS | Arch | Tier | CI leg | Notes |
|----|------|------|--------|-------|
| Linux | x86_64 | Tier 1 | Build & Test, MSRV, Integration Tests, Dogfood | Primary development target. All features including replay are supported. |
| macOS | arm64 | Tier 2 | Build & Test | Binary compiles and passes unit tests. End-to-end language mutation tests are not run on macOS in CI. |
| Windows | x86_64 | Tier 2 | Build & Test | Binary compiles and passes unit tests. **Replay is file-only on Windows when its temp root is a normal path on the Windows system volume; directory overlays stay fail-closed** — see below. |

macOS CI runs on **arm64** (CI's `macos-latest` runners are arm64), which is
the macOS arm64 Tier 2 row above. **ARM64 (aarch64) on Linux and Windows is not
covered by CI and is not claimed as supported.** It may work incidentally but
is not guaranteed.

### Windows replay: file-only, directory overlays fail-closed

`togi replay` supports **file-only replay** on Windows. Disposable-workspace
population, overlay application, guarded mutation, and restoration run through
capability-bounded, no-follow filesystem operations beneath pinned parent
directory handles. Source validation is path-based; Git clone/checkout and user
commands use the pinned workspace path, whose system-volume, temp-root, outer,
and clone components remain held. File and symlink overlay removals are
supported; junction leaves and mid-path components are removed as reparse points
and never traversed, so outside-target contents stay intact.

Replay accepts a temp root only when it is a normal absolute `X:\...` path on
the Windows system volume. UNC, verbatim, mapped, and `SUBST` drive roots fail
closed before temporary-workspace or Git setup. This assumes a normal
non-administrator process: the shared Windows system-volume boot-drive mapping
is the OS-owned anchor. Administrator, LocalSystem, and mount-manager control
are outside this replay path's threat boundary.

Any overlay that would require removing a directory — a directory leaf
removal, a directory↔file type change, or a removal whose source ancestor is
missing or non-directory — remains **fail-closed before any mutation or test
command spawns**. Race-free directory removal is unavailable under
`cap-primitives` 4.0.2 on Windows (it drops the directory handle, then
path-deletes), so replay aborts workspace setup with a diagnostic naming the
requirement:

```text
replay cannot remove directory <name> on Windows: safe disposable workspace
setup requires race-free directory removal; only file and symlink overlay
removals are supported
```

Implemented in [#449](https://github.com/Darkroom4364/togi/issues/449).
Beyond replay, the Windows guarantee is the Tier 2 scope in the matrix above:
the binary compiles and the non-ignored unit test suite passes in the
Build & Test `windows-latest` leg; the end-to-end language mutation fixtures
(Integration Tests leg) are not exercised on Windows in CI.

### CI evidence

- **Build & Test** ([`ci.yml`](../.github/workflows/ci.yml) `check` job,
  matrix: `ubuntu-latest`, `macos-latest`, `windows-latest`): runs
  `cargo build --locked` and `cargo test --locked` on every push to `main` and
  every PR.

---

## Minimum Supported Rust Version (MSRV)

togi's MSRV is **Rust 1.87**, as declared in [`Cargo.toml`](../Cargo.toml)
(`rust-version = "1.87"`) and tested in CI.

### Policy

- The MSRV is the minimum Rust version that togi is guaranteed to compile and
  pass its test suite with.
- The MSRV is tested in the dedicated **MSRV** CI job
  ([`ci.yml`](../.github/workflows/ci.yml) `msrv` job, `ubuntu-latest`,
  toolchain `1.87`), which runs `cargo test --locked`.
- MSRV bumps are a breaking-change decision and must be documented in the
  release notes and in this contract.

---

## JSON Report Schema Stability

### Versioned envelope

togi's normal mutation-report JSON output (`--format json`) carries a
versioned envelope whose `schema_version` field gates `togi replay`
compatibility:

- **Current schema version**: `1` (`REPORT_SCHEMA_VERSION` in
  [`src/replay.rs`](../src/replay.rs))
- **Current report kind**: `"mutation_report"` (`REPORT_KIND`)
- **Generator**: `"togi/<cargo-pkg-version>"` (e.g. `"togi/<version>"`)

A versioned report envelope looks like:

```json
{
  "kind": "mutation_report",
  "schema_version": 1,
  "generator": "togi/<version>",
  "source_revision": "<full git sha>",
  "mutations": [...]
}
```

### Compatibility guarantees

- `togi replay` rejects reports whose `schema_version` does not match the
  current `REPORT_SCHEMA_VERSION`. There is no silent best-effort replay across
  schema versions.
- The cache subsystem uses its own internal schema versions
  (`CACHE_SCHEMA_VERSION = "3"`, `HISTORY_SCHEMA_VERSION = 2`) to invalidate
  stale cache and history entries on upgrade. These are internal
  implementation details and are not part of the public contract.
- Non-normal JSON documents emitted instead of a mutation report (such as
  `dry_run` and `suite_failure` output, which carry their own `kind` tag)
  omit the versioned envelope entirely.
- `schema_version` applies to the report envelope, not to individual
  mutations: a versioned mutation report can contain individual mutations
  whose replay recipe is unavailable (`replay: {"kind": "unavailable", ...}`,
  e.g. capture failure, schemata execution, or a mutation that never ran).

### Stability policy

- The `schema_version` integer is bumped when the JSON report structure changes
  in a way that would break `togi replay` or downstream consumers.
- Within a schema version, fields may be added but existing fields will not be
  removed, renamed, or have their types changed.
- Consumers that parse JSON reports SHOULD tolerate unknown fields (additive
  compatibility).
- Schema-version bumps are documented in the release notes and in this
  contract.

---

## CLI Stability

### Subcommands

togi exposes these subcommands:

| Subcommand | Stability |
|------------|-----------|
| `togi check` | Stable — the primary subcommand for mutation testing. |
| `togi init` | Stable — generates `togi.toml`. |
| `togi clean` | Stable — removes leftover mutant files. |
| `togi explain` | Stable — explains a mutation from a JSON report. |
| `togi replay` | Stable — replays a mutation from a versioned JSON report. |
| `togi list-operators` | Stable — lists available mutation operators. |
| `togi test-map` | Stable — generates a source-line to test-name map. |

### Flag stability

- Flags documented in the `--help` output and in the README are stable.
  Undocumented flags or environment variables are internal and may change
  without notice.
- Flags that accept values (e.g., `--timeout`, `--jobs`, `--fail-under`) will
  not change the type or range of accepted values within a minor version.
- New flags may be added in any release.

### Deprecation policy

There is currently no formal deprecation window. CLI flags and subcommands may
be removed in a minor version bump with notice in the release notes.

### Exit codes

| Code | Meaning |
|------|---------|
| 0 | All mutations killed. No `--fail-under` breach and no baseline regression. |
| 1 | Survivors found, `--fail-under` threshold not met, or baseline regression detected. |
| 2 | Error (configuration, git, parse failure). |
| 130 | Interrupted by signal (SIGINT). |

Exit codes are stable and will not change within a major version.

---

## Version History

| Date | Change |
|------|--------|
| 2026-07-28 | Initial compatibility contract. |
