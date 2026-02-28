use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum LifecycleError {
    #[error("Daemon already running (pid {pid})")]
    AlreadyRunning { pid: u32 },
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// PID file path derived from socket path: `/tmp/krait-<hash>.pid`
#[must_use]
pub fn pid_path(socket_path: &Path) -> PathBuf {
    socket_path.with_extension("pid")
}

/// Write current process PID to file. Checks for an existing live process first.
///
/// # Errors
/// Returns `LifecycleError::AlreadyRunning` if a daemon is already running,
/// or `LifecycleError::Io` on filesystem errors.
pub fn acquire_pid_file(pid_path: &Path) -> Result<(), LifecycleError> {
    if let Ok(contents) = std::fs::read_to_string(pid_path) {
        if let Ok(pid) = contents.trim().parse::<u32>() {
            if is_process_alive(pid) {
                return Err(LifecycleError::AlreadyRunning { pid });
            }
        }
        // Stale PID file — remove it
        let _ = std::fs::remove_file(pid_path);
    }

    let pid = std::process::id();
    std::fs::write(pid_path, pid.to_string())?;
    Ok(())
}

/// Remove PID file and socket file.
pub fn cleanup(socket_path: &Path, pid_path: &Path) {
    let _ = std::fs::remove_file(socket_path);
    let _ = std::fs::remove_file(pid_path);
}

fn is_process_alive(pid: u32) -> bool {
    // SAFETY: kill(pid, 0) checks if process exists without sending a signal
    unsafe { libc::kill(pid.cast_signed(), 0) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_path_from_socket_path() {
        let sock = PathBuf::from("/tmp/krait-abc123.sock");
        assert_eq!(pid_path(&sock), PathBuf::from("/tmp/krait-abc123.pid"));
    }

    #[test]
    fn acquire_pid_file_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.pid");

        acquire_pid_file(&path).unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents, std::process::id().to_string());
    }

    #[test]
    fn acquire_pid_file_rejects_live_process() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.pid");

        // Write our own PID (which is alive)
        std::fs::write(&path, std::process::id().to_string()).unwrap();

        let result = acquire_pid_file(&path);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already running"));
    }

    #[test]
    fn acquire_pid_file_cleans_stale() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.pid");

        // Write a PID that definitely doesn't exist
        std::fs::write(&path, "999999999").unwrap();

        acquire_pid_file(&path).unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents, std::process::id().to_string());
    }

    #[test]
    fn cleanup_removes_files() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("test.sock");
        let pid = dir.path().join("test.pid");

        std::fs::write(&sock, "").unwrap();
        std::fs::write(&pid, "").unwrap();

        cleanup(&sock, &pid);

        assert!(!sock.exists());
        assert!(!pid.exists());
    }
}
