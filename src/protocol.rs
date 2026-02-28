use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Max frame size: 10 MB
pub(crate) const MAX_FRAME_SIZE: u32 = 10 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Status,
    DaemonStop,
    Init,
    Check {
        path: Option<PathBuf>,
        /// If true, suppress warnings and hints.
        errors_only: bool,
    },
    FindSymbol {
        name: String,
        /// Substring filter applied to result paths (for disambiguation)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path_filter: Option<String>,
        /// Exclude noise paths (www/, dist/, `node_modules`/, .d.ts, .mdx)
        #[serde(default)]
        src_only: bool,
        /// Include full symbol body in each result
        #[serde(default)]
        include_body: bool,
    },
    FindImpl {
        name: String,
    },
    FindRefs {
        name: String,
        /// Enrich each reference with its containing symbol
        #[serde(default)]
        with_symbol: bool,
    },
    ListSymbols {
        path: PathBuf,
        depth: u8,
    },
    ReadFile {
        path: PathBuf,
        from: Option<u32>,
        to: Option<u32>,
        max_lines: Option<u32>,
    },
    ReadSymbol {
        name: String,
        signature_only: bool,
        max_lines: Option<u32>,
        /// Substring filter to select the right definition when multiple exist
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path_filter: Option<String>,
        /// Skip overload stubs and return the real implementation body
        #[serde(default)]
        has_body: bool,
    },
    EditReplace {
        symbol: String,
        code: String,
    },
    EditInsertAfter {
        symbol: String,
        code: String,
    },
    EditInsertBefore {
        symbol: String,
        code: String,
    },
    Hover {
        name: String,
    },
    Format {
        path: PathBuf,
    },
    Rename {
        name: String,
        new_name: String,
    },
    Fix {
        path: Option<PathBuf>,
    },
    /// Get running LSP server status from daemon
    ServerStatus,
    /// Restart a language server in the daemon
    ServerRestart {
        language: String,
    },
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Response {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorPayload>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct ErrorPayload {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub advice: Option<String>,
}

impl Response {
    #[must_use]
    pub fn ok(data: serde_json::Value) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn err(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(ErrorPayload {
                code: code.into(),
                message: message.into(),
                advice: None,
            }),
        }
    }

    pub fn err_with_advice(
        code: impl Into<String>,
        message: impl Into<String>,
        advice: impl Into<String>,
    ) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(ErrorPayload {
                code: code.into(),
                message: message.into(),
                advice: Some(advice.into()),
            }),
        }
    }

    #[must_use]
    pub fn not_implemented() -> Self {
        Self::err("not_implemented", "This command is not yet implemented")
    }
}

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("Frame exceeds maximum size of {MAX_FRAME_SIZE} bytes (got {size})")]
    Oversized { size: u32 },
    #[error("Incomplete frame: expected {expected} bytes, got {got}")]
    Incomplete { expected: u32, got: usize },
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Encode a serializable value into a length-prefixed frame.
/// Format: `[4-byte big-endian length][JSON payload]`
///
/// # Errors
/// Returns `FrameError::Oversized` if the payload exceeds 10 MB,
/// or `FrameError::Json` if serialization fails.
pub fn encode_frame<T: Serialize>(value: &T) -> Result<Vec<u8>, FrameError> {
    let json = serde_json::to_vec(value)?;

    let len = u32::try_from(json.len()).map_err(|_| FrameError::Oversized { size: u32::MAX })?;

    if len > MAX_FRAME_SIZE {
        return Err(FrameError::Oversized { size: len });
    }

    let mut buf = Vec::with_capacity(4 + json.len());
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(&json);
    Ok(buf)
}

