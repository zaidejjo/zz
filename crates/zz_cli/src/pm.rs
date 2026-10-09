//! ZZ package manager CLI dispatch.
//!
//! Thin wrapper: parse args → call `zz_pm` functions → format output.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Handle `zz init [--template T] [--author A] [--description D] [--license L] [--repo URL]`.
pub fn init(args: &[String]) -> Result<(), String> {
    let template = parse_flag_value(args, "--template");
    let dir = std::env::current_dir().map_err(|e| format!("cannot get cwd: {e}"))?;
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "my_project".to_string());

    // Check if zz.toml already exists
    if dir.join("zz.toml").exists() {
        return Err("zz.toml already exists in this directory\n\
             hint: use `zz add` to add dependencies, or remove zz.toml and re-run `zz init`"
            .to_string());
    }

    let opts = init_options(args);
    let _manifest = zz_pm::manifest::Manifest::create_init_opts(&dir, &name, &opts)?;
    // Also create src/main.zz if it doesn't exist
    let src_dir = dir.join("src");
    if !src_dir.join("main.zz").exists() {
        std::fs::create_dir_all(&src_dir).map_err(|e| format!("cannot create src/: {e}"))?;
        let content = template_content(template.as_deref());
        std::fs::write(src_dir.join("main.zz"), content)
            .map_err(|e| format!("cannot write src/main.zz: {e}"))?;
    }

    println!("initialized project `{}` in {}", name, dir.display());
    println!("  zz.toml: created");
    println!("  .gitignore: ensured (vendor/, build/, bin/)");
    if !dir.join("src/main.zz").exists() {
        println!("  src/main.zz: created");
    }
    Ok(())
}

/// Handle `zz new <name> [--template T|pkg:P|URL|PATH] [--force] [--no-git] [...]`.
///
/// Templates: builtin (`cli`, `lib`, `web`), `pkg:<registry-name>`,
/// git URL, or local path (a `template/` subdir wins when present).
/// Files support `{{name}}` / `{{Name}}` substitution. Conflicts refuse
/// unless `--force`. Always `git init -b main` unless `--no-git`.
pub fn new(args: &[String]) -> Result<(), String> {
    let template = parse_flag_value(args, "--template");
    let force = args.iter().any(|a| a == "--force");
    let no_git = args.iter().any(|a| a == "--no-git");
    let name = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .ok_or("missing project name\n\nhint: usage: zz new <name> [--template cli|lib|web]")?;
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\\') {
        return Err(format!(
            "invalid project name `{name}`\n\
              hint: use a plain directory name (e.g. myapp)"
        ));
    }

    let parent = std::env::current_dir().map_err(|e| format!("cannot get cwd: {e}"))?;
    let project_dir = parent.join(name);
    if project_dir.exists() && !is_empty_dir(&project_dir) && !force {
        return Err(format!(
            "`{}` already exists and is not empty\n\
              hint: use --force to scaffold into it anyway",
            project_dir.display()
        ));
    }

    match template.as_deref() {
        None | Some("cli") | Some("lib") | Some("web") => {
            let opts = init_options(args);
            zz_pm::manifest::Manifest::create_new_opts(&parent, name, template.as_deref(), &opts)?;
        }
        Some(spec) => {
            new_from_template(&parent, name, spec, args, force)?;
        }
    }

    println!("created project `{name}` at {}", project_dir.display());
    println!("  zz.toml: created");
    println!("  src/main.zz: created");
    println!("  .gitignore: ensured (vendor/, build/, bin/)");

    if no_git {
        return Ok(());
    }
    match git_init_main(&project_dir) {
        GitInit::Done => println!("  git: initialized (branch main)"),
        GitInit::AlreadyRepo => println!("  git: already a repository"),
        GitInit::NoGit => eprintln!("warning: git not found — skipping `git init`"),
        GitInit::Failed(detail) => {
            eprintln!("warning: `git init` failed ({detail}) — continuing without a repo")
        }
    }
    Ok(())
}

/// Is `dir` missing, or an existing empty directory?
fn is_empty_dir(dir: &Path) -> bool {
    match std::fs::read_dir(dir) {
        Ok(mut rd) => rd.next().is_none(),
        Err(_) => true,
    }
}

/// Outcome of the `git init` step.
enum GitInit {
    Done,
    AlreadyRepo,
    NoGit,
    Failed(String),
}

/// `git init -b main` (fallback: plain `init` on old git). Never commits.
fn git_init_main(dir: &Path) -> GitInit {
    if dir.join(".git").exists() {
        return GitInit::AlreadyRepo;
    }
    // Probe once so a missing binary and a failed init report distinctly.
    if std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_err()
    {
        return GitInit::NoGit;
    }
    let main_init = std::process::Command::new("git")
        .arg("init")
        .arg("-b")
        .arg("main")
        .arg("--quiet")
        .current_dir(dir)
        .status();
    match main_init {
        Ok(s) if s.success() => GitInit::Done,
        Ok(_) => {
            // git < 2.28 has no `-b`: plain init, then rename the branch.
            let plain = std::process::Command::new("git")
                .arg("init")
                .arg("--quiet")
                .current_dir(dir)
                .status();
            match plain {
                Ok(p) if p.success() => {
                    let _ = std::process::Command::new("git")
                        .args(["symbolic-ref", "HEAD", "refs/heads/main"])
                        .current_dir(dir)
                        .status();
                    GitInit::Done
                }
                Ok(p) => GitInit::Failed(format!("exit {}", p.code().unwrap_or(-1))),
                Err(e) => GitInit::Failed(format!("cannot run git: {e}")),
            }
        }
        Err(e) => GitInit::Failed(format!("cannot run git: {e}")),
    }
}

/// Scaffold from a registry / git / path template into a fresh manifest.
fn new_from_template(
    parent: &Path,
    name: &str,
    spec: &str,
    args: &[String],
    force: bool,
) -> Result<(), String> {
    // Resolve the template root (owned tempdir when fetched).
    let (hold, root) = fetch_template_root(spec, args)?;
    let src = {
        let nested = root.join("template");
        if nested.is_dir() {
            nested
        } else {
            root.clone()
        }
    };
    if !src.is_dir() {
        return Err(format!(
            "template `{spec}` has no files\n\
              hint: expected a template/ directory or project files at the root"
        ));
    }

    // Manifest first (same as builtin path), then overlay template files —
    // never overwriting the generated zz.toml (deps come via `zz add`).
    let project_dir = parent.join(name);
    std::fs::create_dir_all(project_dir.join("src"))
        .map_err(|e| format!("cannot create src/: {e}"))?;
    let opts = init_options(args);
    zz_pm::manifest::Manifest::create_init_opts(&project_dir, name, &opts)?;
    let mut skipped_toml = false;
    let overlay = overlay_template(&src, &project_dir, name, force, &mut skipped_toml);
    // Fetched templates live in temp: remove regardless of overlay outcome.
    if let Some(tmp) = hold {
        let _ = std::fs::remove_dir_all(&tmp);
    }
    overlay?;
    if skipped_toml {
        println!("  template zz.toml ignored (use `zz add` for dependencies)");
    }
    if !project_dir.join("src").join("main.zz").exists() && !project_dir.join("main.zz").exists() {
        eprintln!("warning: template provides no src/main.zz entry file");
    }
    Ok(())
}

/// Fetch a template by spec; returns (tempdir guard, root dir).
/// Tempdir guard keeps fetched content alive for the overlay step.
fn fetch_template_root(spec: &str, args: &[String]) -> Result<(Option<PathBuf>, PathBuf), String> {
    let trimmed = spec.strip_prefix("pkg:").unwrap_or(spec);
    let is_url = spec.starts_with("http://")
        || spec.starts_with("https://")
        || spec.starts_with("git@")
        || spec.ends_with(".git");
    if is_url {
        let tmp = std::env::temp_dir().join(format!("zz-template-{}", std::process::id()));
        if tmp.exists() {
            let _ = std::fs::remove_dir_all(&tmp);
        }
        let status = std::process::Command::new("git")
            .args(["clone", "--depth", "1", spec])
            .arg(&tmp)
            .status()
            .map_err(|_| "cannot run git — install git to use URL templates".to_string())?;
        if !status.success() {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(format!("cannot clone template `{spec}`"));
        }
        return Ok((Some(tmp.clone()), tmp));
    }
    let as_path = std::path::PathBuf::from(spec);
    if as_path.exists() {
        let root = if as_path.is_absolute() {
            as_path
        } else {
            std::env::current_dir()
                .map_err(|e| format!("cannot get cwd: {e}"))?
                .join(as_path)
        };
        return Ok((None, root));
    }
    // Registry package (bare name or `pkg:name`), latest version.
    let base = registry_base_from(args);
    let client = zz_pm::remote::RegistryClient::new(&base);
    let info = client.fetch_metadata(trimmed).map_err(|e| match e {
        zz_pm::remote::RemoteError::NotFound(_) => format!(
            "template `{spec}` not found\n\
              hint: builtin templates are cli|lib|web; try `zz search {trimmed}`"
        ),
        other => format!("cannot reach {base}: {other}"),
    })?;
    let latest = info.metadata.latest.clone();
    let sha = zz_pm::remote::expected_sha(&info, &latest);
    let spinner = crate::ui::Spinner::start(&format!("Fetching template {trimmed} @ {latest}"));
    let cas_dir = match client.fetch_to_cas(trimmed, &latest, &sha) {
        Ok((dir, _)) => {
            spinner.finish(&format!("fetched template {trimmed} @ {latest}"));
            dir
        }
        Err(e) => {
            drop(spinner);
            return Err(format!("cannot fetch template {trimmed}: {e}"));
        }
    };
    Ok((None, cas_dir))
}

