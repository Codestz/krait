use std::fmt;
use std::time::Duration;

use crate::detect::Language;

/// Errors specific to LSP client operations.
#[derive(Debug)]
pub enum LspError {
    /// The LSP server binary was not found in PATH.
    ServerNotFound { language: Language, advice: String },
    /// The initialize handshake failed.
    InitializeFailed { message: String },
    /// An operation timed out.
    Timeout {
        operation: String,
        duration: Duration,
    },
    /// The LSP server process crashed.
    ServerCrashed { exit_code: Option<i32> },
}

impl fmt::Display for LspError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ServerNotFound { language, advice } => {
                write!(f, "LSP server for {language} not found. {advice}")
            }
            Self::InitializeFailed { message } => {
                write!(f, "LSP initialize failed: {message}")
            }
            Self::Timeout {
                operation,
                duration,
            } => {
                write!(f, "LSP {operation} timed out after {duration:.1?}")
            }
            Self::ServerCrashed { exit_code } => match exit_code {
                Some(code) => write!(f, "LSP server crashed with exit code {code}"),
                None => write!(f, "LSP server crashed (no exit code)"),
            },
        }
    }
}

impl std::error::Error for LspError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_not_found_display_includes_advice() {
        let err = LspError::ServerNotFound {
            language: Language::Rust,
            advice: "Install: rustup component add rust-analyzer".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("rust"));
        assert!(msg.contains("rust-analyzer"));
    }

    #[test]
    fn initialize_failed_display() {
        let err = LspError::InitializeFailed {
            message: "server returned error".to_string(),
        };
        assert!(err.to_string().contains("server returned error"));
    }

    #[test]
    fn timeout_display() {
        let err = LspError::Timeout {
            operation: "initialize".to_string(),
            duration: Duration::from_secs(30),
        };
        let msg = err.to_string();
        assert!(msg.contains("initialize"));
        assert!(msg.contains("30"));
    }

    #[test]
    fn server_crashed_with_code() {
        let err = LspError::ServerCrashed { exit_code: Some(1) };
        assert!(err.to_string().contains("exit code 1"));
    }

    #[test]
    fn server_crashed_no_code() {
        let err = LspError::ServerCrashed { exit_code: None };
        assert!(err.to_string().contains("no exit code"));
    }
}
