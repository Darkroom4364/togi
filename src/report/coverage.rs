use crate::cli::OutputFormat;
use crate::coverage::CoverageGateReport;
use anyhow::Result;
use std::fmt::Write;
use std::path::Path;

pub fn print_terminal(report: &CoverageGateReport) {
    print!("{}", format_terminal(report));
}

pub fn print_github(report: &CoverageGateReport) {
    print!("{}", format_github(report));
}

pub fn print_json(report: &CoverageGateReport) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(report)?);
    Ok(())
}

pub fn write_html(report: &CoverageGateReport, path: &Path) -> Result<()> {
    std::fs::write(path, format_html(report))?;
    Ok(())
}

pub fn print(report: &CoverageGateReport, format: OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Json => print_json(report)?,
        OutputFormat::Github => print_github(report),
        OutputFormat::Html => write_html(report, Path::new("togi-coverage-report.html"))?,
        // SARIF reports surviving mutants, not coverage gates; keep the gate readable.
        OutputFormat::Sarif => print_terminal(report),
        OutputFormat::Terminal => print_terminal(report),
    }
    Ok(())
}

fn format_terminal(report: &CoverageGateReport) -> String {
    let mut out = String::new();
    writeln!(out, "Coverage gate failed").unwrap();
    write_metric(&mut out, "Overall line coverage", &report.line_coverage).unwrap();
    write_metric(&mut out, "Changed-line coverage", &report.diff_coverage).unwrap();
    if report.fail_on_uncovered_diff || !report.uncovered_changed_lines.is_empty() {
        writeln!(out, "Uncovered changed lines:").unwrap();
        for file in &report.uncovered_changed_lines {
            writeln!(
                out,
                "  {}: {}",
                file.file.display(),
                join_lines(&file.lines)
            )
            .unwrap();
        }
    }
    out
}

fn format_github(report: &CoverageGateReport) -> String {
    let mut out = String::new();
    writeln!(out, "## Coverage gate failed").unwrap();
    write_metric_md(&mut out, "Overall line coverage", &report.line_coverage).unwrap();
    write_metric_md(&mut out, "Changed-line coverage", &report.diff_coverage).unwrap();
    if report.fail_on_uncovered_diff || !report.uncovered_changed_lines.is_empty() {
        writeln!(out, "\n### Uncovered changed lines").unwrap();
        for file in &report.uncovered_changed_lines {
            writeln!(
                out,
                "- `{}`: {}",
                file.file.display(),
                join_lines(&file.lines)
            )
            .unwrap();
        }
    }
    out
}

fn format_html(report: &CoverageGateReport) -> String {
    let mut out = String::new();
    write!(
        out,
        "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"UTF-8\">"
    )
    .unwrap();
    write!(out, "<title>togi coverage gate</title>").unwrap();
    write!(
        out,
        "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1.0\">"
    )
    .unwrap();
    write!(
        out,
        "<style>body{{font-family:system-ui,sans-serif;margin:2rem;line-height:1.5}}\
         h1{{margin-bottom:1rem}}.metric{{margin:.5rem 0}}\
         table{{border-collapse:collapse;margin-top:1rem}}td,th{{padding:.4rem .6rem;border-bottom:1px solid #ccc;text-align:left}}\
         code{{font-family:SFMono-Regular,Menlo,monospace}}</style></head><body>"
    )
    .unwrap();
    write!(out, "<h1>Coverage gate failed</h1>").unwrap();
    write_metric_html(&mut out, "Overall line coverage", &report.line_coverage).unwrap();
    write_metric_html(&mut out, "Changed-line coverage", &report.diff_coverage).unwrap();
    if report.fail_on_uncovered_diff || !report.uncovered_changed_lines.is_empty() {
        write!(out, "<h2>Uncovered changed lines</h2><table><thead><tr><th>File</th><th>Lines</th></tr></thead><tbody>").unwrap();
        for file in &report.uncovered_changed_lines {
            write!(
                out,
                "<tr><td><code>{}</code></td><td>{}</td></tr>",
                html_escape(&file.file.display().to_string()),
                html_escape(&join_lines(&file.lines))
            )
            .unwrap();
        }
        write!(out, "</tbody></table>").unwrap();
    }
    write!(out, "</body></html>").unwrap();
    out
}

