use std::path::Path;

use anyhow::{anyhow, Result};

use crate::languages::{self, LanguageSupport};

/// Parse a source file with tree-sitter, auto-detecting language from extension.
pub fn parse_file(path: &Path, source: &[u8]) -> Result<(tree_sitter::Tree, Box<dyn LanguageSupport>)> {
    let lang = detect_language(path)?;
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&lang.tree_sitter_language())?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| anyhow!("failed to parse {}", path.display()))?;
    Ok((tree, lang))
}

/// Detect language from file extension.
fn detect_language(path: &Path) -> Result<Box<dyn LanguageSupport>> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .ok_or_else(|| anyhow!("no file extension: {}", path.display()))?;

    for lang in languages::all() {
        if lang.extensions().contains(&ext) {
            return Ok(lang);
        }
    }

    Err(anyhow!("unsupported language for extension: .{ext}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn detect_go() {
        let lang = detect_language(&PathBuf::from("main.go")).unwrap();
        assert_eq!(lang.name(), "go");
    }

    #[test]
    fn detect_rust() {
        let lang = detect_language(&PathBuf::from("lib.rs")).unwrap();
        assert_eq!(lang.name(), "rust");
    }

    #[test]
    fn detect_unknown_extension() {
        let result = detect_language(&PathBuf::from("script.py"));
        assert!(result.is_err());
    }

    #[test]
    fn parse_go_source() {
        let source = b"package main\n\nfunc main() {}\n";
        let (tree, lang) = parse_file(Path::new("main.go"), source).unwrap();
        assert_eq!(lang.name(), "go");
        assert_eq!(tree.root_node().kind(), "source_file");
    }
}
