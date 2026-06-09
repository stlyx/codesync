use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use fs4::FileExt as Fs4FileExt;
use tracing::warn;

use crate::error::{CodeSyncError, Result};

pub struct FileLock {
    file: File,
    path: PathBuf,
}

impl FileLock {
    pub fn acquire(path: &Path) -> Result<Self> {
        if let Some(parent) = parent_dir_to_create(path) {
            std::fs::create_dir_all(parent)
                .map_err(|source| CodeSyncError::io_error(parent.to_path_buf(), source))?;
        }

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|source| CodeSyncError::io_error(path.to_path_buf(), source))?;

        Fs4FileExt::lock(&file)
            .map_err(|source| CodeSyncError::io_error(path.to_path_buf(), source))?;

        Ok(Self {
            file,
            path: path.to_path_buf(),
        })
    }
}

fn parent_dir_to_create(path: &Path) -> Option<&Path> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
}

impl Drop for FileLock {
    fn drop(&mut self) {
        if let Err(source) = Fs4FileExt::unlock(&self.file) {
            warn!(path = %self.path.display(), error = %source, "failed to unlock file");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::{Mutex, OnceLock};

    use tempfile::tempdir;

    use super::{FileLock, parent_dir_to_create};

    static CURRENT_DIR_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    #[test]
    fn bare_relative_path_has_no_parent_dir_to_create() {
        assert_eq!(parent_dir_to_create(Path::new("sync-test.lock")), None);
    }

    #[test]
    fn file_lock_accepts_bare_relative_path() {
        let _guard = CURRENT_DIR_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("current directory test lock should not be poisoned");
        let original_dir = std::env::current_dir().expect("current dir should be readable");
        let tempdir = tempdir().expect("tempdir should be created");
        std::env::set_current_dir(tempdir.path()).expect("test current dir should be set");

        let result = FileLock::acquire(Path::new("sync-test.lock"));

        std::env::set_current_dir(original_dir).expect("original current dir should be restored");
        let _lock = result.expect("bare relative lock path should be accepted");
        assert!(tempdir.path().join("sync-test.lock").exists());
    }
}