/// Decode a length-prefixed frame into a deserialized value.
/// Returns the value and the number of bytes consumed.
///
/// # Errors
/// Returns `FrameError::Incomplete` if the buffer is too short,
/// `FrameError::Oversized` if the declared length exceeds 10 MB,
/// or `FrameError::Json` if deserialization fails.
pub fn decode_frame<T: for<'de> Deserialize<'de>>(buf: &[u8]) -> Result<(T, usize), FrameError> {
    if buf.len() < 4 {
        return Err(FrameError::Incomplete {
            expected: 4,
            got: buf.len(),
        });
    }

    let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);

    if len > MAX_FRAME_SIZE {
        return Err(FrameError::Oversized { size: len });
    }

    let total = 4 + len as usize;
    if buf.len() < total {
        return Err(FrameError::Incomplete {
            expected: len,
            got: buf.len() - 4,
        });
    }

    let value = serde_json::from_slice(&buf[4..total])?;
    Ok((value, total))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn request_roundtrip_serialization() {
        let requests = vec![
            Request::Status,
            Request::DaemonStop,
            Request::Init {},
            Request::Check {
                path: None,
                errors_only: false,
            },
            Request::Check {
                path: Some(PathBuf::from("src/lib.rs")),
                errors_only: true,
            },
            Request::FindSymbol {
                name: "MyStruct".into(),
                path_filter: None,
                src_only: false,
                include_body: false,
            },
            Request::FindSymbol {
                name: "Foo".into(),
                path_filter: Some("packages/core".into()),
                src_only: true,
                include_body: false,
            },
            Request::FindRefs {
                name: "my_func".into(),
                with_symbol: false,
            },
            Request::FindRefs {
                name: "createStep".into(),
                with_symbol: true,
            },
            Request::ListSymbols {
                path: PathBuf::from("src/lib.rs"),
                depth: 1,
            },
            Request::ReadFile {
                path: PathBuf::from("src/main.rs"),
                from: Some(5),
                to: Some(10),
                max_lines: None,
            },
            Request::ReadSymbol {
                name: "Config".into(),
                signature_only: true,
                max_lines: Some(20),
                path_filter: None,
                has_body: false,
            },
            Request::ReadSymbol {
                name: "CreatePromotionDTO".into(),
                signature_only: false,
                max_lines: None,
                path_filter: Some("packages/core".into()),
                has_body: true,
            },
            Request::EditReplace {
                symbol: "greet".into(),
                code: "fn greet() {}".into(),
            },
            Request::EditInsertAfter {
                symbol: "greet".into(),
                code: "fn helper() {}".into(),
            },
            Request::EditInsertBefore {
                symbol: "greet".into(),
                code: "#[test]".into(),
            },
            Request::ServerStatus,
            Request::ServerRestart {
                language: "rust".into(),
            },
        ];

        for req in &requests {
            let json = serde_json::to_string(req).unwrap();
            let decoded: Request = serde_json::from_str(&json).unwrap();
            assert_eq!(*req, decoded, "roundtrip failed for {json}");
        }
    }

    #[test]
    fn response_roundtrip_serialization() {
        let responses = vec![
            Response::ok(json!({"pid": 1234})),
            Response::err("not_found", "Symbol not found"),
            Response::err_with_advice("lsp_not_found", "LSP not detected", "Install rust-analyzer"),
            Response::not_implemented(),
        ];

        for resp in &responses {
            let json = serde_json::to_string(resp).unwrap();
            let decoded: Response = serde_json::from_str(&json).unwrap();
            assert_eq!(*resp, decoded, "roundtrip failed for {json}");
        }
    }

    #[test]
    fn frame_encode_decode() {
        let req = Request::FindSymbol {
            name: "Foo".into(),
            path_filter: None,
            src_only: false,
            include_body: false,
        };
        let frame = encode_frame(&req).unwrap();
        let (decoded, consumed): (Request, usize) = decode_frame(&frame).unwrap();
        assert_eq!(decoded, req);
        assert_eq!(consumed, frame.len());
    }

    #[test]
    fn frame_empty_payload() {
        let req = Request::Status;
        let frame = encode_frame(&req).unwrap();
        let (decoded, _): (Request, usize) = decode_frame(&frame).unwrap();
        assert_eq!(decoded, req);
    }

    #[test]
    fn frame_large_payload() {
        let big_code = "x".repeat(1_000_000);
        let req = Request::EditReplace {
            symbol: "f".into(),
            code: big_code.clone(),
        };
        let frame = encode_frame(&req).unwrap();
        let (decoded, _): (Request, usize) = decode_frame(&frame).unwrap();
        assert_eq!(
            decoded,
            Request::EditReplace {
                symbol: "f".into(),
                code: big_code,
            }
        );
    }

    #[test]
    fn frame_rejects_oversized() {
        let huge = "x".repeat(11_000_000);
        let req = Request::EditReplace {
            symbol: "f".into(),
            code: huge,
        };
        let result = encode_frame(&req);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("exceeds maximum"));
    }

    #[test]
    fn frame_decode_incomplete_header() {
        let result: Result<(Request, usize), _> = decode_frame(&[0, 1]);
        assert!(result.is_err());
    }

    #[test]
    fn frame_decode_incomplete_payload() {
        let frame = encode_frame(&Request::Status).unwrap();
        let truncated = &frame[..frame.len() - 1];
        let result: Result<(Request, usize), _> = decode_frame(truncated);
        assert!(result.is_err());
    }
}