/// Copy template files into the project with `{{name}}` substitution.
/// Skips `.git` and `zz.toml`; refuses to overwrite without `force`.
fn overlay_template(
    src: &Path,
    dest: &Path,
    name: &str,
    force: bool,
    skipped_toml: &mut bool,
) -> Result<(), String> {
    let mut stack = vec![src.to_path_buf()];
    while let Some(cur) = stack.pop() {
        let rd = std::fs::read_dir(&cur)
            .map_err(|e| format!("cannot read template {}: {e}", cur.display()))?;
        let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        entries.sort();
        for path in entries {
            let file_name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if file_name == ".git" {
                continue;
            }
            let rel = path
                .strip_prefix(src)
                .map_err(|e| format!("bad template path: {e}"))?;
            if path.is_dir() {
                let target = dest.join(substitute_name(&rel.to_string_lossy(), name));
                std::fs::create_dir_all(&target)
                    .map_err(|e| format!("cannot create {}: {e}", target.display()))?;
                stack.push(path);
                continue;
            }
            if rel == Path::new("zz.toml") {
                *skipped_toml = true;
                continue;
            }
            let target_rel = substitute_name(&rel.to_string_lossy(), name);
            let target = dest.join(&target_rel);
            if target.exists() && !force {
                return Err(format!(
                    "template would overwrite `{target_rel}`\n\
                      hint: pass --force to allow it"
                ));
            }
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
            }
            let bytes =
                std::fs::read(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            match String::from_utf8(bytes) {
                Ok(text) => {
                    let rendered = substitute_name(&text, name);
                    std::fs::write(&target, rendered)
                        .map_err(|e| format!("cannot write {}: {e}", target.display()))?;
                }
                Err(_) => {
                    // Binary asset: copy raw, no substitution.
                    let bytes = std::fs::read(&path)
                        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
                    std::fs::write(&target, bytes)
                        .map_err(|e| format!("cannot write {}: {e}", target.display()))?;
                }
            }
        }
    }
    Ok(())
}

/// Replace `{{name}}` (raw) and `{{Name}}` (PascalCase) placeholders.
fn substitute_name(text: &str, name: &str) -> String {
    text.replace("{{name}}", name)
        .replace("{{Name}}", &pascal_case(name))
}

