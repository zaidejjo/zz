//! `zz setup` + `zz completion`: shell integration.
//!
//! - `~/.zz/bin` is the canonical tool directory (compiler + everything
//!   `zz install --path` builds). `zz setup` creates it and wires it into
//!   bash/zsh/fish (PowerShell script staged for future use).
//! - `zz completion <shell>` prints the completion script for one shell.
//! - [`auto_heal`] runs on every `zz` invocation: it silently creates a
//!   missing bin dir and nudges interactive users toward `zz setup` when
//!   the dir is not on `PATH`. Shell rc files are only ever edited by an
//!   explicit `zz setup` (or the install scripts via `zz setup --yes`).

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use crate::ui;

/// Marker around every block this tool (and the install scripts) manage,
/// so re-runs never duplicate lines.
pub const MARK_BEGIN: &str = "# >>> zz-lang >>>";
pub const MARK_END: &str = "# <<< zz-lang <<<";

/// `zz completion` shells.
pub const SHELLS: &[&str] = &["bash", "zsh", "fish", "powershell"];

/// Bash completion: subcommands/flags statically, `*.zz` files for file args.
pub const BASH_COMPLETION: &str = r#"# zz shell completion (bash) — managed by `zz setup`. Do not edit.
_zz_complete() {
    local cur prev cword cmds file_cmds flags
    COMPREPLY=()
    cur="${COMP_WORDS[COMP_CWORD]}"
    prev="${COMP_WORDS[COMP_CWORD-1]}"
    cword=$COMP_CWORD
    cmds="run build test check fix fmt eval init new add install i remove update search info registry login publish cache setup completion help"
    file_cmds=" run build test check fix fmt "
    flags="--help --version -p --release --static --pgo --target --cc --verbose --embed --native --registry --path --git --rev --limit --dry-run --yes --allow-source-builds --allow-hooks --template --author --description --license --repo --browser --skip-tests --check --fix --hard --interactive --stdin"
    case "$prev" in
        --path|--embed)
            COMPREPLY=( $(compgen -d -- "$cur") )
            return 0
            ;;
        --target)
            COMPREPLY=( $(compgen -W "x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu x86_64-apple-darwin aarch64-apple-darwin" -- "$cur") )
            return 0
            ;;
        --cc)
            COMPREPLY=( $(compgen -W "clang zig" -- "$cur") )
            return 0
            ;;
        --template)
            COMPREPLY=( $(compgen -W "cli lib web" -- "$cur") )
            return 0
            ;;
    esac
    if [[ "$cur" == -* ]]; then
        COMPREPLY=( $(compgen -W "$flags" -- "$cur") )
        return 0
    fi
    if [[ "$file_cmds" == *" ${COMP_WORDS[1]} "* ]]; then
        COMPREPLY=( $(compgen -f -X '!*.zz' -- "$cur") $(compgen -d -- "$cur") )
        return 0
    fi
    if [[ $cword -eq 1 ]]; then
        COMPREPLY=( $(compgen -W "$cmds" -- "$cur") )
        return 0
    fi
    if [[ "${COMP_WORDS[1]}" == "completion" && $cword -eq 2 ]]; then
        COMPREPLY=( $(compgen -W "bash zsh fish powershell" -- "$cur") )
        return 0
    fi
    return 0
}
complete -F _zz_complete zz
"#;

/// Zsh completion: subcommands/flags statically, `*.zz` files for file args.
pub const ZSH_COMPLETION: &str = r#"#compdef zz
# zz shell completion (zsh) — managed by `zz setup`. Do not edit.
_zz() {
    local -a cmds flags
    cmds=(run build test check fix fmt eval init new add install i remove update search info registry login publish cache setup completion help)
    flags=(--help --version -p --release --static --pgo --target --cc --verbose --embed --native --registry --path --git --rev --limit --dry-run --yes --allow-source-builds --allow-hooks --template --author --description --license --repo --browser --skip-tests --check --fix --hard --interactive --stdin)
    if (( CURRENT == 2 )); then
        _describe 'zz command' cmds
        return
    fi
    local cur="${words[CURRENT]}" prev="${words[CURRENT-1]}"
    case "$prev" in
        --path|--embed) _files -/; return ;;
        --target) _describe 'target' '(x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu x86_64-apple-darwin aarch64-apple-darwin)'; return ;;
        --cc) _describe 'provider' '(clang zig)'; return ;;
        --template) _describe 'template' '(cli lib web)'; return ;;
    esac
    if [[ "$cur" == -* ]]; then
        _describe 'flag' flags
        return
    fi
    case "${words[2]}" in
        run|build|test|check|fix|fmt)
            _files -g '*.zz'
            ;;
        completion)
            _describe 'shell' '(bash zsh fish powershell)'
            ;;
        *)
            _message 'no more arguments'
            ;;
    esac
}
"#;

