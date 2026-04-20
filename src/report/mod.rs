pub mod json;
pub mod terminal;

pub fn print_report(report: &crate::MutationReport, format: &str) {
    match format {
        "json" => json::print_report(report),
        _ => terminal::print_report(report),
    }
}