/// `my-tool` → `MyTool`.
fn pascal_case(name: &str) -> String {
    name.split(['-', '_', ' '])
        .filter(|s| !s.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

/// Handle `zz clean [--deps]`: remove build outputs (`bin/`, `build/`,
/// `src/bin/`); with `--deps` also `vendor/` + `zz.lock`. Explicit flags
/// only — never prompts, script-safe. Operates on the project root when
/// invoked inside one (outputs live at the root regardless of the
/// invocation subdir); standalone otherwise.
pub fn clean(args: &[String]) -> Result<(), String> {
    let with_deps = args.iter().any(|a| a == "--deps");
    let cwd = std::env::current_dir().map_err(|e| format!("cannot get cwd: {e}"))?;
    let dir = crate::loader::find_project_root(&cwd).unwrap_or(cwd);
    clean_in(&dir, with_deps)?;
    print_global_cache_hint();
    Ok(())
}

/// One-line global cache status so `zz clean` never looks like a no-op
/// while the build cache holds real weight: size plus the reclaim
/// command. Silent when the cache is absent or empty.
fn print_global_cache_hint() {
    let cache_dir = zz_pm::paths::build_cache_dir();
    if !cache_dir.exists() {
        return;
    }
    let (entries, bytes) = zz_pm::paths::dir_usage(&cache_dir);
    if entries == 0 {
        return;
    }
    println!(
        "global build cache: {} in {entries} entries ({})",
        crate::ui::human_bytes(bytes),
        cache_dir.display()
    );
    println!("hint: `zz cache clean` reclaims it (rebuilds on demand)");
}

/// `zz clean` in an explicit directory (split for tests — no cwd games).
fn clean_in(dir: &Path, with_deps: bool) -> Result<(), String> {
    if with_deps && !dir.join("zz.toml").exists() {
        return Err("no zz.toml found in current directory\n\
            hint: --deps removes vendor/ + zz.lock, which need a project"
            .to_string());
    }
    let mut targets = vec![
        dir.join("bin"),
        dir.join("build"),
        dir.join("src").join("bin"),
    ];
    if with_deps {
        targets.push(dir.join("vendor"));
        targets.push(dir.join("zz.lock"));
    }
    let mut freed = 0u64;
    let mut removed = 0u32;
    for target in &targets {
        if !target.exists() && !target.is_symlink() {
            continue;
        }
        let size = if target.is_dir() && !target.is_symlink() {
            zz_pm::paths::dir_usage(target).1
        } else {
            target.metadata().map(|m| m.len()).unwrap_or(0)
        };
        if target.is_dir() && !target.is_symlink() {
            std::fs::remove_dir_all(target)
                .map_err(|e| format!("cannot remove {}: {e}", target.display()))?;
        } else {
            std::fs::remove_file(target)
                .map_err(|e| format!("cannot remove {}: {e}", target.display()))?;
        }
        freed += size;
        removed += 1;
        println!("removed {}", target.display());
    }
    if removed == 0 {
        println!("nothing to clean");
    } else {
        println!(
            "cleaned {removed} path(s), freed {}",
            crate::ui::human_bytes(freed)
        );
    }
    if with_deps {
        println!("hint: run `zz install` to re-fetch dependencies");
    }
    Ok(())
}

/// Handle `zz add <pkg>[@version] [--git URL --rev REV] [--path PATH] [--registry URL]`.
///
/// Bare `name[@req]` consults the local alias registry first, then verifies
/// the package (and requirement) against the remote registry before writing
/// `zz.toml`. A verification failure is an error for unknown packages and a
/// warning for unreachable registries (the dep is still recorded; `zz install`
/// will retry with a precise error).
pub fn add(args: &[String]) -> Result<(), String> {
    let spec = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .ok_or("missing package spec\n\nhint: usage: zz add <pkg>[@version]")?;

    let (pkg_name, version) = parse_pkg_spec(spec);
    let git_url = parse_flag_value(args, "--git");
    let git_rev = parse_flag_value(args, "--rev");
    let path_dep = parse_flag_value(args, "--path");

    let dir = std::env::current_dir().map_err(|e| format!("cannot get cwd: {e}"))?;
    let toml_path = dir.join("zz.toml");

    let mut manifest = zz_pm::manifest::Manifest::load(&toml_path)?;

    // Build the dep spec
    let dep_spec = if let Some(p) = path_dep {
        zz_pm::manifest::DepSpec::Path(zz_pm::manifest::PathDep { path: p })
    } else if let Some(url) = git_url {
        let rev = git_rev.unwrap_or_else(|| "main".to_string());
        zz_pm::manifest::DepSpec::Git(zz_pm::manifest::GitDep {
            version: version.clone().unwrap_or_else(|| "*".into()),
            git: url,
            rev,
        })
    } else {
        // Bare name: consult the local registry before falling back to a
        // remote registry version.
        if let Some(spec) = registry_lookup(&pkg_name) {
            println!("resolved `{pkg_name}` via local registry (~/.zz/registry.toml)");
            spec
        } else {
            // Explicit `name@req`: verify the requirement as-is.
            // Bare `name`: pin the registry's latest as a caret requirement
            // (`^0.1.1`), so 0.x packages work without spelling a version.
            let req = match version {
                Some(v) => {
                    verify_remote_spec(&pkg_name, &v, registry_base_from(args))?;
                    v
                }
                None => resolve_latest_req(&pkg_name, registry_base_from(args))?,
            };
            zz_pm::manifest::DepSpec::Version(req)
        }
    };

    manifest.dependencies.insert(pkg_name.clone(), dep_spec);
    // `zz add` warns (not errors) when the added package already states
    // an unsatisfiable compiler requirement — but only for path deps,
    // whose manifest is readable right now. Registry/git deps are
    // checked by the install leg below, which hard-errors with the
    // upgrade hint.
    if let zz_pm::manifest::DepSpec::Path(p) =
        manifest.dependencies.get(&pkg_name).expect("just inserted")
    {
        let dep_toml = dir.join(&p.path).join("zz.toml");
        if dep_toml.exists() {
            if let Ok(dep_manifest) = zz_pm::manifest::Manifest::load(&dep_toml) {
                if let Some(req) = dep_manifest.package.zz.as_deref() {
                    if zz_pm::manifest::check_compiler_req(req, crate::VERSION).is_err() {
                        eprintln!(
                            "warning: dependency `{pkg_name}` needs zz {req}, \
                             this is zz {}\n\
                             hint: upgrade the compiler or loosen the dep's \
                             `[package] zz` requirement",
                            crate::VERSION
                        );
                    }
                }
            }
        }
    }
    manifest.save(&toml_path)?;

    println!("added `{pkg_name}` to zz.toml");

    // `add` installs immediately: resolve, fetch into vendor/, write zz.lock.
    // Same flags apply (`--registry` flows through to the install leg).
    install(args)
}

/// Bare `zz add <pkg>`: resolve the latest published version and return it
/// as a caret requirement.
///
/// - Unknown package → hard error suggesting `zz search`.
/// - Unreachable registry → warning + legacy `^1.0` fallback; `zz install`
///   retries the fetch (e.g. offline `add` for later install).
fn resolve_latest_req(pkg_name: &str, base: String) -> Result<String, String> {
    let client = zz_pm::remote::RegistryClient::new(&base);
    match client.fetch_metadata(pkg_name) {
        Ok(info) => {
            let latest = info.metadata.latest.clone();
            println!("resolved `{pkg_name}` to latest {latest} on {base}");
            Ok(format!("^{latest}"))
        }
        Err(zz_pm::remote::RemoteError::NotFound(_)) => Err(format!(
            "package `{pkg_name}` not found on {base}\n\
             hint: run `zz search {pkg_name}` to check the spelling"
        )),
        Err(e) => {
            eprintln!("warning: registry check skipped ({e})");
            eprintln!("hint: `zz install` will retry the fetch");
            Ok("^1.0".into())
        }
    }
}

/// Verify a `name@req` against the remote registry before recording it.
///
/// - Unknown package → hard error suggesting `zz search`.
/// - Requirement matching nothing → hard error suggesting `zz info`.
/// - Unreachable registry → warning; the dep is still recorded so
///   `zz install` can retry (e.g. offline `add` for later install).
fn verify_remote_spec(pkg_name: &str, req: &str, base: String) -> Result<(), String> {
    let client = zz_pm::remote::RegistryClient::new(&base);
    match client.fetch_metadata(pkg_name) {
        Ok(info) => match zz_pm::remote::pick_version(&info.metadata.versions, req) {
            Ok(v) => {
                println!("verified `{pkg_name}@{v}` on {base}");
                Ok(())
            }
            Err(_) => Err(format!(
                "no published version of `{pkg_name}` satisfies `{req}`\n\
                 hint: run `zz info {pkg_name}` to list available versions"
            )),
        },
        Err(zz_pm::remote::RemoteError::NotFound(_)) => Err(format!(
            "package `{pkg_name}` not found on {base}\n\
             hint: run `zz search {pkg_name}` to check the spelling"
        )),
        Err(e) => {
            eprintln!("warning: registry check skipped ({e})");
            eprintln!("hint: `zz install` will retry the fetch");
            Ok(())
        }
    }
}

/// Handle `zz search <query> [--limit N] [--registry URL]` (public, no login).
pub fn search(args: &[String]) -> Result<(), String> {
    let query = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .ok_or("missing search query\n\nhint: usage: zz search <query> [--limit N]")?;
    let limit: u32 = parse_flag_value(args, "--limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let base = registry_base_from(args);

    let hits = zz_pm::remote::RegistryClient::new(&base)
        .search(query, limit)
        .map_err(|e| e.to_string())?;
    if hits.is_empty() {
        println!("no packages match `{query}` on {base}");
        return Ok(());
    }
    for hit in &hits {
        let summary = if hit.description.is_empty() {
            "(no description)".to_string()
        } else {
            hit.description.clone()
        };
        println!(
            "{} {} — {} (by {})",
            hit.name, hit.latest, summary, hit.author
        );
    }
    Ok(())
}

/// Handle `zz info <pkg> [--registry URL]` (public, no login).
pub fn info(args: &[String]) -> Result<(), String> {
    let name = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .ok_or("missing package name\n\nhint: usage: zz info <pkg>")?;
    let base = registry_base_from(args);

    let pkg = zz_pm::remote::RegistryClient::new(&base)
        .fetch_metadata(name)
        .map_err(|e| e.to_string())?;
    let m = &pkg.metadata;
    println!("{} {}", m.name, m.latest);
    if !m.description.is_empty() {
        println!("  {0}", m.description);
    }
    println!("  author:  {}", m.author);
    if !m.license.is_empty() {
        println!("  license: {}", m.license);
    }
    if !m.repo.is_empty() {
        println!("  repo:    {}", m.repo);
    }
    if m.versions.is_empty() {
        println!("  versions: (none listed)");
    } else {
        let mut shown: Vec<&str> = m.versions.iter().map(String::as_str).collect();
        shown.sort();
        // Newest first, cap the list so huge histories stay readable.
        shown.reverse();
        let total = shown.len();
        let list: Vec<&str> = shown.into_iter().take(10).collect();
        println!(
            "  versions: {}{}",
            list.join(", "),
            if total > 10 {
                format!(" (+{} more)", total - 10)
            } else {
                String::new()
            }
        );
    }
    if !m.deps.is_empty() {
        println!("  deps of {}:", m.latest);
        let mut deps: Vec<(&String, &serde_json::Value)> = m.deps.iter().collect();
        deps.sort_by_key(|(k, _)| (*k).clone());
        for (dep, req) in deps {
            println!("    {dep} = {req}");
        }
    }
    if !pkg.readme.is_empty() {
        println!();
        // First 20 lines — enough to recognize the package.
        for line in pkg.readme.lines().take(20) {
            println!("  {line}");
        }
    }
    Ok(())
}

/// Resolve a bare `zz add <name>` via the local registry
/// (`~/.zz/registry.toml`). Returns `None` when no alias exists.
fn registry_lookup(name: &str) -> Option<zz_pm::manifest::DepSpec> {
    zz_pm::registry::Registry::load()
        .ok()
        .and_then(|reg| reg.resolve(name))
}

/// Handle `zz registry add|list|remove`.
///
/// Local alias file only (`~/.zz/registry.toml`): name → {path | git}.
/// No server, no publishing — share the file via dotfiles for teams.
pub fn registry(args: &[String]) -> Result<(), String> {
    let sub = args.first().map(String::as_str).unwrap_or("list");
    match sub {
        "add" => {
            let rest = args.get(1..).unwrap_or(&[]);
            let name = rest
                .iter()
                .find(|a| !a.starts_with('-'))
                .ok_or("missing package name\n\nhint: usage: zz registry add <name> [--path PATH | --git URL [--rev REV]]")?;
            let path = parse_flag_value(rest, "--path");
            let git = parse_flag_value(rest, "--git");
            let rev = parse_flag_value(rest, "--rev");
            if path.is_none() && git.is_none() {
                return Err("missing source\n\
                    hint: usage: zz registry add <name> [--path PATH | --git URL [--rev REV]]"
                    .to_string());
            }
            let mut reg = zz_pm::registry::Registry::load()?;
            reg.add(
                name.clone(),
                zz_pm::registry::RegistryEntry {
                    path,
                    git,
                    rev,
                    version: None,
                },
            );
            reg.save()?;
            println!("registry: added `{name}`");
            Ok(())
        }
        "list" => {
            let reg = zz_pm::registry::Registry::load()?;
            if reg.packages.is_empty() {
                println!("registry is empty (~/.zz/registry.toml)");
                println!("hint: zz registry add <name> [--path PATH | --git URL]");
                return Ok(());
            }
            for name in reg.names() {
                let entry = &reg.packages[name];
                if let Some(p) = &entry.path {
                    println!("{name} -> path {p}");
                } else if let Some(g) = &entry.git {
                    let rev = entry.rev.as_deref().unwrap_or("main");
                    println!("{name} -> git {g}#{rev}");
                }
            }
            Ok(())
        }
        "remove" => {
            let rest = args.get(1..).unwrap_or(&[]);
            let name = rest
                .iter()
                .find(|a| !a.starts_with('-'))
                .ok_or("missing package name\n\nhint: usage: zz registry remove <name>")?;
            let mut reg = zz_pm::registry::Registry::load()?;
            if !reg.remove(name) {
                return Err(format!("`{name}` is not in the registry"));
            }
            reg.save()?;
            println!("registry: removed `{name}`");
            Ok(())
        }
        other => Err(format!(
            "unknown registry subcommand `{other}`\n\
              hint: usage: zz registry add|list|remove"
        )),
    }
}
/// Handle `zz install` / `zz i` / `zzpm install`.
///
/// Registry (`Version`) deps resolve against `--registry` / `ZZ_REGISTRY` /
/// the default registry; git + path deps stay offline.
///
/// `zz install --path <dir>` is a separate mode (cargo-like): it builds the
/// project at `<dir>` in release and installs the binary into `~/.zz/bin`.
pub fn install(args: &[String]) -> Result<(), String> {
    if let Some(path) = parse_flag_value(args, "--path") {
        return install_path(&path);
    }
    let dir = std::env::current_dir().map_err(|e| format!("cannot get cwd: {e}"))?;
    let toml_path = dir.join("zz.toml");

    if !toml_path.exists() {
        return Err("no zz.toml found in current directory\n\
             hint: run `zz init` to create a project, or `zz add` to add dependencies"
            .to_string());
    }

    let manifest = zz_pm::manifest::Manifest::load(&toml_path)?;

    if manifest.dependencies.is_empty() {
        println!("no dependencies to install");
        return Ok(());
    }

    // Load existing lockfile if present
    let lock_path = dir.join("zz.lock");
    let existing_lock = zz_pm::lock::Lockfile::load(&lock_path).ok();

    // Amendment 3: Check path-dep staleness even if deps_match() is true
    let deps_hash = manifest.deps_hash();
    if let Some(ref lock) = existing_lock {
        if lock.deps_match(&deps_hash) {
            // Declaration hashes match — but path deps might have changed content
            if zz_pm::resolve::path_deps_stale(&manifest, Some(lock), &dir)
                .map_err(|e| e.to_string())?
            {
                println!("path dependency content changed, re-resolving...");
            } else {
                println!("dependencies unchanged (lockfile is up to date)");
                return Ok(());
            }
        }
    }

    crate::ui::header(&format!(
        "resolving {} dependencies",
        manifest.dependencies.len()
    ));
    crate::ui::step(1, 4, "Resolving versions");

    // Use the resolver to resolve all dependencies (registry-aware).
    let opts = zz_pm::resolve::ResolveOptions::remote(&registry_base_from(args));
    let resolved = zz_pm::resolve::resolve_with(&manifest, existing_lock.as_ref(), &dir, &opts)
        .map_err(|e| e.to_string())?;

    // Build new lockfile
    let mut lock = zz_pm::lock::Lockfile::new();
    for dep in &resolved.locked {
        lock.upsert(dep.clone());
    }

    // Store path dep hashes for future staleness detection
    for (name, hash) in &resolved.path_hashes {
        lock.upsert(zz_pm::lock::LockedDep {
            name: name.clone(),
            version: "*".to_string(),
            source: "path".to_string(),
            hash: hash.clone(),
            commit: None,
            native: None,
        });
    }

    lock.set_manifest_deps_hash(deps_hash);
    lock.save(&lock_path)?;

    // Fetch git deps into CAS
    crate::ui::step(2, 4, "Fetching packages");
    let fetch_total = resolved.locked.len().max(1);
    for (i, dep) in resolved.locked.iter().enumerate() {
        if dep.source.starts_with("git+") {
            // Extract commit from locked dep
            if let Some(commit) = &dep.commit {
                crate::ui::progress(
                    i + 1,
                    fetch_total,
                    &format!(
                        "fetching {} @ {}…",
                        dep.name,
                        &commit[..8.min(commit.len())]
                    ),
                );
                zz_pm::git::fetch_to_cas(
                    dep.source
                        .strip_prefix("git+")
                        .unwrap_or("")
                        .split('#')
                        .next()
                        .unwrap_or(""),
                    commit,
                )
                .map_err(|e| format!("failed to fetch {}: {e}", dep.name))?;
            }
        } else if dep.source.starts_with("registry+") {
            // Resolve already staged the CAS entry, but it may have been
            // GC'd since (or the lock predates the fetch): ensure it.
            // An empty hash addresses the packages dir itself — never
            // treat that as a hit (resolve heals such pins by re-fetch).
            let Some((base, name, version)) = zz_pm::remote::parse_registry_source(&dep.source)
            else {
                return Err(format!(
                    "cannot parse lockfile source for `{}`: {}\n\
                     hint: delete zz.lock and run `zz install` to regenerate",
                    dep.name, dep.source
                ));
            };
            if !dep.hash.is_empty() && zz_pm::paths::cas_entry(&dep.hash).exists() {
                continue;
            }
            crate::ui::progress(i + 1, fetch_total, &format!("fetching {name} @ {version}…"));
            zz_pm::remote::RegistryClient::new(&base)
                .fetch_to_cas(&name, &version, &dep.hash)
                .map_err(|e| format!("failed to fetch {}: {e}", dep.name))?;
        }
    }

    // Link into project
    crate::ui::step(3, 4, "Linking into vendor/");
    let linked =
        zz_pm::link::link_project(&dir, &manifest, &lock, zz_pm::link::LinkStrategy::Symlink)
            .map_err(|e| format!("link failed: {e}"))?;
    let linked_count = linked.len();

    // Register this project for safe GC (best-effort, don't fail install on registry errors)
    if linked_count > 0 {
        let mut known = zz_pm::gc::KnownProjects::load();
        let _ = known.register(&dir);
    }

    println!("lockfile updated: zz.lock");
    if linked_count > 0 {
        println!("linked {linked_count} dependencies into vendor/");
    }
    // Minimum-compiler requirements: the root project first, then every
    // linked dependency that ships a manifest. Unsatisfied requirements
    // fail the install with an upgrade hint (there is no point building
    // native extensions for a toolchain that cannot satisfy the tree).
    manifest
        .check_zz_version(crate::VERSION)
        .map_err(|e| format!("cannot install: {e}"))?;
    for (dep_name, dep_spec) in &manifest.dependencies {
        // Path deps resolve in place (never vendored); registry/git deps
        // are checked through their vendor/ link.
        let dep_toml = match dep_spec {
            zz_pm::manifest::DepSpec::Path(p) => dir.join(&p.path).join("zz.toml"),
            _ => dir.join("vendor").join(dep_name).join("zz.toml"),
        };
        if !dep_toml.exists() {
            continue;
        }
        if let Ok(dep_manifest) = zz_pm::manifest::Manifest::load(&dep_toml) {
            dep_manifest
                .check_zz_version(crate::VERSION)
                .map_err(|e| format!("cannot install dependency `{dep_name}`: {e}"))?;
        }
    }
    // Native plugins: build now so `zz run` works without a prior
    // `zz build` (registry tarballs exclude build outputs). Declarative
    // failures and gate violations fail the install; allowed legacy
    // hooks warn per-dependency inside the builder.
    crate::ui::step(4, 4, "Building native extensions");
    let build_opts = crate::build::NativeBuildOpts {
        allow_source_builds: args.iter().any(|a| a == "--allow-source-builds"),
        allow_hooks: args.iter().any(|a| a == "--allow-hooks"),
    };
    let built = crate::build::build_native_deps(&dir.join("zz.toml"), build_opts)?;
    if !built.audits.is_empty() {
        for (name, rec) in built.audits {
            if let Some(dep) = lock.deps.iter_mut().find(|d| d.name == name) {
                dep.native = Some(rec);
            }
        }
        lock.save(&lock_path)?;
        println!("native audit recorded in zz.lock");
    }
    println!("hint: run `zz build` to compile");
    Ok(())
}

/// `zz install --path <dir|file|pkg>`: build a tool from source (release)
/// and install its binary into `~/.zz/bin` — the cargo-install equivalent.
///
/// - `<dir>` must contain `zz.toml`; the entry is `src/main.zz`, then
///   `main.zz`. The binary is named after `[package] name`.
/// - A direct `.zz` file is also accepted; the binary takes the file stem.
/// - Anything else is treated as a package name (`name[@req]`): it is
///   fetched from the registry (local aliases first), then built exactly
///   like a local project.
/// - Re-running overwrites the installed binary (with a warning).
fn install_path(path_arg: &str) -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("cannot get cwd: {e}"))?;
    let target = if path_arg == "." {
        cwd.clone()
    } else {
        cwd.join(path_arg)
    };
    if target.is_file() {
        if target.extension().and_then(|e| e.to_str()) != Some("zz") {
            return Err(format!(
                "`{}` is not a .zz file\n\
                  hint: usage: zz install --path <dir|file.zz|pkg>",
                target.display()
            ));
        }
        let stem = target
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("cannot derive a binary name from `{}`", target.display()))?;
        let project_dir = target
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| cwd.clone());
        return build_and_install_bin(&target, &project_dir, &stem);
    }
    if target.is_dir() {
        let (project_dir, entry, bin_name) = project_from_dir(&target)?;
        return build_and_install_bin(&entry, &project_dir, &bin_name);
    }
    // Not a local path: treat it as a package name and fetch it.
    install_remote_tool(path_arg, &[])
}

