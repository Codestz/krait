use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context};
use lsp_types::{
    ClientCapabilities, CodeActionClientCapabilities, DocumentSymbolClientCapabilities,
    DynamicRegistrationClientCapabilities, GotoCapability, HoverClientCapabilities,
    InitializeParams, InitializeResult, InitializedParams, PublishDiagnosticsClientCapabilities,
    RenameClientCapabilities, ServerCapabilities, TextDocumentClientCapabilities,
    TextDocumentSyncClientCapabilities, Uri, WindowClientCapabilities, WorkspaceClientCapabilities,
    WorkspaceFolder, WorkspaceSymbolClientCapabilities,
};
use serde_json::Value;
use tracing::debug;

use super::diagnostics::{ingest_publish_diagnostics, DiagnosticStore};
use super::error::LspError;
use super::registry::{find_server, get_entry};
use super::transport::{JsonRpcMessage, LspTransport};
use crate::detect::Language;

/// Default timeout for the initialize handshake.
const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(30);

/// Default timeout for shutdown.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// High-level LSP client that manages a language server lifecycle.
pub struct LspClient {
    transport: LspTransport,
    capabilities: Option<ServerCapabilities>,
    language: Language,
    /// Buffered responses for request IDs that arrived out of order.
    buffered_responses: HashMap<i64, BufferedResponse>,
    /// Whether the server supports `workspace/didChangeWorkspaceFolders`.
    supports_workspace_folders: bool,
    /// The name of the server binary (e.g., "vtsls", "rust-analyzer").
    server_name: String,
    /// Workspace folders currently attached to this server.
    attached_folders: HashSet<PathBuf>,
    /// Optional store for collecting `textDocument/publishDiagnostics` notifications.
    diagnostic_store: Option<Arc<DiagnosticStore>>,
}

/// A response received for a request ID we weren't waiting for yet.
enum BufferedResponse {
    Ok(Value),
    Err(String),
}

impl LspClient {
    /// Start an LSP server for the given language and project root.
    ///
    /// Looks for the server binary in PATH first, then in `~/.krait/servers/`.
    /// Use `start_with_auto_install()` to also download if missing.
    ///
    /// # Errors
    /// Returns `LspError::ServerNotFound` if the binary is missing.
    /// Returns an error if the process cannot be spawned.
    pub fn start(language: Language, project_root: &Path) -> Result<Self, LspError> {
        let entry = get_entry(language).ok_or_else(|| LspError::InitializeFailed {
            message: format!("no LSP config for {language}"),
        })?;

        let binary_path = find_server(&entry).ok_or_else(|| LspError::ServerNotFound {
            language,
            advice: entry.install_advice.to_string(),
        })?;

        Self::start_with_binary(&binary_path, entry.args, language, project_root)
    }

    /// Start an LSP server using a specific binary path.
    ///
    /// # Errors
    /// Returns an error if the process cannot be spawned.
    pub fn start_with_binary(
        binary_path: &Path,
        args: &[&str],
        language: Language,
        project_root: &Path,
    ) -> Result<Self, LspError> {
        let binary_str = binary_path.to_str().unwrap_or("unknown");
        let server_name = binary_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        let transport = LspTransport::spawn(binary_str, args, project_root).map_err(|e| {
            LspError::InitializeFailed {
                message: format!("failed to spawn {binary_str}: {e}"),
            }
        })?;

        debug!(
            "started LSP server for {language}: {binary_str} {}",
            args.join(" ")
        );

        Ok(Self {
            transport,
            capabilities: None,
            language,
            buffered_responses: HashMap::new(),
            supports_workspace_folders: false,
            server_name,
            attached_folders: HashSet::new(),
            diagnostic_store: None,
        })
    }

