use std::path::Path;

use anyhow::Context;
use serde_json::{Value, json};

use crate::commands::find::resolve_symbol_location;
use crate::lsp::client::{LspClient, path_to_uri};
use crate::lsp::files::FileTracker;

/// Fetch hover information for a named symbol.
///
/// Resolves the symbol's location via `workspace/symbol`, then issues a
/// `textDocument/hover` request at that position.
///
/// # Errors
/// Returns an error if the symbol is not found or the LSP request fails.
pub async fn handle_hover(
    name: &str,
    client: &mut LspClient,
    file_tracker: &mut FileTracker,
    project_root: &Path,
) -> anyhow::Result<Value> {
    let (abs_path, line, character) =
        resolve_symbol_location(name, client, project_root).await?;

    file_tracker
        .ensure_open(&abs_path, client.transport_mut())
        .await?;

    let uri = path_to_uri(&abs_path)?;
    let params = json!({
        "textDocument": { "uri": uri.as_str() },
        "position": { "line": line, "character": character }
    });

    let request_id = client
        .transport_mut()
        .send_request("textDocument/hover", params)
        .await?;

    let response = client
        .wait_for_response_public(request_id)
        .await
        .context("textDocument/hover request failed")?;

    let hover_content = extract_hover_content(&response);
    let rel_path = abs_path
        .strip_prefix(project_root)
        .unwrap_or(&abs_path)
        .to_string_lossy()
        .to_string();

    Ok(json!({
        "hover_content": hover_content,
        "path": rel_path,
        "line": line + 1,
    }))
}

/// Extract a plain-text string from a hover response's `contents` field.
fn extract_hover_content(response: &Value) -> String {
    let Some(contents) = response.get("contents") else {
        return String::new();
    };

    // MarkupContent: { kind: "markdown"|"plaintext", value: "..." }
    if let Some(value) = contents.get("value").and_then(Value::as_str) {
        return value.trim().to_string();
    }

    // MarkedString: plain string
    if let Some(s) = contents.as_str() {
        return s.trim().to_string();
    }

    // Array of MarkedString | string
    if let Some(arr) = contents.as_array() {
        let parts: Vec<&str> = arr
            .iter()
            .filter_map(|v| {
                v.as_str()
                    .or_else(|| v.get("value").and_then(Value::as_str))
            })
            .collect();
        return parts.join("\n").trim().to_string();
    }

    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_markup_content() {
        let resp = json!({ "contents": { "kind": "markdown", "value": "fn greet()" } });
        assert_eq!(extract_hover_content(&resp), "fn greet()");
    }

    #[test]
    fn extract_string_contents() {
        let resp = json!({ "contents": "hello world" });
        assert_eq!(extract_hover_content(&resp), "hello world");
    }

    #[test]
    fn extract_array_contents() {
        let resp = json!({ "contents": ["type A", { "language": "rust", "value": "fn a()" }] });
        let out = extract_hover_content(&resp);
        assert!(out.contains("type A"));
        assert!(out.contains("fn a()"));
    }

    #[test]
    fn extract_missing_contents() {
        let resp = json!({ "range": {} });
        assert_eq!(extract_hover_content(&resp), "");
    }
}
