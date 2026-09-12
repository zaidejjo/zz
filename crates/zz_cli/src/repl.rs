//! Modern interactive REPL for the ZZ programming language.
//!
//! Powered by `reedline` for editor-grade features:
//! - Context-aware autocomplete (keywords, builtins, session vars/funcs, modules)
//! - Real-time syntax highlighting via the ZZ lexer
//! - Multi-line input with smart continuation prompts
//! - Persistent command history (~/.zz_history)
//! - Meta commands (:vars, :funcs, :type, :clear, :reset, :exit)
//! - Auto-closing brackets and quotes
//! - Execution timing toggle

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Instant;

use nu_ansi_term::Color;
use reedline::{
    default_emacs_keybindings, ColumnarMenu, CompletionResult, EditCommand, EditMode, Emacs,
    FileBackedHistory, Highlighter, History, KeyCode, KeyModifiers, MenuBuilder, Prompt,
    PromptEditMode, PromptHistorySearch, Reedline, ReedlineEvent, ReedlineMenu, Signal,
    Span as CompletionSpan, StyledText, Suggestion, ValidationResult, Validator,
};

use zz_frontend::lexer::lex;
use zz_frontend::token::TokenKind;

use crate::session::Session;

// ---------------------------------------------------------------------------
// Static data
// ---------------------------------------------------------------------------

const ZZ_KEYWORDS: &[&str] = &[
    "import", "as", "func", "return", "if", "else", "while", "match", "true", "false", "struct",
    "for", "in", "break", "continue", "defer", "pub", "impl", "const",
];

const ZZ_BUILTINS: &[&str] = &[
    "print",
    "println",
    "input",
    "range",
    "len",
    "map",
    "filter",
    "enumerate",
    "zip",
    "typeof",
    "str",
    "int",
    "float",
    "append",
];

const META_COMMANDS: &[&str] = &[
    ":vars", ":funcs", ":type", ":clear", ":reset", ":exit", ":quit", ":help", ":timing",
];

const PROMPT_NORMAL: &str = "zz> ";
const HISTORY_MAX: usize = 10_000;

// ---------------------------------------------------------------------------
// ZZCompleter — context-aware autocompletion
// ---------------------------------------------------------------------------

pub struct ZZCompleter {
    keywords: Vec<String>,
    builtins: Vec<String>,
    session_vars: Vec<String>,
    session_funcs: Vec<String>,
    /// Module name → member names (e.g. "math" → ["abs", "sin", "PI", ...])
    module_members: HashMap<String, Vec<String>>,
}

impl ZZCompleter {
    pub fn new() -> Self {
        Self {
            keywords: ZZ_KEYWORDS.iter().map(|s| s.to_string()).collect(),
            builtins: ZZ_BUILTINS.iter().map(|s| s.to_string()).collect(),
            session_vars: Vec::new(),
            session_funcs: Vec::new(),
            module_members: HashMap::new(),
        }
    }

    /// Refresh completion data from the current session state.
    pub fn refresh(&mut self, session: &Session) {
        self.session_vars = session.env_vars().into_keys().collect();
        self.session_funcs = session.funcs().keys().cloned().collect();

        self.module_members.clear();
        for name in session.funcs().keys() {
            if let Some(dot_pos) = name.rfind('.') {
                let module = &name[..dot_pos];
                let member = &name[dot_pos + 1..];
                self.module_members
                    .entry(module.to_string())
                    .or_default()
                    .push(member.to_string());
            }
        }
    }

    fn complete_meta_commands(&self, prefix: &str) -> Vec<Suggestion> {
        let cursor = prefix.len();
        let p = prefix.to_lowercase();
        META_COMMANDS
            .iter()
            .filter(|cmd| cmd.to_lowercase().starts_with(&p))
            .map(|cmd| Suggestion {
                value: cmd.to_string(),
                display_override: None,
                description: None,
                style: None,
                extra: None,
                span: CompletionSpan {
                    start: 0,
                    end: cursor,
                },
                append_whitespace: true,
                match_indices: None,
            })
            .collect()
    }

