//! Formatter configuration.

/// Line ending policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineEnding {
    /// Detect from the first newline in the source; default `\n`.
    #[default]
    Auto,
    /// Force `\n`.
    Lf,
    /// Force `\r\n`.
    Crlf,
}

/// Trailing-comma policy. Multi-line-only matches Prettier/Go: trailing
/// comma appears when the group breaks across multiple lines, omitted
/// when the group fits on one line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TrailingComma {
    Never,
    /// Default. Trailing comma when the group breaks.
    #[default]
    MultiLineOnly,
    Always,
}

/// Semicolon policy. `Auto` preserves the source's choice; the other
/// variants force a consistent style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SemiStyle {
    #[default]
    Auto,
    Always,
    Never,
}

/// Top-level configuration for the formatter. Cheap to clone (Copy soon).
#[derive(Debug, Clone)]
pub struct FmtConfig {
    /// Target line width. Groups attempt to fit on one line up to this
    /// many characters. Default: 100.
    pub line_width: usize,

    /// Number of spaces per indentation level. Default: 4.
    pub indent_width: usize,

    /// If `true`, indent with `\t` instead of spaces. `indent_width` then
    /// controls the number of tabs per level.
    pub use_tabs: bool,

    /// Newline style.
    pub line_ending: LineEnding,

    /// Trailing-comma policy for multi-line containers.
    pub trailing_comma: TrailingComma,

    /// Semicolon policy.
    pub semicolons: SemiStyle,

    /// Collapse runs of ≥2 blank lines between top-level items down to 1.
    /// Default: true.
    pub collapse_blank_lines: bool,

    /// Preserve a trailing newline at end of file. Default: true.
    pub trailing_newline: bool,
}

impl Default for FmtConfig {
    fn default() -> Self {
        Self {
            line_width: 100,
            indent_width: 4,
            use_tabs: false,
            line_ending: LineEnding::Auto,
            trailing_comma: TrailingComma::MultiLineOnly,
            semicolons: SemiStyle::Auto,
            collapse_blank_lines: true,
            trailing_newline: true,
        }
    }
}
