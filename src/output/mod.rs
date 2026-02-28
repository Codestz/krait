pub mod compact;
pub mod human;
pub mod json;

use crate::cli::OutputFormat;
use crate::commands::search::SearchOutput;
use crate::protocol::Response;

/// Format a response according to the selected output format.
#[must_use]
pub fn format_response(response: &Response, format: OutputFormat) -> String {
    match format {
        OutputFormat::Compact => compact::format(response),
        OutputFormat::Json => json::format(response),
        OutputFormat::Human => human::format(response),
    }
}

/// Format search output according to the selected format.
#[must_use]
pub fn format_search(
    output: &SearchOutput,
    format: OutputFormat,
    with_context: bool,
    files_only: bool,
) -> String {
    match format {
        OutputFormat::Json => serde_json::to_string_pretty(output).unwrap_or_default(),
        _ => compact::format_search(output, with_context, files_only),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::protocol::Response;

    #[test]
    fn format_flag_dispatches_correctly() {
        let resp = Response::ok(json!({"daemon": {"pid": 1, "uptime_secs": 0}}));

        let compact_out = format_response(&resp, OutputFormat::Compact);
        assert!(compact_out.starts_with("daemon:"));

        let json_out = format_response(&resp, OutputFormat::Json);
        assert!(json_out.starts_with('{'));

        let human_out = format_response(&resp, OutputFormat::Human);
        assert!(human_out.starts_with("Daemon Status"));
    }

    #[test]
    fn error_response_formatting_all_formats() {
        let resp = Response::err_with_advice("test_err", "Something failed", "Try again");

        let compact = format_response(&resp, OutputFormat::Compact);
        assert!(compact.contains("error:"));
        assert!(compact.contains("advice:"));

        let json_out = format_response(&resp, OutputFormat::Json);
        let parsed: serde_json::Value = serde_json::from_str(&json_out).unwrap();
        assert_eq!(parsed["code"], "test_err");

        let human = format_response(&resp, OutputFormat::Human);
        assert!(human.contains("Error:"));
        assert!(human.contains("Advice:"));
    }
}
