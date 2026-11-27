//! Formatter configuration.
//!
//! Configuration can be provided programmatically via [`FmtConfig`] or
//! loaded from a `zzfmt.toml` file discovered by walking up from a
//! starting path. See [`load_config`].

use std::path::{Path, PathBuf};

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
#[derive(Debug, Clone, PartialEq, Eq)]
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

/// Serializable form of `FmtConfig` loaded from `zzfmt.toml`.
///
/// All fields are optional so a config file can override only the
/// settings a team cares about and leave the rest at the built-in
/// defaults. Missing file or unparseable TOML degrades gracefully to
/// `FmtConfig::default()`.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FmtConfigFile {
    pub line_width: Option<usize>,
    pub indent_width: Option<usize>,
    pub use_tabs: Option<bool>,
    pub line_ending: Option<String>,
    pub trailing_comma: Option<String>,
    pub semicolons: Option<String>,
    pub collapse_blank_lines: Option<bool>,
    pub trailing_newline: Option<bool>,
}

/// Name of the formatter config file.
pub const FMT_CONFIG_FILENAME: &str = "zzfmt.toml";

/// Section name inside `zz.toml` that holds formatter settings.
pub const FMT_SECTION: &str = "fmt";

/// Load formatter configuration from a `zzfmt.toml` file.
///
/// Walks up from `start_path`'s parent directory looking for
/// `zzfmt.toml`. Stops at the first ancestor that contains it.
/// Falls back to `FmtConfig::default()` when no file is found or the
/// file cannot be parsed — callers never need to handle config errors.
///
/// Also accepts a `[fmt]` table inside `zz.toml` as an alternative
/// location (lower priority than a dedicated `zzfmt.toml`).
pub fn load_config(start_path: &Path) -> FmtConfig {
    let dir = match start_path.parent() {
        Some(d) => d.to_path_buf(),
        None => return FmtConfig::default(),
    };

    // 1. Dedicated zzfmt.toml, walking up the directory tree.
    if let Some(config_path) = find_config_upwards(&dir, FMT_CONFIG_FILENAME) {
        match parse_config_file(&config_path) {
            Ok(cfg) => return cfg,
            Err(e) => eprintln!("warning: failed to parse {}: {e}", config_path.display()),
        }
    }

    // 2. [fmt] table inside zz.toml, walking up the directory tree.
    if let Some(zz_toml) = find_config_upwards(&dir, "zz.toml") {
        match parse_zz_toml_fmt_section(&zz_toml) {
            Ok(cfg) => return cfg,
            Err(e) => eprintln!(
                "warning: failed to parse [fmt] in {}: {e}",
                zz_toml.display()
            ),
        }
    }

    FmtConfig::default()
}

/// Walk up from `dir` (inclusive) looking for `filename`, returning the
/// first match. Stops at the filesystem root.
fn find_config_upwards(dir: &Path, filename: &str) -> Option<PathBuf> {
    let mut current = dir.to_path_buf();
    loop {
        let candidate = current.join(filename);
        if candidate.is_file() {
            return Some(candidate);
        }
        match current.parent() {
            Some(parent) if parent != current => current = parent.to_path_buf(),
            _ => return None,
        }
    }
}

/// Parse a `zzfmt.toml` file into an `FmtConfig`, overriding defaults.
fn parse_config_file(path: &Path) -> Result<FmtConfig, Box<dyn std::error::Error + Send + Sync>> {
    let content = std::fs::read_to_string(path)?;
    let file: FmtConfigFile = toml::from_str(&content)?;
    Ok(apply_config_file(&file))
}

/// Parse the `[fmt]` table of a `zz.toml` file into an `FmtConfig`.
fn parse_zz_toml_fmt_section(
    path: &Path,
) -> Result<FmtConfig, Box<dyn std::error::Error + Send + Sync>> {
    let content = std::fs::read_to_string(path)?;
    let table: toml::Table = content.parse()?;
    let Some(fmt_table) = table.get(FMT_SECTION).and_then(toml::Value::as_table) else {
        return Err("no [fmt] section".into());
    };
    let mut cfg = FmtConfig::default();
    if let Some(v) = fmt_table
        .get("line_width")
        .and_then(toml::Value::as_integer)
    {
        cfg.line_width = v as usize;
    }
    if let Some(v) = fmt_table
        .get("indent_width")
        .and_then(toml::Value::as_integer)
    {
        cfg.indent_width = v as usize;
    }
    if let Some(v) = fmt_table.get("use_tabs").and_then(toml::Value::as_bool) {
        cfg.use_tabs = v;
    }
    if let Some(v) = fmt_table.get("line_ending").and_then(toml::Value::as_str) {
        cfg.line_ending = match v {
            "lf" => LineEnding::Lf,
            "crlf" => LineEnding::Crlf,
            _ => LineEnding::Auto,
        };
    }
    if let Some(v) = fmt_table
        .get("trailing_comma")
        .and_then(toml::Value::as_str)
    {
        cfg.trailing_comma = match v {
            "never" => TrailingComma::Never,
            "always" => TrailingComma::Always,
            _ => TrailingComma::MultiLineOnly,
        };
    }
    if let Some(v) = fmt_table.get("semicolons").and_then(toml::Value::as_str) {
        cfg.semicolons = match v {
            "always" => SemiStyle::Always,
            "never" => SemiStyle::Never,
            _ => SemiStyle::Auto,
        };
    }
    if let Some(v) = fmt_table
        .get("collapse_blank_lines")
        .and_then(toml::Value::as_bool)
    {
        cfg.collapse_blank_lines = v;
    }
    if let Some(v) = fmt_table
        .get("trailing_newline")
        .and_then(toml::Value::as_bool)
    {
        cfg.trailing_newline = v;
    }
    Ok(cfg)
}