/// Fish completion: subcommands plus `*.zz` suffix completion for file args.
pub const FISH_COMPLETION: &str = r#"# zz shell completion (fish) — managed by `zz setup`. Do not edit.
set -l zz_cmds run build test check fix fmt eval init new add install i remove update search info registry login publish cache setup completion help
complete -c zz -f -n '__fish_use_subcommand' -a "$zz_cmds"
complete -c zz -n '__fish_seen_subcommand_from run build test check fix fmt' -a '(__fish_complete_suffix .zz)' -d 'ZZ source file'
complete -c zz -n '__fish_seen_subcommand_from completion' -f -a 'bash zsh fish powershell' -d 'Shell'
complete -c zz -s h -l help -d 'Show help'
complete -c zz -l version -d 'Show version'
complete -c zz -s p -l release -d 'Optimized release build'
complete -c zz -l static -d 'Static self-contained binary'
complete -c zz -l pgo -d 'Profile-guided build'
complete -c zz -l target -d 'Cross-compile triple' -x -a 'x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu x86_64-apple-darwin aarch64-apple-darwin'
complete -c zz -l cc -d 'Compiler provider' -x -a 'clang zig'
complete -c zz -l registry -d 'Registry URL' -x
complete -c zz -l path -d 'Local path' -x -a '(__fish_complete_directories)'
complete -c zz -l verbose -d 'Print the clang command'
complete -c zz -l native -d 'Use the native AOT compiler'
"#;

/// PowerShell completion (staged for future Windows support).
pub const POWERSHELL_COMPLETION: &str = r#"# zz shell completion (powershell) — managed by `zz setup`. Do not edit.
Register-ArgumentCompleter -Native -CommandName @('zz') -ScriptBlock {
    param($wordToComplete, $commandAst, $cursorPosition)
    $commands = @('run','build','test','check','fix','fmt','eval','init','new','add','install','i','remove','update','search','info','registry','login','publish','cache','setup','completion','help')
    $fileCmds = @('run','build','test','check','fix','fmt')
    $elements = $commandAst.CommandElements
    if ($elements.Count -ge 2 -and $fileCmds -contains $elements[1]) {
        Get-ChildItem "$wordToComplete*.zz" | ForEach-Object {
            [System.Management.Automation.CompletionResult]::new($_.FullName, $_.Name, 'ProviderItem', $_.FullName)
        }
    } else {
        $commands | Where-Object { $_ -like "$wordToComplete*" } | ForEach-Object {
            [System.Management.Automation.CompletionResult]::new($_, $_, 'ParameterName', $_)
        }
    }
}
"#;

/// Script text for a named shell, or `None` for unknown shells.
pub fn completion_script(shell: &str) -> Option<&'static str> {
    match shell {
        "bash" => Some(BASH_COMPLETION),
        "zsh" => Some(ZSH_COMPLETION),
        "fish" => Some(FISH_COMPLETION),
        "powershell" => Some(POWERSHELL_COMPLETION),
        _ => None,
    }
}

/// `zz completion [shell]`: print the script (default: detect current shell).
pub fn print_completion(args: &[String]) -> Result<(), String> {
    let shell: String = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .cloned()
        .unwrap_or_else(|| detect_shell().to_string());
    match completion_script(&shell) {
        Some(script) => {
            print!("{script}");
            Ok(())
        }
        None => Err(format!(
            "unknown shell `{shell}`\n\
              hint: usage: zz completion [{}]",
            SHELLS.join("|")
        )),
    }
}

/// Best-effort current-shell detection via `$SHELL` basename.
pub fn detect_shell() -> &'static str {
    let shell = std::env::var("SHELL").unwrap_or_default();
    let base = shell.rsplit('/').next().unwrap_or("");
    match base {
        "zsh" => "zsh",
        "fish" => "fish",
        "powershell" | "pwsh" => "powershell",
        _ => "bash",
    }
}