fn write_metric(
    out: &mut String,
    label: &str,
    metric: &crate::coverage::CoverageMetric,
) -> std::fmt::Result {
    let threshold = metric
        .threshold
        .map(|value| format!(" (threshold {value:.1}%)"))
        .unwrap_or_default();
    writeln!(
        out,
        "{label}: {:.1}% ({}/{}){threshold}",
        metric.percent(),
        metric.covered,
        metric.total
    )
}

fn write_metric_md(
    out: &mut String,
    label: &str,
    metric: &crate::coverage::CoverageMetric,
) -> std::fmt::Result {
    let threshold = metric
        .threshold
        .map(|value| format!(" threshold {value:.1}%"))
        .unwrap_or_default();
    writeln!(
        out,
        "- **{label}**: {:.1}% ({}/{}){threshold}",
        metric.percent(),
        metric.covered,
        metric.total
    )
}

fn write_metric_html(
    out: &mut String,
    label: &str,
    metric: &crate::coverage::CoverageMetric,
) -> std::fmt::Result {
    let threshold = metric
        .threshold
        .map(|value| format!(" (threshold {value:.1}%)"))
        .unwrap_or_default();
    writeln!(
        out,
        "<div class=\"metric\"><strong>{label}</strong>: {:.1}% ({}/{}){threshold}</div>",
        metric.percent(),
        metric.covered,
        metric.total
    )
}

