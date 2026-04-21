// Unified diff parsing → Vec<ChangedFile>

use crate::{ChangedFile, LineRange};
use std::path::{Path, PathBuf};

/// Build ChangedFile entries for every supported file in the project tree.
/// Respects `.gitignore` rules. Skips test files.
pub fn collect_all_supported_files(project_root: &Path) -> anyhow::Result<Vec<ChangedFile>> {
    use crate::languages;

    let langs = languages::all();
    let supported_extensions: Vec<&str> = langs
        .iter()
        .flat_map(|lang| lang.extensions().to_vec())
        .collect();

    let mut files = Vec::new();

    let mut entries: Vec<_> = ignore::WalkBuilder::new(project_root)
        .hidden(true)
        .git_ignore(true)
        .build()
        .filter_map(|e| match e {
            Ok(entry) => Some(entry),
            Err(err) => {
                eprintln!("warning: skipping path during walk: {err}");
                None
            }
        })
        .filter(|e| e.file_type().is_some_and(|ft| ft.is_file()))
        .collect();
    entries.sort_by(|a, b| a.path().cmp(b.path()));

    for entry in &entries {
        let path = entry.path();
        let ext = match path.extension().and_then(|e| e.to_str()) {
            Some(e) => e,
            None => continue,
        };
        if !supported_extensions.contains(&ext) {
            continue;
        }
        let rel_path = path
            .strip_prefix(project_root)
            .unwrap_or(path)
            .to_path_buf();
        if is_test_file(&rel_path) {
            continue;
        }

        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("warning: could not read {}: {e}", path.display());
                continue;
            }
        };
        let newlines = bytecount::count(&bytes, b'\n');
        let line_count = if bytes.is_empty() {
            0
        } else if bytes.last() == Some(&b'\n') {
            newlines
        } else {
            newlines + 1
        };
        if line_count == 0 {
            continue;
        }

        files.push(ChangedFile {
            path: rel_path,
            hunks: vec![LineRange {
                start: 1,
                end: line_count,
            }],
        });
    }

    Ok(files)
}

/// Returns true for files that look like test files across supported languages.
fn is_test_file(path: &Path) -> bool {
    // Check if any ancestor directory is a known test directory
    for component in path.components() {
        let s = component.as_os_str().to_str().unwrap_or("");
        if matches!(
            s,
            "tests" | "__tests__" | "__test__" | "spec" | "specs" | "testdata" | "fixtures"
        ) {
            return true;
        }
    }
    // Java/Kotlin/Gradle: src/test/... — match "test" only when preceded by "src"
    let comps: Vec<_> = path.components().collect();
    if comps
        .windows(2)
        .any(|w| w[0].as_os_str() == "src" && w[1].as_os_str() == "test")
    {
        return true;
    }
    let name = match path.file_stem().and_then(|s| s.to_str()) {
        Some(n) => n,
        None => return false,
    };
    // Go: *_test.go
    // Python: test_*.py / *_test.py
    // Ruby: *_test.rb / test_*.rb / *_spec.rb
    // Java/C#: *Test.java / *Tests.java / *Test.cs
    // JS/TS: *.test.ts / *.spec.ts (stem after first dot)
    if name.ends_with("_test") || name.starts_with("test_") || name.ends_with("_spec") {
        return true;
    }
    if name.ends_with("Test") || name.ends_with("Tests") || name.ends_with("Spec") {
        return true;
    }
    // *.test.ts, *.spec.ts — file_stem gives "foo.test"
    if name.ends_with(".test") || name.ends_with(".spec") {
        return true;
    }
    false
}