    fn complete_module_members(
        &self,
        module: &str,
        member_prefix: &str,
        full_prefix_len: usize,
    ) -> Vec<Suggestion> {
        match self.module_members.get(module) {
            Some(members) => {
                let p = member_prefix.to_lowercase();
                members
                    .iter()
                    .filter(|m| m.to_lowercase().starts_with(&p))
                    .map(|m| Suggestion {
                        value: m.clone(),
                        display_override: None,
                        description: None,
                        style: None,
                        extra: None,
                        span: CompletionSpan {
                            start: full_prefix_len - member_prefix.len(),
                            end: full_prefix_len,
                        },
                        append_whitespace: true,
                        match_indices: None,
                    })
                    .collect()
            }
            None => Vec::new(),
        }
    }

    fn complete_flat(&self, prefix: &str, full_len: usize) -> Vec<Suggestion> {
        let mut suggestions = Vec::new();
        let mut seen = HashSet::new();
        let p = prefix.to_lowercase();

        // Keywords
        for kw in &self.keywords {
            if kw.to_lowercase().starts_with(&p) && seen.insert(kw.to_lowercase()) {
                suggestions.push(Suggestion {
                    value: kw.clone(),
                    display_override: None,
                    description: Some("keyword".to_string()),
                    style: None,
                    extra: None,
                    span: CompletionSpan {
                        start: 0,
                        end: full_len,
                    },
                    append_whitespace: true,
                    match_indices: None,
                });
            }
        }

        // Builtins
        for bi in &self.builtins {
            if bi.to_lowercase().starts_with(&p) && seen.insert(bi.to_lowercase()) {
                suggestions.push(Suggestion {
                    value: bi.clone(),
                    display_override: None,
                    description: Some("builtin".to_string()),
                    style: None,
                    extra: None,
                    span: CompletionSpan {
                        start: 0,
                        end: full_len,
                    },
                    append_whitespace: true,
                    match_indices: None,
                });
            }
        }

        // Session variables
        for var in &self.session_vars {
            if var.to_lowercase().starts_with(&p) && seen.insert(var.to_lowercase()) {
                suggestions.push(Suggestion {
                    value: var.clone(),
                    display_override: None,
                    description: Some("variable".to_string()),
                    style: None,
                    extra: None,
                    span: CompletionSpan {
                        start: 0,
                        end: full_len,
                    },
                    append_whitespace: true,
                    match_indices: None,
                });
            }
        }

        // Session functions
        for func in &self.session_funcs {
            if func.to_lowercase().starts_with(&p) {
                if seen.insert(func.to_lowercase()) {
                    suggestions.push(Suggestion {
                        value: func.clone(),
                        display_override: None,
                        description: Some("function".to_string()),
                        style: None,
                        extra: None,
                        span: CompletionSpan {
                            start: 0,
                            end: full_len,
                        },
                        append_whitespace: true,
                        match_indices: None,
                    });
                }
            } else if let Some(last) = func.rsplit('.').next() {
                let last_lower = last.to_lowercase();
                if last_lower.starts_with(&p) && seen.insert(last_lower) {
                    suggestions.push(Suggestion {
                        value: last.to_string(),
                        display_override: None,
                        description: Some(format!("from {func}")),
                        style: None,
                        extra: None,
                        span: CompletionSpan {
                            start: 0,
                            end: full_len,
                        },
                        append_whitespace: true,
                        match_indices: None,
                    });
                }
            }
        }

        // Stdlib module names as top-level completions
        for module in zz_stdlib::STDLIB_MODULES {
            if module.to_lowercase().starts_with(&p) && seen.insert(module.to_lowercase()) {
                suggestions.push(Suggestion {
                    value: module.to_string(),
                    display_override: None,
                    description: Some("module".to_string()),
                    style: None,
                    extra: None,
                    span: CompletionSpan {
                        start: 0,
                        end: full_len,
                    },
                    append_whitespace: true,
                    match_indices: None,
                });
            }
        }

        suggestions
    }
}

impl reedline::Completer for ZZCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> CompletionResult {
        let prefix = extract_word_before_cursor(line, pos);

        // Meta commands (start with `:`)
        if prefix.starts_with(':') {
            return CompletionResult::fresh(self.complete_meta_commands(&prefix));
        }

        // Dot-completion: `module.member_prefix`
        if let Some(dot_pos) = prefix.rfind('.') {
            let module = &prefix[..dot_pos];
            let member_prefix = &prefix[dot_pos + 1..];
            return CompletionResult::fresh(self.complete_module_members(
                module,
                member_prefix,
                pos,
            ));
        }

