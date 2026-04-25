use crate::{MutationReport, MutationResult};
use anyhow::Result;
use std::collections::BTreeMap;
use std::fmt::Write;

struct FileStats {
    total: usize,
    build_errors: usize,
    killed: usize,
    mutations: Vec<MutationEntry>,
}

struct MutationEntry {
    line: usize,
    operator: String,
    description: String,
    original: String,
    replacement: String,
    result: &'static str,
}

pub fn generate_report(report: &MutationReport) -> Result<String> {
    let mut files: BTreeMap<String, FileStats> = BTreeMap::new();

    for (mutation, result) in &report.results {
        let file_path = mutation.file.display().to_string();
        let result_str = match result {
            MutationResult::Killed => "killed",
            MutationResult::Survived => "survived",
            MutationResult::Timeout => "timeout",
            MutationResult::BuildError => "build_error",
        };
        let killed = matches!(result, MutationResult::Killed);
        let build_error = matches!(result, MutationResult::BuildError);

        let stats = files.entry(file_path).or_insert(FileStats {
            total: 0,
            build_errors: 0,
            killed: 0,
            mutations: Vec::new(),
        });
        stats.total += 1;
        if killed {
            stats.killed += 1;
        }
        if build_error {
            stats.build_errors += 1;
        }
        stats.mutations.push(MutationEntry {
            line: mutation.line,
            operator: mutation.operator.clone(),
            description: mutation.description.clone(),
            original: mutation.original.clone(),
            replacement: mutation.replacement.clone(),
            result: result_str,
        });
    }

    let tested = report.total - report.build_errors;
    let score_pct = if tested > 0 {
        (report.killed as f64 / tested as f64) * 100.0
    } else if report.total == 0 {
        100.0
    } else {
        0.0
    };

    let mut html = String::new();
    write!(
        html,
        "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"UTF-8\">"
    )?;
    write!(
        html,
        "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1.0\">"
    )?;
    write!(html, "<title>togi — Mutation Testing Report</title>")?;
    write!(html, "<style>{}</style>", CSS)?;
    write!(html, "</head><body>")?;

    // Header
    write!(html, "<header><h1>togi mutation report</h1>")?;
    write!(
        html,
        "<div class=\"summary\"><span class=\"score\">{:.1}%</span> mutation score \
         &mdash; {}/{} killed, {} survived, {} timeout, {} build errors \
         &mdash; {:.2}s</div></header>",
        score_pct,
        report.killed,
        tested,
        report.survived,
        report.timeout,
        report.build_errors,
        report.duration.as_secs_f64()
    )?;

    // Layout
    write!(html, "<div class=\"container\"><nav class=\"file-tree\">")?;
    write!(html, "<h2>Files</h2><ul>")?;

    for (path, stats) in &files {
        let file_tested = stats.total.saturating_sub(stats.build_errors);
        let file_pct = if file_tested > 0 {
            (stats.killed as f64 / file_tested as f64) * 100.0
        } else if stats.total == 0 {
            100.0
        } else {
            0.0
        };
        let class = score_class(file_pct);
        write!(
            html,
            "<li><a href=\"#{}\" class=\"{}\"><code>{}</code> \
             <span class=\"badge\">{}/{}</span></a></li>",
            slug(path),
            class,
            html_escape(path),
            stats.killed,
            file_tested
        )?;
    }

    write!(html, "</ul></nav><main>")?;

    // Per-file sections
    for (path, stats) in &files {
        let file_tested = stats.total.saturating_sub(stats.build_errors);
        let file_pct = if file_tested > 0 {
            (stats.killed as f64 / file_tested as f64) * 100.0
        } else if stats.total == 0 {
            100.0
        } else {
            0.0
        };

        write!(
            html,
            "<section id=\"{}\"><h3><code>{}</code> \
             <span class=\"score-inline {}\">({:.0}%)</span></h3>",
            slug(path),
            html_escape(path),
            score_class(file_pct),
            file_pct
        )?;

        write!(
            html,
            "<table><thead><tr>\
            <th>Line</th><th>Operator</th><th>Description</th>\
            <th>Original</th><th>Replacement</th><th>Result</th>\
            </tr></thead><tbody>"
        )?;

        for m in &stats.mutations {
            let result_class = match m.result {
                "killed" => "result-killed",
                "survived" => "result-survived",
                "timeout" => "result-timeout",
                _ => "result-build-error",
            };
            write!(
                html,
                "<tr class=\"{}\"><td>{}</td><td><code>{}</code></td><td>{}</td>\
                 <td><code class=\"orig\">{}</code></td>\
                 <td><code class=\"repl\">{}</code></td>\
                 <td>{}</td></tr>",
                result_class,
                m.line,
                html_escape(&m.operator),
                html_escape(&m.description),
                html_escape(&m.original),
                html_escape(&m.replacement),
                m.result
            )?;
        }

        write!(html, "</tbody></table></section>")?;
    }

    write!(html, "</main></div></body></html>")?;
    Ok(html)
}

pub fn write_report(report: &MutationReport, path: &std::path::Path) -> Result<()> {
    let html = generate_report(report)?;
    std::fs::write(path, html)?;
    Ok(())
}

