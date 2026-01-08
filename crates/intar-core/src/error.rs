use thiserror::Error;

#[derive(Error, Debug)]
pub enum CoreError {
    #[error("Failed to parse HCL: {0}")]
    HclParse(String),

    #[error("Invalid scenario: {0}")]
    InvalidScenario(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Probe '{0}' not found in scenario")]
    ProbeNotFound(String),

    #[error("VM '{0}' not found in scenario")]
    VmNotFound(String),

    #[error("Image '{0}' not found in scenario")]
    ImageNotFound(String),
}
