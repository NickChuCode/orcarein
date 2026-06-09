//! Errors for the hardware layer.

/// Anything that can go wrong loading a profile, validating args, talking to a
/// transport, or driving the Python sidecar.
#[derive(Debug, thiserror::Error)]
pub enum HardwareError {
    #[error("profile parse error: {0}")]
    Parse(String),

    #[error("validation error: {0}")]
    Validation(String),

    #[error("transport error: {0}")]
    Transport(String),

    #[error("sidecar error: {0}")]
    Sidecar(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