/// Read `zz.toml` + entry file from an existing project directory.
/// Returns `(project_dir, entry, bin_name)`.
fn project_from_dir(target: &std::path::Path) -> Result<(PathBuf, PathBuf, String), String> {
    let manifest = zz_pm::manifest::Manifest::load(&target.join("zz.toml")).map_err(|_| {
        format!(
            "no zz.toml in `{}`\n\
                  hint: run `zz init` there first, point --path at a .zz file, or pass a registry package name",
            target.display()
        )
    })?;
    let name = manifest.package.name.clone();
    check_bin_name(&name)?;
    let entry = ["src/main.zz", "main.zz"]
        .iter()
        .map(|c| target.join(c))
        .find(|p| p.is_file())
        .ok_or_else(|| {
            format!(
                "no entry file in `{}`\n\
                  hint: expected src/main.zz or main.zz",
                target.display()
            )
        })?;
    Ok((target.to_path_buf(), entry, name))
}

/// Binary names become file names in `~/.zz/bin`: reject separators.
fn check_bin_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err(format!("invalid package name `{name}` in zz.toml"));
    }
    Ok(())
}

/// `zz install --path <name[@req]>`: fetch a package (local aliases, then
/// the registry), build it from source, and install it into `~/.zz/bin`.
fn install_remote_tool(spec: &str, args: &[String]) -> Result<(), String> {
    let (pkg_name, version) = parse_pkg_spec(spec);
    if pkg_name.is_empty()
        || pkg_name.contains('/')
        || pkg_name.contains('\\')
        || pkg_name.contains("..")
    {
        return Err(format!(
            "no such file or directory: `{spec}`\n\
              hint: usage: zz install --path <dir|file.zz|pkg> (try --path .)"
        ));
    }

    // Local aliases first: a `path` alias builds in place, a `git` alias
    // is fetched into the CAS — both stay offline.
    if let Some(alias) = registry_lookup(&pkg_name) {
        match alias {
            zz_pm::manifest::DepSpec::Path(p) => {
                let dir = std::env::current_dir()
                    .map_err(|e| format!("cannot get cwd: {e}"))?
                    .join(&p.path);
                if !dir.is_dir() {
                    return Err(format!(
                        "alias `{pkg_name}` points at missing dir `{}`",
                        p.path
                    ));
                }
                let (project_dir, entry, bin_name) = project_from_dir(&dir)?;
                return build_and_install_bin(&entry, &project_dir, &bin_name);
            }
            zz_pm::manifest::DepSpec::Git(g) => {
                crate::ui::header(&format!("installing {pkg_name} from git"));
                let spinner =
                    crate::ui::Spinner::start(&format!("Fetching {} #{}", pkg_name, g.rev));
                let commit = match zz_pm::git::resolve_rev(&g.git, &g.rev) {
                    Ok(commit) => commit,
                    Err(e) => {
                        drop(spinner);
                        return Err(format!("cannot resolve {}#{}: {e}", g.git, g.rev));
                    }
                };
                let cas_dir = match zz_pm::git::fetch_to_cas(&g.git, &commit) {
                    Ok(dir) => dir,
                    Err(e) => {
                        drop(spinner);
                        return Err(format!("cannot fetch {pkg_name}: {e}"));
                    }
                };
                spinner.finish(&format!("fetched {pkg_name}"));
                let (project_dir, entry, bin_name) = project_from_dir(&cas_dir)?;
                return build_and_install_bin(&entry, &project_dir, &bin_name);
            }
            // A bare version never comes from the alias file; fall through.
            zz_pm::manifest::DepSpec::Version(_) => {}
        }
    }

    // Registry: latest (or `name@req`), verified hash, staged in the CAS.
    let base = registry_base_from(args);
    let client = zz_pm::remote::RegistryClient::new(&base);
    crate::ui::header(&format!("installing {pkg_name} from {base}"));
    crate::ui::step(1, 4, "Resolving version");
    let info = client.fetch_metadata(&pkg_name).map_err(|e| match e {
        zz_pm::remote::RemoteError::NotFound(_) => format!(
            "package `{pkg_name}` not found on {base}\n\
              hint: run `zz search {pkg_name}` to check the spelling"
        ),
        other => format!("cannot reach {base}: {other}"),
    })?;
    let req = version
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "*".to_string());
    let picked = zz_pm::remote::pick_version(&info.metadata.versions, &req).map_err(|_| {
        format!(
            "no published version of `{pkg_name}` satisfies `{req}`\n\
              hint: run `zz info {pkg_name}` to list available versions"
        )
    })?;
    let sha = zz_pm::remote::expected_sha(&info, &picked);
    let spinner = crate::ui::Spinner::start(&format!("Fetching {pkg_name} @ {picked}"));
    let cas_dir = match client.fetch_to_cas(&pkg_name, &picked, &sha) {
        Ok((dir, _)) => {
            spinner.finish(&format!("fetched {pkg_name} @ {picked}"));
            dir
        }
        Err(e) => {
            drop(spinner);
            return Err(format!("cannot fetch {pkg_name} @ {picked}: {e}"));
        }
    };
    let (project_dir, entry, bin_name) = project_from_dir(&cas_dir)?;
    build_and_install_bin(&entry, &project_dir, &bin_name)
}

