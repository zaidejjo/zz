//! `zz_fmt` — production-grade source code formatter for the ZZ language.
//!
//! Pipeline:
//!   1. Lex source → lossless token stream (trivia attached).
//!   2. Parse → AST with spans.
//!   3. Classify trivia (comments, blank lines, doc-comments).
//!   4. Lower AST + trivia → Doc IR (zero-copy `Doc<'src>`).
//!   5. Render Doc via Wadler best-fit pretty-printer.
//!   6. Verify: re-parse output, compare structural fingerprint.
//!   7. Diff + write (or report).
//!
//! Public entry points are re-exported at the crate root.

pub mod config;
pub mod diff;
pub mod doc;
pub mod error;
pub mod fs;
pub mod ir;
pub mod parallel;
pub mod pipeline;
pub mod printer;
pub mod trivia;
pub mod verify;

pub use config::{FmtConfig, LineEnding, SemiStyle, TrailingComma};
pub use error::FmtError;
pub use fs::discover;
pub use parallel::{format_paths, format_paths_parallel, format_sources_parallel};
pub use pipeline::{format_file, format_source, FormattedFile, TriviaReport};
