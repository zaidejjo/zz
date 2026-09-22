//! ZZ package manager CLI dispatch.
//!
//! Thin wrapper: parse args → call `zz_pm` functions → format output.

use std::path::Path;

/// Handle `zz init [--template T]`.
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

    let _manifest = zz_pm::manifest::Manifest::create_init(&dir, &name)?;
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
    println!("  .gitignore: ensured (vendor/, build/, src/bin/)");
    if !dir.join("src/main.zz").exists() {
        println!("  src/main.zz: created");
    }
    Ok(())
}

/// Handle `zz new <name> [--template cli|lib|web]`.
pub fn new(args: &[String]) -> Result<(), String> {
    let template = parse_flag_value(args, "--template");
    let name = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .ok_or("missing project name\n\nhint: usage: zz new <name> [--template cli|lib|web]")?;

    let parent = std::env::current_dir().map_err(|e| format!("cannot get cwd: {e}"))?;
    let project_dir = zz_pm::manifest::Manifest::create_new(&parent, name, template.as_deref())?;

    println!("created project `{}` at {}", name, project_dir.display());
    println!("  zz.toml: created");
    println!("  src/main.zz: created");
    println!("  .gitignore: ensured (vendor/, build/, src/bin/)");
    Ok(())
}

/// Handle `zz add <pkg>[@version] [--git URL --rev REV] [--path PATH]`.
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
        // plain version range (which needs a hosted registry to resolve).
        if let Some(spec) = registry_lookup(&pkg_name) {
            println!("resolved `{pkg_name}` via local registry (~/.zz/registry.toml)");
            spec
        } else {
            zz_pm::manifest::DepSpec::Version(version.unwrap_or_else(|| "^1.0".into()))
        }
    };

    manifest.dependencies.insert(pkg_name.clone(), dep_spec);
    manifest.save(&toml_path)?;

    println!("added `{pkg_name}` to zz.toml");
    println!("hint: run `zz install` to resolve and fetch");
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
/// Handle `zz install` / `zz i`.
pub fn install(_args: &[String]) -> Result<(), String> {
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

    println!("resolving {} dependencies...", manifest.dependencies.len());

    // Use the resolver to resolve all dependencies
    let resolved = zz_pm::resolve::resolve(&manifest, existing_lock.as_ref(), &dir)
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
        });
    }

    lock.set_manifest_deps_hash(deps_hash);
    lock.save(&lock_path)?;

    // Fetch git deps into CAS
    for dep in &resolved.locked {
        if dep.source.starts_with("git+") {
            // Extract commit from locked dep
            if let Some(commit) = &dep.commit {
                println!(
                    "  fetching {} @ {}...",
                    dep.name,
                    &commit[..8.min(commit.len())]
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
        }
    }

    // Link into project
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
    println!("hint: run `zz build` to compile");
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
            let cache_dir = zz_pm::paths::cache_objects_dir();
            if cache_dir.exists() {
                let count = count_entries(&cache_dir);
                std::fs::remove_dir_all(&cache_dir)
                    .map_err(|e| format!("cannot remove cache: {e}"))?;
                println!("cleared {count} cache entries from {}", cache_dir.display());
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

/// Handle `zz login`.
pub fn login(_args: &[String]) -> Result<(), String> {
    let (url, token, username) = zz_pm::auth::prompt_credentials()?;

    let mut creds = zz_pm::auth::Credentials::load()?;
    creds.set_token(&url, token, username);
    creds.save()?;

    // Verify permissions
    let path = zz_pm::auth::credentials_path();
    zz_pm::auth::verify_permissions(&path)?;

    println!("credentials saved for {url}");
    Ok(())
}

/// Handle `zz publish`.
pub fn publish(_args: &[String]) -> Result<(), String> {
    let dir = std::env::current_dir().map_err(|e| format!("cannot get cwd: {e}"))?;
    let toml_path = dir.join("zz.toml");

    if !toml_path.exists() {
        return Err("no zz.toml found in current directory".to_string());
    }

    let manifest = zz_pm::manifest::Manifest::load(&toml_path)?;

    // Validate
    zz_pm::publish::validate(&manifest).map_err(|e| e.to_string())?;

    // Run tests
    println!("running tests...");
    zz_pm::publish::run_tests(&dir).map_err(|e| e.to_string())?;
    println!("tests passed");

    // Pack
    println!("packing...");
    let tarball = zz_pm::publish::pack(&dir, &manifest).map_err(|e| e.to_string())?;
    println!("created: {}", tarball.display());
    println!("hint: upload not implemented yet (no real registry target)");

    Ok(())
}

/// Handle `zz update [pkg]`.
/// Handle `zz update [pkg]`.
///
/// Amendment 8: In M2, `zz update` only re-resolves floating git refs
/// (branch/tag → new resolved commit). Plain-version deps are a no-op
/// until a real registry exists.
pub fn update(args: &[String]) -> Result<(), String> {
    let dir = std::env::current_dir().map_err(|e| format!("cannot get cwd: {e}"))?;
    let toml_path = dir.join("zz.toml");

    if !toml_path.exists() {
        return Err("no zz.toml found in current directory".to_string());
    }

    let manifest = zz_pm::manifest::Manifest::load(&toml_path)?;
    let lock_path = dir.join("zz.lock");
    let lockfile = zz_pm::lock::Lockfile::load(&lock_path).ok();

    // Check if there are any git deps to update
    let has_git_deps = manifest
        .dependencies
        .values()
        .any(|s| matches!(s, zz_pm::manifest::DepSpec::Git(_)));

    let has_plain_deps = manifest
        .dependencies
        .values()
        .any(|s| matches!(s, zz_pm::manifest::DepSpec::Version(_)));

    if has_plain_deps {
        println!("hint: update only re-resolves git dependencies; no registry configured yet");
    }

    if !has_git_deps {
        println!("no git dependencies to update");
        return Ok(());
    }

    // Filter to requested packages (or all git deps)
    let requested: Vec<&str> = args
        .iter()
        .filter(|a| !a.starts_with('-'))
        .map(|s| s.as_str())
        .collect();

    let to_update: Vec<_> = manifest
        .dependencies
        .iter()
        .filter(|(name, spec)| {
            if !matches!(spec, zz_pm::manifest::DepSpec::Git(_)) {
                return false;
            }
            if requested.is_empty() {
                return true;
            }
            requested.contains(&name.as_str())
        })
        .collect();

    if to_update.is_empty() {
        println!("no git dependencies match the filter");
        return Ok(());
    }

    println!("updating {} git dependencies...", to_update.len());

    // Re-resolve each git dep to get latest commit for its ref
    let mut lock = lockfile.unwrap_or_default();
    for (name, spec) in &to_update {
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
                    });
                    println!("    → {}", &commit[..8.min(commit.len())]);
                }
                Err(e) => {
                    eprintln!("    warning: failed to resolve {name}: {e}");
                }
            }
        }
    }

    lock.set_manifest_deps_hash(manifest.deps_hash());
    lock.save(&lock_path)?;
    println!("lockfile updated: zz.lock");
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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

/// Count entries in a directory (non-recursive).
fn count_entries(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok()).count())
        .unwrap_or(0)
}