/// Release-build `entry` and install the result as `bin_name` in
/// `~/.zz/bin`. Shared by local and remote `install --path` flows.
fn build_and_install_bin(
    entry: &std::path::Path,
    project_dir: &std::path::Path,
    bin_name: &str,
) -> Result<(), String> {
    // Fail fast on an unsatisfied `[package] zz` compiler requirement
    // (bare .zz files have no manifest to check — skip those).
    let tool_toml = project_dir.join("zz.toml");
    if tool_toml.exists() {
        if let Ok(manifest) = zz_pm::manifest::Manifest::load(&tool_toml) {
            manifest
                .check_zz_version(crate::VERSION)
                .map_err(|e| format!("cannot install tool `{bin_name}`: {e}"))?;
        }
    }
    crate::ui::header(&format!(
        "installing {bin_name} from {}",
        project_dir.display()
    ));
    crate::ui::step(1, 3, &format!("Building {} (release)", entry.display()));
    // The clang link can run for minutes silently — spin with elapsed
    // time so a big tool build never looks frozen.
    let spinner = crate::ui::Spinner::start("Compiling release");
    let built = match crate::build::build_release(
        entry,
        crate::build::BuildMode::Release,
        &crate::build::ReleaseOptions::default(),
    ) {
        Ok(built) => {
            let size = std::fs::metadata(&built).map(|m| m.len()).unwrap_or(0);
            spinner.finish(&format!("built {}", crate::ui::human_bytes(size)));
            built
        }
        Err(e) => {
            drop(spinner);
            return Err(e);
        }
    };
    crate::ui::progress(
        2,
        3,
        &format!(
            "built {}",
            crate::ui::human_bytes(std::fs::metadata(&built).map(|m| m.len()).unwrap_or(0))
        ),
    );

    crate::ui::step(3, 3, "Installing into ~/.zz/bin");
    let bin_dir = zz_pm::paths::bin_dir();
    std::fs::create_dir_all(&bin_dir)
        .map_err(|e| format!("cannot create {}: {e}", bin_dir.display()))?;
    let dest = bin_dir.join(bin_name);
    if dest.exists() {
        crate::ui::warn(&format!("overwriting existing {}", dest.display()));
    }
    std::fs::copy(&built, &dest)
        .map_err(|e| format!("cannot install to {}: {e}", dest.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&dest)
            .map_err(|e| format!("cannot stat {}: {e}", dest.display()))?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&dest, perms)
            .map_err(|e| format!("cannot chmod {}: {e}", dest.display()))?;
    }
    crate::ui::ok(&format!("installed {bin_name} to {}", dest.display()));
    if !crate::setup::bin_on_path(&bin_dir) {
        crate::ui::warn("~/.zz/bin is not on PATH — run `zz setup`, then restart your shell.");
    }
    println!("installed {bin_name} ({})", dest.display());
    Ok(())
}

/// Handle `zz remove <pkg>` / `zz uninstall <pkg>`.
pub fn remove(args: &[String]) -> Result<(), String> {
    let pkg = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .ok_or("missing package name\n\nhint: usage: zz remove <pkg>")?;

    let dir = std::env::current_dir().map_err(|e| format!("cannot get cwd: {e}"))?;
    let toml_path = dir.join("zz.toml");

    if !toml_path.exists() {
        return Err("no zz.toml found in current directory".to_string());
    }

    let mut manifest = zz_pm::manifest::Manifest::load(&toml_path)?;

    if manifest.dependencies.remove(pkg).is_none() {
        return Err(format!(
            "`{pkg}` is not a dependency in zz.toml\n\
             hint: check `zz.toml` for the correct package name"
        ));
    }

    manifest.save(&toml_path)?;

    // Update lockfile
    let lock_path = dir.join("zz.lock");
    if lock_path.exists() {
        let mut lock = zz_pm::lock::Lockfile::load(&lock_path)?;
        lock.remove(pkg);
        lock.set_manifest_deps_hash(manifest.deps_hash());
        lock.save(&lock_path)?;
    }

    println!("removed `{pkg}` from zz.toml");
    println!("hint: run `zz install` to re-link");
    Ok(())
}

/// Handle `zz cache gc` / `zz cache clean`.
pub fn cache(args: &[String]) -> Result<(), String> {
    let subcmd = args.first().map(String::as_str);
    match subcmd {
        Some("gc") => {
            let result = zz_pm::gc::gc()?;
            println!("CAS garbage collection complete:");
            println!("  removed: {} entries", result.removed.len());
            println!("  kept: {} entries", result.kept.len());
            if result.bytes_freed > 0 {
                println!("  freed: {} bytes", result.bytes_freed);
            }
            Ok(())
        }
        Some("clean") | Some("clear") => {
            // The whole build-cache tree: native binary entries, precompiled
            // runtime archives, and per-module objects — all rebuildable.
            let cache_dir = zz_pm::paths::build_cache_dir();
            if cache_dir.exists() {
                let (entries, bytes) = zz_pm::paths::dir_usage(&cache_dir);
                std::fs::remove_dir_all(&cache_dir)
                    .map_err(|e| format!("cannot remove cache: {e}"))?;
                println!(
                    "cleared build cache: {entries} entries, freed {} ({})",
                    crate::ui::human_bytes(bytes),
                    cache_dir.display()
                );
            } else {
                println!("no cache to clear");
            }
            Ok(())
        }
        _ => Err("usage: zz cache gc | zz cache clean\n\
             hint: `gc` removes unused CAS entries, `clean` removes build cache"
            .to_string()),
    }
}

/// Handle `zz login [--browser] [--registry URL]`.
///
/// Default: prompt for a token (paste the `zz_pat_*` from the website) and
/// store it in `~/.zz/credentials.toml` (0600).
/// `--browser`: print the OAuth entry URL first, then prompt for the token.
pub fn login(args: &[String]) -> Result<(), String> {
    let base = registry_base_from(args);
    if args.iter().any(|a| a == "--browser") {
        println!("open this URL in your browser to authenticate:");
        println!("  {base}/api/auth/github");
        println!("paste the `zz_pat_*` token shown after login when prompted.");
    }
    let (url, token, username) = if args.iter().any(|a| a == "--registry") {
        prompt_credentials_for(&base)?
    } else {
        zz_pm::auth::prompt_credentials()?
    };

    let mut creds = zz_pm::auth::Credentials::load()?;
    creds.set_token(&url, token, username);
    creds.save()?;

    // Verify permissions
    let path = zz_pm::auth::credentials_path();
    zz_pm::auth::verify_permissions(&path)?;

    println!("credentials saved for {url}");
    Ok(())
}

/// Prompt for a token bound to a known registry URL (skips the URL prompt).
fn prompt_credentials_for(base: &str) -> Result<(String, String, Option<String>), String> {
    eprint!("Auth token for {base}: ");
    let token = read_login_token()?;
    if token.is_empty() {
        return Err("auth token cannot be empty".to_string());
    }
    Ok((base.to_string(), token, None))
}

/// Read one secret line from `/dev/tty` (falls back to stdin).
fn read_login_token() -> Result<String, String> {
    use std::io::BufRead;
    if let Ok(tty) = std::fs::File::open("/dev/tty") {
        let mut reader = std::io::BufReader::new(tty);
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .map_err(|e| format!("cannot read token: {e}"))?;
        return Ok(line.trim().to_string());
    }
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| format!("cannot read token: {e}"))?;
    Ok(line.trim().to_string())
}

/// Handle `zz publish [--dry-run] [--skip-tests] [--registry URL]`.
///
/// Validates, runs `zz test`, packs, then uploads to the registry.
/// `--dry-run` stops after packing (no network, no token needed).
/// `--skip-tests` skips the gate (native packages with external harnesses).
pub fn publish(args: &[String]) -> Result<(), String> {
    let dir = std::env::current_dir().map_err(|e| format!("cannot get cwd: {e}"))?;
    let toml_path = dir.join("zz.toml");

    if !toml_path.exists() {
        return Err("no zz.toml found in current directory".to_string());
    }

    let manifest = zz_pm::manifest::Manifest::load(&toml_path)?;

    // Validate
    zz_pm::publish::validate(&manifest).map_err(|e| e.to_string())?;
    for warning in zz_pm::publish::warnings(&manifest) {
        eprintln!("warning: {warning}");
    }

    // `--artifact-dir <dir>`: verify each `<name>-<version>-<tag>.tgz`
    // (full build/ layout + both-engine symbol check) and print the
    // `[native.prebuilt]` stanza to paste into zz.toml. Missing dir
    // entries mean a source-only release (allowed, warns).
    if let Some(artifact_dir_str) = parse_flag_value(args, "--artifact-dir") {
        let artifact_dir = std::path::PathBuf::from(&artifact_dir_str);
        let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(&artifact_dir)
            .map_err(|e| format!("cannot read --artifact-dir {artifact_dir_str}: {e}"))?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("tgz"))
            .collect();
        entries.sort();
        if entries.is_empty() {
            eprintln!("warning: --artifact-dir has no .tgz files; source-only release");
        }
        for tgz in &entries {
            match zz_pm::native_build::verify_artifact_file(tgz, &dir, &manifest.package.name) {
                Ok((tag, sha256)) => {
                    println!("artifact ok: {} ({})", tgz.display(), tag);
                    println!("  [target.{tag}]");
                    println!(
                        "  url = \"https://github.com/<org>/<repo>/releases/download/v{}/{}\"",
                        manifest.package.version,
                        tgz.file_name().unwrap_or_default().to_string_lossy()
                    );
                    println!("  sha256 = \"{sha256}\"");
                }
                Err(e) => return Err(format!("artifact rejected: {e}")),
            }
        }
    }

    // Run tests (skippable for native packages whose suites live
    // outside `zz test`, e.g. `zz run`-based e2e harnesses — the runner
    // never dlopens the package's own build/*.so).
    if args.iter().any(|a| a == "--skip-tests") {
        println!("skipping tests (--skip-tests)");
    } else {
        println!("running tests...");
        zz_pm::publish::run_tests(&dir).map_err(|e| {
            format!(
                "{e}\n\
                 hint: if this package tests itself outside `zz test`, \
                 use `zz publish --skip-tests`"
            )
        })?;
        println!("tests passed");
    }

    // Pack
    println!("packing...");
    let tarball = zz_pm::publish::pack(&dir, &manifest).map_err(|e| e.to_string())?;
    println!("created: {}", tarball.display());

    if args.iter().any(|a| a == "--dry-run") {
        println!("dry run: skipping upload");
        return Ok(());
    }

    let base = registry_base_from(args);
    let token = resolve_publish_token(&base)?;

    // Payload: tarball bytes (base64) + manifest metadata.
    let bytes = std::fs::read(&tarball).map_err(|e| format!("cannot read tarball: {e}"))?;
    let sha256 = zz_pm::hash::hash_bytes(&bytes);
    use base64::Engine as _;
    let req = zz_pm::remote::PublishRequest {
        name: manifest.package.name.clone(),
        version: manifest.package.version.clone(),
        tarball_b64: base64::engine::general_purpose::STANDARD.encode(&bytes),
        tarball_sha256: sha256,
        readme_md: std::fs::read_to_string(dir.join("README.md")).ok(),
        deps: manifest
            .dependencies
            .iter()
            .map(|(k, v)| (k.clone(), dep_spec_string(v)))
            .collect(),
        description: manifest.package.description.clone().unwrap_or_default(),
        repo: manifest.package.repository.clone().unwrap_or_default(),
        license: manifest.package.license.clone().unwrap_or_default(),
        category: manifest
            .package
            .category
            .as_deref()
            .map(|c| c.to_lowercase())
            .unwrap_or_default(),
        keywords: manifest.package.keywords.clone(),
    };

    println!("uploading {}@{} to {base}...", req.name, req.version);
    let resp = zz_pm::remote::RegistryClient::new(&base)
        .publish(&token, &req)
        .map_err(|e| e.to_string())?;
    println!("published: {base}{}", resp.url);
    if !resp.sha256.is_empty() {
        println!("sha256: {}", resp.sha256);
    }
    Ok(())
}

