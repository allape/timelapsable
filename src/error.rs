//! Application-wide error helpers and process exit codes.

use std::process::ExitCode;

/// Runtime failure that has no more specific exit code.
pub const EXIT_GENERAL: u8 = 1;
/// Invalid command-line arguments or argument values.
pub const EXIT_USAGE: u8 = 2;
/// Stabilization was requested, but a frame was too blurred or too difficult to align.
pub const EXIT_STABILIZATION: u8 = 4;

/// An error raised when stabilization cannot produce a safe crop.
#[derive(Debug, thiserror::Error)]
#[error("stabilization failed: {0}")]
pub struct StabilizationError(pub String);

/// An error raised when a parsed value is valid TOML/CLI syntax but not a usable option.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct UsageError(pub String);

/// Maps an error chain to the documented process exit status.
pub fn exit_code_for(error: &anyhow::Error) -> u8 {
    if error.downcast_ref::<StabilizationError>().is_some() {
        EXIT_STABILIZATION
    } else if error.downcast_ref::<UsageError>().is_some() {
        EXIT_USAGE
    } else {
        EXIT_GENERAL
    }
}

/// Converts an exit status to a [`std::process::ExitCode`].
pub fn exit_code(status: u8) -> ExitCode {
    ExitCode::from(status)
}