/// True when `bin` (or its `$HOME`-relative spelling) is on `PATH`.
pub fn bin_on_path(bin: &Path) -> bool {
    let home_bin = home_dir().join(".zz").join("bin");
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|p| p == bin || p == home_bin))
}

fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"))
}

/// PATH export line for a bin dir: `$HOME`-relative when it is the default
/// location, absolute otherwise (e.g. custom `ZZ_HOME`).
fn export_line_for(bin: &Path) -> String {
    if bin == home_dir().join(".zz").join("bin") {
        "export PATH=\"$HOME/.zz/bin:$PATH\"".to_string()
    } else {
        format!("export PATH=\"{}:$PATH\"", bin.display())
    }
}

fn fish_line_for(bin: &Path) -> String {
    if bin == home_dir().join(".zz").join("bin") {
        "set -gx PATH $HOME/.zz/bin $PATH".to_string()
    } else {
        format!("set -gx PATH {} $PATH", bin.display())
    }
}

/// Render a managed marker block for `lines`.
fn render_block(lines: &[&str]) -> String {
    let mut out = String::new();
    out.push('\n');
    out.push_str(MARK_BEGIN);
    out.push('\n');
    for l in lines {
        out.push_str(l);
        out.push('\n');
    }
    out.push_str(MARK_END);
    out.push('\n');
    out
}

/// Like the default call, but when the file has no marker block yet and
/// contains `anchor` (e.g. zsh `compinit`), the block is inserted *before*
/// the anchor line instead of appended — appended `fpath` lines would run
/// after `compinit` and never take effect. Pass `None` to always append.
///
/// Returns `true` when the file was changed.
pub fn ensure_block_before(
    file: &Path,
    lines: &[&str],
    anchor: Option<&str>,
) -> Result<bool, String> {
    let current = std::fs::read_to_string(file).unwrap_or_default();
    if current.contains(MARK_BEGIN) {
        return top_up_block(file, &current, lines);
    }
    if lines.iter().all(|l| current.contains(l)) {
        return Ok(false);
    }
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let block = render_block(lines);
    let out = match anchor {
        Some(a) if !current.is_empty() => match first_anchor_line(&current, a) {
            Some(idx) => {
                let mut out = current.clone();
                out.insert_str(idx, &block);
                out
            }
            None => append_block(&current, &block),
        },
        _ => append_block(&current, &block),
    };
    std::fs::write(file, out).map_err(|e| format!("cannot write {}: {e}", file.display()))?;
    Ok(true)
}

/// Byte offset of the start of the first line containing `anchor`.
fn first_anchor_line(content: &str, anchor: &str) -> Option<usize> {
    let mut offset = 0usize;
    for line in content.split_inclusive('\n') {
        if line.contains(anchor) {
            return Some(offset);
        }
        offset += line.len();
    }
    None
}

fn append_block(current: &str, block: &str) -> String {
    let mut out = current.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(block);
    out
}

/// Add `lines` missing from an existing marker block. Returns `true` when
/// the file was changed.
fn top_up_block(file: &Path, current: &str, lines: &[&str]) -> Result<bool, String> {
    let missing: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| !current.contains(l))
        .collect();
    if missing.is_empty() {
        return Ok(false);
    }
    let mut out = current.to_string();
    if !out.ends_with('\n') {
        out.push('\n');
    }
    // Insert before the end marker so the block stays tidy.
    match out.rfind(MARK_END) {
        Some(idx) => {
            let mut ins = String::new();
            for l in missing {
                ins.push_str(l);
                ins.push('\n');
            }
            out.insert_str(idx, &ins);
        }
        None => {
            for l in missing {
                out.push_str(l);
                out.push('\n');
            }
        }
    }
    std::fs::write(file, out).map_err(|e| format!("cannot write {}: {e}", file.display()))?;
    Ok(true)
}