/// Parse unified diff output (from `git diff`) into a list of changed files
/// with their modified line ranges. Only tracks added/modified lines.
pub fn parse_diff(input: &str) -> Vec<ChangedFile> {
    let mut files: Vec<ChangedFile> = Vec::new();
    let mut current_path: Option<PathBuf> = None;
    let mut current_hunks: Vec<LineRange> = Vec::new();

    // Per-hunk state
    let mut in_hunk = false;
    let mut new_line: usize = 0; // current line number in the new file
    let mut range_start: Option<usize> = None;
    let mut range_end: usize = 0;

    for line in input.lines() {
        if let Some(path_str) = line.strip_prefix("+++ ") {
            // Flush previous hunk range
            flush_range(&mut current_hunks, &mut range_start, range_end);
            in_hunk = false;

            // Flush previous file
            if let Some(path) = current_path.take()
                && !current_hunks.is_empty()
            {
                files.push(ChangedFile {
                    path,
                    hunks: current_hunks,
                });
            }
            current_hunks = Vec::new();

            // Extract path, stripping the `b/` prefix
            let path = if let Some(stripped) = path_str.strip_prefix("b/") {
                PathBuf::from(stripped)
            } else {
                PathBuf::from(path_str)
            };
            current_path = Some(path);
        } else if line.starts_with("@@ ") {
            // Flush any open range from a previous hunk
            flush_range(&mut current_hunks, &mut range_start, range_end);
            in_hunk = true;

            // Parse `@@ -old_start,old_count +new_start,new_count @@`
            if let Some(new_spec) = parse_hunk_header(line) {
                new_line = new_spec;
            }
        } else if in_hunk {
            if line.starts_with('+') {
                // Added line — track it
                if range_start.is_none() {
                    range_start = Some(new_line);
                }
                range_end = new_line;
                new_line += 1;
            } else if line.starts_with('-') {
                // Deleted line — flush any open range, don't advance new_line
                flush_range(&mut current_hunks, &mut range_start, range_end);
            } else {
                // Context line
                flush_range(&mut current_hunks, &mut range_start, range_end);
                new_line += 1;
            }
        }
    }

    // Flush final state
    flush_range(&mut current_hunks, &mut range_start, range_end);
    if let Some(path) = current_path.take()
        && !current_hunks.is_empty()
    {
        files.push(ChangedFile {
            path,
            hunks: current_hunks,
        });
    }

    files
}

fn flush_range(hunks: &mut Vec<LineRange>, start: &mut Option<usize>, end: usize) {
    if let Some(s) = start.take() {
        hunks.push(LineRange { start: s, end });
    }
}

