use thiserror::Error;

#[derive(Debug, Error)]
pub enum CodegenError {
    #[error("failed to initialise native target: {0}")]
    TargetInit(String),
    #[error("failed to look up target triple '{0}': {1}")]
    TargetLookup(String, String),
    #[error("failed to create target machine")]
    MachineCreation,
    #[error("failed to write object file: {0}")]
    WriteObject(String),
    #[error("unsupported feature: {0}")]
    Unsupported(String),
}