        // Flat completion from all sources
        CompletionResult::fresh(self.complete_flat(&prefix, pos))
    }
}

/// Extract the word (identifier) immediately before the cursor position.
fn extract_word_before_cursor(line: &str, pos: usize) -> String {
    let before = &line[..pos];
    let word_start = before
        .rfind(|c: char| !c.is_alphanumeric() && c != '_' && c != '.')
        .map(|i| i + 1)
        .unwrap_or(0);
    before[word_start..].to_string()
}

// ---------------------------------------------------------------------------
// ZZHighlighter — real-time syntax coloring via ZZ lexer
// ---------------------------------------------------------------------------

pub struct ZZHighlighter;

impl ZZHighlighter {
    fn style_for_token(kind: &TokenKind, text: &str) -> (Color, bool) {
        match kind {
            // Keywords — bold purple
            TokenKind::Import
            | TokenKind::As
            | TokenKind::Func
            | TokenKind::Return
            | TokenKind::If
            | TokenKind::Else
            | TokenKind::While
            | TokenKind::Match
            | TokenKind::True
            | TokenKind::False
            | TokenKind::Struct
            | TokenKind::For
            | TokenKind::In
            | TokenKind::Break
            | TokenKind::Continue
            | TokenKind::Defer
            | TokenKind::Pub
            | TokenKind::Impl
            | TokenKind::Const
            | TokenKind::Extern
            | TokenKind::Mut => (Color::Purple, true),

            // Numeric literals — cyan
            TokenKind::Int | TokenKind::Float => (Color::Cyan, false),

            // String literals — green
            TokenKind::Str | TokenKind::StrFmt => (Color::Green, false),

            // Identifiers
            TokenKind::Ident => {
                if ZZ_BUILTINS.contains(&text) {
                    (Color::LightBlue, true)
                } else if ZZ_KEYWORDS.contains(&text) {
                    (Color::Purple, true)
                } else {
                    (Color::White, false)
                }
            }

            // Comparison/logical operators — white
            TokenKind::Plus
            | TokenKind::Minus
            | TokenKind::Star
            | TokenKind::StarStar
            | TokenKind::Slash
            | TokenKind::Percent
            | TokenKind::Eq
            | TokenKind::Ne
            | TokenKind::Lt
            | TokenKind::Gt
            | TokenKind::Le
            | TokenKind::Ge
            | TokenKind::AndAnd
            | TokenKind::OrOr
            | TokenKind::Bang
            | TokenKind::Question
            | TokenKind::QuestionQuestion
            | TokenKind::DotDot
            | TokenKind::Arrow
            | TokenKind::PipeGt => (Color::White, false),

            // Assignment — red
            TokenKind::Assign | TokenKind::ColonEq => (Color::Red, false),

            // Delimiters — dark gray
            TokenKind::LParen
            | TokenKind::RParen
            | TokenKind::LBrace
            | TokenKind::RBrace
            | TokenKind::LBracket
            | TokenKind::RBracket
            | TokenKind::At => (Color::DarkGray, false),

            // Punctuation
            TokenKind::Colon | TokenKind::Comma | TokenKind::Dot | TokenKind::Pipe => {
                (Color::DarkGray, false)
            }

            // Statement end / EOF — invisible
            TokenKind::StmtEnd | TokenKind::Eof => (Color::DarkGray, false),
        }
    }
}

impl Highlighter for ZZHighlighter {
    fn highlight(&self, line: &str, _cursor: usize) -> StyledText {
        let mut styled = StyledText::new();

        if line.is_empty() {
            return styled;
        }

        let lexed = lex(line);
        let mut last_end: usize = 0;

        for token in &lexed.tokens {
            let start = token.span.start as usize;
            let end = token.span.end as usize;

            // Emit any gap as unstyled
            if start > last_end && start <= line.len() {
                let gap = &line[last_end..start.min(line.len())];
                if !gap.is_empty() {
                    styled.push((nu_ansi_term::Style::default(), gap.to_string()));
                }
            }

            if token.kind == TokenKind::Eof {
                break;
            }

            // Use the original source text (from the span) rather than
            // `token.text`. The lexer strips outer quotes from string
            // tokens (`"hello"` → text `"hello"`, span `0..7`), so
            // token.text is shorter than the span. Using the span slice
            // keeps the displayed fragment byte-identical to the buffer,
            // preventing visual cursor displacement.
            let span_text = &line[start..end.min(line.len())];

            let (color, bold) = Self::style_for_token(&token.kind, span_text);
            let style = if bold {
                nu_ansi_term::Style::new().fg(color).bold()
            } else {
                nu_ansi_term::Style::new().fg(color)
            };
            styled.push((style, span_text.to_string()));

            last_end = end;
        }

        // Emit trailing text
        if last_end < line.len() {
            styled.push((nu_ansi_term::Style::default(), line[last_end..].to_string()));
        }

        styled
    }
}

