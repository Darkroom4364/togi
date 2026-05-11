# togi

[![CI](https://github.com/Darkroom4364/togi/actions/workflows/ci.yml/badge.svg)](https://github.com/Darkroom4364/togi/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![Rust](https://img.shields.io/badge/rust-1.87%2B-orange.svg)](Cargo.toml)

togi (鍛 — Japanese for "sharpening") is a fast, diff-targeted mutation testing engine for pull requests.

It finds the test gaps hidden behind green builds: togi mutates the code you changed, runs the relevant tests, and shows the exact mutants your suite failed to kill.

## What it does

Mutation testing is usually too slow to run on every PR. togi is built for the PR loop:

- **Diff-targeted by default**: mutates changed lines instead of the whole repository
- **Multi-language**: Go, Rust, Python, TypeScript, Java, C, C++, Ruby, and C#
- **Single binary**: no service, database, or language-specific plugin stack
- **Zero config start**: auto-detects common test commands, with `togi init` for explicit config
- **CI-ready reports**: terminal, JSON, GitHub annotations, HTML, and PR comment markdown
- **Performance controls**: caching, sharding, fail-fast commands, LCOV filtering, and Go test-selection maps
- **Guardrails**: build pre-checks, baselines, operator filters, noisy-file skips, and path-safe mutation execution

If a mutation survives, your tests still pass after behavior changed. That is a concrete test gap.

```
$ togi check --base HEAD~1

  ✓ KILLED  src/auth.rs:47  - lt_to_lte: changed < to <=
  ✗ SURVIVED  src/handler.rs:15  - eq_to_neq: changed == to !=
  ✓ KILLED  src/handler.rs:31  - remove_if_body: removed if body

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Results: 2/3 mutations killed (1 survived)
Duration: 0.84s
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

## Why

Good mutation testing tools exist, but each makes trade-offs:

| Tool | Languages | Diff-targeted | Zero config | Single binary |
|------|-----------|---------------|-------------|---------------|
| Stryker | JS/TS/.NET | Incremental mode | No | No |
| cargo-mutants | Rust | `--in-diff` flag | Partial | Yes |
| mewt | Multi (tree-sitter) | No | No | Yes |
| mutahunter | Multi (LLM) | Yes | No | No |
| **togi** | **9 languages** | **By default** | **Yes** | **Yes** |

togi's differentiator: multi-language + diff-targeted by default + single binary + zero config. It mutates only changed lines, keeps reports tied to reviewable code, and has the CI mechanics needed for real repositories: cache identity, baselines, coverage filtering, sharding, GitHub annotations, and machine-readable output.

## Install

```bash
cargo install --git https://github.com/Darkroom4364/togi
```

Or build from source:

```bash
git clone https://github.com/Darkroom4364/togi
cd togi
cargo build --release
```

## Usage

```bash
# Run mutation testing on your current changes vs main
togi check

# Diff against a specific branch or commit
togi check --base HEAD~1
togi check --base origin/develop

# See what mutations would be generated without running tests
togi check --dry-run

# JSON output for CI
togi check --format json

# Explain a mutation from a JSON report
togi check --format json > togi-report.json
togi explain 1 --report togi-report.json

# GitHub annotations or HTML report
togi check --format github
togi check --format html

# Mutate all supported files (not just the diff)
togi check --all

# Adjust parallelism and timeout
togi check --jobs 8 --timeout 60

# Cap mutation count for a bounded exploratory run
togi check --max-per-run 50  # --max-per-run 0 = unlimited

# Scope to a directory
togi check --all --path src/rules/

# Exclude noisy operators
togi check --operators=-string_to_empty

# Include only specific categories
togi check --operators=binary,removal

# Stop on first test failure per mutation
togi check --fail-fast

# Use a build check before running tests
togi check --build-cmd "cargo check"

# Only mutate lines covered by an LCOV file
togi check --coverage-file coverage/lcov.info

# Generate and use a Go source-line to test-name map
togi test-map --path . --output coverage/test-selection.json
togi check --test-selection-file coverage/test-selection.json

# Fail below a mutation score threshold
togi check --fail-under 80

# Split mutation work across parallel CI jobs
togi check --shard 1/4

# Save or compare against a baseline
togi check --save-baseline
togi check --check-baseline

# Write a PR comment body to a markdown file
togi check --pr-comment togi-pr-comment.md

# Show each mutation as it runs
togi check --verbose

# List all operators and categories
togi list-operators

# Clear mutation cache
togi clean

# Generate a config file
togi init
```

Mutation cache entries include the Togi package version and an internal cache
schema version, so upgrades and operator behavior changes automatically stop
matching older `.togi-cache` entries.

## Configuration

Optional. togi works with zero config — it auto-detects your language and test command.

Run `togi init` to auto-detect your language, test runner (including pnpm/yarn/bun), and generate a `togi.toml`. For polyglot repos it creates per-language sections automatically.

Create a `togi.toml` for customization:

```toml
[test]
command = ["go", "test", "./..."]
timeout = 30
jobs = 4
build_command = ["go", "build", "./..."]

[test.languages.python]
command = ["pytest"]
timeout = 45

[projects.api]
path = "services/api"

[projects.api.test]
command = ["go", "test", "./services/api/..."]
timeout = 60

[diff]
base = "origin/main"

[mutations]
max_per_run = 20
max_per_file = 20
coverage_file = "coverage/lcov.info"
test_selection_file = "coverage/test-selection.json"
operators = ["-string_to_empty"]
exclude_paths = ["vendor/**"]
skip_noisy_files = true
respect_workspace_ignores = true
```

Test command precedence is: CLI `--test-cmd` / `--timeout`, matching
`[projects.*.test]` by longest path prefix, matching `[test.languages.*]`,
then the global `[test]` command.
When `--test-cmd` is set, `--fail-fast` does not modify the custom command;
include runner-specific fail-fast flags in `--test-cmd` instead.

## CI workflows

Use togi as a PR gate, a non-blocking annotation job, or a scheduled deeper scan.

### Fast PR gate

```bash
togi check --base origin/main --fail-under 80
```

### GitHub annotations

```bash
togi check --base origin/main --format github
```

### PR comment body

```bash
togi check --base origin/main --pr-comment togi-pr-comment.md
```

### Parallel CI shards

```bash
togi check --base origin/main --shard 1/4
togi check --base origin/main --shard 2/4
togi check --base origin/main --shard 3/4
togi check --base origin/main --shard 4/4
```

### Regression baseline

```bash
togi check --save-baseline
togi check --check-baseline
```

Baselines let existing weak spots stay visible without blocking every PR. New regressions still fail the run.

## Coverage and test selection

For large repos, togi can avoid work before the runner starts:

- `--coverage-file coverage/lcov.info` keeps only mutations on covered lines
- `togi test-map` generates a Go line-to-test map from per-test coverage
- `--test-selection-file coverage/test-selection.json` narrows each Go mutant to the tests that cover that line

```bash
togi test-map --path . --output coverage/test-selection.json
togi check --coverage-file coverage/lcov.info --test-selection-file coverage/test-selection.json
```

## Example: finding real test gaps

The repo includes a Go fixture (`tests/fixtures/go/`) with deliberately weak tests.
Running togi against it produces:

```
  ✓ KILLED  calc.go:5   — plus_to_minus: Replace + with -
  ✓ KILLED  calc.go:10  — zero_to_one: Replace 0 with 1
  ✓ KILLED  calc.go:11  — true_to_false: Replace true with false
  ✗ SURVIVED  calc.go:13  — false_to_true: Replace false with true
              Your tests don't catch this mutation.
  ✗ SURVIVED  calc.go:18  — gt_to_gte: Replace > with >=
              Your tests don't catch this mutation.
  ✗ SURVIVED  calc.go:19  — return_empty: Replace return value with default
              Your tests don't catch this mutation.
  ✓ KILLED  calc.go:21  — return_empty: Replace return value with default
  ✗ SURVIVED  calc.go:26  — zero_to_one: Replace 0 with 1
              Your tests don't catch this mutation.
  ✗ SURVIVED  calc.go:29  — return_empty: Replace return value with default
              Your tests don't catch this mutation.

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Results: 4/9 mutations killed (5 survived)
Duration: 1.59s
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

Each surviving mutation reveals a concrete test gap:

- **`false_to_true` at line 13** — `TestIsPositive` never checks `IsPositive(0)` or negative inputs
- **`gt_to_gte` at line 18** — `TestMax` only tests `Max(3,5)`, never the `a > b` path
- **`return_empty` at line 19** — same: `Max` return value never verified for first-arg-wins
- **`zero_to_one` at line 26** — `TestAbs` is entirely missing
- **`return_empty` at line 29** — `Abs` return value never tested

Run it yourself: `cargo test -- --ignored` (requires Go).

## Supported languages

| Language | Extensions |
|----------|------------|
| Go | `.go` |
| Rust | `.rs` |
| Python | `.py` |
| TypeScript | `.ts`, `.tsx` |
| Java | `.java` |
| C | `.c`, `.h` |
| C++ | `.cpp`, `.cc`, `.cxx`, `.hpp`, `.hxx` |
| Ruby | `.rb` |
| C# | `.cs` |

Adding a language is ~5-10 lines via the `define_language!` macro.

## Mutation operators

togi applies 26 targeted mutation operators:

| Category (--operators name) | Mutations |
|-----------------------------|-----------|
| `binary` | `<` to `<=`, `>` to `>=`, `==` to `!=`, `&&` to `\|\|`, `\|\|` to `&&`, `*` to `/`, `/` to `*`, `%` to `*` |
| `literal` | `true` to `false`, `false` to `true`, `0` to `1`, string to `""`, increment numeric, decrement numeric |
| `boundary` | `+` to `-`, `-` to `+` |
| `removal` | Remove if body, remove else branch, remove call statement, remove assignment |
| `unary` | Remove `!`, remove unary `-` |
| `loop` | Remove `break`, remove `continue` |
| `return` | Replace return value with default |
| `negate` | Negate condition expression |

Run `togi list-operators` to see the exact operator IDs accepted by `--operators`.

## Explaining mutations

Use JSON output as the handoff format for `togi explain`:

```bash
togi check --format json > togi-report.json
togi explain 1 --report togi-report.json
```

The explanation includes the mutation location, operator, result, before/after values, diff when available, and the recorded test/build command context.

## Baselines

Save a passing mutation baseline and fail later runs only when the score regresses:

```bash
togi check --save-baseline
togi check --check-baseline
```

Baselines are stored in `.togi-baseline`.

## Workspace copies

togi runs mutations in temporary workspace copies so parallel jobs do not observe
each other's edits. These copies respect project ignore rules from `.ignore` and
`.gitignore`, while always excluding VCS metadata, togi internals, and common
dependency/build directories such as `node_modules`, `.venv`, `dist`, `build`,
and `target`. Set `[mutations] respect_workspace_ignores = false` only when a
test command genuinely needs ignored files copied into the mutation workspace.

## How it works

1. Parse `git diff` to find changed lines
2. Parse changed files with [tree-sitter](https://tree-sitter.github.io/)
3. Map changed lines to AST nodes
4. Apply targeted mutations to those nodes only
5. Run your test suite against each mutant in parallel
6. Report which mutations survived

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | All mutations killed — tests are solid |
| 1 | Some mutations survived — test gaps found |
| 2 | Error (config, git, parse failure) |

## CI Integration

Add togi to your pull request workflow:

```yaml
# .github/workflows/togi.yml
name: Mutation Testing
on: [pull_request]
jobs:
  togi:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo install --git https://github.com/Darkroom4364/togi
      - run: togi check --base origin/main --format github --fail-under 80
```

## License

MIT OR Apache-2.0