/// Apply an `FmtConfigFile` over `FmtConfig::default()`.
fn apply_config_file(file: &FmtConfigFile) -> FmtConfig {
    let mut cfg = FmtConfig::default();
    if let Some(v) = file.line_width {
        cfg.line_width = v;
    }
    if let Some(v) = file.indent_width {
        cfg.indent_width = v;
    }
    if let Some(v) = file.use_tabs {
        cfg.use_tabs = v;
    }
    if let Some(v) = &file.line_ending {
        cfg.line_ending = match v.as_str() {
            "lf" => LineEnding::Lf,
            "crlf" => LineEnding::Crlf,
            _ => LineEnding::Auto,
        };
    }
    if let Some(v) = &file.trailing_comma {
        cfg.trailing_comma = match v.as_str() {
            "never" => TrailingComma::Never,
            "always" => TrailingComma::Always,
            _ => TrailingComma::MultiLineOnly,
        };
    }
    if let Some(v) = &file.semicolons {
        cfg.semicolons = match v.as_str() {
            "always" => SemiStyle::Always,
            "never" => SemiStyle::Never,
            _ => SemiStyle::Auto,
        };
    }
    if let Some(v) = file.collapse_blank_lines {
        cfg.collapse_blank_lines = v;
    }
    if let Some(v) = file.trailing_newline {
        cfg.trailing_newline = v;
    }
    cfg
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn write_config(dir: &Path, name: &str, content: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn load_config_defaults_when_no_file() {
        let tmp = env::temp_dir().join("zz_fmt_config_test_none");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let cfg = load_config(&tmp.join("src/main.zz"));
        assert_eq!(cfg, FmtConfig::default());
    }

    #[test]
    fn load_config_finds_zzfmt_toml() {
        let tmp = env::temp_dir().join("zz_fmt_config_test_found");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("sub")).unwrap();
        write_config(
            &tmp,
            FMT_CONFIG_FILENAME,
            "line_width = 80\nindent_width = 2\n",
        );
        let cfg = load_config(&tmp.join("sub/file.zz"));
        assert_eq!(cfg.line_width, 80);
        assert_eq!(cfg.indent_width, 2);
        // Non-overridden fields stay at defaults.
        assert!(!cfg.use_tabs);
        assert!(cfg.trailing_newline);
    }

    #[test]
    fn load_config_walks_up_to_parent() {
        let tmp = env::temp_dir().join("zz_fmt_config_test_walk");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("a/b/c")).unwrap();
        write_config(&tmp, FMT_CONFIG_FILENAME, "line_width = 60\n");
        let cfg = load_config(&tmp.join("a/b/c/file.zz"));
        assert_eq!(cfg.line_width, 60);
    }

    #[test]
    fn load_config_uses_zz_toml_fmt_section() {
        let tmp = env::temp_dir().join("zz_fmt_config_test_zz_toml");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        write_config(
            &tmp,
            "zz.toml",
            "[fmt]\nline_width = 120\ntrailing_newline = false\n",
        );
        let cfg = load_config(&tmp.join("src/main.zz"));
        assert_eq!(cfg.line_width, 120);
        assert!(!cfg.trailing_newline);
    }

    #[test]
    fn load_config_zzfmt_toml_wins_over_zz_toml() {
        let tmp = env::temp_dir().join("zz_fmt_config_test_priority");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        write_config(&tmp, "zz.toml", "[fmt]\nline_width = 30\n");
        write_config(&tmp, FMT_CONFIG_FILENAME, "line_width = 90\n");
        let cfg = load_config(&tmp.join("file.zz"));
        assert_eq!(cfg.line_width, 90);
    }

    #[test]
    fn load_config_ignores_malformed_toml() {
        let tmp = env::temp_dir().join("zz_fmt_config_test_bad");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        write_config(&tmp, FMT_CONFIG_FILENAME, "= not valid toml =\n");
        let cfg = load_config(&tmp.join("file.zz"));
        assert_eq!(cfg, FmtConfig::default());
    }

    #[test]
    fn load_config_line_ending_and_policies() {
        let tmp = env::temp_dir().join("zz_fmt_config_test_policies");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        write_config(
            &tmp,
            FMT_CONFIG_FILENAME,
            "line_ending = \"crlf\"\ntrailing_comma = \"always\"\nsemicolons = \"never\"\n",
        );
        let cfg = load_config(&tmp.join("file.zz"));
        assert_eq!(cfg.line_ending, LineEnding::Crlf);
        assert_eq!(cfg.trailing_comma, TrailingComma::Always);
        assert_eq!(cfg.semicolons, SemiStyle::Never);
    }
}
