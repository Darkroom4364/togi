# togi

Fast, diff-targeted mutation testing. Language-agnostic. No LLM. Runs on every PR in seconds.

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

## Supported languages

| Language | Extension | Status |
|----------|-----------|--------|
| Go | `.go` | Supported |
| Rust | `.rs` | Supported |
| Python | `.py` | Planned |
| TypeScript | `.ts/.tsx` | Planned |

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

## License

Apache-2.0
