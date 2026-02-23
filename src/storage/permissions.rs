use anyhow::Result;
use std::fs::{self, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Create a directory (and parents) with owner-only permissions (0700).
/// If the directory already exists, its permissions are updated.
pub fn create_dir_restricted(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, Permissions::from_mode(0o700))?;
    Ok(())
}

/// Set owner-only read/write permissions (0600) on an existing file.
pub fn set_file_restricted(path: &Path) -> Result<()> {
    fs::set_permissions(path, Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn create_dir_restricted_sets_0700() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("secure");
        create_dir_restricted(&dir).unwrap();

        let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "expected 0700, got {:o}", mode);
    }

    #[test]
    fn create_dir_restricted_with_nested_parents() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("a").join("b").join("c");
        create_dir_restricted(&dir).unwrap();

        let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "expected 0700, got {:o}", mode);
    }

    #[test]
    fn create_dir_restricted_updates_existing() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("existing");
        fs::create_dir_all(&dir).unwrap();
        // Default permissions may be 0755; update to 0700
        create_dir_restricted(&dir).unwrap();

        let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "expected 0700, got {:o}", mode);
    }

    #[test]
    fn set_file_restricted_sets_0600() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("secret.txt");
        fs::write(&file, "sensitive data").unwrap();

        set_file_restricted(&file).unwrap();

        let mode = fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0600, got {:o}", mode);
    }
}