/// Resolve the registry token for publishing, first hit wins:
/// stored credentials → `ZZ_REGISTRY_TOKEN` env → interactive prompt
/// (same secret entry as `zz login`). Non-interactive sessions without
/// the env var keep the clean error — there is nobody to ask.
fn resolve_publish_token(base: &str) -> Result<String, String> {
    if let Some(token) = zz_pm::auth::Credentials::load()
        .ok()
        .and_then(|c| c.get_token(base).map(str::to_string))
    {
        return Ok(token);
    }
    if let Ok(env_token) = std::env::var("ZZ_REGISTRY_TOKEN") {
        let env_token = env_token.trim().to_string();
        if !env_token.is_empty() {
            return Ok(env_token);
        }
    }
    let interactive = std::io::IsTerminal::is_terminal(&std::io::stdin())
        || std::fs::File::open("/dev/tty").is_ok();
    if !interactive {
        return Err(format!(
            "not logged in for {base}\n\
             hint: `zz login --registry {base}` or set ZZ_REGISTRY_TOKEN"
        ));
    }
    eprint!("Auth token for {base} (paste `zz_pat_*`, Enter to abort): ");
    let token = read_login_token()?;
    if token.is_empty() {
        return Err(format!(
            "no token provided\n\
             hint: `zz login --registry {base}` or set ZZ_REGISTRY_TOKEN"
        ));
    }
    // Remember for next time (the store `zz login` uses).
    if let Ok(mut creds) = zz_pm::auth::Credentials::load() {
        creds.set_token(base, token.clone(), None);
        if let Err(e) = creds.save() {
            eprintln!("warning: could not save credentials: {e}");
        }
    }
    Ok(token)
}

/// Stringify a dep spec for the registry `deps` table.
fn dep_spec_string(spec: &zz_pm::manifest::DepSpec) -> String {
    match spec {
        zz_pm::manifest::DepSpec::Version(v) => v.clone(),
        zz_pm::manifest::DepSpec::Git(g) => format!("git+{}#{}", g.git, g.rev),
        zz_pm::manifest::DepSpec::Path(p) => format!("path:{}", p.path),
    }
}

/// Handle `zz update [pkg] [--registry URL]`.
///
/// Re-resolves floating refs: git branch/tags → new commits, registry
/// requirements → newest matching versions. Path deps are pinned by
/// content and never updated.
pub fn update(args: &[String]) -> Result<(), String> {
    let dir = std::env::current_dir().map_err(|e| format!("cannot get cwd: {e}"))?;
    let toml_path = dir.join("zz.toml");

    if !toml_path.exists() {
        return Err("no zz.toml found in current directory".to_string());
    }

    let manifest = zz_pm::manifest::Manifest::load(&toml_path)?;
    let lock_path = dir.join("zz.lock");
    let lockfile = zz_pm::lock::Lockfile::load(&lock_path).ok();

    // Filter to requested packages (or everything).
    let requested: Vec<&str> = args
        .iter()
        .filter(|a| !a.starts_with('-'))
        .map(|s| s.as_str())
        .collect();
    let wanted = |name: &str| requested.is_empty() || requested.contains(&name);

    let mut lock = lockfile.unwrap_or_default();
    let mut changed = false;

    // --- Git deps: re-resolve floating refs to new commits. ---
    let to_update_git: Vec<_> = manifest
        .dependencies
        .iter()
        .filter(|(name, spec)| matches!(spec, zz_pm::manifest::DepSpec::Git(_)) && wanted(name))
        .collect();

    if !to_update_git.is_empty() {
        println!("updating {} git dependencies...", to_update_git.len());
    }
    for (name, spec) in &to_update_git {
        if let zz_pm::manifest::DepSpec::Git(git_dep) = spec {
            println!("  {} @ {}...", name, git_dep.rev);
            match zz_pm::git::resolve_rev(&git_dep.git, &git_dep.rev) {
                Ok(commit) => {
                    lock.upsert(zz_pm::lock::LockedDep {
                        name: (*name).clone(),
                        version: git_dep.version.clone(),
                        source: format!("git+{}#{}", git_dep.git, git_dep.rev),
                        hash: String::new(),
                        commit: Some(commit.clone()),
                        native: None,
                    });
                    changed = true;
                    println!("    → {}", &commit[..8.min(commit.len())]);
                }
                Err(e) => {
                    eprintln!("    warning: failed to resolve {name}: {e}");
                }
            }
        }
    }

    // --- Registry deps: newest version still satisfying each requirement. ---
    let to_update_reg: Vec<_> = manifest
        .dependencies
        .iter()
        .filter(|(name, spec)| matches!(spec, zz_pm::manifest::DepSpec::Version(_)) && wanted(name))
        .collect();

    if !to_update_reg.is_empty() {
        let base = registry_base_from(args);
        let client = zz_pm::remote::RegistryClient::new(&base);
        println!("updating {} registry dependencies...", to_update_reg.len());
        for (name, spec) in &to_update_reg {
            if let zz_pm::manifest::DepSpec::Version(req) = spec {
                let current = lock
                    .find(name)
                    .map(|l| l.version.clone())
                    .unwrap_or_default();
                match client.fetch_metadata(name) {
                    Ok(info) => match zz_pm::remote::pick_version(&info.metadata.versions, req) {
                        Ok(picked) => {
                            if picked != current {
                                let expected = zz_pm::remote::expected_sha(&info, &picked);
                                lock.upsert(zz_pm::lock::LockedDep {
                                    name: (*name).clone(),
                                    version: picked.clone(),
                                    source: format!("registry+{base}/{name}#{picked}"),
                                    hash: expected,
                                    commit: None,
                                    native: None,
                                });
                                changed = true;
                                println!("    {name}: {current} → {picked}");
                            } else {
                                println!("    {name} is up to date ({current})");
                            }
                        }
                        Err(e) => eprintln!("    warning: {e}"),
                    },
                    Err(e) => eprintln!("    warning: failed to check {name}: {e}"),
                }
            }
        }
        println!("hint: run `zz install` to fetch and link");
    }

    if !changed {
        if to_update_git.is_empty() && to_update_reg.is_empty() {
            println!("nothing to update (no git or registry dependencies match)");
        } else {
            println!("already up to date");
        }
        return Ok(());
    }

    lock.set_manifest_deps_hash(manifest.deps_hash());
    lock.save(&lock_path)?;
    println!("lockfile updated: zz.lock");
    println!("hint: run `zz install` to fetch and link");
    Ok(())
}

// ---------------------------------------------------------------------------
// Dependency inspection: outdated / deps / audit
// ---------------------------------------------------------------------------

/// Load the project manifest (required) and lockfile (optional).
fn load_manifest_and_lock() -> Result<
    (
        PathBuf,
        zz_pm::manifest::Manifest,
        Option<zz_pm::lock::Lockfile>,
    ),
    String,
> {
    let dir = std::env::current_dir().map_err(|e| format!("cannot get cwd: {e}"))?;
    let toml_path = dir.join("zz.toml");
    if !toml_path.exists() {
        return Err("no zz.toml found in current directory\n\
            hint: run `zz init` to create a project"
            .to_string());
    }
    let manifest = zz_pm::manifest::Manifest::load(&toml_path)?;
    let lock = zz_pm::lock::Lockfile::load(&dir.join("zz.lock")).ok();
    Ok((dir, manifest, lock))
}

/// Short source label for a locked dep: registry, git, path, or other.
fn dep_source_label(source: &str) -> &'static str {
    if source.starts_with("registry+") {
        "registry"
    } else if source.starts_with("git+") {
        "git"
    } else if source == "path" {
        "path"
    } else {
        "other"
    }
}

/// Read a locked dep's own manifest (for graph traversal), best effort:
/// path deps from disk, registry/git deps from the CAS. `None` when the
/// content is not available locally (never touches the network).
fn locked_dep_manifest(
    dir: &Path,
    manifest: &zz_pm::manifest::Manifest,
    dep: &zz_pm::lock::LockedDep,
) -> Option<zz_pm::manifest::Manifest> {
    if dep.source == "path" {
        let rel = match manifest.dependencies.get(&dep.name)? {
            zz_pm::manifest::DepSpec::Path(p) => p.path.clone(),
            _ => return None,
        };
        return zz_pm::manifest::Manifest::load(&dir.join(&rel).join("zz.toml")).ok();
    }
    if dep.source.starts_with("registry+") {
        if dep.hash.is_empty() {
            return None;
        }
        return zz_pm::manifest::Manifest::load(
            &zz_pm::paths::cas_entry(&dep.hash).join("zz.toml"),
        )
        .ok();
    }
    if dep.source.starts_with("git+") {
        let commit = dep.commit.as_deref()?;
        return zz_pm::manifest::Manifest::load(&zz_pm::paths::cas_entry(commit).join("zz.toml"))
            .ok();
    }
    None
}

