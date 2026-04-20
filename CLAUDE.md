# togi

Fast, diff-targeted, language-agnostic mutation testing engine.

## Architecture

Single Rust binary. Pipeline: diff → parse → map → mutate → run → report.

- `src/diff.rs` — parse unified diff into changed file/line ranges
- `src/parser.rs` — tree-sitter language detection and AST parsing
- `src/mapper.rs` — map changed lines to mutable AST nodes
- `src/mutator.rs` — combine mapper + operators to generate mutations
- `src/runner.rs` — parallel test execution with timeouts and file guards
- `src/report/` — terminal and JSON output
- `src/operators/` — mutation operators (binary, literal, boundary, removal)
- `src/languages/` — per-language tree-sitter node mappings
- `src/config.rs` — togi.toml parsing with auto-detection
- `src/cli.rs` — clap CLI definitions

## Conventions

- Minimal diffs. One concern per commit.
- All changes via PR, never direct to main.
- `cargo test` must pass. `cargo clippy -- -D warnings` must pass.
- `cargo fmt` enforced.

## Adding a language

1. Add the tree-sitter grammar crate to Cargo.toml
2. Create `src/languages/{lang}.rs` implementing `LanguageSupport` trait (~50 lines)
3. Add to `all()` in `src/languages/mod.rs`
4. Add auto-detection in `src/config.rs` `detect_test_command()`
5. Add a test parsing sample code to verify node kinds

## Adding a mutation operator

1. Implement `MutationOperator` trait in the appropriate file under `src/operators/`
2. Add to `all_operators()` in `src/operators/mod.rs`
3. The operator checks `node.kind()` and returns `MutationCandidate` with byte range and replacement

## Test command

```bash
cargo test                    # unit + integration (skips ignored)
cargo test -- --ignored       # run end-to-end tests (requires go)
```
