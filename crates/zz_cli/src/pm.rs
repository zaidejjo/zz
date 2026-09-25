//! ZZ package manager CLI dispatch.
//!
//! Thin wrapper: parse args → call `zz_pm` functions → format output.

use std::path::Path;

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
    println!("  .gitignore: ensured (vendor/, build/, src/bin/)");
    if !dir.join("src/main.zz").exists() {
        println!("  src/main.zz: created");
    }
    Ok(())
}

/// Handle `zz new <name> [--template cli|lib|web] [--author A] [--description D] [--license L] [--repo URL]`.
pub fn new(args: &[String]) -> Result<(), String> {
    let template = parse_flag_value(args, "--template");
    let name = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .ok_or("missing project name\n\nhint: usage: zz new <name> [--template cli|lib|web]")?;

    let parent = std::env::current_dir().map_err(|e| format!("cannot get cwd: {e}"))?;
    let opts = init_options(args);
    let project_dir =
        zz_pm::manifest::Manifest::create_new_opts(&parent, name, template.as_deref(), &opts)?;

    println!("created project `{}` at {}", name, project_dir.display());
    println!("  zz.toml: created");
    println!("  src/main.zz: created");
    println!("  .gitignore: ensured (vendor/, build/, src/bin/)");
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
pub fn install(args: &[String]) -> Result<(), String> {
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
            println!("  fetching {name} @ {version}...");
            zz_pm::remote::RegistryClient::new(&base)
                .fetch_to_cas(&name, &version, &dep.hash)
                .map_err(|e| format!("failed to fetch {}: {e}", dep.name))?;
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

/// Count entries in a directory (non-recursive).
fn count_entries(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok()).count())
        .unwrap_or(0)
}
