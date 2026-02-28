use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tracing::debug;

use crate::protocol::{Request, Response};

const MAX_RETRIES: u32 = 3;
const RETRY_DELAYS_MS: [u64; 3] = [100, 200, 500];

pub struct DaemonClient {
    stream: UnixStream,
}

impl DaemonClient {
    /// Connect to a running daemon at the given socket path.
    ///
    /// # Errors
    /// Returns an error if the connection fails.
    pub async fn connect(socket_path: &Path) -> anyhow::Result<Self> {
        let stream = UnixStream::connect(socket_path)
            .await
            .with_context(|| format!("failed to connect to daemon at {}", socket_path.display()))?;
        Ok(Self { stream })
    }

    /// Connect to the daemon, auto-starting it if not running.
    ///
    /// # Errors
    /// Returns an error if the daemon cannot be started or connected to.
    pub async fn connect_or_start(socket_path: &Path) -> anyhow::Result<Self> {
        if let Ok(client) = Self::connect(socket_path).await {
            return Ok(client);
        }

        debug!("daemon not running, starting it");
        spawn_daemon()?;

        for (attempt, delay_ms) in RETRY_DELAYS_MS.iter().enumerate() {
            tokio::time::sleep(Duration::from_millis(*delay_ms)).await;
            match Self::connect(socket_path).await {
                Ok(client) => {
                    debug!("connected after {} retries", attempt + 1);
                    return Ok(client);
                }
                Err(e) if attempt == (MAX_RETRIES as usize - 1) => return Err(e),
                Err(_) => {}
            }
        }

        bail!(
            "Daemon failed to start after {MAX_RETRIES} attempts. \
             Run `krait daemon start` manually for debug output."
        )
    }

    /// Send a request and receive the response.
    ///
    /// # Errors
    /// Returns an error on IO or serialization failure.
    pub async fn send(&mut self, request: &Request) -> anyhow::Result<Response> {
        let json = serde_json::to_vec(request)?;
        let len = u32::try_from(json.len())?;

        self.stream.write_u32(len).await?;
        self.stream.write_all(&json).await?;
        self.stream.flush().await?;

        let resp_len = self.stream.read_u32().await?;
        if resp_len > crate::protocol::MAX_FRAME_SIZE {
            anyhow::bail!("oversized response frame: {resp_len} bytes");
        }
        let mut buf = vec![0u8; resp_len as usize];
        self.stream.read_exact(&mut buf).await?;

        let response = serde_json::from_slice(&buf)?;
        Ok(response)
    }
}

