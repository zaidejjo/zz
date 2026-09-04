//! Parallel driver (M5). M0/M1 use the sequential path; this module
//! exposes the same surface and is a placeholder until rayon is wired.

use crate::error::FmtError;
use crate::pipeline::{format_file, FormattedFile};
use crate::FmtConfig;
use std::path::PathBuf;

/// Format all paths sequentially. Parallel implementation lands in M5.
pub fn format_paths(paths: &[PathBuf], config: &FmtConfig) -> Vec<Result<FormattedFile, FmtError>> {
    paths.iter().map(|p| format_file(p, config)).collect()
}