/// Handle `zz outdated`: locked vs wanted vs latest per registry dep.
pub fn outdated(args: &[String]) -> Result<(), String> {
    let (_dir, manifest, lock) = load_manifest_and_lock()?;
    let lock = lock.ok_or_else(|| {
        "no zz.lock found\n\
          hint: run `zz install` first"
            .to_string()
    })?;
    let mut wanted: Vec<(&String, &String)> = manifest
        .dependencies
        .iter()
        .filter_map(|(name, spec)| match spec {
            zz_pm::manifest::DepSpec::Version(req) => Some((name, req)),
            _ => None,
        })
        .collect();
    wanted.sort_by(|a, b| a.0.cmp(b.0));
    if wanted.is_empty() {
        println!("no registry dependencies to check");
        return Ok(());
    }

    let base = registry_base_from(args);
    let client = zz_pm::remote::RegistryClient::new(&base);
    crate::ui::header(&format!("checking {} packages on {base}", wanted.len()));
    let mut rows: Vec<(String, String, String, String, String)> = Vec::new();
    for (i, (name, req)) in wanted.iter().enumerate() {
        crate::ui::progress(i + 1, wanted.len(), name);
        let locked = lock
            .find(name)
            .map(|l| l.version.clone())
            .unwrap_or_else(|| "—".to_string());
        let info = match client.fetch_metadata(name) {
            Ok(info) => info,
            Err(zz_pm::remote::RemoteError::NotFound(_)) => {
                rows.push((
                    (*name).clone(),
                    locked,
                    "?".into(),
                    "?".into(),
                    "not found".into(),
                ));
                continue;
            }
            Err(e) => {
                crate::ui::warn(&format!("skipping {name}: {e}"));
                rows.push((
                    (*name).clone(),
                    locked,
                    "?".into(),
                    "?".into(),
                    "offline".into(),
                ));
                continue;
            }
        };
        let latest = info.metadata.latest.clone();
        let wanted_ver = zz_pm::remote::pick_version(&info.metadata.versions, req)
            .map(|v| v.to_string())
            .unwrap_or_else(|_| "∅".to_string());
        let status = if wanted_ver == "∅" {
            "no match"
        } else if locked == latest {
            "up to date"
        } else if locked == wanted_ver {
            "update available"
        } else {
            "behind requirement"
        };
        rows.push((
            (*name).clone(),
            locked,
            wanted_ver,
            latest,
            status.to_string(),
        ));
    }
    print!("{}", format_outdated_table(&rows));
    if rows.iter().any(|r| r.4 == "no match") {
        println!("hint: `zz info <pkg>` lists the available versions");
    } else if rows
        .iter()
        .any(|r| r.4 == "update available" || r.4 == "behind requirement")
    {
        println!("hint: run `zz update [pkg]` to re-resolve, then `zz install`");
    }
    Ok(())
}

/// Aligned `outdated` table (pure for tests).
fn format_outdated_table(rows: &[(String, String, String, String, String)]) -> String {
    let header = ("package", "locked", "wanted", "latest", "status");
    let mut widths = [
        header.0.len(),
        header.1.len(),
        header.2.len(),
        header.3.len(),
        header.4.len(),
    ];
    for r in rows {
        widths[0] = widths[0].max(r.0.len());
        widths[1] = widths[1].max(r.1.len());
        widths[2] = widths[2].max(r.2.len());
        widths[3] = widths[3].max(r.3.len());
        widths[4] = widths[4].max(r.4.len());
    }
    let mut out = format!(
        "{:<w0$}  {:<w1$}  {:<w2$}  {:<w3$}  {}\n",
        header.0,
        header.1,
        header.2,
        header.3,
        header.4,
        w0 = widths[0],
        w1 = widths[1],
        w2 = widths[2],
        w3 = widths[3],
    );
    for r in rows {
        out.push_str(&format!(
            "{:<w0$}  {:<w1$}  {:<w2$}  {:<w3$}  {}\n",
            r.0,
            r.1,
            r.2,
            r.3,
            r.4,
            w0 = widths[0],
            w1 = widths[1],
            w2 = widths[2],
            w3 = widths[3],
        ));
    }
    out
}

/// Handle `zz deps tree [--depth N]` / `zz deps why <pkg>`.
/// Flags without a subcommand (`zz deps --depth 2`) default to `tree`.
pub fn deps(args: &[String]) -> Result<(), String> {
    let (sub, rest) = match args.first().map(String::as_str) {
        Some(s) if !s.starts_with('-') => (s, &args[1..]),
        _ => ("tree", args),
    };
    match sub {
        "tree" => {
            let depth = rest
                .iter()
                .position(|a| a == "--depth")
                .and_then(|i| rest.get(i + 1))
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(usize::MAX);
            deps_tree(depth)
        }
        "why" => {
            let target = rest
                .first()
                .ok_or("missing package name\n\nhint: usage: zz deps why <pkg>")?;
            deps_why(target)
        }
        other => Err(format!(
            "unknown deps subcommand `{other}`\n\
              hint: usage: zz deps tree [--depth N] | zz deps why <pkg>"
        )),
    }
}

/// Child version label for display: locked version or requirement.
fn dep_version_label(
    name: &str,
    manifest: &zz_pm::manifest::Manifest,
    lock: &Option<zz_pm::lock::Lockfile>,
) -> String {
    if let Some(locked) = lock.as_ref().and_then(|l| l.find(name)) {
        return format!("{} ({})", locked.version, dep_source_label(&locked.source));
    }
    match manifest.dependencies.get(name) {
        Some(zz_pm::manifest::DepSpec::Version(req)) => format!("{req} (unresolved)"),
        Some(zz_pm::manifest::DepSpec::Git(_)) => "git (unresolved)".to_string(),
        Some(zz_pm::manifest::DepSpec::Path(_)) => "path (unresolved)".to_string(),
        None => "?".to_string(),
    }
}

