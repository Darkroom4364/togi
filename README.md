# togi

togi (鍛 — Japanese for "sharpening"), hone your tests by finding the mutations they miss.

Fast, diff-targeted mutation testing. Multi-language. No LLM. Runs on every PR in seconds.

## What it does

togi mutates the code you changed and checks if your tests catch it. If a mutation survives (tests still pass), you have a test gap.

```
$ togi check --base HEAD~1

  ✓ KILLED  src/auth.rs:47  — binary/lt_to_lte: changed < to <=
  ✗ SURVIVED  src/handler.rs:15  — binary/eq_to_neq: changed == to !=
  ✓ KILLED  src/handler.rs:31  — removal/if_body: removed if body

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Results: 2/3 mutations killed (1 survived)
Duration: 0.84s
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

## Why

Existing mutation testing tools are either too slow for CI, locked to one language, or depend on LLM calls:

| Tool | Languages | CI-fast? | Deterministic? |
|------|-----------|----------|----------------|
| Stryker | JS/TS/.NET | Yes (with caching) | Yes |
| cargo-mutants | Rust only | Depends | Yes |
| mewt | Multi (tree-sitter) | Explicitly no | Yes |
| mutahunter | Multi (LLM) | No | No |
| **togi** | **Multi (tree-sitter)** | **Yes** | **Yes** |

togi is fast because it only mutates the diff — 5-15 targeted mutations instead of thousands — and runs them in parallel.

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

# Mutate all supported files (not just the diff)
togi check --all

# Adjust parallelism and timeout
togi check --jobs 8 --timeout 60

# Generate a config file
togi init
```

## Configuration

Optional. togi works with zero config — it auto-detects your language and test command.

Create a `togi.toml` for customization:

```toml
[test]
command = ["go", "test", "./..."]
timeout = 30
jobs = 4

[diff]
base = "origin/main"

[mutations]
max_per_run = 20
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

| Language | Extension | Status |
|----------|-----------|--------|
| Go | `.go` | Supported |
| Rust | `.rs` | Supported |
| Python | `.py` | Supported |
| TypeScript | `.ts/.tsx` | Supported |

Adding a language is ~50 lines of tree-sitter node mappings.

## Mutation operators

togi applies 14 targeted mutation operators:

| Category | Mutations |
|----------|-----------|
| Binary | `<` to `<=`, `>` to `>=`, `==` to `!=`, `&&` to `\|\|`, `\|\|` to `&&` |
| Literal | `true` to `false`, `false` to `true`, `0` to `1` |
| Boundary | `+` to `-`, `-` to `+` |
| Removal | Remove if body, remove else branch |
| Return | Replace return value with default |
| Negate | Negate condition expression |

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
      - uses: Darkroom4364/togi@v1
        with:
          base: origin/main
```

## License

Apache-2.0
