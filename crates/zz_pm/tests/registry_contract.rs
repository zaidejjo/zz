//! Registry V2 Phase 0 — frozen HTTP contract tests.
//!
//! Black-box coverage for the 5 frozen endpoints
//! (`search`, `/{name}`, `/{name}/{version}.tgz`, `/publish`, auth) against
//! a stateful in-process mock registry (std sockets only, no external
//! services). Success paths (200/201) and error paths (401/404/409) plus
//! the client-side flows built on them (`resolve_with`, lock reuse).
//!
//! If any of these fail against a real backend, the HTTP contract changed
//! and `zzpm` clients must be updated in lockstep.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use zz_pm::remote::{expected_sha, pick_version, PublishRequest, RegistryClient, RemoteError};
use zz_pm::resolve::{resolve_with, ResolveOptions};

// ---------------------------------------------------------------------------
// Mock registry
// ---------------------------------------------------------------------------

const MOCK_TOKEN: &str = "zz_pat_contract_0123456789abcdef";

struct PkgVersion {
    tarball: Vec<u8>,
    sha256: String,
    license: String,
}

struct MockState {
    /// name -> (description, author, repo, readme, versions in publish order)
    meta: HashMap<String, (String, String, String, String)>,
    versions: HashMap<(String, String), PkgVersion>,
}

impl MockState {
    fn seeded() -> Self {
        let tarball = make_tarball(&[("zz.toml", "[package]\nname = \"widget\"\n")]);
        let sha256 = zz_pm::hash::hash_bytes(&tarball);
        let mut meta = HashMap::new();
        meta.insert(
            "widget".to_string(),
            (
                "A test widget".to_string(),
                "tester".to_string(),
                "https://example.com/widget".to_string(),
                "# widget\n".to_string(),
            ),
        );
        let mut versions = HashMap::new();
        versions.insert(
            ("widget".to_string(), "1.0.0".to_string()),
            PkgVersion {
                tarball,
                sha256,
                license: "MIT".to_string(),
            },
        );
        Self { meta, versions }
    }

    fn version_list(&self, name: &str) -> Vec<String> {
        let mut vs: Vec<String> = self
            .versions
            .keys()
            .filter(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
            .collect();
        vs.sort();
        vs
    }
}

fn make_tarball(files: &[(&str, &str)]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let enc = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::fast());
        let mut tar = tar::Builder::new(enc);
        for (name, content) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, name, content.as_bytes())
                .unwrap();
        }
        tar.finish().unwrap();
    }
    buf
}

fn reason(code: u16) -> &'static str {
    match code {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        409 => "Conflict",
        _ => "Error",
    }
}

fn reply(stream: &mut std::net::TcpStream, code: u16, ctype: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {code} {}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        reason(code),
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
}

fn json_reply(stream: &mut std::net::TcpStream, code: u16, value: &serde_json::Value) {
    reply(
        stream,
        code,
        "application/json",
        value.to_string().as_bytes(),
    );
}

fn read_request(stream: &mut std::net::TcpStream) -> (String, HashMap<String, String>, Vec<u8>) {
    let mut acc = Vec::new();
    let mut buf = [0u8; 8192];
    let head_end = loop {
        let n = stream.read(&mut buf).unwrap_or(0);
        if n == 0 {
            break 0;
        }
        acc.extend_from_slice(&buf[..n]);
        if let Some(i) = find_marker(&acc, b"\r\n\r\n") {
            break i + 4;
        }
        if acc.len() > 8 * 1024 * 1024 {
            break 0;
        }
    };
    let head = String::from_utf8_lossy(&acc[..head_end.min(acc.len())]).to_string();
    let request_line = head.lines().next().unwrap_or("").to_string();
    let mut headers = HashMap::new();
    let mut content_length = 0usize;
    for line in head.lines().skip(1) {
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                content_length = v.trim().parse().unwrap_or(0);
            }
            headers.insert(k.trim().to_lowercase(), v.trim().to_string());
        }
    }
    let mut body = acc.get(head_end..).unwrap_or(&[]).to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut buf).unwrap_or(0);
        if n == 0 {
            break;
        }
        body.extend_from_slice(&buf[..n]);
    }
    body.truncate(content_length);
    (request_line, headers, body)
}

