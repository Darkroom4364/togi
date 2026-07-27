use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};

/// Normalize a source path to Togi's canonical project-relative slash form.
///
/// This is intentionally shared by baseline comparison and replay so both
/// features agree on source identity.
pub(crate) fn normalized_project_relative_path(project_root: &Path, file: &Path) -> Option<String> {
    let relative = if file.is_absolute() {
        file.strip_prefix(project_root).ok()?
    } else {
        file
    };
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => {
                let part = part.to_str()?;
                if part.contains('\\') {
                    return None;
                }
                parts.push(part);
            }
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// Return whether a stored path is already a canonical project-relative path.
pub(crate) fn is_normalized_project_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// Resolve a stored canonical path without permitting traversal or symlink
/// escape from the project root.
pub(crate) fn resolve_normalized_project_relative_path(
    project_root: &Path,
    path: &str,
) -> Option<PathBuf> {
    if !is_normalized_project_relative_path(path) {
        return None;
    }
    let root = project_root.canonicalize().ok()?;
    let candidate = root.join(path);
    let resolved = candidate.canonicalize().ok()?;
    resolved.strip_prefix(&root).ok()?;
    Some(resolved)
}

/// SHA-256 of the exact on-disk source bytes, including a stable algorithm tag.
pub(crate) fn source_fingerprint(source: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(source))
}

/// Verify that a stored byte range is in bounds and still names the original
/// UTF-8 mutation bytes exactly.
pub(crate) fn range_matches(source: &[u8], start: usize, end: usize, original: &str) -> bool {
    start <= end
        && end <= source.len()
        && source
            .get(start..end)
            .is_some_and(|bytes| bytes == original.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_path_rejects_escape_and_normalizes_dot_components() {
        let root = Path::new("/repo");
        assert_eq!(
            normalized_project_relative_path(root, Path::new("./src/../src/lib.rs")),
            Some("src/lib.rs".into())
        );
        assert_eq!(
            normalized_project_relative_path(root, Path::new("../outside.rs")),
            None
        );
        assert!(!is_normalized_project_relative_path("src/../lib.rs"));
    }

    #[test]
    fn fingerprint_is_raw_sha256_and_ranges_are_byte_exact() {
        assert_eq!(
            source_fingerprint(b"abc"),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(range_matches("aé".as_bytes(), 1, 3, "é"));
        assert!(!range_matches("aé".as_bytes(), 1, 2, "é"));
    }

    #[cfg(unix)]
    #[test]
    fn resolved_path_rejects_symlink_escape_from_project_root() {
        use std::os::unix::fs::symlink;

        let tempdir = tempfile::tempdir().unwrap();
        let root = tempdir.path().join("project");
        let outside = tempdir.path().join("outside.rs");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(&outside, b"outside").unwrap();
        symlink(&outside, root.join("alias.rs")).unwrap();

        assert!(resolve_normalized_project_relative_path(&root, "alias.rs").is_none());
    }
}
