use anyhow::{anyhow, Result};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::Path;

#[derive(Debug)]
pub struct WriteGuard {
    _file: File,
}

/// Exclusive non-blocking flock on `dir/palace.write.lock`.
/// Released when the guard (and thus the process fd) is dropped.
pub fn try_acquire(dir: &str) -> Result<WriteGuard> {
    std::fs::create_dir_all(dir)?;
    let path = Path::new(dir).join("palace.write.lock");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)?;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        return Err(anyhow!(
            "PalaceLocked: another mempalace process holds the writer lock. \
             If a resident MCP server (e.g. via llmserverplus) is running, \
             perform writes through its mempalace_* tools, or retry when it is idle."
        ));
    }
    let _ = writeln!(file, "{}", std::process::id());
    Ok(WriteGuard { _file: file })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_second_writer_refused() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().to_str().unwrap();
        let _g1 = try_acquire(path).unwrap();
        let err = try_acquire(path).unwrap_err();
        assert!(err.to_string().contains("PalaceLocked"));
    }

    #[test]
    fn test_palace_locked_error_has_guidance() {
        // Phase 28: the busy-lock message must tell the user what to do —
        // a resident MCP server (llmserverplus) may legitimately hold the
        // lock; CLI users should be pointed at the MCP tools or a retry.
        let dir = TempDir::new().unwrap();
        let path = dir.path().to_str().unwrap();
        let _g = try_acquire(path).unwrap();
        let err = try_acquire(path).unwrap_err().to_string();
        assert!(err.contains("PalaceLocked"));
        assert!(
            err.contains("MCP") || err.contains("server"),
            "guidance missing: {err}"
        );
        assert!(
            err.to_lowercase().contains("retry"),
            "retry hint missing: {err}"
        );
    }

    #[test]
    fn test_stale_lock_stolen_when_pid_dead() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().to_str().unwrap();
        std::fs::write(dir.path().join("palace.write.lock"), "1\n").unwrap();
        let _g = try_acquire(path).expect("flock should succeed on a stale unlocked file");
    }
}