/// Shell rc files to manage: (label, file, lines, anchor).
///
/// The zsh anchor keeps our `fpath` line ahead of `compinit`, which only
/// scans `fpath` once at startup — an appended line would never load.
fn rc_targets(bin: &Path) -> Vec<(&'static str, PathBuf, Vec<String>, Option<&'static str>)> {
    let home = home_dir();
    let export = export_line_for(bin);
    let fish_line = fish_line_for(bin);
    vec![
        (
            "bash",
            home.join(".bashrc"),
            vec![
                export.clone(),
                "source ~/.zz/completions/zz.bash".to_string(),
            ],
            None,
        ),
        (
            "zsh",
            home.join(".zshrc"),
            vec![export, "fpath=(~/.zfunc $fpath)".to_string()],
            Some("compinit"),
        ),
        (
            "fish",
            home.join(".config").join("fish").join("config.fish"),
            vec![fish_line],
            None,
        ),
    ]
}

/// Stage all completion scripts under `~/.zz/completions/` and link them
/// into per-shell homes. Returns `(label, changed)` per item.
fn install_completions() -> Result<Vec<(String, bool)>, String> {
    let home = home_dir();
    let stage = home.join(".zz").join("completions");
    std::fs::create_dir_all(&stage)
        .map_err(|e| format!("cannot create {}: {e}", stage.display()))?;
    let staged: &[(&str, &str)] = &[
        ("zz.bash", BASH_COMPLETION),
        ("zz.fish", FISH_COMPLETION),
        ("zz.ps1", POWERSHELL_COMPLETION),
    ];
    let mut out = Vec::new();
    for (name, script) in staged {
        let dest = stage.join(name);
        let changed = write_if_different(&dest, script)?;
        out.push((format!("completions/{name}"), changed));
    }
    // zsh expects the file named `_zz` on fpath.
    let zfunc = home.join(".zfunc");
    std::fs::create_dir_all(&zfunc)
        .map_err(|e| format!("cannot create {}: {e}", zfunc.display()))?;
    let changed = write_if_different(&zfunc.join("_zz"), ZSH_COMPLETION)?;
    out.push((".zfunc/_zz".to_string(), changed));
    // bash-completion's user directory auto-loads this path.
    let bash_comp = home
        .join(".local")
        .join("share")
        .join("bash-completion")
        .join("completions");
    std::fs::create_dir_all(&bash_comp)
        .map_err(|e| format!("cannot create {}: {e}", bash_comp.display()))?;
    let changed = write_if_different(&bash_comp.join("zz"), BASH_COMPLETION)?;
    out.push(("bash-completion/zz".to_string(), changed));
    // fish autoloads this path.
    let fish_comp = home.join(".config").join("fish").join("completions");
    std::fs::create_dir_all(&fish_comp)
        .map_err(|e| format!("cannot create {}: {e}", fish_comp.display()))?;
    let changed = write_if_different(&fish_comp.join("zz.fish"), FISH_COMPLETION)?;
    out.push(("fish/completions/zz.fish".to_string(), changed));
    Ok(out)
}

fn write_if_different(path: &Path, content: &str) -> Result<bool, String> {
    match std::fs::read_to_string(path) {
        Ok(cur) if cur == content => Ok(false),
        _ => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
            }
            std::fs::write(path, content)
                .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
            Ok(true)
        }
    }
}

/// `zz setup [--yes]`: create `~/.zz/bin`, wire PATH + completions.
/// `--yes` skips nothing interactive (setup never prompts) — it exists so
/// the install scripts can call setup non-interactively and explicitly.
pub fn run(_args: &[String]) -> Result<(), String> {
    let bin = zz_pm::paths::bin_dir();
    ui::header("zz setup — shell integration");

    // 1/3 bin dir.
    ui::step(1, 3, &format!("Tool directory {}", bin.display()));
    std::fs::create_dir_all(&bin).map_err(|e| format!("cannot create {}: {e}", bin.display()))?;
    ui::ok(&format!("{} exists", bin.display()));

    // 2/3 PATH.
    ui::step(2, 3, "Shell PATH");
    let mut touched = false;
    for (label, file, lines, anchor) in rc_targets(&bin) {
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        match ensure_block_before(&file, &refs, anchor) {
            Ok(true) => {
                ui::ok(&format!("{label}: updated {}", file.display()));
                touched = true;
            }
            Ok(false) => ui::ok(&format!("{label}: already set")),
            Err(e) => ui::warn(&format!("{label}: skipped ({e})")),
        }
    }
    if !bin_on_path(&bin) {
        ui::warn("~/.zz/bin is not on PATH in this session — restart your shell.");
    } else if !touched {
        ui::ok("PATH already wired in this session");
    }

    // 3/3 completions.
    ui::step(3, 3, "Shell completions (bash, zsh, fish)");
    for (label, changed) in install_completions()? {
        if changed {
            ui::ok(&format!("wrote {label}"));
        } else {
            ui::ok(&format!("{label} up to date"));
        }
    }

    clear_hint_stamp();
    let shell = detect_shell();
    println!("setup complete for {shell} — reload your shell, then try: zz <TAB>");
    match shell {
        "fish" => println!("  reload with: source ~/.config/fish/config.fish"),
        "zsh" => println!("  reload with: exec zsh -l   (or: source ~/.zshrc)"),
        _ => println!("  reload with: exec $SHELL -l   (or: source ~/.bashrc)"),
    }
    Ok(())
}

