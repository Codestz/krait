use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};

use anyhow::{Context, bail};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tracing::debug;

/// A JSON-RPC message received from the LSP server.
#[derive(Debug)]
pub enum JsonRpcMessage {
    Response {
        id: i64,
        result: Option<Value>,
        error: Option<Value>,
    },
    Notification {
        method: String,
        params: Option<Value>,
    },
    ServerRequest {
        id: Value,
        method: String,
        params: Option<Value>,
    },
}

/// Transport layer for communicating with an LSP server over stdio.
pub struct LspTransport {
    child: Child,
    writer: BufWriter<ChildStdin>,
    reader: BufReader<ChildStdout>,
    next_id: AtomicI64,
}

impl LspTransport {
    /// Spawn an LSP server process and connect to its stdio.
    ///
    /// # Errors
    /// Returns an error if the binary cannot be spawned.
    pub fn spawn(binary: &str, args: &[&str], cwd: &Path) -> anyhow::Result<Self> {
        let mut child = Command::new(binary)
            .args(args)
            .current_dir(cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("failed to spawn LSP server: {binary}"))?;

        let stdin = child.stdin.take().context("failed to open LSP stdin")?;
        let stdout = child.stdout.take().context("failed to open LSP stdout")?;

        Ok(Self {
            child,
            writer: BufWriter::new(stdin),
            reader: BufReader::new(stdout),
            next_id: AtomicI64::new(1),
        })
    }

    /// Send a JSON-RPC request. Returns the request ID.
    ///
    /// # Errors
    /// Returns an error on IO or serialization failure.
    pub async fn send_request(&mut self, method: &str, params: Value) -> anyhow::Result<i64> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let message = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.write_message(&message).await?;
        debug!("sent request id={id} method={method}");
        Ok(id)
    }

    /// Send a JSON-RPC notification (no response expected).
    ///
    /// # Errors
    /// Returns an error on IO or serialization failure.
    pub async fn send_notification(&mut self, method: &str, params: Value) -> anyhow::Result<()> {
        let message = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.write_message(&message).await?;
        debug!("sent notification method={method}");
        Ok(())
    }

    /// Read the next JSON-RPC message from the server.
    ///
    /// # Errors
    /// Returns an error on IO, framing, or JSON parse failure.
    pub async fn read_message(&mut self) -> anyhow::Result<JsonRpcMessage> {
        let content_length = self.read_headers().await?;

        let mut body = vec![0u8; content_length];
        self.reader.read_exact(&mut body).await?;

        let value: Value = serde_json::from_slice(&body)?;
        classify_message(&value)
    }

    /// Kill the child process.
    ///
    /// # Errors
    /// Returns an error if the kill signal fails.
    pub async fn kill(&mut self) -> anyhow::Result<()> {
        self.child.kill().await.context("failed to kill LSP process")?;
        let _ = self.child.wait().await; // reap zombie
        Ok(())
    }

    /// Check if the child process is still running.
    #[must_use]
    pub fn is_alive(&mut self) -> bool {
        self.child.try_wait().ok().flatten().is_none()
    }

    /// Write raw bytes to the server's stdin.
    ///
    /// # Errors
    /// Returns an error on IO failure.
    pub async fn write_raw(&mut self, data: &[u8]) -> anyhow::Result<()> {
        self.writer.write_all(data).await?;
        Ok(())
    }

    /// Flush the writer.
    ///
    /// # Errors
    /// Returns an error on IO failure.
    pub async fn flush(&mut self) -> anyhow::Result<()> {
        self.writer.flush().await?;
        Ok(())
    }

    async fn write_message(&mut self, message: &Value) -> anyhow::Result<()> {
        let body = serde_json::to_string(message)?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());

        self.writer.write_all(header.as_bytes()).await?;
        self.writer.write_all(body.as_bytes()).await?;
        // Flush is deferred — callers that send batches should call flush_writer()
        // explicitly after the last message in the batch.
        self.writer.flush().await?;
        Ok(())
    }

    /// Flush the write buffer. Call at the end of a batch of requests
    /// to avoid per-message syscall overhead.
    ///
    /// # Errors
    /// Returns an error on IO failure.
    pub async fn flush_writer(&mut self) -> anyhow::Result<()> {
        self.writer.flush().await?;
        Ok(())
    }

    async fn read_headers(&mut self) -> anyhow::Result<usize> {
        let mut content_length: Option<usize> = None;

        loop {
            let mut line = String::new();
            let bytes_read = self.reader.read_line(&mut line).await?;
            if bytes_read == 0 {
                bail!("LSP server closed its stdout");
            }

            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }

            if let Some(len_str) = trimmed.strip_prefix("Content-Length: ") {
                content_length = Some(len_str.parse().context("invalid Content-Length")?);
            }
        }

        content_length.context("missing Content-Length header")
    }
}