// ---------------------------------------------------------------------------
// ZZValidator — multi-line input detection
// ---------------------------------------------------------------------------

pub struct ZZValidator;

/// Why the input is incomplete.
#[derive(Debug, Clone, PartialEq)]
enum IncompleteReason {
    None,
    OpenBrace(usize),
    UnterminatedString,
    TrailingOp,
    UnterminatedComment,
}

fn check_incomplete(input: &str) -> IncompleteReason {
    let lexed = lex(input);

    // Unterminated block comment
    if lexed
        .errors
        .iter()
        .any(|e| e.message.contains("unterminated block comment"))
    {
        return IncompleteReason::UnterminatedComment;
    }

    // Track bracket depth and trailing operator
    let mut depth: usize = 0;
    let mut last_significant: Option<&TokenKind> = None;

    for token in &lexed.tokens {
        match token.kind {
            TokenKind::LParen | TokenKind::LBrace | TokenKind::LBracket => {
                depth += 1;
            }
            TokenKind::RParen | TokenKind::RBrace | TokenKind::RBracket => {
                depth = depth.saturating_sub(1);
            }
            TokenKind::Eof | TokenKind::StmtEnd => {}
            _ => {
                last_significant = Some(&token.kind);
            }
        }
    }

    // Unterminated string (lexer error)
    if lexed
        .errors
        .iter()
        .any(|e| e.message.contains("unterminated string"))
    {
        return IncompleteReason::UnterminatedString;
    }

    // Open brackets at depth > 0
    if depth > 0 {
        return IncompleteReason::OpenBrace(depth);
    }

    // Trailing operator
    match last_significant {
        Some(
            TokenKind::Plus
            | TokenKind::Minus
            | TokenKind::Star
            | TokenKind::Slash
            | TokenKind::Percent
            | TokenKind::Assign
            | TokenKind::ColonEq
            | TokenKind::Pipe
            | TokenKind::PipeGt
            | TokenKind::Arrow
            | TokenKind::AndAnd
            | TokenKind::OrOr
            | TokenKind::Comma
            | TokenKind::Colon,
        ) => {
            return IncompleteReason::TrailingOp;
        }
        _ => {}
    }

    // Trailing keyword detection is intentionally omitted — it requires
    // parser-level context (tracking whether an if/while/for block is open)
    // which the lexer cannot provide. Incomplete blocks like `if x > 0\n`
    // are caught by the parser as errors instead.

    IncompleteReason::None
}

impl Validator for ZZValidator {
    fn validate(&self, line: &str) -> ValidationResult {
        match check_incomplete(line) {
            IncompleteReason::None => ValidationResult::Complete,
            _ => ValidationResult::Incomplete,
        }
    }
}

// ---------------------------------------------------------------------------
// ZZPrompt — custom two-line prompt
// ---------------------------------------------------------------------------

pub struct ZZPrompt {
    had_error: bool,
}

impl ZZPrompt {
    pub fn new() -> Self {
        Self { had_error: false }
    }

    pub fn set_error(&mut self, had_error: bool) {
        self.had_error = had_error;
    }
}

impl Prompt for ZZPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Borrowed(PROMPT_NORMAL)
    }

    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }

    fn render_prompt_indicator(&self, _prompt_mode: PromptEditMode) -> Cow<'_, str> {
        Cow::Borrowed("")
    }

    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        // Indent is injected into the buffer directly, not shown as a visual prefix.
        Cow::Borrowed("")
    }

    fn render_prompt_history_search_indicator(
        &self,
        history_search: PromptHistorySearch,
    ) -> Cow<'_, str> {
        Cow::Owned(format!("(search: {}) ", history_search.term))
    }
}

