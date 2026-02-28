use std::path::Path;

use anyhow::Context;
use serde_json::{json, Value};

use crate::commands::workspace_edit::apply_workspace_edit;
use crate::lsp::client::{path_to_uri, LspClient};
use crate::lsp::files::FileTracker;

/// Format a file using the LSP `textDocument/formatting` request.
///
/// Re-opens the file to ensure the LSP has fresh on-disk content, then
/// applies the returned `TextEdit` list atomically.
///
/// # Errors
/// Returns an error if the LSP request fails or the file cannot be written.
pub async fn handle_format(
    path: &Path,
    client: &mut LspClient,
    file_tracker: &mut FileTracker,
    project_root: &Path,
) -> anyhow::Result<Value> {
    let abs_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        project_root.join(path)
    };

    // Re-open so the LSP sees current on-disk content
    file_tracker
        .reopen(&abs_path, client.transport_mut())
        .await?;

    let uri = path_to_uri(&abs_path)?;
    let params = json!({
        "textDocument": { "uri": uri.as_str() },
        "options": { "tabSize": 4, "insertSpaces": true }
    });

    let request_id = client
        .transport_mut()
        .send_request("textDocument/formatting", params)
        .await?;

    let response = client
        .wait_for_response_public(request_id)
        .await
        .context("textDocument/formatting request failed")?;

    let edits: Vec<Value> = if let Value::Array(arr) = &response {
        arr.clone()
    } else {
        vec![]
    };

    let n = edits.len();
    let rel = abs_path
        .strip_prefix(project_root)
        .unwrap_or(&abs_path)
        .to_string_lossy()
        .to_string();

    if n == 0 {
        return Ok(json!({
            "path": rel,
            "edits_applied": 0,
        }));
    }

    // Build a workspace_edit from the flat list of TextEdits
    let mut changes = serde_json::Map::new();
    changes.insert(uri.as_str().to_string(), serde_json::to_value(&edits)?);
    let workspace_edit = json!({ "changes": changes });
    apply_workspace_edit(&workspace_edit, project_root)?;

    Ok(json!({
        "path": rel,
        "edits_applied": n,
    }))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn format_response_shape() {
        let data = json!({ "path": "src/lib.rs", "edits_applied": 5 });
        assert_eq!(data["edits_applied"].as_u64(), Some(5));
    }
}
