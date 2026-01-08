use std::path::Path;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum VmError {
    #[error("QEMU error: {0}")]
    Qemu(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Cloud-init error: {0}")]
    CloudInit(String),

    #[error("Serial communication error: {0}")]
    Serial(String),

    #[error("QMP error: {0}")]
    Qmp(String),

    #[error("Timeout: {0}")]
    Timeout(String),

    #[error("VM not found: {0}")]
    VmNotFound(String),

    #[error("No free port found")]
    NoFreePort,

    #[error("Directory error: {0}")]
    Directory(String),

    #[error("Invalid path: {0}")]
    InvalidPath(String),
}

/// Convert a `Path` to `&str` for use with external commands.
///
/// # Errors
/// Returns `VmError::InvalidPath` if the path contains invalid UTF-8.
pub fn path_to_str(path: &Path) -> Result<&str, VmError> {
    path.to_str()
        .ok_or_else(|| VmError::InvalidPath(path.display().to_string()))
}