// ---------------------------------------------------------------------------
// Meta command handling
// ---------------------------------------------------------------------------

enum MetaCommandResult {
    Handled(String),
    Exit,
    NotAMetaCommand,
}

fn handle_meta_command(input: &str, session: &mut Session, timing: &mut bool) -> MetaCommandResult {
    let trimmed = input.trim();
    // Allow bare aliases (exit, quit, cls, clear) without the ':' prefix.
    const BARE_ALIASES: &[&str] = &["exit", "quit", "cls", "clear"];
    if !trimmed.starts_with(':') && !BARE_ALIASES.contains(&trimmed) {
        return MetaCommandResult::NotAMetaCommand;
    }

    let parts: Vec<&str> = trimmed.splitn(2, ' ').collect();
    let cmd = parts[0];
    let arg = parts.get(1).map(|s| s.trim()).unwrap_or("");

    match cmd {
        ":help" => {
            let help = "\
Available commands:
  :vars              List all session variables with their values
  :funcs             List all functions in scope
  :type <expr>       Show the type of an expression
  :clear             Clear the terminal screen (aliases: cls, clear)
  :reset             Reset the session (clear all bindings)
  :timing            Toggle execution timing display
  :help              Show this help message
  :exit, :quit       Exit the REPL (aliases: exit, quit)

Keyboard shortcuts:
  Tab                Trigger autocompletion
  Ctrl+R             Reverse history search
  Ctrl+C             Cancel current input
  Ctrl+D             Exit the REPL
  Up/Down            Navigate history";
            MetaCommandResult::Handled(help.to_string())
        }

        ":vars" => {
            let vars = session.env_vars();
            if vars.is_empty() {
                return MetaCommandResult::Handled(
                    "No variables defined in this session.".to_string(),
                );
            }
            let mut output = String::from("Session variables:\n");
            let mut sorted: Vec<_> = vars.iter().collect();
            sorted.sort_by_key(|(k, _)| (*k).clone());
            for (name, value) in &sorted {
                let type_str = session
                    .var_type(name)
                    .map(|t| format!("{t}"))
                    .unwrap_or_else(|| format_type_for_display(value));
                output.push_str(&format!("  {name}: {type_str} = {value}\n"));
            }
            MetaCommandResult::Handled(output)
        }

        ":funcs" => {
            let funcs = session.funcs();
            if funcs.is_empty() {
                return MetaCommandResult::Handled(
                    "No functions defined in this session.".to_string(),
                );
            }
            let mut output = String::from("Functions in scope:\n");
            let mut sorted: Vec<_> = funcs.iter().collect();
            sorted.sort_by_key(|(k, _)| (*k).clone());
            for (name, sig) in &sorted {
                let params: Vec<String> = sig
                    .params
                    .iter()
                    .map(|(pname, pty)| {
                        if pname.is_empty() {
                            format!("{pty}")
                        } else {
                            format!("{pname}: {pty}")
                        }
                    })
                    .collect();
                output.push_str(&format!("  {name}({}) -> {}\n", params.join(", "), sig.ret));
            }
            MetaCommandResult::Handled(output)
        }

        ":type" => {
            if arg.is_empty() {
                return MetaCommandResult::Handled("Usage: :type <expression>".to_string());
            }
            let expr = format!("typeof({arg})");
            let out = session.eval(&expr);
            if let Some(errs) = &out.errors {
                MetaCommandResult::Handled(format!("Error: {errs}"))
            } else if out.output.is_empty() {
                MetaCommandResult::Handled("(unit)".to_string())
            } else {
                MetaCommandResult::Handled(out.output)
            }
        }

        ":clear" | "cls" | "clear" => MetaCommandResult::Handled("\x1B[2J\x1B[1;1H".to_string()),

        ":reset" => {
            *session = Session::new("<repl>");
            MetaCommandResult::Handled("Session reset. All bindings cleared.".to_string())
        }

        ":timing" => {
            *timing = !*timing;
            let state = if *timing { "ON" } else { "OFF" };
            MetaCommandResult::Handled(format!("Execution timing: {state}"))
        }

        ":exit" | ":quit" | "exit" | "quit" => MetaCommandResult::Exit,

        _ => MetaCommandResult::Handled(format!(
            "Unknown command: {cmd}\nType :help for available commands."
        )),
    }
}

