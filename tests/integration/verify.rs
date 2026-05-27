//! Mutation verification: replay each mutation independently and confirm
//! togi's reported outcome matches the actual test result.
//!
//! Uses `-count=1` with a temporary GOCACHE so Go reruns tests while keeping
//! the required build cache enabled.

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
#[test]
#[ignore]
fn verify_mutation_outcomes_match_independent_replay() {
    let _fixture_guard = crate::go_fixture_lock();
    let root = fixture_path();
    togi::cache::clear(&root).expect("failed to clear togi cache");
    let calc_path = root.join("calc.go");

    let changed = vec![ChangedFile {
        path: PathBuf::from("calc.go"),
        hunks: vec![LineRange { start: 1, end: 32 }],
    }];

    let mutations = togi::mutator::generate_mutations(&changed, &root, 200, 0, &[]).unwrap();
    assert!(!mutations.is_empty());

    // Capture pristine fixture before runner.run() touches it
    let original = std::fs::read(&calc_path).expect("failed to read calc.go");

    // Force fresh test execution while keeping Go's required build cache enabled.
    let go_cache = tempfile::tempdir().expect("failed to create temporary Go cache");
    let go_env: std::collections::HashMap<String, String> = [
        ("GOFLAGS".into(), "-count=1".into()),
        (
            "GOCACHE".into(),
            go_cache.path().to_string_lossy().into_owned(),
        ),
    ]
    .into();
    let baseline = Command::new("go")
        .args(["test", "./..."])
        .envs(&go_env)
        .current_dir(&root)
        .output()
        .expect("failed to run baseline go test");
    assert!(
        baseline.status.success(),
        "baseline go test failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&baseline.stdout),
        String::from_utf8_lossy(&baseline.stderr)
    );

    let runner = togi::runner::TestRunner {
        commands: togi::runner::CommandConfig {
            command: vec!["go".into(), "test".into(), "./...".into()],
            force_default_command: false,
            force_default_timeout: false,
            project_commands: vec![],
            language_commands: std::collections::HashMap::new(),
            build_command: vec![],
            build_command_explicit: false,
            timeout: Duration::from_secs(30),
            language_timeouts: std::collections::HashMap::new(),
            test_selection: None,
        },
        parallelism: 1,
        project_root: root.clone(),
        verbose: false,
        show_output: false,
        max_tested: None,
        early_stop: Default::default(),
        respect_workspace_ignores: true,
        env: go_env.clone(),
        incremental_history: true,
        force_rerun: false,
        cancelled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };

    togi::cache::clear(&root).expect("failed to clear togi cache before verification run");
    let report = runner.run(mutations).report;

    // Verify runner restored the file
    let after_run = std::fs::read(&calc_path).expect("failed to re-read calc.go");
    assert_eq!(
        after_run, original,
        "runner did not restore calc.go to its original contents"
    );

    let mut verified = 0;
    let mut mismatches = Vec::new();

    for (mutation, togi_result) in &report.results {
        if *togi_result == MutationResult::BuildError || *togi_result == MutationResult::Timeout {
            continue;
        }

        let range = mutation.byte_range.clone();
        assert!(
            range.start <= range.end && range.end <= original.len(),
            "mutation {} has invalid byte range {:?} for calc.go (len {})",
            mutation.id,
            range,
            original.len(),
        );
        assert_eq!(
            &original[range.clone()],
            mutation.original.as_bytes(),
            "mutation {} byte_range does not match original source bytes",
            mutation.id,
        );

        let mut mutated = original.clone();
        mutated.splice(range, mutation.replacement.as_bytes().iter().copied());
        std::fs::write(&calc_path, &mutated).expect("failed to write mutated file");

        // Replay with same cache-defeating env
        let output = Command::new("go")
            .args(["test", "./..."])
            .envs(&go_env)
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

    assert!(
        verified > 0,
        "no mutations were actually compared — all were build errors or timeouts"
    );

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
