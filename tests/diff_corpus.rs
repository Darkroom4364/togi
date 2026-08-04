//! Checked-in diff corpus: parser hardening expectations for adversarial and
//! unusual unified-diff inputs.

use std::path::{Path, PathBuf};

fn corpus(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/diffs")
        .join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("could not read corpus file {}: {e}", path.display()))
}

fn assert_range_invariants(files: &[togi::ChangedFile]) {
    for file in files {
        assert!(
            !file.hunks.is_empty(),
            "{}: empty hunks",
            file.path.display()
        );
        for (i, range) in file.hunks.iter().enumerate() {
            assert!(range.start >= 1, "{}: 0-based range", file.path.display());
            assert!(
                range.start <= range.end,
                "{}: inverted range",
                file.path.display()
            );
            if let Some(prev) = i.checked_sub(1).map(|p| &file.hunks[p]) {
                assert!(
                    prev.end < range.start,
                    "{}: unsorted or overlapping ranges",
                    file.path.display()
                );
            }
        }
    }
}

#[test]
fn malformed_hunks_drop_only_their_own_files() {
    let files = togi::diff::parse_diff(&corpus("malformed-hunks.diff"));
    assert_range_invariants(&files);
    assert_eq!(files.len(), 1, "only the well-formed file survives");
    assert_eq!(files[0].path, PathBuf::from("src/good.rs"));
    assert_eq!(files[0].hunks, vec![togi::LineRange { start: 2, end: 2 }]);
}

#[test]
fn unsafe_post_image_paths_are_rejected() {
    let files = togi::diff::parse_diff(&corpus("unsafe-paths.diff"));
    assert_range_invariants(&files);
    assert_eq!(files.len(), 1, "only the safe path survives");
    assert_eq!(files[0].path, PathBuf::from("src/safe.rs"));
}

#[test]
fn unicode_path_is_preserved_verbatim() {
    // Git diff runs with core.quotePath=false, so non-ASCII paths arrive as
    // raw UTF-8 rather than octal escapes.
    let files = togi::diff::parse_diff(&corpus("unicode-path.diff"));
    assert_range_invariants(&files);
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, PathBuf::from("src/ünïcode.rs"));
    assert_eq!(files[0].hunks, vec![togi::LineRange { start: 2, end: 2 }]);
}

#[test]
fn rename_targets_the_post_image_path() {
    let files = togi::diff::parse_diff(&corpus("rename.diff"));
    assert_range_invariants(&files);
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, PathBuf::from("new/name.rs"));
    assert_eq!(files[0].hunks, vec![togi::LineRange { start: 2, end: 2 }]);
}

#[test]
fn os_rejected_path_is_tolerated_without_panic() {
    let files = togi::diff::parse_diff(&corpus("os-rejected-path.diff"));
    assert_range_invariants(&files);
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].hunks, vec![togi::LineRange { start: 2, end: 2 }]);

    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let sentinel_bytes = b"package main // sentinel\n".to_vec();
    let sentinel = tmp.path().join("sentinel.go");
    std::fs::write(&sentinel, &sentinel_bytes).unwrap();
    let _ = std::fs::write(
        project.join(&files[0].path),
        b"package main\n\nfunc f() int {\n\treturn 1 + 2\n}\n",
    );
    let mutations = togi::mutator::generate_mutations(&files, &project, 16, 0, &[]).unwrap();
    for mutation in &mutations {
        let source = std::fs::read(project.join(&mutation.file)).unwrap();
        assert_eq!(
            &source[mutation.byte_range.clone()],
            mutation.original.as_bytes()
        );
    }
    assert_eq!(std::fs::read(&sentinel).unwrap(), sentinel_bytes);
}

#[test]
fn directory_prefix_collision_skips_without_aborting() {
    let files = togi::diff::parse_diff(&corpus("dir-collision.diff"));
    assert_range_invariants(&files);
    assert_eq!(files.len(), 2);

    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("project");
    let go_source = b"package main\n\nfunc f() int {\n\treturn 1 + 2\n}\n";
    std::fs::create_dir_all(project.join("new/name.go")).unwrap();
    std::fs::write(project.join("new/name.go/name.go"), go_source).unwrap();

    let mutations = togi::mutator::generate_mutations(&files, &project, 16, 0, &[]).unwrap();
    assert!(!mutations.is_empty());
    for mutation in &mutations {
        assert_eq!(mutation.file, PathBuf::from("new/name.go/name.go"));
        let source = std::fs::read(project.join(&mutation.file)).unwrap();
        assert_eq!(
            &source[mutation.byte_range.clone()],
            mutation.original.as_bytes()
        );
    }
}
