//! One server process per data directory.
//!
//! Room state lives in memory and the store has one writer, so two processes on one database would each order
//! the ops of a project on their own. An exclusive lock on a file in the data directory makes the second one
//! refuse to start. The lock goes away with the process, however it ends, so a crash leaves nothing to clean up.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};

pub struct DataLock {
    _file: File,
}

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("another dibibest-server is already using {0}")]
    Held(PathBuf),
    #[error("lock file {0}: {1}")]
    Io(PathBuf, std::io::Error),
}

impl DataLock {
    /// Creates `dir` if needed and takes the lock, or fails at once if another process holds it.
    pub fn acquire(dir: &Path) -> Result<Self, LockError> {
        std::fs::create_dir_all(dir).map_err(|e| LockError::Io(dir.to_path_buf(), e))?;
        let path = dir.join("dibibest.lock");
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| LockError::Io(path.clone(), e))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Err(LockError::Held(dir.to_path_buf())),
            Err(TryLockError::Error(e)) => return Err(LockError::Io(path, e)),
        }
        // The pid is only a hint for a person looking at the file; the lock is what counts.
        let _ = file.set_len(0);
        let _ = writeln!(file, "{}", std::process::id());
        Ok(Self { _file: file })
    }
}
