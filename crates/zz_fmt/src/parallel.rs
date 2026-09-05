//! Parallel driver (M5).
//!
//! `format_paths_parallel` processes discovered `.zz` files concurrently
//! across all available CPU cores via `rayon`. The sequential
//! `format_paths` remains for single-file use or when parallelism is
//! undesirable.

use crate::error::FmtError;
use crate::pipeline::{format_file, format_source, FormattedFile};
use crate::FmtConfig;
use rayon::prelude::*;
use std::path::PathBuf;

/// Format all paths sequentially.
pub fn format_paths(paths: &[PathBuf], config: &FmtConfig) -> Vec<Result<FormattedFile, FmtError>> {
    paths.iter().map(|p| format_file(p, config)).collect()
}

/// Format all paths in parallel using a rayon thread-pool sized to
/// the number of available CPU cores. Results are returned in the same
/// order as the input slice.
pub fn format_paths_parallel(
    paths: &[PathBuf],
    config: &FmtConfig,
) -> Vec<Result<FormattedFile, FmtError>> {
    paths.par_iter().map(|p| format_file(p, config)).collect()
}

/// Format multiple source strings in parallel. Returns results in the
/// same order as the input slice. Pure — no disk I/O.
pub fn format_sources_parallel(
    sources: &[(&PathBuf, &str)],
    config: &FmtConfig,
) -> Vec<Result<String, FmtError>> {
    sources
        .par_iter()
        .map(|(_path, src)| format_source(src, config))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FmtConfig;
    use crate::fs;

    #[test]
    fn parallel_matches_sequential_via_source() {
        // Use format_source (pure) to verify parallel and sequential
        // produce identical output for each file, without touching disk.
        let fixtures_dir =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
        if !fixtures_dir.exists() {
            eprintln!("fixtures dir not found, skipping parallel comparison test");
            return;
        }

        let mut paths = fs::discover(&[fixtures_dir]).unwrap();
        paths.truncate(20);
        if paths.is_empty() {
            return;
        }

        let config = FmtConfig::default();

        // Read all files into memory.
        let sources: Vec<(PathBuf, String)> = paths
            .iter()
            .filter_map(|p| {
                std::fs::read_to_string(p)
                    .ok()
                    .map(|s| (p.clone(), s))
                    .filter(|(_, s)| !s.is_empty())
            })
            .collect();

        // Sequential: format one by one.
        let seq_results: Vec<String> = sources
            .iter()
            .filter_map(|(_p, src)| format_source(src, &config).ok())
            .collect();

        // Parallel: format all at once via rayon.
        let src_refs: Vec<(&PathBuf, &str)> =
            sources.iter().map(|(p, s)| (p, s.as_str())).collect();
        let par_results: Vec<String> = format_sources_parallel(&src_refs, &config)
            .into_iter()
            .filter_map(|r| r.ok())
            .collect();

        assert_eq!(
            seq_results.len(),
            par_results.len(),
            "both paths must process same files"
        );
        for (i, (s, p)) in seq_results.iter().zip(par_results.iter()).enumerate() {
            assert_eq!(s, p, "output mismatch at index {i}");
        }
    }

    #[test]
    fn parallel_handles_empty_input() {
        let config = FmtConfig::default();
        let results = format_paths_parallel(&[], &config);
        assert!(results.is_empty());
    }
}