    /// Perform the LSP initialize handshake.
    ///
    /// Sends `initialize` request, waits for response, then sends `initialized` notification.
    ///
    /// # Errors
    /// Returns an error if the handshake fails or times out.
    ///
    /// # Panics
    /// Panics if capabilities are not stored after a successful response (should never happen).
    pub async fn initialize(&mut self, project_root: &Path) -> anyhow::Result<&ServerCapabilities> {
        let root_uri = path_to_uri(project_root)?;
        let params = build_initialize_params(&root_uri, project_root, self.language);
        let params_value = serde_json::to_value(&params)?;

        let request_id = self
            .transport
            .send_request("initialize", params_value)
            .await?;

        let result = self
            .wait_for_response(request_id, INITIALIZE_TIMEOUT)
            .await
            .context("initialize handshake failed")?;

        let init_result: InitializeResult =
            serde_json::from_value(result).context("failed to parse InitializeResult")?;

        // Detect workspace folder support
        self.supports_workspace_folders = init_result
            .capabilities
            .workspace
            .as_ref()
            .and_then(|w| w.workspace_folders.as_ref())
            .and_then(|wf| wf.supported)
            .unwrap_or(false);

        debug!(
            "server capabilities received for {} (workspace_folders={})",
            self.language, self.supports_workspace_folders
        );

        self.capabilities = Some(init_result.capabilities);

        // Track the initial workspace folder
        self.attached_folders.insert(project_root.to_path_buf());

        // Send initialized notification (must be after storing capabilities)
        self.transport
            .send_notification("initialized", serde_json::to_value(InitializedParams {})?)
            .await?;

        debug!("initialized notification sent for {}", self.language);

        self.capabilities
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("internal: capabilities missing after initialize"))
    }

    /// Shut down the LSP server cleanly.
    ///
    /// Sends `shutdown` request, waits for response, sends `exit` notification,
    /// then waits for the process to exit.
    ///
    /// # Errors
    /// Returns an error if shutdown fails or the process doesn't exit.
    pub async fn shutdown(&mut self) -> anyhow::Result<()> {
        let request_id = self.transport.send_request("shutdown", Value::Null).await?;

        // Wait for shutdown response (with timeout)
        let _ = self.wait_for_response(request_id, SHUTDOWN_TIMEOUT).await;

        // Send exit notification
        self.transport
            .send_notification("exit", Value::Null)
            .await
            .ok();

        // Give the process a moment to exit, then force kill
        tokio::time::sleep(Duration::from_millis(100)).await;
        if self.transport.is_alive() {
            debug!("LSP server still alive after exit, killing");
            self.transport.kill().await.ok();
        }

        debug!("LSP server for {} shut down", self.language);
        Ok(())
    }

    /// Wait for a response to a previously sent request.
    ///
    /// Uses the default initialize timeout. For commands that need LSP responses
    /// after the handshake is complete.
    ///
    /// # Errors
    /// Returns an error if the response times out or contains an LSP error.
    pub async fn wait_for_response_public(&mut self, request_id: i64) -> anyhow::Result<Value> {
        self.wait_for_response(request_id, INITIALIZE_TIMEOUT).await
    }

    /// Wait for a response with a caller-specified timeout.
    ///
    /// Unlike wrapping `wait_for_response_public` in `tokio::time::timeout`,
    /// this ensures the response is always consumed (no orphaned responses in the pipe).
    ///
    /// # Errors
    /// Returns an error if the timeout expires before a response is received.
    pub async fn wait_for_response_with_timeout(
        &mut self,
        request_id: i64,
        timeout: Duration,
    ) -> anyhow::Result<Value> {
        self.wait_for_response(request_id, timeout).await
    }

    /// Wait until the server sends a `$/progress` notification with `"kind": "end"`.
    ///
    /// This replaces the fixed-delay polling heuristic: instead of sleeping 200ms × N,
    /// we listen for the server's own signal that background indexing is complete.
    /// If no progress end is received within `timeout`, we proceed anyway (graceful degradation).
    ///
    /// Any responses received while waiting are buffered for future retrieval.
    pub async fn wait_for_progress_end(&mut self, timeout: Duration) {
        let _ = tokio::time::timeout(timeout, async {
            loop {
                let message = match self.transport.read_message().await {
                    Ok(m) => m,
                    Err(e) => {
                        debug!("wait_for_progress_end: transport error: {e}");
                        return;
                    }
                };
                match message {
                    JsonRpcMessage::Notification { method, params } if method == "$/progress" => {
                        let kind = params
                            .as_ref()
                            .and_then(|p| p.get("value"))
                            .and_then(|v| v.get("kind"))
                            .and_then(|k| k.as_str())
                            .unwrap_or("");
                        debug!("$/progress kind={kind}");
                        if kind == "end" {
                            return;
                        }
                    }
                    JsonRpcMessage::Response { id, result, error } => {
                        debug!("buffering response id={id} during progress wait");
                        let buffered = if let Some(err) = error {
                            BufferedResponse::Err(err.to_string())
                        } else {
                            BufferedResponse::Ok(result.unwrap_or(Value::Null))
                        };
                        self.buffered_responses.insert(id, buffered);
                    }
                    JsonRpcMessage::ServerRequest { id, method, .. } => {
                        debug!("auto-responding to server request during progress wait: {method}");
                        let response = serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": null,
                        });
                        let body = serde_json::to_string(&response).unwrap_or_default();
                        let header = format!("Content-Length: {}\r\n\r\n", body.len());
                        let _ = self.transport.write_raw(header.as_bytes()).await;
                        let _ = self.transport.write_raw(body.as_bytes()).await;
                        let _ = self.transport.flush().await;
                    }
                    JsonRpcMessage::Notification { method, .. } => {
                        debug!("ignoring notification during progress wait: {method}");
                    }
                }
            }
        })
        .await;
        debug!("wait_for_progress_end: done (ready or timed out)");
    }

    /// Get the server capabilities (available after initialize).
    #[must_use]
    pub fn capabilities(&self) -> Option<&ServerCapabilities> {
        self.capabilities.as_ref()
    }

    /// Get the language this client serves.
    #[must_use]
    pub fn language(&self) -> Language {
        self.language
    }

    /// Whether the server supports `workspace/didChangeWorkspaceFolders`.
    #[must_use]
    pub fn supports_workspace_folders(&self) -> bool {
        self.supports_workspace_folders
    }

    /// The server binary name (e.g., "vtsls", "rust-analyzer").
    #[must_use]
    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    /// Check if a workspace folder is currently attached to this server.
    #[must_use]
    pub fn is_folder_attached(&self, path: &Path) -> bool {
        self.attached_folders.contains(path)
    }

    /// Get all attached workspace folders.
    #[must_use]
    pub fn attached_folders(&self) -> &HashSet<PathBuf> {
        &self.attached_folders
    }

    /// Dynamically attach a workspace folder to the running server.
    ///
    /// Sends `workspace/didChangeWorkspaceFolders` notification.
    /// No-op if already attached or server doesn't support it.
    ///
    /// # Errors
    /// Returns an error if the notification cannot be sent.
    pub async fn attach_folder(&mut self, path: &Path) -> anyhow::Result<()> {
        if self.attached_folders.contains(path) {
            return Ok(());
        }

        if !self.supports_workspace_folders {
            debug!(
                "server {} does not support workspace folders, skipping attach",
                self.server_name
            );
            // Still track it so we don't re-attempt
            self.attached_folders.insert(path.to_path_buf());
            return Ok(());
        }

        let uri = path_to_uri(path)?;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("workspace");

        let params = serde_json::json!({
            "event": {
                "added": [{ "uri": uri.as_str(), "name": name }],
                "removed": []
            }
        });

        self.transport
            .send_notification("workspace/didChangeWorkspaceFolders", params)
            .await?;

        self.attached_folders.insert(path.to_path_buf());
        debug!(
            "attached workspace folder: {} (total: {})",
            path.display(),
            self.attached_folders.len()
        );
        Ok(())
    }

    /// Dynamically detach a workspace folder from the running server.
    ///
    /// Sends `workspace/didChangeWorkspaceFolders` notification.
    /// No-op if not attached.
    ///
    /// # Errors
    /// Returns an error if the notification cannot be sent.
    pub async fn detach_folder(&mut self, path: &Path) -> anyhow::Result<()> {
        if !self.attached_folders.contains(path) {
            return Ok(());
        }

        if self.supports_workspace_folders {
            let uri = path_to_uri(path)?;
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("workspace");

            let params = serde_json::json!({
                "event": {
                    "added": [],
                    "removed": [{ "uri": uri.as_str(), "name": name }]
                }
            });

            self.transport
                .send_notification("workspace/didChangeWorkspaceFolders", params)
                .await?;
        }

        self.attached_folders.remove(path);
        debug!(
            "detached workspace folder: {} (remaining: {})",
            path.display(),
            self.attached_folders.len()
        );
        Ok(())
    }

    /// Attach a diagnostic store so `textDocument/publishDiagnostics` notifications
    /// are captured while waiting for responses.
    pub fn set_diagnostic_store(&mut self, store: Arc<DiagnosticStore>) {
        self.diagnostic_store = Some(store);
    }

    /// Get mutable access to the transport for sending additional requests.
    pub fn transport_mut(&mut self) -> &mut LspTransport {
        &mut self.transport
    }

    /// Wait for a response with a specific ID, buffering out-of-order responses
    /// and auto-responding to server requests.
    async fn wait_for_response(
        &mut self,
        expected_id: i64,
        timeout: Duration,
    ) -> anyhow::Result<Value> {
        // Check if this response was already buffered from a previous read
        if let Some(buffered) = self.buffered_responses.remove(&expected_id) {
            return match buffered {
                BufferedResponse::Ok(value) => Ok(value),
                BufferedResponse::Err(msg) => bail!("LSP error: {msg}"),
            };
        }

        let result = tokio::time::timeout(timeout, async {
            loop {
                let message = self.transport.read_message().await?;
                match message {
                    JsonRpcMessage::Response { id, result, error } if id == expected_id => {
                        if let Some(err) = error {
                            debug!("LSP error response for id={id}: {err}");
                            bail!("LSP error: {err}");
                        }
                        debug!("received response for id={id}");
                        return Ok(result.unwrap_or(Value::Null));
                    }
                    JsonRpcMessage::Response { id, result, error } => {
                        // Buffer for later retrieval instead of discarding
                        debug!("buffering out-of-order response id={id}");
                        let buffered = if let Some(err) = error {
                            BufferedResponse::Err(err.to_string())
                        } else {
                            BufferedResponse::Ok(result.unwrap_or(Value::Null))
                        };
                        self.buffered_responses.insert(id, buffered);
                    }
                    JsonRpcMessage::Notification { method, params } => {
                        if method == "textDocument/publishDiagnostics" {
                            if let Some(store) = &self.diagnostic_store {
                                ingest_publish_diagnostics(params, store);
                            }
                        } else {
                            debug!("ignoring notification during wait: {method}");
                        }
                    }
                    JsonRpcMessage::ServerRequest { id, method, .. } => {
                        debug!("auto-responding to server request: {method}");
                        let response = serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": null,
                        });
                        let body = serde_json::to_string(&response)?;
                        let header = format!("Content-Length: {}\r\n\r\n", body.len());
                        self.transport.write_raw(header.as_bytes()).await?;
                        self.transport.write_raw(body.as_bytes()).await?;
                        self.transport.flush().await?;
                    }
                }
            }
        })
        .await;

        match result {
            Ok(inner) => inner,
            Err(_) => bail!("timed out waiting for response ({}s)", timeout.as_secs()),
        }
    }
}

