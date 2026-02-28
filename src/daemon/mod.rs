pub mod lifecycle;
pub mod server;

use std::path::Path;
use std::time::Duration;

use tracing::info;

use crate::detect;

/// Default idle timeout: 30 minutes
const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 30 * 60;

/// Run the daemon for the given project root. Handles full lifecycle:
/// PID file, socket server, cleanup on exit.
///
/// # Errors
/// Returns an error if the daemon fails to start (PID conflict, socket bind failure).
pub async fn run_daemon(project_root: &Path) -> anyhow::Result<()> {
    let socket_path = detect::socket_path(project_root);
    let pid_path = lifecycle::pid_path(&socket_path);

    lifecycle::acquire_pid_file(&pid_path)?;
    info!("daemon starting for {}", project_root.display());

    let idle_timeout = Duration::from_secs(
        std::env::var("KRAIT_IDLE_TIMEOUT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_IDLE_TIMEOUT_SECS),
    );

    let result = server::run_server(&socket_path, idle_timeout, project_root).await;

    lifecycle::cleanup(&socket_path, &pid_path);
    info!("daemon stopped");

    result
}
