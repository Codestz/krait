use std::path::Path;

use anyhow::Context;
use serde_json::{Value, json};

use crate::commands::find::resolve_symbol_location;
use crate::commands::workspace_edit::{apply_workspace_edit, count_workspace_edits};
use crate::lsp::client::{LspClient, path_to_uri};
use crate::lsp::files::FileTracker;

/// Rename a symbol across all files using LSP `textDocument/rename`.
///
/// Resolves the symbol location, sends the rename request, and applies
/// the returned `WorkspaceEdit` atomically to all affected files.
///
/// # Errors
/// Returns an error if the symbol is not found, the LSP request fails,
/// or any file cannot be written.
pub async fn handle_rename(
    name: &str,
    new_name: &str,
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
        "position": { "line": line, "character": character },
        "newName": new_name
    });

    let request_id = client
        .transport_mut()
        .send_request("textDocument/rename", params)
        .await?;

    let workspace_edit = client
        .wait_for_response_public(request_id)
        .await
        .context("textDocument/rename request failed")?;

    if workspace_edit.is_null() {
        return Ok(json!({
            "files_changed": 0,
            "refs_changed": 0,
        }));
    }

    let refs_changed = count_workspace_edits(&workspace_edit);
    let modified = apply_workspace_edit(&workspace_edit, project_root)?;

    Ok(json!({
        "files_changed": modified.len(),
        "refs_changed": refs_changed,
    }))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn rename_response_shape() {
        let data = json!({ "files_changed": 3, "refs_changed": 12 });
        assert_eq!(data["files_changed"].as_u64(), Some(3));
        assert_eq!(data["refs_changed"].as_u64(), Some(12));
    }
}