fn classify_message(value: &Value) -> anyhow::Result<JsonRpcMessage> {
    // Response: has "id" and ("result" or "error")
    if let Some(id) = value.get("id") {
        if value.get("result").is_some() || value.get("error").is_some() {
            let id = id.as_i64().context("response id must be an integer")?;
            return Ok(JsonRpcMessage::Response {
                id,
                result: value.get("result").cloned(),
                error: value.get("error").cloned(),
            });
        }

        // Server request: has "id" and "method"
        if let Some(method) = value.get("method").and_then(Value::as_str) {
            return Ok(JsonRpcMessage::ServerRequest {
                id: id.clone(),
                method: method.to_string(),
                params: value.get("params").cloned(),
            });
        }
    }

    // Notification: has "method" but no "id"
    if let Some(method) = value.get("method").and_then(Value::as_str) {
        return Ok(JsonRpcMessage::Notification {
            method: method.to_string(),
            params: value.get("params").cloned(),
        });
    }

    bail!("unrecognized JSON-RPC message: {value}")
}

/// Encode a JSON-RPC payload with Content-Length framing (for testing).
#[must_use]
pub fn frame_message(payload: &Value) -> Vec<u8> {
    let body = serde_json::to_string(payload).unwrap_or_default();
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    let mut msg = header.into_bytes();
    msg.extend_from_slice(body.as_bytes());
    msg
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn frame_encode_format() {
        let payload = json!({"jsonrpc": "2.0", "id": 1, "method": "test"});
        let framed = frame_message(&payload);
        let framed_str = String::from_utf8(framed).unwrap();

        assert!(framed_str.starts_with("Content-Length: "));
        assert!(framed_str.contains("\r\n\r\n"));

        let parts: Vec<&str> = framed_str.splitn(2, "\r\n\r\n").collect();
        let header = parts[0];
        let body = parts[1];

        let declared_len: usize = header
            .strip_prefix("Content-Length: ")
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(declared_len, body.len());
    }

    #[test]
    fn classify_response() {
        let msg = json!({"jsonrpc": "2.0", "id": 1, "result": {"capabilities": {}}});
        let classified = classify_message(&msg).unwrap();
        assert!(matches!(classified, JsonRpcMessage::Response { id: 1, .. }));
    }

    #[test]
    fn classify_error_response() {
        let msg = json!({"jsonrpc": "2.0", "id": 2, "error": {"code": -32600, "message": "bad"}});
        let classified = classify_message(&msg).unwrap();
        assert!(matches!(
            classified,
            JsonRpcMessage::Response { id: 2, error: Some(_), .. }
        ));
    }

    #[test]
    fn classify_notification() {
        let msg = json!({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": {}});
        let classified = classify_message(&msg).unwrap();
        assert!(
            matches!(classified, JsonRpcMessage::Notification { ref method, .. } if method == "textDocument/publishDiagnostics")
        );
    }

    #[test]
    fn classify_server_request() {
        let msg =
            json!({"jsonrpc": "2.0", "id": 5, "method": "window/workDoneProgress/create", "params": {}});
        let classified = classify_message(&msg).unwrap();
        assert!(
            matches!(classified, JsonRpcMessage::ServerRequest { ref method, .. } if method == "window/workDoneProgress/create")
        );
    }

    #[test]
    fn request_ids_increment() {
        let next_id = AtomicI64::new(1);

        let id1 = next_id.fetch_add(1, Ordering::SeqCst);
        let id2 = next_id.fetch_add(1, Ordering::SeqCst);
        let id3 = next_id.fetch_add(1, Ordering::SeqCst);

        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        assert_eq!(id3, 3);
    }

    #[test]
    fn frame_message_content_length_matches_body() {
        let payload = json!({"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {}});
        let framed = frame_message(&payload);
        let text = String::from_utf8(framed).unwrap();
        let (header, body) = text.split_once("\r\n\r\n").unwrap();
        let declared: usize = header
            .strip_prefix("Content-Length: ")
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(declared, body.len());
        assert!(!body.is_empty());
    }

    #[test]
    fn classify_unrecognized_message_returns_error() {
        let msg = json!({"jsonrpc": "2.0"});
        let result = classify_message(&msg);
        assert!(result.is_err(), "message with no method or id should error");
    }
}
