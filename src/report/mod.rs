pub mod html;
pub mod json;
pub mod terminal;

pub fn print_report(report: &crate::MutationReport, format: &str) -> anyhow::Result<()> {
    match format {
        "json" => json::print_report(report)?,
        "html" => {
            let path = std::path::Path::new("togi-report.html");
            html::write_report(report, path)?;
            eprintln!("HTML report written to {}", path.display());
        }
        _ => terminal::print_report(report),
    }
    Ok(())
}