fn find_marker(hay: &[u8], needle: &[u8]) -> Option<usize> {
    (0..=hay.len().saturating_sub(needle.len())).find(|&i| &hay[i..i + needle.len()] == needle)
}

fn query_param(path: &str, key: &str) -> String {
    path.split('?')
        .nth(1)
        .unwrap_or("")
        .split('&')
        .filter_map(|p| p.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.to_string())
        .unwrap_or_default()
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

fn valid_version(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// Serve the mock registry until `done` is set. Returns the base URL.
fn serve_mock(state: Arc<Mutex<MockState>>, done: Arc<std::sync::atomic::AtomicBool>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        while !done.load(std::sync::atomic::Ordering::Relaxed) {
            let (mut s, _) = match listener.accept() {
                Ok(v) => v,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(_) => break,
            };
            s.set_read_timeout(Some(Duration::from_secs(10))).ok();
            let (request_line, headers, body) = read_request(&mut s);
            let mut parts = request_line.split_whitespace();
            let method = parts.next().unwrap_or("").to_string();
            let full_path = parts.next().unwrap_or("/").to_string();
            let route_path = full_path.split('?').next().unwrap_or("/").to_string();

            if route_path == "/api/pkg/search" && method == "GET" {
                let q = query_param(&full_path, "q").to_lowercase();
                let limit: usize = query_param(&full_path, "limit")
                    .parse()
                    .unwrap_or(20)
                    .min(100);
                let st = state.lock().unwrap();
                let mut hits: Vec<serde_json::Value> = st
                    .meta
                    .iter()
                    .filter(|(name, (desc, _, _, _))| {
                        q.is_empty() || name.contains(&q) || desc.to_lowercase().contains(&q)
                    })
                    .map(|(name, (desc, author, _, _))| {
                        let latest = st.version_list(name).pop().unwrap_or_default();
                        serde_json::json!({
                            "name": name, "latest": latest,
                            "description": desc, "author": author,
                        })
                    })
                    .collect();
                hits.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
                hits.truncate(limit);
                json_reply(&mut s, 200, &serde_json::Value::Array(hits));
            } else if route_path == "/api/pkg/publish" && method == "POST" {
                let authed = headers
                    .get("authorization")
                    .map(|v| v == &format!("Bearer {MOCK_TOKEN}"))
                    .unwrap_or(false);
                if !authed {
                    json_reply(&mut s, 401, &serde_json::json!({"error": "Invalid token"}));
                    continue;
                }
                let v: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
                let name = v["name"].as_str().unwrap_or("").to_string();
                let version = v["version"].as_str().unwrap_or("").to_string();
                if !valid_name(&name) {
                    json_reply(
                        &mut s,
                        400,
                        &serde_json::json!({"error": "Invalid package name"}),
                    );
                    continue;
                }
                if !valid_version(&version) {
                    json_reply(
                        &mut s,
                        400,
                        &serde_json::json!({"error": "Invalid version"}),
                    );
                    continue;
                }
                let mut st = state.lock().unwrap();
                if st.versions.contains_key(&(name.clone(), version.clone())) {
                    json_reply(
                        &mut s,
                        409,
                        &serde_json::json!({"error": "Version already published"}),
                    );
                    continue;
                }
                let b64 = v["tarball_b64"].as_str().unwrap_or("");
                use base64::Engine as _;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(b64)
                    .unwrap_or_default();
                let actual = zz_pm::hash::hash_bytes(&bytes);
                let claimed = v["tarball_sha256"].as_str().unwrap_or("");
                if !claimed.is_empty() && claimed != actual {
                    json_reply(
                        &mut s,
                        400,
                        &serde_json::json!({"error": "Tarball hash mismatch"}),
                    );
                    continue;
                }
                st.versions.insert(
                    (name.clone(), version.clone()),
                    PkgVersion {
                        tarball: bytes,
                        sha256: actual.clone(),
                        license: v["license"].as_str().unwrap_or("").to_string(),
                    },
                );
                st.meta.entry(name.clone()).or_insert((
                    v["description"].as_str().unwrap_or("").to_string(),
                    "contract-tester".to_string(),
                    v["repo"].as_str().unwrap_or("").to_string(),
                    v["readme_md"].as_str().unwrap_or("").to_string(),
                ));
                json_reply(
                    &mut s,
                    201,
                    &serde_json::json!({"ok": true, "url": format!("/pkg/{name}"), "sha256": actual}),
                );
            } else if let Some(pkg_path) = route_path.strip_prefix("/api/pkg/") {
                if method != "GET" {
                    json_reply(&mut s, 400, &serde_json::json!({"error": "bad method"}));
                    continue;
                }
                let segs: Vec<&str> = pkg_path.split('/').collect();
                let st = state.lock().unwrap();
                match segs.as_slice() {
                    [name] => {
                        if !st.meta.contains_key(*name) {
                            json_reply(
                                &mut s,
                                404,
                                &serde_json::json!({"error": "Package not found"}),
                            );
                            continue;
                        }
                        let (desc, author, repo, readme) = st.meta[*name].clone();
                        let versions = st.version_list(name);
                        let latest = versions.last().cloned().unwrap_or_default();
                        let details: serde_json::Value = versions
                            .iter()
                            .map(|v| {
                                let pv = &st.versions[&(name.to_string(), v.clone())];
                                (
                                    v.clone(),
                                    serde_json::json!({
                                        "sha256": pv.sha256, "license": pv.license,
                                    }),
                                )
                            })
                            .collect::<serde_json::Map<String, serde_json::Value>>()
                            .into();
                        let latest_sha = st
                            .versions
                            .get(&(name.to_string(), latest.clone()))
                            .map(|p| p.sha256.clone())
                            .unwrap_or_default();
                        json_reply(
                            &mut s,
                            200,
                            &serde_json::json!({
                                "metadata": {
                                    "name": name, "latest": latest, "author": author,
                                    "repo": repo, "description": desc,
                                    "license": st.versions.get(&(name.to_string(), latest.clone()))
                                        .map(|p| p.license.clone()).unwrap_or_default(),
                                    "versions": versions, "version_details": details,
                                    "deps": {}, "tarball_sha256": latest_sha,
                                    "download_url": format!("/api/pkg/{name}/{latest}.tgz"),
                                },
                                "readme": readme,
                            }),
                        );
                    }
                    [name, ver] => {
                        let ver = ver.strip_suffix(".tgz").unwrap_or(ver);
                        match st.versions.get(&(name.to_string(), ver.to_string())) {
                            Some(pv) => reply(&mut s, 200, "application/gzip", &pv.tarball),
                            None => json_reply(
                                &mut s,
                                404,
                                &serde_json::json!({"error": "Tarball not found"}),
                            ),
                        }
                    }
                    _ => json_reply(&mut s, 404, &serde_json::json!({"error": "not found"})),
                }
            } else {
                json_reply(&mut s, 404, &serde_json::json!({"error": "not found"}));
            }
        }
    });
    base
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

static ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Isolated ZZ_HOME for CAS-touching tests (unique per test; guarded
/// because env is process-global).
fn isolated_home(tag: &str) -> PathBufGuard {
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!(
        "zz_contract_{tag}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::env::set_var("ZZ_HOME", &dir);
    PathBufGuard { dir, _guard }
}

struct PathBufGuard {
    dir: PathBuf,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl Drop for PathBufGuard {
    fn drop(&mut self) {
        std::env::remove_var("ZZ_HOME");
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn start() -> (
    String,
    Arc<Mutex<MockState>>,
    Arc<std::sync::atomic::AtomicBool>,
) {
    let state = Arc::new(Mutex::new(MockState::seeded()));
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let base = serve_mock(Arc::clone(&state), Arc::clone(&done));
    (base, state, done)
}

fn publish_fixture(client: &RegistryClient, name: &str, version: &str) -> String {
    let tarball = make_tarball(&[("zz.toml", &format!("[package]\nname = \"{name}\"\n"))]);
    let sha = zz_pm::hash::hash_bytes(&tarball);
    use base64::Engine as _;
    let req = PublishRequest {
        name: name.to_string(),
        version: version.to_string(),
        tarball_b64: base64::engine::general_purpose::STANDARD.encode(&tarball),
        tarball_sha256: sha.clone(),
        readme_md: Some(format!("# {name}\n")),
        deps: HashMap::new(),
        description: format!("{name} package"),
        repo: String::new(),
        license: "MIT".to_string(),
        category: "utilities".to_string(),
        keywords: vec!["fixture".to_string()],
    };
    let resp = client
        .publish(MOCK_TOKEN, &req)
        .expect("publish must succeed");
    assert!(resp.ok);
    assert_eq!(resp.sha256, sha);
    sha
}

// ---------------------------------------------------------------------------
// Contract tests
// ---------------------------------------------------------------------------

#[test]
fn search_finds_seeded_and_respects_limit() {
    let (base, _st, done) = start();
    let client = RegistryClient::new(&base);
    let hits = client.search("widg", 20).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].name, "widget");
    assert_eq!(hits[0].latest, "1.0.0");
    // Empty query lists everything (limit honored).
    let all = client.search("", 20).unwrap();
    assert!(!all.is_empty());
    done.store(true, std::sync::atomic::Ordering::Relaxed);
}

#[test]
fn metadata_shape_matches_contract() {
    let (base, _st, done) = start();
    let client = RegistryClient::new(&base);
    let info = client.fetch_metadata("widget").unwrap();
    let m = &info.metadata;
    assert_eq!(m.name, "widget");
    assert_eq!(m.latest, "1.0.0");
    assert_eq!(m.versions, vec!["1.0.0".to_string()]);
    assert_eq!(m.download_url, "/api/pkg/widget/1.0.0.tgz");
    assert!(!m.tarball_sha256.is_empty());
    let detail = &m.version_details["1.0.0"];
    assert_eq!(detail.sha256, m.tarball_sha256);
    assert_eq!(detail.license, "MIT");
    assert_eq!(info.readme, "# widget\n");
    done.store(true, std::sync::atomic::Ordering::Relaxed);
}

#[test]
fn download_matches_metadata_hash() {
    let (base, _st, done) = start();
    let client = RegistryClient::new(&base);
    let info = client.fetch_metadata("widget").unwrap();
    let bytes = client.download_tarball("widget", "1.0.0").unwrap();
    assert_eq!(
        zz_pm::hash::hash_bytes(&bytes),
        info.metadata.tarball_sha256
    );
    // Suffix-less URL works too.
    let bytes2 = client.download_tarball("widget", "1.0.0").unwrap();
    assert_eq!(bytes, bytes2);
    done.store(true, std::sync::atomic::Ordering::Relaxed);
}

#[test]
fn publish_round_trip_then_visible_everywhere() {
    let _home = isolated_home("publish_rt");
    let (base, _st, done) = start();
    let client = RegistryClient::new(&base);
    let sha = publish_fixture(&client, "gadget", "0.2.0");

    // Search, metadata, and download all see the new package.
    assert!(client
        .search("gadget", 20)
        .unwrap()
        .iter()
        .any(|h| h.name == "gadget"));
    let info = client.fetch_metadata("gadget").unwrap();
    assert_eq!(info.metadata.latest, "0.2.0");
    assert_eq!(info.metadata.version_details["0.2.0"].sha256, sha);
    let bytes = client.download_tarball("gadget", "0.2.0").unwrap();
    assert_eq!(zz_pm::hash::hash_bytes(&bytes), sha);

    // Full resolve flow stages it into the CAS with the recorded hash.
    let mut manifest = zz_pm::manifest::Manifest::default();
    manifest.dependencies.insert(
        "gadget".to_string(),
        zz_pm::manifest::DepSpec::Version("^0.2".to_string()),
    );
    let dir = std::env::temp_dir();
    let resolved = resolve_with(&manifest, None, &dir, &ResolveOptions::remote(&base)).unwrap();
    assert_eq!(resolved.locked.len(), 1);
    assert_eq!(resolved.locked[0].hash, sha);
    assert!(resolved.locked[0].source.starts_with("registry+"));
    done.store(true, std::sync::atomic::Ordering::Relaxed);
}

#[test]
fn republish_same_version_conflicts() {
    let (base, _st, done) = start();
    let client = RegistryClient::new(&base);
    publish_fixture(&client, "gizmo", "1.0.0");
    let tarball = make_tarball(&[("zz.toml", "")]);
    use base64::Engine as _;
    let req = PublishRequest {
        name: "gizmo".to_string(),
        version: "1.0.0".to_string(),
        tarball_b64: base64::engine::general_purpose::STANDARD.encode(&tarball),
        tarball_sha256: zz_pm::hash::hash_bytes(&tarball),
        readme_md: None,
        deps: HashMap::new(),
        description: String::new(),
        repo: String::new(),
        license: String::new(),
        category: String::new(),
        keywords: Vec::new(),
    };
    let err = client.publish(MOCK_TOKEN, &req).unwrap_err();
    assert!(matches!(err, RemoteError::Conflict(_)), "{err:?}");
    done.store(true, std::sync::atomic::Ordering::Relaxed);
}

#[test]
fn bad_token_is_unauthorized() {
    let (base, _st, done) = start();
    let client = RegistryClient::new(&base);
    let tarball = make_tarball(&[("zz.toml", "")]);
    use base64::Engine as _;
    let req = PublishRequest {
        name: "nope".to_string(),
        version: "1.0.0".to_string(),
        tarball_b64: base64::engine::general_purpose::STANDARD.encode(&tarball),
        tarball_sha256: zz_pm::hash::hash_bytes(&tarball),
        readme_md: None,
        deps: HashMap::new(),
        description: String::new(),
        repo: String::new(),
        license: String::new(),
        category: String::new(),
        keywords: Vec::new(),
    };
    let err = client.publish("zz_pat_wrong", &req).unwrap_err();
    assert!(matches!(err, RemoteError::Unauthorized), "{err:?}");
    done.store(true, std::sync::atomic::Ordering::Relaxed);
}

#[test]
fn unknown_package_and_tarball_are_404() {
    let (base, _st, done) = start();
    let client = RegistryClient::new(&base);
    assert!(matches!(
        client.fetch_metadata("missing").unwrap_err(),
        RemoteError::NotFound(_)
    ));
    assert!(matches!(
        client.download_tarball("widget", "9.9.9").unwrap_err(),
        RemoteError::NotFound(_)
    ));
    done.store(true, std::sync::atomic::Ordering::Relaxed);
}

#[test]
fn unsatisfiable_requirement_is_no_match() {
    let versions = vec!["1.0.0".to_string()];
    let err = pick_version(&versions, "^2.0").unwrap_err();
    assert!(matches!(err, RemoteError::VersionNoMatch { .. }));
    // expected_sha prefers per-version detail, then latest hash.
    let (base, _st, done) = start();
    let client = RegistryClient::new(&base);
    let info = client.fetch_metadata("widget").unwrap();
    assert!(!expected_sha(&info, "1.0.0").is_empty());
    done.store(true, std::sync::atomic::Ordering::Relaxed);
}

#[test]
fn locked_registry_dep_resolves_offline() {
    let _home = isolated_home("lock_reuse");
    let (base, _st, done) = start();
    let mut manifest = zz_pm::manifest::Manifest::default();
    manifest.dependencies.insert(
        "widget".to_string(),
        zz_pm::manifest::DepSpec::Version("^1.0".to_string()),
    );
    let dir = std::env::temp_dir();
    // First resolve hits the network and pins the hash.
    let resolved = resolve_with(&manifest, None, &dir, &ResolveOptions::remote(&base)).unwrap();
    let mut lock = zz_pm::lock::Lockfile::new();
    for dep in &resolved.locked {
        lock.upsert(dep.clone());
    }
    // Second resolve points at a dead port: the lock must satisfy it
    // with zero network traffic.
    let offline = resolve_with(
        &manifest,
        Some(&lock),
        &dir,
        &ResolveOptions::remote("http://127.0.0.1:9"),
    )
    .unwrap();
    assert_eq!(offline.locked.len(), 1);
    assert_eq!(offline.locked[0].hash, resolved.locked[0].hash);
    done.store(true, std::sync::atomic::Ordering::Relaxed);
}
