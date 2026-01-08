use thiserror::Error;

#[derive(Error, Debug)]
pub enum ProbeError {
    #[error("Probe evaluation failed: {0}")]
    EvaluationFailed(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Regex error: {0}")]
    Regex(#[from] regex::Error),

    #[error("Command execution failed: {0}")]
    CommandFailed(String),
}
