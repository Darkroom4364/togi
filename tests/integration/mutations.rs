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
    let root = fixture_path();

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

    // Some mutations should survive because the tests are deliberately weak
    assert!(
        report.survived > 0,
        "expected some mutations to survive due to weak tests, but all were killed"
    );

    // Specifically: a mutation of > to >= in Max (line 18) should survive
    // because the test only checks Max(3,5) — never the a > b case
    let gt_to_gte_survived = report.results.iter().any(|(m, r)| {
        m.operator.contains("gt_to_gte") && m.line == 18 && *r == MutationResult::Survived
    });
    assert!(
        gt_to_gte_survived,
        "expected > to >= mutation in Max (line 18) to survive"
    );
}
