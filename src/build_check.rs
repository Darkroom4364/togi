// Pre-flight build check: filter out mutations that don't compile.
//
// Applying a mutation that causes a syntax or type error wastes a full
// test-suite run only to get an obvious "killed" result.  This module
// applies each mutation to the file in-place, runs a fast
// compile-check command, restores the original, and reports whether
// the mutation produces valid code.

use crate::Mutation;
use std::path::Path;

/// Check whether a mutation still compiles.
///
/// Applies the mutation's byte-range replacement to the file in-place,
/// runs `check_command` (e.g. `cargo check`), and restores the
/// original.  Returns `true` when the mutated code compiles
/// successfully and the original file is restored.
///
/// # Arguments
///
/// * `mutation`      – the mutation to verify
/// * `project_root`  – root directory of the project under test
/// * `check_command` – shell tokens for the compile-check command
///   (e.g. `["cargo", "check", "--quiet"]`)
pub fn check_builds(mutation: &Mutation, project_root: &Path, check_command: &[String]) -> bool {
    if check_command.is_empty() {
        // No check command configured – assume buildable.
        return true;
    }

    let file_path = project_root.join(&mutation.file);

    // Read the original source.
    let original = match std::fs::read(&file_path) {
        Ok(c) => c,
        Err(_) => return false,
    };

    // Apply the mutation via byte-range splice (same logic as runner).
    let range = mutation.byte_range.clone();
    if range.start > range.end || range.end > original.len() {
        return false;
    }
    let mut mutated = original.clone();
    mutated.splice(range, mutation.replacement.as_bytes().iter().copied());

    // Write the mutated content, run the check, then restore.
    if std::fs::write(&file_path, &mutated).is_err() {
        return false;
    }

    let result = run_check(check_command, project_root);

    // Restore original file – propagate failure.
    let restored = std::fs::write(&file_path, &original).is_ok();

    result && restored
}

/// Run the check command synchronously and return `true` on exit-code 0.
fn run_check(command: &[String], cwd: &Path) -> bool {
    std::process::Command::new(&command[0])
        .args(&command[1..])
        .current_dir(cwd)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Mutation;
    use std::path::PathBuf;

    fn sample_mutation(file: &Path, original: &str, replacement: &str) -> Mutation {
        let start = 0;
        let end = original.len();
        Mutation {
            id: 1,
            file: file.to_path_buf(),
            line: 1,
            column: 1,
            operator: "test".into(),
            description: "test mutation".into(),
            original: original.into(),
            replacement: replacement.into(),
            byte_range: start..end,
        }
    }

    #[test]
    fn empty_check_command_returns_true() {
        let m = sample_mutation(Path::new("dummy.rs"), "a", "b");
        assert!(check_builds(&m, Path::new("."), &[]));
    }

    #[test]
    fn bad_byte_range_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "hi").unwrap();

        let mut m = sample_mutation(Path::new("test.txt"), "hi", "bye");
        m.byte_range = 0..999; // out of bounds
        assert!(!check_builds(&m, dir.path(), &["true".into()]));
    }

    #[test]
    fn passing_check_command_returns_true() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "hello").unwrap();

        let m = sample_mutation(Path::new("test.txt"), "hello", "world");
        assert!(check_builds(&m, dir.path(), &["true".into()]));

        // Verify original content was restored.
        let restored = std::fs::read_to_string(&file).unwrap();
        assert_eq!(restored, "hello");
    }

    #[test]
    fn failing_check_command_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "hello").unwrap();

        let m = sample_mutation(Path::new("test.txt"), "hello", "world");
        assert!(!check_builds(&m, dir.path(), &["false".into()]));

        // Verify original content was restored.
        let restored = std::fs::read_to_string(&file).unwrap();
        assert_eq!(restored, "hello");
    }

    #[test]
    fn missing_file_returns_false() {
        let m = sample_mutation(Path::new("nonexistent.rs"), "a", "b");
        assert!(!check_builds(&m, Path::new("/tmp"), &["true".into()]));
    }
}
