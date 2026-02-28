use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Context;
use tracing::debug;

use super::client::path_to_uri;
use super::transport::LspTransport;
use crate::detect::Language;

/// Tracks which files are open in the LSP server.
///
/// LSP requires `textDocument/didOpen` before any queries on a file.
/// This tracker ensures idempotent opens and bulk close on shutdown.
pub struct FileTracker {
    open_files: HashSet<PathBuf>,
    language: Language,
}

impl FileTracker {
    /// Create a new tracker for the given language.
    #[must_use]
    pub fn new(language: Language) -> Self {
        Self {
            open_files: HashSet::new(),
            language,
        }
    }

    /// Ensure a file is open in the LSP server.
    /// If already open, this is a no-op.
    ///
    /// # Errors
    /// Returns an error if the file can't be read or the notification fails.
    pub async fn ensure_open(
        &mut self,
        path: &Path,
        transport: &mut LspTransport,
    ) -> anyhow::Result<()> {
        let canonical = std::fs::canonicalize(path)
            .with_context(|| format!("file not found: {}", path.display()))?;

        if self.open_files.contains(&canonical) {
            return Ok(());
        }

        let uri = path_to_uri(&canonical)?;
        let text = std::fs::read_to_string(&canonical)
            .with_context(|| format!("failed to read: {}", canonical.display()))?;

        let params = serde_json::json!({
            "textDocument": {
                "uri": uri.as_str(),
                "languageId": self.language.name(),
                "version": 0,
                "text": text,
            }
        });

        transport
            .send_notification("textDocument/didOpen", params)
            .await?;

        debug!("opened file: {}", canonical.display());
        self.open_files.insert(canonical);
        Ok(())
    }

    /// Open a file with pre-read content (avoids duplicate disk I/O during indexing).
    ///
    /// # Errors
    /// Returns an error if the notification fails.
    pub async fn open_with_content(
        &mut self,
        path: &Path,
        uri: &str,
        content: &str,
        transport: &mut LspTransport,
    ) -> anyhow::Result<()> {
        let canonical = std::fs::canonicalize(path)
            .unwrap_or_else(|_| path.to_path_buf());

        if self.open_files.contains(&canonical) {
            return Ok(());
        }

        let params = serde_json::json!({
            "textDocument": {
                "uri": uri,
                "languageId": self.language.name(),
                "version": 0,
                "text": content,
            }
        });

        transport
            .send_notification("textDocument/didOpen", params)
            .await?;

        self.open_files.insert(canonical);
        Ok(())
    }

    /// Force-reopen a file, sending fresh content to the LSP.
    ///
    /// Unlike `ensure_open`, this always sends `didClose` (if already open) followed by
    /// `didOpen` with the current on-disk content. Use this after a file has been edited
    /// so the language server analyses the new version.
    ///
    /// # Errors
    /// Returns an error if the file can't be read or the notification fails.
    pub async fn reopen(
        &mut self,
        path: &Path,
        transport: &mut LspTransport,
    ) -> anyhow::Result<()> {
        let canonical = std::fs::canonicalize(path)
            .with_context(|| format!("file not found: {}", path.display()))?;

        // Close first (no-op if not open, but always remove from tracker)
        if self.open_files.remove(&canonical) {
            let uri = path_to_uri(&canonical)?;
            let params = serde_json::json!({
                "textDocument": { "uri": uri.as_str() }
            });
            transport
                .send_notification("textDocument/didClose", params)
                .await?;
            debug!("closed (for reopen): {}", canonical.display());
        }

        // Now open with fresh content
        self.ensure_open(path, transport).await
    }

    /// Close a file in the LSP server.
    ///
    /// # Errors
    /// Returns an error if the notification fails.
    pub async fn close(
        &mut self,
        path: &Path,
        transport: &mut LspTransport,
    ) -> anyhow::Result<()> {
        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());

        if !self.open_files.remove(&canonical) {
            return Ok(());
        }

        let uri = path_to_uri(&canonical)?;
        let params = serde_json::json!({
            "textDocument": {
                "uri": uri.as_str(),
            }
        });

        transport
            .send_notification("textDocument/didClose", params)
            .await?;

        debug!("closed file: {}", canonical.display());
        Ok(())
    }

    /// Close all open files.
    ///
    /// # Errors
    /// Returns an error if any close notification fails.
    pub async fn close_all(&mut self, transport: &mut LspTransport) -> anyhow::Result<()> {
        let paths: Vec<PathBuf> = self.open_files.drain().collect();
        for path in &paths {
            let uri = path_to_uri(path)?;
            let params = serde_json::json!({
                "textDocument": {
                    "uri": uri.as_str(),
                }
            });
            transport
                .send_notification("textDocument/didClose", params)
                .await?;
            debug!("closed file: {}", path.display());
        }
        Ok(())
    }

    /// Check if a file is currently open.
    #[must_use]
    pub fn is_open(&self, path: &Path) -> bool {
        std::fs::canonicalize(path)
            .map(|c| self.open_files.contains(&c))
            .unwrap_or(false)
    }

    /// Number of currently open files.
    #[must_use]
    pub fn open_count(&self) -> usize {
        self.open_files.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_tracker_is_empty() {
        let tracker = FileTracker::new(Language::Rust);
        assert_eq!(tracker.open_count(), 0);
    }

    #[test]
    fn is_open_returns_false_for_unknown_file() {
        let tracker = FileTracker::new(Language::Rust);
        assert!(!tracker.is_open(Path::new("/tmp/nonexistent.rs")));
    }
}