/// Infer a display type string for a runtime value.
fn format_type_for_display(value: &zz_runtime::Value) -> String {
    use zz_runtime::Value;
    match value {
        Value::Int(_) => "int".to_string(),
        Value::Float(_) => "float".to_string(),
        Value::Str(_) => "str".to_string(),
        Value::Bool(_) => "bool".to_string(),
        Value::Unit => "()".to_string(),
        Value::Option(_) => "option".to_string(),
        Value::Result(_) => "result".to_string(),
        Value::Array(_) => "array".to_string(),
        Value::Dict(_) => "dict".to_string(),
        Value::Func(_) => "func".to_string(),
        Value::Tuple(_) => "tuple".to_string(),
        Value::Object(_) => "object".to_string(),
        Value::Range(_) => "range".to_string(),
        Value::Native(_) => "native".to_string(),
        _ => "unknown".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

#[allow(dead_code)]
fn needs_more_input_simple(src: &str) -> bool {
    matches!(
        check_incomplete(src),
        IncompleteReason::OpenBrace(_)
            | IncompleteReason::UnterminatedString
            | IncompleteReason::UnterminatedComment
            | IncompleteReason::TrailingOp
    )
}

fn display_eval_result(result: &crate::session::EvalOutput, prompt: &mut ZZPrompt) {
    if let Some(errs) = &result.errors {
        eprintln!("{errs}");
        prompt.set_error(true);
    } else if !result.output.is_empty() {
        println!("{}", result.output);
        prompt.set_error(false);
    } else {
        prompt.set_error(false);
    }
}

fn display_eval_result_with_timing(
    result: &crate::session::EvalOutput,
    elapsed: std::time::Duration,
    prompt: &mut ZZPrompt,
) {
    if let Some(errs) = &result.errors {
        eprintln!("{errs}");
        eprintln!("  // took {elapsed:?}");
        prompt.set_error(true);
    } else if !result.output.is_empty() {
        println!("{}", result.output);
        println!("  // took {elapsed:?}");
        prompt.set_error(false);
    } else {
        println!("  // took {elapsed:?}");
        prompt.set_error(false);
    }
}
fn history_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".zz_history"))
}

/// Create a history backed by the file (reloads from disk each time).
fn make_history() -> Box<dyn History> {
    match history_path() {
        Some(path) => Box::new(
            FileBackedHistory::with_file(HISTORY_MAX, path)
                .unwrap_or_else(|_| FileBackedHistory::new(HISTORY_MAX).unwrap()),
        ),
        None => Box::new(FileBackedHistory::new(HISTORY_MAX).unwrap()),
    }
}

// ---------------------------------------------------------------------------
// Build a Reedline editor with current state
// ---------------------------------------------------------------------------