/// Stamp file remembering the one-time PATH hint was shown, so interactive
/// users see it once — not on every invocation — until `zz setup` runs.
fn hint_stamp_path() -> PathBuf {
    zz_pm::paths::zz_home().join(".setup-hint-shown")
}

fn hint_already_shown() -> bool {
    hint_stamp_path().exists()
}

fn mark_hint_shown() {
    let _ = std::fs::write(hint_stamp_path(), "run `zz setup` to wire up ~/.zz/bin\n");
}

fn clear_hint_stamp() {
    let _ = std::fs::remove_file(hint_stamp_path());
}

/// Runs on every `zz` invocation before command dispatch.
///
/// - Creates a missing bin dir silently (cheap, idempotent).
/// - Nudges interactive users toward `zz setup` **once** when the bin dir
///   is not on `PATH` (a stamp file suppresses repeats; `zz setup`
///   clears it). Piped/non-TTY runs stay silent.
pub fn auto_heal() {
    let bin = zz_pm::paths::bin_dir();
    if !bin.exists() {
        let _ = std::fs::create_dir_all(&bin);
    }
    if bin_on_path(&bin) {
        clear_hint_stamp();
        return;
    }
    if hint_already_shown() {
        return;
    }
    if std::io::stderr().is_terminal() {
        eprintln!("zz: hint: ~/.zz/bin is not on PATH — run `zz setup` (shown once)");
        mark_hint_shown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn isolated_home(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zz_setup_test_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn completion_scripts_cover_all_shells() {
        let _g = TEST_LOCK.lock().unwrap();
        for shell in SHELLS {
            let script = completion_script(shell).expect("every shell has a script");
            assert!(script.contains("zz"), "{shell} script mentions zz");
        }
        assert!(completion_script("tcsh").is_none());
        // File completion rules: `zz run <TAB>` must offer .zz files.
        assert!(BASH_COMPLETION.contains("*.zz"));
        assert!(ZSH_COMPLETION.contains("*.zz"));
        assert!(FISH_COMPLETION.contains(".zz"));
        assert!(POWERSHELL_COMPLETION.contains("*.zz"));
        // Every script lists the real subcommands.
        for script in [BASH_COMPLETION, ZSH_COMPLETION, FISH_COMPLETION] {
            for cmd in ["run", "build", "setup", "completion", "install"] {
                assert!(script.contains(cmd), "script missing `{cmd}`");
            }
        }
    }

    #[test]
    fn ensure_block_is_idempotent() {
        let _g = TEST_LOCK.lock().unwrap();
        let dir = isolated_home("block");
        let file = dir.join(".bashrc");
        std::fs::write(&file, "# existing\nexport FOO=1\n").unwrap();
        let changed =
            ensure_block_before(&file, &["export PATH=\"$HOME/.zz/bin:$PATH\""], None).unwrap();
        assert!(changed);
        let changed2 =
            ensure_block_before(&file, &["export PATH=\"$HOME/.zz/bin:$PATH\""], None).unwrap();
        assert!(!changed2, "second run must be a no-op");
        let content = std::fs::read_to_string(&file).unwrap();
        assert_eq!(content.matches(MARK_BEGIN).count(), 1);
        assert!(content.contains("# existing"), "existing content preserved");
        // Topping up a second line keeps one block.
        let changed3 = ensure_block_before(
            &file,
            &[
                "export PATH=\"$HOME/.zz/bin:$PATH\"",
                "source ~/.zz/completions/zz.bash",
            ],
            None,
        )
        .unwrap();
        assert!(changed3);
        let content = std::fs::read_to_string(&file).unwrap();
        assert_eq!(content.matches(MARK_BEGIN).count(), 1);
        assert!(content.contains("source ~/.zz/completions/zz.bash"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_block_goes_before_compinit() {
        let _g = TEST_LOCK.lock().unwrap();
        let dir = isolated_home("compinit");
        let file = dir.join(".zshrc");
        std::fs::write(&file, "export FOO=1\ncompinit -u\nsource other\n").unwrap();
        let changed =
            ensure_block_before(&file, &["fpath=(~/.zfunc $fpath)"], Some("compinit")).unwrap();
        assert!(changed);
        let content = std::fs::read_to_string(&file).unwrap();
        let block_pos = content.find(MARK_BEGIN).unwrap();
        let compinit_pos = content.find("compinit -u").unwrap();
        assert!(
            block_pos < compinit_pos,
            "marker block must precede compinit"
        );
        // Second run is a no-op even though the anchor still matches.
        let changed2 =
            ensure_block_before(&file, &["fpath=(~/.zfunc $fpath)"], Some("compinit")).unwrap();
        assert!(!changed2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hint_stamp_suppresses_repeats() {
        let _g = TEST_LOCK.lock().unwrap();
        let dir = isolated_home("hint");
        std::env::set_var("ZZ_HOME", &dir);
        assert!(!hint_already_shown());
        mark_hint_shown();
        assert!(hint_already_shown());
        clear_hint_stamp();
        assert!(!hint_already_shown());
        std::env::remove_var("ZZ_HOME");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Drive the real bash completion function and assert `.zz` preference:
    /// with `main.py` + `main.zz` present, `zz run m<TAB>` offers only
    /// `main.zz`, and `zz build --<TAB>` offers flags, not files.
    #[test]
    #[cfg(unix)]
    fn bash_completion_end_to_end() {
        let _g = TEST_LOCK.lock().unwrap();
        let dir = isolated_home("comp");
        let work = dir.join("work");
        std::fs::create_dir_all(&work).unwrap();
        for name in ["main.py", "main.zz", "other.txt"] {
            std::fs::write(work.join(name), "").unwrap();
        }
        let script = dir.join("zz.bash");
        std::fs::write(&script, BASH_COMPLETION).unwrap();

        let complete = |words: &str, cword: usize| -> Option<Vec<String>> {
            let out = std::process::Command::new("bash")
                .arg("-c")
                .arg(
                    r#"source "$0"; cd "$1"; IFS=' ' read -r -a COMP_WORDS <<< "$2"; COMP_CWORD=$3; COMPREPLY=(); _zz_complete; printf '%s\n' "${COMPREPLY[@]}""#,
                )
                .arg(&script)
                .arg(&work)
                .arg(words)
                .arg(cword.to_string())
                .output()
                .ok()?;
            if !out.status.success() {
                return None;
            }
            Some(
                String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .filter(|l| !l.is_empty())
                    .map(str::to_string)
                    .collect(),
            )
        };

        let got = complete("zz b", 1).expect("bash must exist for this test");
        assert!(got.contains(&"build".to_string()), "got: {got:?}");

        let got = complete("zz run m", 2).expect("bash must exist for this test");
        assert_eq!(got, vec!["main.zz".to_string()], "got: {got:?}");

        let got = complete("zz build --", 2).expect("bash must exist for this test");
        assert!(
            got.iter().any(|c| c == "--release"),
            "flags expected, got: {got:?}"
        );
        assert!(
            !got.iter().any(|c| c.ends_with(".zz")),
            "no files expected, got: {got:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bin_on_path_detects_entries() {
        let _g = TEST_LOCK.lock().unwrap();
        let dir = isolated_home("path");
        let bin = dir.join("bin");
        let old = std::env::var_os("PATH");
        // Absolute entry.
        std::env::set_var("PATH", format!("/usr/bin:{}", bin.display()));
        // bin_on_path compares against ZZ_HOME/bin and ~/.zz/bin; point
        // ZZ_HOME at the temp dir so the temp bin matches.
        std::env::set_var("ZZ_HOME", &dir);
        assert!(bin_on_path(&bin));
        std::env::set_var("PATH", "/usr/bin:/bin");
        assert!(!bin_on_path(&bin));
        if let Some(p) = old {
            std::env::set_var("PATH", p);
        }
        std::env::remove_var("ZZ_HOME");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
