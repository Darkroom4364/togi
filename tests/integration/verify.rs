//! Mutation verification: replay each mutation independently and confirm
//! togi's reported outcome matches the actual test result.
//!
//! Sets GOCACHE=off because Go's build cache can return stale binaries
//! when source files change rapidly via atomic rename.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;
use togi::{ChangedFile, LineRange, MutationResult};

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/go")
}

fn classify_result(status: std::process::ExitStatus) -> MutationResult {
    if status.success() {
        MutationResult::Survived
    } else {
        MutationResult::Killed
    }
}

/// Generate mutations, run them via togi, then replay each one independently
/// and assert the outcomes match.
///
/// Requires `go` to be installed. Run with: cargo test -- --ignored
#[tokio::test]
#[ignore]
async fn verify_mutation_outcomes_match_independent_replay() {
    let root = fixture_path();
    let calc_path = root.join("calc.go");

    // Disable Go build+test caching so each mutation gets a fresh compile
    unsafe {
        std::env::set_var("GOFLAGS", "-count=1");
        std::env::set_var("GOCACHE", "off");
    }

    let changed = vec![ChangedFile {
        path: PathBuf::from("calc.go"),
        hunks: vec![LineRange { start: 1, end: 32 }],
    }];

    let mutations = togi::mutator::generate_mutations(&changed, &root, 200, 0, &[]).unwrap();
    assert!(!mutations.is_empty());

    let runner = togi::runner::TestRunner {
        command: vec!["go".into(), "test".into(), "./...".into()],
        language_commands: std::collections::HashMap::new(),
        timeout: Duration::from_secs(30),
        parallelism: 1,
        project_root: root.clone(),
        verbose: false,
        build_command: vec![],
        build_command_explicit: false,
        max_tested: None,
        show_output: false,
    };

    let report = runner.run(mutations).await;
    let original = std::fs::read(&calc_path).expect("failed to read calc.go");

    let mut verified = 0;
    let mut mismatches = Vec::new();

    for (mutation, togi_result) in &report.results {
        if *togi_result == MutationResult::BuildError || *togi_result == MutationResult::Timeout {
            continue;
        }

        let range = mutation.byte_range.clone();
        if range.end > original.len() {
            continue;
        }

        let mut mutated = original.clone();
        mutated.splice(range, mutation.replacement.as_bytes().iter().copied());
        std::fs::write(&calc_path, &mutated).expect("failed to write mutated file");

        let output = Command::new("go")
            .args(["test", "./..."])
            .current_dir(&root)
            .output()
            .expect("failed to run go test");

        let actual_result = classify_result(output.status);
        std::fs::write(&calc_path, &original).expect("failed to restore calc.go");
        verified += 1;

        if actual_result != *togi_result {
            mismatches.push(format!(
                "  [{}] {}:{} {} '{}' → '{}': togi={}, actual={}",
                mutation.id,
                mutation.file.display(),
                mutation.line,
                mutation.operator,
                mutation.original,
                mutation.replacement,
                togi_result,
                actual_result,
            ));
        }
    }

    std::fs::write(&calc_path, &original).expect("failed to restore calc.go");

    if !mismatches.is_empty() {
        panic!(
            "mutation verification failed — {}/{} mismatches:\n{}",
            mismatches.len(),
            verified,
            mismatches.join("\n")
        );
    }

    println!("verified {verified} mutations: all outcomes match independent replay");
}
