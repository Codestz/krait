use crate::protocol::Response;

/// Format response as raw JSON for programmatic consumption.
#[must_use]
pub fn format(response: &Response) -> String {
    if let Some(error) = &response.error {
        return serde_json::to_string(error).unwrap_or_default();
    }

    let Some(data) = &response.data else {
        return String::new();
    };

    serde_json::to_string(data).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn json_status_output() {
        let resp = Response::ok(json!({"daemon": {"pid": 12345, "uptime_secs": 300}}));
        let out = format(&resp);
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["daemon"]["pid"], 12345);
    }

    #[test]
    fn json_error_output() {
        let resp = Response::err("not_found", "Symbol not found");
        let out = format(&resp);
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["code"], "not_found");
    }
}