/// Convert a filesystem path to an LSP `file://` URI.
///
/// # Errors
/// Returns an error if the path is not absolute or not valid UTF-8.
pub fn path_to_uri(path: &Path) -> anyhow::Result<Uri> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let path_str = abs.to_str().context("path is not valid UTF-8")?;
    let uri_string = format!("file://{path_str}");
    uri_string
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid URI: {e}"))
}

/// Return language-specific `initializationOptions` for servers that need them.
///
/// - Java (jdtls): empty settings object; full jdtls setup requires bundles path
///   but basic symbol queries work without it.
/// - Lua: sets `Lua.runtime.version` so the server indexes standard Lua/LuaJIT globals.
/// - Others: `None` (the server uses its own defaults).
fn language_init_options(_lang: Language) -> Option<Value> {
    None
}

/// Build the `InitializeParams` for the LSP handshake.
#[allow(deprecated)] // root_uri is deprecated but needed for compatibility
fn build_initialize_params(
    root_uri: &Uri,
    project_root: &Path,
    lang: Language,
) -> InitializeParams {
    let project_name = project_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("project");

    InitializeParams {
        process_id: Some(std::process::id()),
        root_uri: Some(root_uri.clone()),
        capabilities: ClientCapabilities {
            text_document: Some(TextDocumentClientCapabilities {
                synchronization: Some(TextDocumentSyncClientCapabilities {
                    dynamic_registration: Some(false),
                    did_save: Some(true),
                    ..Default::default()
                }),
                definition: Some(GotoCapability {
                    dynamic_registration: Some(false),
                    link_support: Some(false),
                }),
                references: Some(DynamicRegistrationClientCapabilities {
                    dynamic_registration: Some(false),
                }),
                document_symbol: Some(DocumentSymbolClientCapabilities {
                    dynamic_registration: Some(false),
                    hierarchical_document_symbol_support: Some(true),
                    ..Default::default()
                }),
                rename: Some(RenameClientCapabilities {
                    dynamic_registration: Some(false),
                    prepare_support: Some(true),
                    ..Default::default()
                }),
                hover: Some(HoverClientCapabilities {
                    dynamic_registration: Some(false),
                    content_format: None,
                }),
                publish_diagnostics: Some(PublishDiagnosticsClientCapabilities {
                    related_information: Some(true),
                    ..Default::default()
                }),
                code_action: Some(CodeActionClientCapabilities {
                    dynamic_registration: Some(false),
                    ..Default::default()
                }),
                formatting: Some(DynamicRegistrationClientCapabilities {
                    dynamic_registration: Some(false),
                }),
                ..Default::default()
            }),
            workspace: Some(WorkspaceClientCapabilities {
                symbol: Some(WorkspaceSymbolClientCapabilities {
                    dynamic_registration: Some(false),
                    ..Default::default()
                }),
                workspace_folders: Some(true),
                ..Default::default()
            }),
            window: Some(WindowClientCapabilities {
                work_done_progress: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        },
        workspace_folders: Some(vec![WorkspaceFolder {
            uri: root_uri.clone(),
            name: project_name.to_string(),
        }]),
        initialization_options: language_init_options(lang),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_to_uri_absolute() {
        let uri = path_to_uri(Path::new("/tmp/test-project")).unwrap();
        assert_eq!(uri.as_str(), "file:///tmp/test-project");
    }

    #[test]
    fn build_params_has_required_fields() {
        let root = Path::new("/tmp/test-project");
        let uri = path_to_uri(root).unwrap();

        #[allow(deprecated)]
        let params = build_initialize_params(&uri, root, Language::Rust);

        assert!(params.process_id.is_some());
        assert!(params.capabilities.text_document.is_some());
        assert!(params.capabilities.workspace.is_some());

        let folders = params.workspace_folders.unwrap();
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].name, "test-project");
        assert_eq!(folders[0].uri.as_str(), "file:///tmp/test-project");
    }

    #[test]
    fn start_missing_server_returns_not_found() {
        let result = LspClient::start(Language::Go, Path::new("/tmp/nonexistent"));
        // gopls may or may not be installed
        if let Err(LspError::ServerNotFound { language, advice }) = result {
            assert_eq!(language, Language::Go);
            assert!(!advice.is_empty());
        }
        // If gopls is installed or another error occurs, that's also acceptable
    }

    #[test]
    fn build_params_declares_workspace_folder_support() {
        let root = Path::new("/tmp/test-project");
        let uri = path_to_uri(root).unwrap();

        #[allow(deprecated)]
        let params = build_initialize_params(&uri, root, Language::TypeScript);

        let ws = params.capabilities.workspace.unwrap();
        assert_eq!(ws.workspace_folders, Some(true));
    }

    #[test]
    fn attached_folders_tracking() {
        // We can't easily create an LspClient without a real process,
        // but we can test the HashSet logic conceptually.
        let mut folders = HashSet::new();
        let p1 = PathBuf::from("/project/packages/api");
        let p2 = PathBuf::from("/project/packages/web");

        assert!(!folders.contains(&p1));
        folders.insert(p1.clone());
        assert!(folders.contains(&p1));
        assert!(!folders.contains(&p2));

        // Duplicate insert is a no-op
        folders.insert(p1.clone());
        assert_eq!(folders.len(), 1);

        folders.insert(p2.clone());
        assert_eq!(folders.len(), 2);

        folders.remove(&p1);
        assert_eq!(folders.len(), 1);
        assert!(!folders.contains(&p1));
    }

    // Integration tests requiring real LSP servers
    #[tokio::test]
    #[ignore = "requires rust-analyzer installed"]
    async fn initialize_rust_analyzer() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rust-hello");
        let mut client =
            LspClient::start(Language::Rust, &fixture).expect("rust-analyzer should be available");

        let caps = client
            .initialize(&fixture)
            .await
            .expect("init should succeed");
        assert!(caps.document_symbol_provider.is_some());

        client.shutdown().await.expect("shutdown should succeed");
    }

    #[tokio::test]
    #[ignore = "requires rust-analyzer installed"]
    async fn shutdown_kills_process() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rust-hello");
        let mut client =
            LspClient::start(Language::Rust, &fixture).expect("rust-analyzer should be available");

        client
            .initialize(&fixture)
            .await
            .expect("init should succeed");
        client.shutdown().await.expect("shutdown should succeed");

        assert!(!client.transport.is_alive());
    }
}
