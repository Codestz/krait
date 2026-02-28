use std::fmt::Write;

use crate::protocol::Response;

/// Format response as human-readable, aligned output for terminal use.
#[must_use]
pub fn format(response: &Response) -> String {
    if let Some(error) = &response.error {
        let mut out = format!("Error: {}\nCode:  {}", error.message, error.code);
        if let Some(advice) = &error.advice {
            let _ = write!(out, "\n\nAdvice: {advice}");
        }
        return out;
    }

    let Some(data) = &response.data else {
        return String::new();
    };

    // Status response
    if let Some(daemon) = data.get("daemon") {
        let pid = daemon
            .get("pid")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let uptime = daemon
            .get("uptime_secs")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let mut out = format!(
            "Daemon Status\n  PID:    {pid}\n  Uptime: {}",
            format_duration_human(uptime)
        );

        if let Some(lsp) = data.get("lsp") {
            if !lsp.is_null() {
                let lang = lsp.get("language").and_then(|v| v.as_str()).unwrap_or("?");
                let status = lsp.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                let server = lsp.get("server").and_then(|v| v.as_str()).unwrap_or("?");
                let _ = write!(out, "\n\nLSP Server\n  Language: {lang}\n  Status:   {status}\n  Server:   {server}");
            }
        }

        if let Some(project) = data.get("project") {
            if let Some(root) = project.get("root").and_then(|v| v.as_str()) {
                let _ = write!(out, "\n\nProject\n  Root:      {root}");
            }
            if let Some(langs) = project.get("languages").and_then(|v| v.as_array()) {
                let names: Vec<&str> = langs.iter().filter_map(|v| v.as_str()).collect();
                if !names.is_empty() {
                    let _ = write!(out, "\n  Languages: {}", names.join(", "));
                }
            }
        }

        return out;
    }

    // Generic: pretty-printed JSON
    serde_json::to_string_pretty(data).unwrap_or_default()
}

fn format_duration_human(secs: u64) -> String {
    if secs < 60 {
        format!("{secs} seconds")
    } else if secs < 3600 {
        let m = secs / 60;
        if m == 1 {
            "1 minute".into()
        } else {
            format!("{m} minutes")
        }
    } else {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        let h_str = if h == 1 {
            "1 hour".into()
        } else {
            format!("{h} hours")
        };
        if m == 0 {
            h_str
        } else {
            format!("{h_str} {m} minutes")
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn human_status_output() {
        let resp = Response::ok(json!({"daemon": {"pid": 12345, "uptime_secs": 300}}));
        let out = format(&resp);
        assert!(out.contains("Daemon Status"));
        assert!(out.contains("PID:    12345"));
        assert!(out.contains("Uptime: 5 minutes"));
    }

    #[test]
    fn human_error_with_advice() {
        let resp = Response::err_with_advice("lsp_not_found", "LSP not detected", "Install it");
        let out = format(&resp);
        assert!(out.contains("Error: LSP not detected"));
        assert!(out.contains("Advice: Install it"));
    }
}