fn score_class(pct: f64) -> &'static str {
    if pct >= 80.0 {
        "score-good"
    } else if pct >= 50.0 {
        "score-ok"
    } else {
        "score-bad"
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn slug(path: &str) -> String {
    path.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

const CSS: &str = r#"
*{margin:0;padding:0;box-sizing:border-box}
body{font-family:system-ui,-apple-system,sans-serif;background:#1a1a2e;color:#e0e0e0;line-height:1.5}
header{background:#16213e;padding:1.5rem 2rem;border-bottom:2px solid #0f3460}
h1{font-size:1.4rem;color:#e94560}
.summary{margin-top:.5rem;font-size:.95rem;color:#a0a0b0}
.summary .score{font-size:1.3rem;font-weight:700;color:#e94560}
.container{display:flex;min-height:calc(100vh - 100px)}
.file-tree{width:280px;background:#16213e;padding:1rem;overflow-y:auto;border-right:1px solid #0f3460;flex-shrink:0}
.file-tree h2{font-size:.9rem;text-transform:uppercase;letter-spacing:.1em;color:#888;margin-bottom:.5rem}
.file-tree ul{list-style:none}
.file-tree li{margin:.25rem 0}
.file-tree a{text-decoration:none;display:flex;justify-content:space-between;align-items:center;padding:.3rem .5rem;border-radius:4px;font-size:.85rem}
.file-tree a:hover{background:#0f3460}
.badge{font-size:.75rem;background:#0f3460;padding:.1rem .4rem;border-radius:3px;color:#ccc}
main{flex:1;padding:1.5rem 2rem;overflow-x:auto}
section{margin-bottom:2rem}
h3{font-size:1rem;margin-bottom:.75rem;color:#ccc}
.score-inline{font-size:.85rem;font-weight:400}
table{width:100%;border-collapse:collapse;font-size:.85rem}
th{text-align:left;padding:.5rem;background:#16213e;border-bottom:1px solid #0f3460;color:#888;font-weight:600}
td{padding:.5rem;border-bottom:1px solid #222}
code{font-family:'SF Mono',Menlo,monospace;font-size:.82rem}
.orig{color:#e94560;text-decoration:line-through}
.repl{color:#53d769}
.result-killed{opacity:.6}
.result-survived{background:rgba(233,69,96,.08)}
.result-timeout{background:rgba(255,200,50,.06)}
.result-build-error{background:rgba(255,200,50,.06)}
.score-good,.score-good .badge{color:#53d769}
.score-ok,.score-ok .badge{color:#f5a623}
.score-bad,.score-bad .badge{color:#e94560}
a.score-good,a.score-ok,a.score-bad{color:inherit}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Mutation;
    use std::path::PathBuf;
    use std::time::Duration;

    fn sample_report() -> MutationReport {
        MutationReport {
            results: vec![
                (
                    Mutation {
                        id: 1,
                        file: PathBuf::from("src/auth.rs"),
                        language: "rust".to_string(),
                        line: 47,
                        column: 10,
                        operator: "binary/lt_to_lte".to_string(),
                        description: "changed < to <=".to_string(),
                        original: "<".to_string(),
                        replacement: "<=".to_string(),
                        byte_range: 0..1,
                    },
                    MutationResult::Killed,
                ),
                (
                    Mutation {
                        id: 2,
                        file: PathBuf::from("src/handler.rs"),
                        language: "rust".to_string(),
                        line: 15,
                        column: 5,
                        operator: "binary/eq_to_neq".to_string(),
                        description: "changed == to !=".to_string(),
                        original: "==".to_string(),
                        replacement: "!=".to_string(),
                        byte_range: 0..2,
                    },
                    MutationResult::Survived,
                ),
            ],
            duration: Duration::from_millis(1234),
            total: 2,
            killed: 1,
            survived: 1,
            timeout: 0,
            build_errors: 0,
        }
    }

    #[test]
    fn html_contains_doctype() {
        let html = generate_report(&sample_report()).unwrap();
        assert!(html.starts_with("<!DOCTYPE html>"));
    }

    #[test]
    fn html_contains_file_sections() {
        let html = generate_report(&sample_report()).unwrap();
        assert!(html.contains("src/auth.rs"));
        assert!(html.contains("src/handler.rs"));
    }

    #[test]
    fn html_contains_mutation_details() {
        let html = generate_report(&sample_report()).unwrap();
        assert!(html.contains("binary/lt_to_lte"));
        assert!(html.contains("changed &lt; to &lt;="));
        assert!(html.contains("killed"));
        assert!(html.contains("survived"));
    }

    #[test]
    fn html_contains_score() {
        let html = generate_report(&sample_report()).unwrap();
        assert!(html.contains("50.0%"));
        assert!(html.contains("1/2 killed"));
    }

    #[test]
    fn html_escapes_special_chars() {
        let report = MutationReport {
            results: vec![(
                Mutation {
                    id: 1,
                    file: PathBuf::from("src/test.rs"),
                    language: "rust".to_string(),
                    line: 1,
                    column: 1,
                    operator: "test".to_string(),
                    description: "a < b & c > d".to_string(),
                    original: "<".to_string(),
                    replacement: ">".to_string(),
                    byte_range: 0..1,
                },
                MutationResult::Killed,
            )],
            duration: Duration::from_millis(100),
            total: 1,
            killed: 1,
            survived: 0,
            timeout: 0,
            build_errors: 0,
        };
        let html = generate_report(&report).unwrap();
        assert!(html.contains("a &lt; b &amp; c &gt; d"));
    }
}
