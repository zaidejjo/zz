//! Formatter error types.

use std::path::PathBuf;
use thiserror::Error;

/// Errors that can occur while formatting.
///
/// `Verify` is the critical one: it means the formatter produced output
/// whose AST does not match the input's. This should never happen in
/// practice; if it does the formatter refuses to write the bad output.
#[derive(Debug, Error)]
pub enum FmtError {
    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("parse errors in {path} (refusing to format)")]
    Parse {
        path: PathBuf,
        /// Human-readable summary of the parse errors.
        summary: String,
    },

    #[error(
        "verification failed: formatted AST does not match input AST\n  \
         path: {path}\n  \
         mismatch: {mismatch}"
    )]
    Verify { path: PathBuf, mismatch: String },

    #[error("directory walk failed: {0}")]
    Walk(String),

    #[error("invalid argument: {0}")]
    Arg(String),
}

impl FmtError {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        FmtError::Io {
            path: path.into(),
            source,
        }
    }
}