fn deps_tree(max_depth: usize) -> Result<(), String> {
    let (dir, manifest, lock) = load_manifest_and_lock()?;
    if manifest.dependencies.is_empty() {
        println!("no dependencies");
        return Ok(());
    }
    let mut roots: Vec<String> = manifest.dependencies.keys().cloned().collect();
    roots.sort();
    let mut seen = HashSet::new();
    for (i, root) in roots.iter().enumerate() {
        let last_root = i + 1 == roots.len();
        println!("{} {}", root, dep_version_label(root, &manifest, &lock));
        print_tree_children(
            &dir, &manifest, &lock, root, "", last_root, 1, max_depth, &mut seen,
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn print_tree_children(
    dir: &Path,
    manifest: &zz_pm::manifest::Manifest,
    lock: &Option<zz_pm::lock::Lockfile>,
    parent: &str,
    prefix: &str,
    parent_last: bool,
    depth: usize,
    max_depth: usize,
    seen: &mut HashSet<String>,
) {
    if depth > max_depth {
        return;
    }
    let locked = lock.as_ref().and_then(|l| l.find(parent));
    let Some(dep) = locked else { return };
    let Some(child_manifest) = locked_dep_manifest(dir, manifest, dep) else {
        return;
    };
    let mut subs: Vec<String> = child_manifest.dependencies.keys().cloned().collect();
    subs.sort();
    for (i, sub) in subs.iter().enumerate() {
        let last = i + 1 == subs.len();
        let branch = if last { "└─ " } else { "├─ " };
        let stem = if parent_last { "   " } else { "│  " };
        if !seen.insert(sub.clone()) {
            println!("{prefix}{branch}{sub} (cycle)");
            continue;
        }
        println!(
            "{prefix}{branch}{sub} {}",
            dep_version_label(sub, manifest, lock)
        );
        let next_prefix = format!("{prefix}{stem}");
        print_tree_children(
            dir,
            manifest,
            lock,
            sub,
            &next_prefix,
            last,
            depth + 1,
            max_depth,
            seen,
        );
        seen.remove(sub);
    }
}

fn deps_why(target: &str) -> Result<(), String> {
    let (dir, manifest, lock) = load_manifest_and_lock()?;
    if manifest.dependencies.contains_key(target) {
        println!("{target} is a direct dependency (zz.toml)");
        return Ok(());
    }
    let lock = lock.as_ref().ok_or_else(|| {
        "no zz.lock found\n\
          hint: run `zz install` first"
            .to_string()
    })?;
    // Reverse edges: child -> parents, from every readable manifest.
    let mut parents: HashMap<String, Vec<String>> = HashMap::new();
    let mut queue: Vec<String> = manifest.dependencies.keys().cloned().collect();
    let mut visited = HashSet::new();
    while let Some(pkg) = queue.pop() {
        if !visited.insert(pkg.clone()) {
            continue;
        }
        let locked = lock.find(&pkg);
        let Some(dep) = locked else { continue };
        let Some(child_manifest) = locked_dep_manifest(&dir, &manifest, dep) else {
            continue;
        };
        for sub in child_manifest.dependencies.keys() {
            parents.entry(sub.clone()).or_default().push(pkg.clone());
            queue.push(sub.clone());
        }
    }
    if !parents.contains_key(target) {
        return Err(format!(
            "nothing depends on `{target}`\n\
              hint: check the spelling with `zz deps tree`"
        ));
    }
    // All chains from roots to target (DFS, capped).
    let roots: HashSet<String> = manifest.dependencies.keys().cloned().collect();
    let mut chains: Vec<Vec<String>> = Vec::new();
    let mut stack = vec![vec![target.to_string()]];
    while let Some(chain) = stack.pop() {
        if chains.len() >= 10 {
            break;
        }
        let head = chain.last().cloned().unwrap_or_default();
        if roots.contains(&head) {
            let mut full = chain.clone();
            full.reverse();
            chains.push(full);
            continue;
        }
        if let Some(ps) = parents.get(&head) {
            for p in ps {
                if chain.contains(p) {
                    continue;
                }
                let mut next = chain.clone();
                next.push(p.clone());
                stack.push(next);
            }
        }
    }
    chains.sort();
    chains.dedup();
    if chains.is_empty() {
        return Err(format!(
            "nothing depends on `{target}`\n\
              hint: check the spelling with `zz deps tree`"
        ));
    }
    for chain in &chains {
        println!("{}", chain.join(" → "));
    }
    Ok(())
}

/// Handle `zz audit`: integrity of locked pins (published? hash matches?
/// licensed? content present?). This is tamper/rot detection — not
/// vulnerability scanning (the registry publishes no advisory feed).
pub fn audit(args: &[String]) -> Result<(), String> {
    let (_dir, _manifest, lock) = load_manifest_and_lock()?;
    let lock = lock.ok_or_else(|| {
        "no zz.lock found\n\
          hint: run `zz install` first"
            .to_string()
    })?;
    let locked: Vec<&zz_pm::lock::LockedDep> = lock
        .deps
        .iter()
        .filter(|d| d.source.starts_with("registry+"))
        .collect();
    if locked.is_empty() {
        println!("no registry pins to audit");
        return Ok(());
    }
    let base = registry_base_from(args);
    let client = zz_pm::remote::RegistryClient::new(&base);
    crate::ui::header(&format!("auditing {} pins on {base}", locked.len()));
    let mut errors = 0u32;
    let mut warnings = 0u32;
    for (i, dep) in locked.iter().enumerate() {
        crate::ui::progress(i + 1, locked.len(), &dep.name);
        let Some((_, name, version)) = zz_pm::remote::parse_registry_source(&dep.source) else {
            crate::ui::warn(&format!("{}: unreadable lockfile source", dep.name));
            warnings += 1;
            continue;
        };
        let info = match client.fetch_metadata(&name) {
            Ok(info) => info,
            Err(e) => {
                crate::ui::warn(&format!("{}: cannot verify ({e})", dep.name));
                warnings += 1;
                continue;
            }
        };
        if !info.metadata.versions.contains(&version) {
            crate::ui::warn(&format!("{}@{version}: no longer published!", dep.name));
            errors += 1;
            continue;
        }
        let expected = zz_pm::remote::expected_sha(&info, &version);
        if !expected.is_empty() && expected != dep.hash {
            crate::ui::warn(&format!(
                "{}@{version}: lockfile hash does not match registry!",
                dep.name
            ));
            errors += 1;
            continue;
        }
        let licensed = info
            .metadata
            .version_details
            .get(&version)
            .map(|d| !d.license.is_empty())
            .unwrap_or(false)
            || !info.metadata.license.is_empty();
        if !licensed {
            crate::ui::warn(&format!("{}@{version}: no license metadata", dep.name));
            warnings += 1;
        }
        if !dep.hash.is_empty() && !zz_pm::paths::cas_entry(&dep.hash).exists() {
            crate::ui::warn(&format!(
                "{}@{version}: content missing locally (run `zz install`)",
                dep.name
            ));
            warnings += 1;
        }
    }
    if errors > 0 {
        return Err(format!(
            "audit failed: {errors} error(s), {warnings} warning(s)\n\
              hint: `zz update` + `zz install` re-pins from the registry"
        ));
    }
    if warnings > 0 {
        println!("audit passed with {warnings} warning(s)");
    } else {
        println!("audit passed: {} pins verified", locked.len());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Registry base URL: `--registry URL` flag wins, then `ZZ_REGISTRY`,
/// then the default (see `zz_pm::remote::registry_base`).
fn registry_base_from(args: &[String]) -> String {
    if let Some(flag) = parse_flag_value(args, "--registry") {
        return flag.trim_end_matches('/').to_string();
    }
    zz_pm::remote::registry_base()
}

/// Build `InitOptions` from `zz init` / `zz new` flags.
/// `--author` is repeatable and also splits on commas.
fn init_options(args: &[String]) -> zz_pm::manifest::InitOptions {
    let authors: Vec<String> = parse_flag_values(args, "--author")
        .iter()
        .flat_map(|s| s.split(','))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    zz_pm::manifest::InitOptions {
        authors,
        description: parse_flag_value(args, "--description"),
        license: parse_flag_value(args, "--license"),
        repository: parse_flag_value(args, "--repo"),
    }
}

/// Parse a `--flag value` or `--flag=value` CLI flag.
fn parse_flag_value(args: &[String], flag: &str) -> Option<String> {
    let mut iter = args.iter().peekable();
    while let Some(a) = iter.next() {
        if let Some(v) = a.strip_prefix(&format!("{flag}=")) {
            return Some(v.to_string());
        }
        if a == flag {
            if let Some(v) = iter.next() {
                return Some(v.clone());
            }
        }
    }
    None
}

/// Parse all values of a repeatable `--flag value` / `--flag=value` CLI flag.
fn parse_flag_values(args: &[String], flag: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut iter = args.iter().peekable();
    while let Some(a) = iter.next() {
        if let Some(v) = a.strip_prefix(&format!("{flag}=")) {
            out.push(v.to_string());
        } else if a == flag {
            if let Some(v) = iter.next() {
                out.push(v.clone());
            }
        }
    }
    out
}

/// Parse `name@version` or just `name`.
fn parse_pkg_spec(spec: &str) -> (String, Option<String>) {
    match spec.split_once('@') {
        Some((name, version)) => (name.to_string(), Some(version.to_string())),
        None => (spec.to_string(), None),
    }
}

/// Starter content for `zz init` / `zz new`.
fn template_content(template: Option<&str>) -> &'static str {
    match template {
        Some("lib") => {
            "/// Add one to a number.\npub func add_one(n: int) -> int {\n    n + 1\n}\n"
        }
        Some("web") => {
            "import std.http\n\nfunc main() {\n    s := http.server()\n    s2 := http.route_get(s, \"/\", |req| \"Hello, ZZ!\")\n    http.listen(s2, 8080) ?? println(\"failed to start server\")\n}\n"
        }
        _ => {
            "func main() {\n    println(\"Hello, ZZ!\")\n}\n"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "zz_pm_test_{name}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn outdated_table_aligns_columns() {
        let rows = vec![
            (
                "a".to_string(),
                "0.1.0".to_string(),
                "0.1.0".to_string(),
                "0.2.0".to_string(),
                "update available".to_string(),
            ),
            (
                "longer-name".to_string(),
                "1.0.0".to_string(),
                "1.0.0".to_string(),
                "1.0.0".to_string(),
                "up to date".to_string(),
            ),
        ];
        let table = format_outdated_table(&rows);
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("package"));
        assert!(lines[1].contains("update available"));
        // Aligned: status column starts at the same offset.
        let status_off = |l: &str| l.find("update available").or_else(|| l.find("up to date"));
        assert_eq!(status_off(lines[1]), status_off(lines[2]));
    }

    #[test]
    fn pascal_and_substitution() {
        assert_eq!(pascal_case("my-tool"), "MyTool");
        assert_eq!(pascal_case("my_tool"), "MyTool");
        assert_eq!(pascal_case("x"), "X");
        assert_eq!(
            substitute_name("pkg {{name}} struct {{Name}}", "my-tool"),
            "pkg my-tool struct MyTool"
        );
    }

    #[test]
    fn bin_name_rejects_separators() {
        assert!(check_bin_name("my-tool").is_ok());
        assert!(check_bin_name("").is_err());
        assert!(check_bin_name("a/b").is_err());
        assert!(check_bin_name("..").is_err());
    }

    #[test]
    fn overlay_copies_with_substitution_and_skips() {
        let dir = temp_root("overlay");
        let src = dir.join("tpl");
        std::fs::create_dir_all(src.join("src")).unwrap();
        std::fs::create_dir_all(src.join(".git")).unwrap();
        std::fs::write(src.join("zz.toml"), "[package]\nname=\"x\"\n").unwrap();
        std::fs::write(src.join(".git").join("config"), "x").unwrap();
        std::fs::write(src.join("src").join("{{name}}.zz"), "struct {{Name}} {}\n").unwrap();
        std::fs::write(src.join("blob.bin"), [0xff, 0x00, 0x41]).unwrap();
        let dest = dir.join("proj");
        std::fs::create_dir_all(&dest).unwrap();
        let mut skipped = false;
        overlay_template(&src, &dest, "my-tool", false, &mut skipped).unwrap();
        assert!(skipped, "template zz.toml must be skipped");
        assert!(!dest.join("zz.toml").exists());
        assert!(!dest.join(".git").exists());
        assert_eq!(
            std::fs::read_to_string(dest.join("src").join("my-tool.zz")).unwrap(),
            "struct MyTool {}\n"
        );
        assert_eq!(
            std::fs::read(dest.join("blob.bin")).unwrap(),
            vec![0xff, 0x00, 0x41]
        );
        // Refuses to overwrite without --force.
        let mut skipped2 = false;
        assert!(overlay_template(&src, &dest, "my-tool", false, &mut skipped2).is_err());
        assert!(overlay_template(&src, &dest, "my-tool", true, &mut skipped2).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clean_removes_build_outputs() {
        let dir = temp_root("clean");
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::create_dir_all(dir.join("src").join("bin")).unwrap();
        std::fs::write(dir.join("bin").join("app"), "12345678").unwrap();
        std::fs::write(dir.join("keep.zz"), "x := 1\n").unwrap();
        clean_in(&dir, false).unwrap();
        assert!(!dir.join("bin").exists());
        assert!(!dir.join("src").join("bin").exists());
        assert!(dir.join("keep.zz").exists());
        // --deps needs a project.
        assert!(clean_in(&dir, true).is_err());
        std::fs::write(
            dir.join("zz.toml"),
            "[package]\nname=\"x\"\nversion=\"0.1.0\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("vendor")).unwrap();
        std::fs::write(dir.join("zz.lock"), "lock").unwrap();
        clean_in(&dir, true).unwrap();
        assert!(!dir.join("vendor").exists());
        assert!(!dir.join("zz.lock").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
