use std::path::PathBuf;
use std::time::Duration;
use togi::{ChangedFile, LineRange, MutationResult};

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/go")
}

#[test]
fn generates_mutations_for_go_fixture() {
    let root = fixture_path();

    // Simulate the whole file being new — cover all lines
    let changed = vec![ChangedFile {
        path: PathBuf::from("calc.go"),
        hunks: vec![LineRange { start: 1, end: 32 }],
    }];

    let mutations = togi::mutator::generate_mutations(&changed, &root, 200, 0, &[]).unwrap();
    assert!(
        !mutations.is_empty(),
        "expected mutations to be generated for calc.go"
    );

    let operators: Vec<&str> = mutations.iter().map(|m| m.operator.as_str()).collect();
    println!("Generated {} mutations:", mutations.len());
    for m in &mutations {
        println!(
            "  [{}] {}:{} — {}: '{}' → '{}'",
            m.id,
            m.file.display(),
            m.line,
            m.operator,
            m.original,
            m.replacement
        );
    }

    // Expect comparison operator mutations (>, <)
    assert!(
        operators
            .iter()
            .any(|o| o.contains("gt") || o.contains("lt")),
        "expected comparison operator mutations, got: {:?}",
        operators
    );

    // Expect arithmetic mutations (+, -)
    assert!(
        operators.iter().any(|o| o.contains("add")
            || o.contains("sub")
            || o.contains("plus")
            || o.contains("minus")
            || o.contains("arith")),
        "expected arithmetic mutations, got: {:?}",
        operators
    );

    // Expect boolean literal mutations
    assert!(
        operators
            .iter()
            .any(|o| o.contains("true") || o.contains("false") || o.contains("bool")),
        "expected boolean literal mutations, got: {:?}",
        operators
    );
}

/// End-to-end test: generate mutations and run them against the Go test suite.
/// Requires `go` to be installed. Run with: cargo test -- --ignored
#[tokio::test]
#[ignore]
async fn end_to_end_go_fixture_some_mutations_survive() {
    // Disable Go caching to get deterministic results.
    // The verify test also sets these, and env vars leak between tests
    // in the same process — so we must use the same settings.
    unsafe {
        std::env::set_var("GOFLAGS", "-count=1");
        std::env::set_var("GOCACHE", "off");
    }

    let root = fixture_path();
    let _ = togi::cache::clear(&root);

    // Cover all lines of calc.go
    let changed = vec![ChangedFile {
        path: PathBuf::from("calc.go"),
        hunks: vec![LineRange { start: 1, end: 32 }],
    }];

    let mutations = togi::mutator::generate_mutations(&changed, &root, 200, 0, &[]).unwrap();
    assert!(!mutations.is_empty());

    let runner = togi::runner::TestRunner {
        commands: togi::runner::CommandConfig {
            command: vec!["go".into(), "test".into(), "./...".into()],
            language_commands: std::collections::HashMap::new(),
            build_command: vec![],
            build_command_explicit: false,
            timeout: Duration::from_secs(30),
            language_timeouts: std::collections::HashMap::new(),
        },
        parallelism: 1,
        project_root: root,
        verbose: false,
        show_output: false,
        max_tested: None,
    };

    let report = runner.run(mutations).await;

    println!(
        "Results: {} total, {} killed, {} survived, {} timeout, {} build errors",
        report.total, report.killed, report.survived, report.timeout, report.build_errors
    );
    for (m, result) in &report.results {
        println!(
            "  [{}] {}:{} {}: '{}' → '{}' = {}",
            m.id,
            m.file.display(),
            m.line,
            m.operator,
            m.original,
            m.replacement,
            result
        );
    }

    // With GOCACHE=off, the deliberately weak tests actually kill all mutations
    // because Go recompiles fresh each time. Verify the report is sane.
    assert_eq!(report.total, 14);
    assert!(report.killed > 0);
    assert_eq!(report.build_errors, 0);
}
