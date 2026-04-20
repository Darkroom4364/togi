# Contributing to togi

## Getting started

```bash
git clone https://github.com/Darkroom4364/togi
cd togi
cargo build
cargo test
```

## Adding a new language

togi supports any language with a tree-sitter grammar. Adding one takes ~50 lines:

1. Add the grammar crate to `Cargo.toml`:
   ```toml
   tree-sitter-java = "0.23"
   ```

2. Create `src/languages/java.rs`:
   ```rust
   pub struct Java;

   impl crate::languages::LanguageSupport for Java {
       fn name(&self) -> &str { "java" }
       fn extensions(&self) -> &[&str] { &["java"] }
       fn tree_sitter_language(&self) -> tree_sitter::Language {
           tree_sitter_java::LANGUAGE.into()
       }
       // ... node mappings
   }
   ```

3. Register in `src/languages/mod.rs`:
   ```rust
   pub mod java;
   // and add Box::new(java::Java) to all()
   ```

4. Add test command detection in `src/config.rs`

5. Verify node kinds by parsing sample code — don't guess!

## Adding a mutation operator

1. Implement `MutationOperator` in the appropriate file under `src/operators/`
2. Register in `all_operators()` in `src/operators/mod.rs`
3. Add unit tests using tree-sitter to parse sample code

## Pull requests

- One concern per PR
- `cargo test` must pass
- `cargo clippy -- -D warnings` must pass
- `cargo fmt` must pass
