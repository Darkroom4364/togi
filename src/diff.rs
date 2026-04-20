// Unified diff parsing → Vec<ChangedFile>

use crate::{ChangedFile, LineRange};
use std::path::PathBuf;

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
        if line.starts_with("+++ ") {
            // Flush previous hunk range
            flush_range(&mut current_hunks, &mut range_start, range_end);
            in_hunk = false;

            // Flush previous file
            if let Some(path) = current_path.take() {
                if !current_hunks.is_empty() {
                    files.push(ChangedFile { path, hunks: current_hunks });
                }
            }
            current_hunks = Vec::new();

            // Extract path, stripping the `b/` prefix
            let path_str = &line[4..];
            let path = if path_str.starts_with("b/") {
                PathBuf::from(&path_str[2..])
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
    if let Some(path) = current_path.take() {
        if !current_hunks.is_empty() {
            files.push(ChangedFile { path, hunks: current_hunks });
        }
    }

    // Warn if diff is very large
    let total_changed: usize = files.iter().flat_map(|f| &f.hunks).map(|h| h.end - h.start + 1).sum();
    if total_changed > 1000 {
        eprintln!(
            "warning: diff contains {} changed lines across {} files; mutations will be capped by max_per_run",
            total_changed,
            files.len()
        );
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
        assert!(files.is_empty(), "deleted-only file should produce no hunks");
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
}