fn build_editor(completer: &ZZCompleter) -> Reedline {
    let completion_menu = Box::new(
        ColumnarMenu::default()
            .with_name("completion_menu")
            .with_columns(3),
    );

    let mut keybindings = default_emacs_keybindings();

    // Tab → trigger completion menu / next item
    keybindings.add_binding(
        KeyModifiers::NONE,
        KeyCode::Tab,
        ReedlineEvent::UntilFound(vec![
            ReedlineEvent::Menu("completion_menu".to_string()),
            ReedlineEvent::MenuNext,
        ]),
    );

    // Shift+Tab (BackTab) → previous item in completion menu
    keybindings.add_binding(
        KeyModifiers::NONE,
        KeyCode::BackTab,
        ReedlineEvent::MenuPrevious,
    );

    // Ctrl+R → reverse history search
    keybindings.add_binding(
        KeyModifiers::CONTROL,
        KeyCode::Char('r'),
        ReedlineEvent::SearchHistory,
    );

    // Ctrl+L → clear screen
    keybindings.add_binding(
        KeyModifiers::CONTROL,
        KeyCode::Char('l'),
        ReedlineEvent::ClearScreen,
    );

    // NOTE: Enter is NOT re-bound here. We use reedline's default SubmitOrNewline
    // behavior so that the built-in menu-active check (engine.rs line 1577) fires
    // first and correctly accepts completion menu selections before submitting.
    // Binding Enter to ExecuteHostCommand bypasses that check and breaks completion.

    // Auto-close opening brackets: (, [, {
    for (open, close) in [('(', ')'), ('[', ']'), ('{', '}')] {
        let pair = format!("{open}{close}");
        keybindings.add_binding(
            KeyModifiers::NONE,
            KeyCode::Char(open),
            ReedlineEvent::Edit(vec![
                EditCommand::InsertString(pair),
                EditCommand::MoveLeft { select: false },
            ]),
        );
    }

    // Auto-close quotes: " and '
    // Insert the pair then move cursor left one to sit between them.
    // The highlighter must emit the original source text (including quotes)
    // so the visual rendering stays in sync with the buffer.
    for quote in ['"', '\''] {
        let pair = format!("{quote}{quote}");
        keybindings.add_binding(
            KeyModifiers::NONE,
            KeyCode::Char(quote),
            ReedlineEvent::Edit(vec![
                EditCommand::InsertString(pair),
                EditCommand::MoveLeft { select: false },
            ]),
        );
    }

    // Skip-over closing brackets: ), ], }
    // If cursor is at end, auto-insert the char normally (default behavior).
    // Otherwise, try to move right first (skip over existing closer).
    for (close, open) in [(')', '('), (']', '['), ('}', '{')] {
        let pair = format!("{open}{close}");
        keybindings.add_binding(
            KeyModifiers::NONE,
            KeyCode::Char(close),
            ReedlineEvent::UntilFound(vec![
                ReedlineEvent::Edit(vec![EditCommand::MoveRight { select: false }]),
                ReedlineEvent::Edit(vec![
                    EditCommand::InsertString(pair),
                    EditCommand::MoveLeft { select: false },
                ]),
            ]),
        );
    }

    let edit_mode: Box<dyn EditMode> = Box::new(Emacs::new(keybindings));

    // Build completer with cloned snapshot
    let mut comp = ZZCompleter::new();
    comp.keywords.clone_from(&completer.keywords);
    comp.builtins.clone_from(&completer.builtins);
    comp.session_vars.clone_from(&completer.session_vars);
    comp.session_funcs.clone_from(&completer.session_funcs);
    comp.module_members.clone_from(&completer.module_members);

    let history = make_history();

    Reedline::create()
        .with_completer(Box::new(comp))
        .with_highlighter(Box::new(ZZHighlighter))
        .with_validator(Box::new(ZZValidator))
        .with_menu(ReedlineMenu::EngineCompleter(completion_menu))
        .with_history(history)
        .with_edit_mode(edit_mode)
}

// ---------------------------------------------------------------------------
// Main REPL loop
// ---------------------------------------------------------------------------

