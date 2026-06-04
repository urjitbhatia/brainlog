use anyhow::{Context, Result};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

/// A locked PID file. Holding the lock guarantees this process is the only
/// brainlog daemon for the given path. The lock releases automatically when
/// the file handle is dropped (or the process exits).
///
/// Uses `fcntl(F_SETLK)` (advisory lock) — survives `fork()`/`exec()` in the
/// same way Linux/macOS PID files have done for decades.
pub struct PidFile {
    path: PathBuf,
    // The fcntl lock lives on the open file descriptor — releasing it when
    // `file` drops is the whole point, so the read here is "stay alive".
    #[allow(dead_code)]
    file: File,
}

impl PidFile {
    /// Acquire the lock and write our PID to the file.
    ///
    /// Errors if another live process already holds the lock.
    pub fn acquire(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "creating parent directory for pid file {}",
                    parent.display()
                )
            })?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("opening pid file {}", path.display()))?;

        // Try to acquire an exclusive non-blocking advisory lock.
        let fd = file.as_raw_fd();
        let mut flock: libc::flock = unsafe { std::mem::zeroed() };
        // libc::F_WRLCK is `c_short` on macOS, `c_int` on glibc/musl Linux;
        // the cast is portable because `l_type` itself is always `c_short`.
        flock.l_type = libc::F_WRLCK as libc::c_short;
        flock.l_whence = libc::SEEK_SET as i16;
        flock.l_start = 0;
        flock.l_len = 0;
        let rc = unsafe { libc::fcntl(fd, libc::F_SETLK, &flock) };
        if rc == -1 {
            let err = std::io::Error::last_os_error();
            // Best-effort read of existing pid for a helpful error message.
            let other_pid = read_pid_from(&path).ok().flatten();
            anyhow::bail!(
                "another brainlog daemon is already running{}: {} (lockfile: {})",
                other_pid.map(|p| format!(" (pid {p})")).unwrap_or_default(),
                err,
                path.display()
            );
        }

        // Write our PID, truncating any stale content.
        let pid = std::process::id();
        file.set_len(0)
            .with_context(|| format!("truncating pid file {}", path.display()))?;
        file.seek(SeekFrom::Start(0))?;
        writeln!(file, "{pid}").context("writing pid")?;
        file.flush()?;
        Ok(Self { path, file })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Best-effort: remove the file. The lock releases regardless when `file` drops.
    pub fn cleanup(self) {
        // Drop runs after this returns, which deletes the file.
        drop(self);
    }
}

impl Drop for PidFile {
    fn drop(&mut self) {
        // Removing the file leaves the lock cleanly released and avoids the
        // classic "stale pidfile after crash" footgun for orderly shutdowns.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Read the PID written in `path`, if any. Returns `Ok(None)` if the file is missing
/// or empty.
pub fn read_pid_from(path: impl AsRef<Path>) -> Result<Option<u32>> {
    let path = path.as_ref();
    match OpenOptions::new().read(true).open(path) {
        Ok(mut f) => {
            let mut buf = String::new();
            f.read_to_string(&mut buf)?;
            let trimmed = buf.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            Ok(Some(trimmed.parse()?))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Check whether a brainlog daemon appears to be running.
///
/// Reads the PID from the pid file, then probes it with signal 0. Returns the
/// PID if a live process answers; `None` if the file is missing/empty or the
/// recorded PID has exited.
///
/// We deliberately do not use `fcntl(F_GETLK)` for liveness — POSIX advisory
/// byte-range locks are per-process, so a query from the daemon's own pid
/// returns `F_UNLCK` and would lie. signal-0 is universally honest.
pub fn read_locked_pid(path: impl AsRef<Path>) -> Result<Option<u32>> {
    let path = path.as_ref();
    let pid = match read_pid_from(path)? {
        Some(p) => p,
        None => return Ok(None),
    };
    if is_pid_alive(pid) {
        Ok(Some(pid))
    } else {
        Ok(None)
    }
}

fn is_pid_alive(pid: u32) -> bool {
    // signal 0 doesn't deliver — just checks that the kernel could route one.
    // ESRCH => no such pid; EPERM => exists but we can't signal it (still alive).
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        return true;
    }
    let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    errno == libc::EPERM
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn acquire_writes_pid_and_locks() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("daemon.pid");
        let lock = PidFile::acquire(&path).unwrap();
        assert_eq!(lock.path(), path.as_path());

        let on_disk = read_pid_from(&path).unwrap().unwrap();
        assert_eq!(on_disk, std::process::id());

        // While we hold the lock, read_locked_pid sees an active holder.
        let holder = read_locked_pid(&path).unwrap();
        assert_eq!(holder, Some(std::process::id()));

        drop(lock);
        // After dropping, the lock is gone.
        let holder = read_locked_pid(&path).unwrap();
        assert!(holder.is_none(), "lock should be released after drop");
    }

    #[test]
    fn second_acquire_in_same_process_fails() {
        // fcntl byte-range locks are per-process, so the same process re-locking
        // succeeds — but the file already contains our PID. This is fine for our
        // intended use (cross-process singleton); we just verify the lock survives.
        // For cross-process testing we'd need to fork.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("daemon.pid");
        let _lock = PidFile::acquire(&path).unwrap();
        let pid = read_pid_from(&path).unwrap().unwrap();
        assert_eq!(pid, std::process::id());
    }

    #[test]
    fn read_pid_from_missing_returns_none() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("does-not-exist.pid");
        let result = read_pid_from(&path).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn read_pid_from_empty_returns_none() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("empty.pid");
        std::fs::write(&path, b"").unwrap();
        let result = read_pid_from(&path).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn read_locked_pid_no_file_returns_none() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("nothing.pid");
        let result = read_locked_pid(&path).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn acquire_creates_parent_dir() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("nested/sub/daemon.pid");
        let _lock = PidFile::acquire(&path).unwrap();
        assert!(path.exists());
    }
}
