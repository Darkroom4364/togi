// Parse LCOV coverage data into a map of file -> covered lines.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Map from file path (relative to project root) to set of covered line numbers.
pub type CoverageMap = HashMap<PathBuf, HashSet<usize>>;

/// Parse an LCOV file and return a coverage map with paths relative to `project_root`.
pub fn parse_lcov(content: &str, project_root: &Path) -> CoverageMap {
    let mut map = CoverageMap::new();
    let mut current_file: Option<PathBuf> = None;

    for line in content.lines() {
        let line = line.trim();
        if let Some(path_str) = line.strip_prefix("SF:") {
            let abs = PathBuf::from(path_str);
            let rel = abs
                .strip_prefix(project_root)
                .map(|p| p.to_path_buf())
                .unwrap_or(abs);
            current_file = Some(rel);
        } else if let Some(da) = line.strip_prefix("DA:") {
            if let Some(ref file) = current_file {
                let mut parts = da.split(',');
                if let (Some(line_str), Some(count_str)) = (parts.next(), parts.next())
                    && let (Ok(line_no), Ok(count)) =
                        (line_str.parse::<usize>(), count_str.parse::<u64>())
                    && count > 0
                {
                    map.entry(file.clone()).or_default().insert(line_no);
                }
            }
        } else if line == "end_of_record" {
            current_file = None;
        }
    }

    map
}

/// Filter mutations to only those on lines present in the coverage map.
pub fn filter_by_coverage(
    mutations: Vec<crate::Mutation>,
    coverage: &CoverageMap,
    project_root: &Path,
) -> Vec<crate::Mutation> {
    mutations
        .into_iter()
        .filter(|m| {
            let rel = m
                .file
                .strip_prefix(project_root)
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|_| m.file.clone());
            coverage
                .get(&rel)
                .is_some_and(|lines| lines.contains(&m.line))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_lcov_basic() {
        let lcov = "\
SF:/project/src/main.go
DA:1,1
DA:2,0
DA:3,5
end_of_record
SF:/project/src/util.go
DA:10,1
end_of_record
";
        let root = Path::new("/project");
        let map = parse_lcov(lcov, root);

        let main_lines = map.get(Path::new("src/main.go")).unwrap();
        assert!(main_lines.contains(&1));
        assert!(!main_lines.contains(&2));
        assert!(main_lines.contains(&3));

        let util_lines = map.get(Path::new("src/util.go")).unwrap();
        assert!(util_lines.contains(&10));
    }

    #[test]
    fn parse_lcov_empty() {
        let map = parse_lcov("", Path::new("/project"));
        assert!(map.is_empty());
    }

    #[test]
    fn parse_lcov_relative_paths() {
        let lcov = "SF:src/lib.rs\nDA:5,1\nend_of_record\n";
        let map = parse_lcov(lcov, Path::new("/project"));
        assert!(map.contains_key(Path::new("src/lib.rs")));
    }

    #[test]
    fn filter_removes_uncovered_mutations() {
        let root = Path::new("/project");
        let mut coverage = CoverageMap::new();
        coverage
            .entry(PathBuf::from("src/main.go"))
            .or_default()
            .insert(10);

        let mutations = vec![
            crate::Mutation {
                id: 0,
                file: root.join("src/main.go"),
                language: "go".into(),
                line: 10,
                column: 1,
                operator: "lt_to_lte".into(),
                description: "< to <=".into(),
                original: "<".into(),
                replacement: "<=".into(),
                byte_range: 0..1,
            },
            crate::Mutation {
                id: 1,
                file: root.join("src/main.go"),
                language: "go".into(),
                line: 20,
                column: 1,
                operator: "lt_to_lte".into(),
                description: "< to <=".into(),
                original: "<".into(),
                replacement: "<=".into(),
                byte_range: 0..1,
            },
        ];

        let filtered = filter_by_coverage(mutations, &coverage, root);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].line, 10);
    }
}
