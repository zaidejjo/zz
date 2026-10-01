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
    local cur cword cmds file_cmds
    COMPREPLY=()
    cur="${COMP_WORDS[COMP_CWORD]}"
    cword=$COMP_CWORD
    cmds="run build test check fix fmt eval init new add install i remove update search info registry login publish cache setup completion help"
    file_cmds=" run build test check fix fmt "
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
    if [[ "$cur" == -* ]]; then
        COMPREPLY=( $(compgen -W "--help --version -p --release --static --pgo --target --cc --verbose --embed --native --registry --path --git --rev --limit --dry-run --yes --allow-source-builds --allow-hooks" -- "$cur") )
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
    local -a cmds
    cmds=(run build test check fix fmt eval init new add install i remove update search info registry login publish cache setup completion help)
    if (( CURRENT == 2 )); then
        _describe 'zz command' cmds
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
complete -c zz -n '__fish_seen_subcommand_from run build test check fix fmt' -k -a '(__fish_complete_suffix .zz)' -d 'ZZ source file'
complete -c zz -n '__fish_seen_subcommand_from completion' -f -a 'bash zsh fish powershell' -d 'Shell'
complete -c zz -s h -l help -d 'Show help'
complete -c zz -l version -d 'Show version'
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

/// Ensure `file` contains a managed marker block with exactly `lines`.
/// Returns `true` when the file was changed.
pub fn ensure_block(file: &Path, lines: &[&str]) -> Result<bool, String> {
    let current = std::fs::read_to_string(file).unwrap_or_default();
    if current.contains(MARK_BEGIN) {
        // Top up missing lines inside the existing block.
        let mut missing: Vec<&str> = lines
            .iter()
            .copied()
            .filter(|l| !current.contains(l))
            .collect();
        if missing.is_empty() {
            return Ok(false);
        }
        let mut out = current;
        if !out.ends_with('\n') {
            out.push('\n');
        }
        // Insert before the end marker so the block stays tidy.
        match out.rfind(MARK_END) {
            Some(idx) => {
                let mut ins = String::new();
                for l in missing.drain(..) {
                    ins.push_str(l);
                    ins.push('\n');
                }
                out.insert_str(idx, &ins);
            }
            None => {
                for l in missing.drain(..) {
                    out.push_str(l);
                    out.push('\n');
                }
            }
        }
        std::fs::write(file, out).map_err(|e| format!("cannot write {}: {e}", file.display()))?;
        return Ok(true);
    }
    if lines.iter().all(|l| current.contains(l)) {
        return Ok(false);
    }
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let mut out = current;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');
    out.push_str(MARK_BEGIN);
    out.push('\n');
    for l in lines {
        out.push_str(l);
        out.push('\n');
    }
    out.push_str(MARK_END);
    out.push('\n');
    std::fs::write(file, out).map_err(|e| format!("cannot write {}: {e}", file.display()))?;
    Ok(true)
}

/// Shell rc files to manage: (label, file, lines).
fn rc_targets(bin: &Path) -> Vec<(&'static str, PathBuf, Vec<String>)> {
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
        ),
        (
            "zsh",
            home.join(".zshrc"),
            vec![export, "fpath=(~/.zfunc $fpath)".to_string()],
        ),
        (
            "fish",
            home.join(".config").join("fish").join("config.fish"),
            vec![fish_line],
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
    for (label, file, lines) in rc_targets(&bin) {
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        match ensure_block(&file, &refs) {
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

    println!("setup complete — restart your shell, then try: zz <TAB>");
    Ok(())
}

/// Runs on every `zz` invocation before command dispatch.
///
/// - Creates a missing bin dir silently (cheap, idempotent).
/// - Nudges interactive users toward `zz setup` when the bin dir is not
///   on `PATH`. Piped/non-TTY runs stay silent.
pub fn auto_heal() {
    let bin = zz_pm::paths::bin_dir();
    if !bin.exists() {
        let _ = std::fs::create_dir_all(&bin);
    }
    if bin_on_path(&bin) {
        return;
    }
    if std::io::stderr().is_terminal() {
        eprintln!("zz: hint: ~/.zz/bin is not on PATH — run `zz setup` to wire up your shell");
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
        let changed = ensure_block(&file, &["export PATH=\"$HOME/.zz/bin:$PATH\""]).unwrap();
        assert!(changed);
        let changed2 = ensure_block(&file, &["export PATH=\"$HOME/.zz/bin:$PATH\""]).unwrap();
        assert!(!changed2, "second run must be a no-op");
        let content = std::fs::read_to_string(&file).unwrap();
        assert_eq!(content.matches(MARK_BEGIN).count(), 1);
        assert!(content.contains("# existing"), "existing content preserved");
        // Topping up a second line keeps one block.
        let changed3 = ensure_block(
            &file,
            &[
                "export PATH=\"$HOME/.zz/bin:$PATH\"",
                "source ~/.zz/completions/zz.bash",
            ],
        )
        .unwrap();
        assert!(changed3);
        let content = std::fs::read_to_string(&file).unwrap();
        assert_eq!(content.matches(MARK_BEGIN).count(), 1);
        assert!(content.contains("source ~/.zz/completions/zz.bash"));
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