/// Spawn the daemon as a detached background process.
fn spawn_daemon() -> anyhow::Result<()> {
    let exe = std::env::current_exe().context("failed to get current executable path")?;

    std::process::Command::new(exe)
        .args(["daemon", "start"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("failed to spawn daemon process")?;

    Ok(())
}

/// Convert CLI command to protocol request.
#[must_use]
pub fn command_to_request(command: &crate::cli::Command) -> Request {
    use crate::cli::{Command, EditCommand, FindCommand, ListCommand, ReadCommand};

    match command {
        Command::Init { .. } => unreachable!("init is handled locally"),
        Command::Status => Request::Status,
        Command::Check { path, errors_only } => Request::Check {
            path: path.clone(),
            errors_only: *errors_only,
        },
        Command::Find(FindCommand::Symbol {
            name,
            path,
            src_only,
            include_body,
        }) => Request::FindSymbol {
            name: name.clone(),
            path_filter: path.clone(),
            src_only: *src_only,
            include_body: *include_body,
        },
        Command::Find(FindCommand::Refs { name, with_symbol }) => Request::FindRefs {
            name: name.clone(),
            with_symbol: *with_symbol,
        },
        Command::Find(FindCommand::Impl { name }) => Request::FindImpl { name: name.clone() },
        Command::List(ListCommand::Symbols { path, depth }) => Request::ListSymbols {
            path: path.clone(),
            depth: *depth,
        },
        Command::Read(ReadCommand::File {
            path,
            from,
            to,
            max_lines,
        }) => Request::ReadFile {
            path: path.clone(),
            from: *from,
            to: *to,
            max_lines: *max_lines,
        },
        Command::Read(ReadCommand::Symbol {
            name,
            signature_only,
            max_lines,
            path,
            has_body,
        }) => Request::ReadSymbol {
            name: name.clone(),
            signature_only: *signature_only,
            max_lines: *max_lines,
            path_filter: path.clone(),
            has_body: *has_body,
        },
        Command::Edit(EditCommand::Replace { symbol }) => Request::EditReplace {
            symbol: symbol.clone(),
            code: String::new(), // stdin will be read separately
        },
        Command::Edit(EditCommand::InsertAfter { symbol }) => Request::EditInsertAfter {
            symbol: symbol.clone(),
            code: String::new(),
        },
        Command::Edit(EditCommand::InsertBefore { symbol }) => Request::EditInsertBefore {
            symbol: symbol.clone(),
            code: String::new(),
        },
        Command::Daemon(_) => unreachable!("daemon commands are handled directly"),
        Command::Hover { name } => Request::Hover { name: name.clone() },
        Command::Format { path } => Request::Format { path: path.clone() },
        Command::Rename { symbol, new_name } => Request::Rename {
            name: symbol.clone(),
            new_name: new_name.clone(),
        },
        Command::Fix { path } => Request::Fix { path: path.clone() },
        Command::Watch { .. } => unreachable!("watch is handled client-side"),
        Command::Search { .. } => unreachable!("search is handled client-side"),
        Command::Server(_) => unreachable!("server commands are handled client-side"),
    }
}

/// Format a PID file path from a socket path for display purposes.
#[must_use]
pub fn pid_path_from_socket(socket_path: &Path) -> PathBuf {
    socket_path.with_extension("pid")
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::daemon::server::run_server;

    #[tokio::test]
    async fn client_connects_to_running_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("test.sock");
        let dir_root = dir.path().to_path_buf();

        let sock_clone = sock.clone();
        let _handle = tokio::spawn(async move {
            run_server(&sock_clone, Duration::from_secs(5), &dir_root)
                .await
                .unwrap();
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut client = DaemonClient::connect(&sock).await.unwrap();
        let resp = client.send(&Request::Status).await.unwrap();
        assert!(resp.success);

        // Clean up
        let mut client = DaemonClient::connect(&sock).await.unwrap();
        client.send(&Request::DaemonStop).await.unwrap();
    }

    #[tokio::test]
    async fn client_stop_shuts_down_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("test.sock");
        let dir_root = dir.path().to_path_buf();

        let sock_clone = sock.clone();
        let handle = tokio::spawn(async move {
            run_server(&sock_clone, Duration::from_secs(5), &dir_root)
                .await
                .unwrap();
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut client = DaemonClient::connect(&sock).await.unwrap();
        let resp = client.send(&Request::DaemonStop).await.unwrap();
        assert!(resp.success);

        let _ = handle.await;
    }

    #[test]
    fn command_to_request_list_symbols() {
        use crate::cli::{Command, ListCommand};
        use crate::protocol::Request;
        use std::path::PathBuf;

        let cmd = Command::List(ListCommand::Symbols {
            path: PathBuf::from("src/lib.rs"),
            depth: 2,
        });
        let req = command_to_request(&cmd);
        assert!(matches!(req, Request::ListSymbols { depth: 2, .. }));
    }

    #[test]
    fn command_to_request_read_file() {
        use crate::cli::{Command, ReadCommand};
        use crate::protocol::Request;
        use std::path::PathBuf;

        let cmd = Command::Read(ReadCommand::File {
            path: PathBuf::from("main.rs"),
            from: Some(1),
            to: Some(10),
            max_lines: None,
        });
        let req = command_to_request(&cmd);
        assert!(matches!(
            req,
            Request::ReadFile {
                from: Some(1),
                to: Some(10),
                ..
            }
        ));
    }

    #[test]
    fn command_to_request_read_symbol() {
        use crate::cli::{Command, ReadCommand};
        use crate::protocol::Request;

        let cmd = Command::Read(ReadCommand::Symbol {
            name: "Config".into(),
            signature_only: true,
            max_lines: Some(20),
            path: None,
            has_body: false,
        });
        let req = command_to_request(&cmd);
        assert!(matches!(
            req,
            Request::ReadSymbol {
                signature_only: true,
                max_lines: Some(20),
                ..
            }
        ));
    }

    #[tokio::test]
    async fn handle_connection_rejects_oversized_frame() {
        use crate::daemon::server::run_server;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::UnixStream;

        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("test.sock");
        let dir_root = dir.path().to_path_buf();

        let sock_clone = sock.clone();
        let _handle = tokio::spawn(async move {
            run_server(&sock_clone, Duration::from_secs(5), &dir_root)
                .await
                .unwrap();
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Send an oversized frame (20 MB > 10 MB limit)
        let mut stream = UnixStream::connect(&sock).await.unwrap();
        let oversized_len: u32 = 20 * 1024 * 1024;
        stream.write_u32(oversized_len).await.unwrap();
        stream.flush().await.unwrap();

        // The connection should be closed by the server (we won't get a valid response)
        let result = stream.read_u32().await;
        assert!(
            result.is_err(),
            "server should close connection on oversized frame"
        );

        // Clean up
        if let Ok(mut client) = DaemonClient::connect(&sock).await {
            let _ = client.send(&Request::DaemonStop).await;
        }
    }

    #[tokio::test]
    async fn client_connect_fails_without_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("nonexistent.sock");

        let result = DaemonClient::connect(&sock).await;
        assert!(result.is_err());
    }
}
