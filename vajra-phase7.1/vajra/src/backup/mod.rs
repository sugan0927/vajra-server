//! Auto-backup — policy is in `[backup]` config, execution is here.
//!
//! Day 24: local destination (mysqldump + tar of wp-content).
//! Day 25: S3 destination (rust-s3, blocking).
//!
//! See `docs/backup.md` for the design.

pub mod local;
pub mod s3;
pub mod scheduler;

use std::path::PathBuf;

/// What a single backup run produced, for logging + metrics.
#[derive(Debug, Clone)]
pub struct BackupReport {
    /// Absolute path to the archive that was written locally (if any).
    pub local_path: Option<PathBuf>,
    /// Size in bytes of the archive.
    pub archive_bytes: u64,
    /// How many old archives were pruned from the local dir.
    pub pruned_local: usize,
    /// S3 object key that was written (Day 25), if any.
    pub s3_key: Option<String>,
    /// How many old objects were pruned from S3 (Day 25).
    pub pruned_s3: usize,
}

/// Errors from a backup run. Kept simple on purpose — the executor
/// (`vajra backup run`) prints these to stderr and exits non-zero.
#[derive(Debug)]
pub enum BackupError {
    Io(std::io::Error),
    Mysqldump(String),
    Config(String),
    S3(String),
}

impl std::fmt::Display for BackupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackupError::Io(e) => write!(f, "io: {e}"),
            BackupError::Mysqldump(s) => write!(f, "mysqldump: {s}"),
            BackupError::Config(s) => write!(f, "config: {s}"),
            BackupError::S3(s) => write!(f, "s3: {s}"),
        }
    }
}

impl std::error::Error for BackupError {}

impl From<std::io::Error> for BackupError {
    fn from(e: std::io::Error) -> Self {
        BackupError::Io(e)
    }
}
