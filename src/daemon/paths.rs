use std::path::{Path, PathBuf};

/// Filesystem locations for the brainlog daemon: socket and pid file.
///
/// Both live under the brainlog base directory (typically `~/.brainlog`),
/// so isolated tests setting `HOME` automatically get an isolated daemon.
#[derive(Debug, Clone)]
pub struct DaemonPaths {
    base_dir: PathBuf,
}

impl DaemonPaths {
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
        }
    }

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    pub fn socket_path(&self) -> PathBuf {
        self.base_dir.join("daemon.sock")
    }

    pub fn pid_file(&self) -> PathBuf {
        self.base_dir.join("daemon.pid")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_under_base_dir() {
        let paths = DaemonPaths::new("/tmp/brainlog-test");
        assert_eq!(
            paths.socket_path(),
            PathBuf::from("/tmp/brainlog-test/daemon.sock")
        );
        assert_eq!(
            paths.pid_file(),
            PathBuf::from("/tmp/brainlog-test/daemon.pid")
        );
        assert_eq!(paths.base_dir(), Path::new("/tmp/brainlog-test"));
    }

    #[test]
    fn paths_isolated_per_base() {
        let a = DaemonPaths::new("/tmp/a");
        let b = DaemonPaths::new("/tmp/b");
        assert_ne!(a.socket_path(), b.socket_path());
        assert_ne!(a.pid_file(), b.pid_file());
    }
}