pub fn run() -> std::io::Result<()> {
    let mut session = Session::new("<repl>");

    // --- Completer state ---
    let mut completer = ZZCompleter::new();
    completer.refresh(&session);

    let mut prompt = ZZPrompt::new();
    let mut timing_enabled = false;

    // Welcome message
    println!("ZZ REPL — type :help for commands, Ctrl+D to exit");
    println!();

    loop {
        let mut editor = build_editor(&completer);

        let result = editor.read_line(&prompt);

        match result {
            Ok(Signal::Success(input)) => {
                let trimmed = input.trim();
                if trimmed.is_empty() {
                    continue;
                }

                // Meta commands
                let meta_result = handle_meta_command(trimmed, &mut session, &mut timing_enabled);
                match meta_result {
                    MetaCommandResult::Handled(output) => {
                        if !output.is_empty() {
                            if output == "\x1B[2J\x1B[1;1H" {
                                print!("\x1B[2J\x1B[1;1H");
                            } else {
                                print!("{output}");
                                if !output.ends_with('\n') {
                                    println!();
                                }
                            }
                        }
                        prompt.set_error(false);
                        continue;
                    }
                    MetaCommandResult::Exit => {
                        println!();
                        return Ok(());
                    }
                    MetaCommandResult::NotAMetaCommand => {}
                }

                // Evaluation with optional timing
                let start = if timing_enabled {
                    Some(Instant::now())
                } else {
                    None
                };

                let eval_result = session.eval(&input);

                if let Some(start) = start {
                    let elapsed = start.elapsed();
                    display_eval_result_with_timing(&eval_result, elapsed, &mut prompt);
                } else {
                    display_eval_result(&eval_result, &mut prompt);
                }

                // Refresh completer for next prompt
                completer.refresh(&session);
            }

            Ok(Signal::CtrlC) => {
                println!("^C");
                continue;
            }

            Ok(Signal::CtrlD) => {
                println!();
                return Ok(());
            }

            Ok(Signal::HostCommand(_cmd)) => {
                // No custom HostCommands are bound anymore.
                continue;
            }

            Err(e) => {
                eprintln!("REPL error: {e}");
            }

            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuation_on_trailing_operator() {
        assert!(needs_more_input_simple("x := 1 +\n"));
    }

    #[test]
    fn continuation_on_open_paren() {
        assert!(needs_more_input_simple("(1 +\n"));
    }

    #[test]
    fn no_continuation_on_complete_line() {
        assert!(!needs_more_input_simple("let x = 1 + 2\n"));
    }

    #[test]
    fn no_continuation_on_multiline_parens_complete() {
        assert!(!needs_more_input_simple("(1 +\n2)\n"));
    }

    #[test]
    fn continuation_on_unterminated_comment() {
        assert!(needs_more_input_simple("1 /* oops\n"));
    }

    #[test]
    fn continuation_on_trailing_keyword() {
        // `if x > 0\n` is NOT detected as incomplete at the lexer level —
        // it's a parser error. Trailing keyword detection requires parser
        // context that the lexer cannot provide.
        assert!(!needs_more_input_simple("if x > 0\n"));
    }

    #[test]
    fn continuation_on_trailing_comma() {
        assert!(needs_more_input_simple("func add(a: int,\n"));
    }

    #[test]
    fn extract_word_simple() {
        assert_eq!(extract_word_before_cursor("hello world", 5), "hello");
    }

    #[test]
    fn extract_word_partial() {
        assert_eq!(extract_word_before_cursor("hel", 3), "hel");
    }

    #[test]
    fn extract_word_after_space() {
        assert_eq!(extract_word_before_cursor("hello wor", 9), "wor");
    }

    #[test]
    fn extract_word_empty() {
        assert_eq!(extract_word_before_cursor("", 0), "");
    }

    #[test]
    fn extract_word_with_dot() {
        assert_eq!(extract_word_before_cursor("math.", 5), "math.");
    }

    #[test]
    fn extract_word_after_dot() {
        assert_eq!(extract_word_before_cursor("math.ab", 7), "math.ab");
    }

    #[test]
    fn open_brace_depth_tracking() {
        assert_eq!(check_incomplete("(1 + (2"), IncompleteReason::OpenBrace(2));
    }

    #[test]
    fn open_bracket_detection() {
        assert_eq!(check_incomplete("[1, 2"), IncompleteReason::OpenBrace(1));
    }

    #[test]
    fn highlighter_includes_quotes_in_string_spans() {
        // The lexer strips quotes from token.text but the span includes them.
        // The highlighter must use the span slice (original source) so that
        // the styled fragment matches the buffer byte-for-byte.
        let hl = ZZHighlighter;
        let input = r#"println("hello")"#;
        let styled = hl.highlight(input, 0);

        // Verify total styled text length matches the full input (including quotes).
        let total_len: usize = styled.buffer.iter().map(|(_, s)| s.len()).sum();
        assert_eq!(
            total_len,
            input.len(),
            "styled fragments must cover the entire input including quotes"
        );
    }

    #[test]
    fn highlighter_string_token_length_matches_span() {
        // For a string like "hi", span is 0..4 (includes both quotes),
        // but the lexer's token.text is just "hi" (2 bytes).
        // The highlighter must emit 4 bytes of styled text, not 2.
        let hl = ZZHighlighter;
        let input = r#""hi""#; // 4 bytes: " h i "
        let styled = hl.highlight(input, 0);
        let total_len: usize = styled.buffer.iter().map(|(_, s)| s.len()).sum();
        assert_eq!(total_len, input.len());
    }

    #[test]
    fn highlighter_fstring_includes_quotes() {
        let hl = ZZHighlighter;
        let input = r#"f"val={x}""#;
        let styled = hl.highlight(input, 0);
        let total_len: usize = styled.buffer.iter().map(|(_, s)| s.len()).sum();
        assert_eq!(total_len, input.len());
    }
}