fn join_lines(lines: &[usize]) -> String {
    lines
        .iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coverage::{CoverageMetric, CoverageUncoveredFile};
    use std::path::Path;
    use std::process::{Command, Output};
    use tempfile::TempDir;

    const CHILD_TEST: &str = "report::coverage::tests::coverage_router_child";
    const REPORT_FILE: &str = "togi-coverage-report.html";
    const STDOUT_BEGIN: &str = "__TOGI_COVERAGE_ROUTER_STDOUT_BEGIN__";
    const STDOUT_END: &str = "__TOGI_COVERAGE_ROUTER_STDOUT_END__";

    fn sample_report() -> CoverageGateReport {
        CoverageGateReport {
            line_coverage: CoverageMetric {
                covered: 3,
                total: 4,
                threshold: Some(90.0),
            },
            diff_coverage: CoverageMetric {
                covered: 1,
                total: 2,
                threshold: Some(80.0),
            },
            uncovered_changed_lines: vec![CoverageUncoveredFile {
                file: "src/lib.rs".into(),
                lines: vec![12],
            }],
            fail_on_uncovered_diff: true,
        }
    }

    fn output_format(name: &str) -> OutputFormat {
        match name {
            "terminal" => OutputFormat::Terminal,
            "json" => OutputFormat::Json,
            "github" => OutputFormat::Github,
            "html" => OutputFormat::Html,
            "sarif" => OutputFormat::Sarif,
            _ => panic!("unknown output format: {name}"),
        }
    }

    #[test]
    fn coverage_router_child() {
        let Ok(api) = std::env::var("TOGI_COVERAGE_ROUTER_API") else {
            return;
        };
        let format = output_format(
            &std::env::var("TOGI_COVERAGE_ROUTER_FORMAT")
                .expect("coverage router child format must be set"),
        );
        let report = sample_report();

        println!("{STDOUT_BEGIN}");
        let result = match api.as_str() {
            "canonical" => print(&report, format),
            "wrapper" => crate::report::print_coverage_gate_report(&report, format),
            _ => panic!("unknown coverage router API: {api}"),
        };
        result.unwrap();
        println!("{STDOUT_END}");
    }

    fn invoke(api: &str, format: &str, directory: &Path) -> Output {
        // foxguard: ignore[rs/no-command-injection]
        // `current_exe` is this active test binary; fixed child-test arguments
        // isolate stdout and current-directory behavior in a subprocess.
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", CHILD_TEST, "--nocapture"])
            .env("TOGI_COVERAGE_ROUTER_API", api)
            .env("TOGI_COVERAGE_ROUTER_FORMAT", format)
            .current_dir(directory)
            .output()
            .unwrap()
    }

    fn routed_stdout(output: &[u8]) -> &str {
        let output = std::str::from_utf8(output).unwrap();
        let (_, output) = output
            .split_once(STDOUT_BEGIN)
            .expect("coverage router stdout start marker must be present");
        let output = output
            .strip_prefix('\n')
            .expect("coverage router stdout start marker must end its line");
        output
            .split_once(STDOUT_END)
            .expect("coverage router stdout end marker must be present")
            .0
    }

    fn json_from_output(output: &str) -> serde_json::Value {
        serde_json::from_str(output).expect("coverage JSON must parse")
    }

    #[test]
    fn public_coverage_report_routes_match() {
        for (format, writes_html) in [
            ("terminal", false),
            ("json", false),
            ("github", false),
            ("html", true),
            ("sarif", false),
        ] {
            let canonical_dir = TempDir::new().unwrap();
            let wrapper_dir = TempDir::new().unwrap();
            let canonical = invoke("canonical", format, canonical_dir.path());
            let wrapper = invoke("wrapper", format, wrapper_dir.path());

            assert!(
                canonical.status.success(),
                "canonical {format} route failed\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&canonical.stdout),
                String::from_utf8_lossy(&canonical.stderr),
            );
            assert!(
                wrapper.status.success(),
                "wrapper {format} route failed\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&wrapper.stdout),
                String::from_utf8_lossy(&wrapper.stderr),
            );
            let canonical_stdout = routed_stdout(&canonical.stdout);
            let wrapper_stdout = routed_stdout(&wrapper.stdout);
            assert_eq!(
                canonical_stdout, wrapper_stdout,
                "{format} routes must have identical stdout"
            );
            if format == "html" {
                assert!(
                    canonical_stdout.is_empty(),
                    "HTML route must not write stdout"
                );
            } else if format != "json" {
                assert!(
                    canonical_stdout.contains("Coverage gate failed"),
                    "{format} route stdout: {canonical_stdout}"
                );
            }

            let canonical_html = canonical_dir.path().join(REPORT_FILE);
            let wrapper_html = wrapper_dir.path().join(REPORT_FILE);
            assert_eq!(
                canonical_html.exists(),
                writes_html,
                "canonical {format} route wrote an unexpected destination"
            );
            assert_eq!(
                wrapper_html.exists(),
                writes_html,
                "wrapper {format} route wrote an unexpected destination"
            );

            if format == "json" {
                assert_eq!(
                    json_from_output(canonical_stdout),
                    json_from_output(wrapper_stdout)
                );
            }

            if writes_html {
                assert_eq!(
                    std::fs::read(canonical_html).unwrap(),
                    std::fs::read(wrapper_html).unwrap(),
                    "HTML routes must write the same report"
                );
                assert!(
                    canonical.stderr.is_empty(),
                    "canonical HTML route must not write stderr"
                );
                assert_eq!(
                    String::from_utf8(wrapper.stderr).unwrap(),
                    "HTML coverage report written to togi-coverage-report.html\n"
                );
            } else {
                assert_eq!(
                    canonical.stderr, wrapper.stderr,
                    "{format} routes must have identical stderr"
                );
                assert!(
                    canonical.stderr.is_empty(),
                    "{format} routes must not write stderr"
                );
            }
        }
    }
}