/// Parse `@@ ... +new_start,new_count @@` → new_start
fn parse_hunk_header(line: &str) -> Option<usize> {
    // Find the `+` part after the first `@@`
    let after_at = line.strip_prefix("@@ ")?;
    let plus_idx = after_at.find('+')?;
    let rest = &after_at[plus_idx + 1..];
    let end = rest.find(|c: char| !c.is_ascii_digit())?;
    rest[..end].parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_DIFF: &str = r#"diff --git a/src/main.rs b/src/main.rs
index 1234567..abcdefg 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,6 +1,7 @@
 use std::io;

 fn main() {
+    println!("hello");
     let x = 1;
     let y = 2;
+    let z = x + y;
 }
@@ -10,3 +11,5 @@ fn other() {
     foo();
+    bar();
+    baz();
 }
diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -5,4 +5,6 @@ pub mod config;
 pub mod diff;
+pub mod languages;
+pub mod mapper;
 pub mod runner;
"#;

    #[test]
    fn parses_multiple_files() {
        let files = parse_diff(SAMPLE_DIFF);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, PathBuf::from("src/main.rs"));
        assert_eq!(files[1].path, PathBuf::from("src/lib.rs"));
    }

    #[test]
    fn parses_hunks_correctly() {
        let files = parse_diff(SAMPLE_DIFF);
        let main_rs = &files[0];
        // First hunk: line 4 added, then line 7 added (with gap)
        assert_eq!(main_rs.hunks.len(), 3);
        assert_eq!(main_rs.hunks[0], LineRange { start: 4, end: 4 });
        assert_eq!(main_rs.hunks[1], LineRange { start: 7, end: 7 });
        // Second hunk: lines 12-13 added consecutively
        assert_eq!(main_rs.hunks[2], LineRange { start: 12, end: 13 });
    }

    #[test]
    fn handles_deletions_only() {
        let diff = r#"diff --git a/foo.rs b/foo.rs
--- a/foo.rs
+++ b/foo.rs
@@ -1,5 +1,3 @@
 line1
-deleted1
-deleted2
 line2
 line3
"#;
        let files = parse_diff(diff);
        // No added lines → no hunks → file not included
        assert!(files.is_empty());
    }

    #[test]
    fn skips_binary_files() {
        let diff = r#"diff --git a/image.png b/image.png
Binary files a/image.png and b/image.png differ
diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,3 +1,4 @@
 fn main() {
+    println!("hi");
 }
"#;
        let files = parse_diff(diff);
        // Binary file has no `+++ b/...` line, so it is skipped
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, PathBuf::from("src/main.rs"));
    }

    #[test]
    fn deleted_file_produces_no_mutations() {
        let diff = r#"diff --git a/removed.rs b/removed.rs
--- a/removed.rs
+++ b/removed.rs
@@ -1,4 +1,0 @@
-fn old() {
-    // deleted
-    println!("gone");
-}
"#;
        let files = parse_diff(diff);
        assert!(
            files.is_empty(),
            "deleted-only file should produce no hunks"
        );
    }

    #[test]
    fn strips_b_prefix() {
        let diff = r#"diff --git a/path/to/file.rs b/path/to/file.rs
--- a/path/to/file.rs
+++ b/path/to/file.rs
@@ -1,3 +1,4 @@
 existing
+new_line
 end
"#;
        let files = parse_diff(diff);
        assert_eq!(files[0].path, PathBuf::from("path/to/file.rs"));
    }

    #[test]
    fn collect_all_finds_supported_files_and_respects_gitignore() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // Init a real git repo so the ignore crate respects .gitignore
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(root)
            .status()
            .expect("git init failed");

        // Supported file
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("main.rs"), "fn main() {}\n").unwrap();

        // Unsupported extension
        std::fs::write(root.join("readme.txt"), "hi\n").unwrap();

        // Hidden dir (should be skipped)
        let hidden = root.join(".hidden");
        std::fs::create_dir_all(&hidden).unwrap();
        std::fs::write(hidden.join("secret.rs"), "fn x() {}\n").unwrap();

        // Gitignored dir
        std::fs::write(root.join(".gitignore"), "generated/\n").unwrap();
        let generated = root.join("generated");
        std::fs::create_dir_all(&generated).unwrap();
        std::fs::write(generated.join("out.rs"), "fn gen() {}\n").unwrap();

        // Test file (should be skipped)
        std::fs::write(src.join("main_test.go"), "package main\n").unwrap();

        // File inside tests/ directory (should be skipped)
        let tests_dir = root.join("tests");
        std::fs::create_dir_all(&tests_dir).unwrap();
        std::fs::write(tests_dir.join("helper.rs"), "fn help() {}\n").unwrap();

        let files = collect_all_supported_files(root).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, PathBuf::from("src/main.rs"));
        assert_eq!(files[0].hunks.len(), 1);
        assert_eq!(files[0].hunks[0], LineRange { start: 1, end: 1 });
    }

    #[test]
    fn is_test_file_detects_common_patterns() {
        assert!(is_test_file(Path::new("foo_test.go")));
        assert!(is_test_file(Path::new("test_utils.py")));
        assert!(is_test_file(Path::new("UserTest.java")));
        assert!(is_test_file(Path::new("UserTests.cs")));
        assert!(is_test_file(Path::new("app.test.ts")));
        assert!(is_test_file(Path::new("app.spec.tsx")));
        assert!(is_test_file(Path::new("widget_spec.rb")));
        assert!(!is_test_file(Path::new("main.rs")));
        assert!(!is_test_file(Path::new("utils.py")));
        assert!(!is_test_file(Path::new("contest.go")));
        // Directory-based detection
        assert!(is_test_file(Path::new("tests/helper.rs")));
        assert!(is_test_file(Path::new("__tests__/utils.ts")));
        assert!(is_test_file(Path::new("src/test/java/Foo.java")));
        assert!(is_test_file(Path::new("module/src/test/kotlin/Bar.kt")));
        assert!(!is_test_file(Path::new("test/integration/main.go")));
        assert!(!is_test_file(Path::new("cmd/test/server.go")));
        assert!(is_test_file(Path::new("testdata/input.go")));
        assert!(is_test_file(Path::new("fixtures/setup.py")));
    }
}
