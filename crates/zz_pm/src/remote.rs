//! Remote registry client (`zz-website` backend).
//!
//! Endpoints (see `zz-website/functions/api/pkg`):
//! - `GET {base}/api/pkg/search?q=&limit=` — public package search
//! - `GET {base}/api/pkg/{name}` — metadata + versions + readme
//! - `GET {base}/api/pkg/{name}/{version}.tgz` — tarball download (R2)
//! - `POST {base}/api/pkg/publish` — publish (`Bearer zz_pat_*`)
//!
//! Auth tokens live only in request headers and are never included in
//! errors or `Debug` output.

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::hash;

/// Default registry (mirrors `SITE_DOMAIN` in `zz-website/wrangler.toml`).
pub const DEFAULT_REGISTRY: &str = "https://zz-lang.pages.dev";

/// Resolve the registry base URL: `ZZ_REGISTRY` env var wins, otherwise
/// the default. A single trailing `/` is stripped so path joins are stable.
pub fn registry_base() -> String {
    let raw = std::env::var("ZZ_REGISTRY")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_REGISTRY.to_string());
    raw.trim_end_matches('/').to_string()
}

/// Errors from registry I/O. Tokens are never embedded in messages.
#[derive(Debug, Clone)]
pub enum RemoteError {
    /// TCP/TLS/DNS failure or timeout.
    Network(String),
    /// 401 — bad or missing token.
    Unauthorized,
    /// 404 — package / version / tarball absent.
    NotFound(String),
    /// 409 — version already published.
    Conflict(String),
    /// Other 4xx/5xx with body excerpt.
    Server(u16, String),
    /// 2xx body that does not match the expected schema.
    InvalidResponse(String),
    /// No published version satisfies the requirement.
    VersionNoMatch { name: String, req: String },
    /// Tarball bytes do not match the recorded SHA-256.
    HashMismatch { name: String, version: String },
    /// Tarball extraction failure (I/O or unsafe paths).
    Tarball(String),
    /// Local filesystem failure.
    Io(String),
}

impl std::fmt::Display for RemoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network(detail) => write!(
                f,
                "registry unreachable: {detail}\n\
                 hint: check your network connection, or set ZZ_REGISTRY to a reachable registry"
            ),
            Self::Unauthorized => write!(
                f,
                "registry rejected the credentials (401)\n\
                 hint: run `zz login` to refresh the token for this registry"
            ),
            Self::NotFound(what) => write!(
                f,
                "package not found: {what}\n\
                 hint: run `zz search {what}` to check the spelling"
            ),
            Self::Conflict(what) => write!(
                f,
                "version already published: {what}\n\
                 hint: bump `version` in zz.toml to publish again"
            ),
            Self::Server(code, body) => write!(
                f,
                "registry error (HTTP {code}): {body}\n\
                 hint: retry in a moment; if it persists the registry may be down"
            ),
            Self::InvalidResponse(detail) => write!(
                f,
                "registry returned an unexpected response: {detail}\n\
                 hint: the registry may be too old for this client — try `zz update`?"
            ),
            Self::VersionNoMatch { name, req } => write!(
                f,
                "no published version of `{name}` satisfies `{req}`\n\
                 hint: run `zz info {name}` to list available versions"
            ),
            Self::HashMismatch { name, version } => write!(
                f,
                "tarball for `{name}@{version}` failed integrity verification\n\
                 hint: the registry copy may be corrupt — retry `zz install`"
            ),
            Self::Tarball(detail) => write!(f, "cannot unpack tarball: {detail}"),
            Self::Io(msg) => write!(f, "registry I/O error: {msg}"),
        }
    }
}

impl std::error::Error for RemoteError {}

/// Convert a `ureq` failure into a `RemoteError` (status codes preserved,
/// transports collapsed into `Network`, timeouts named explicitly).
fn map_ureq(err: ureq::Error, context: &str) -> RemoteError {
    match err {
        ureq::Error::StatusCode(code) => {
            RemoteError::Server(code, format!("{context}: server replied HTTP {code}"))
        }
        other => {
            let detail = other.to_string();
            if detail.contains("timed out") || detail.contains("timeout") {
                return RemoteError::Network(format!(
                    "{context}: request timed out ({detail})\n\
                     hint: the registry may be slow — retry, or check ZZ_REGISTRY"
                ));
            }
            RemoteError::Network(format!("{context}: {detail}"))
        }
    }
}

/// Thin wrapper over `ureq` with a fixed base URL and timeout.
///
/// The token is supplied per call (publish) so one client serves both
/// anonymous reads and authenticated writes.
pub struct RegistryClient {
    base: String,
    timeout: Duration,
}

// Manual `Debug`: the struct holds no token today, but keeping a manual
// impl guarantees a future token field cannot leak via `#[derive]`.
impl std::fmt::Debug for RegistryClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegistryClient")
            .field("base", &self.base)
            .field("timeout", &self.timeout)
            .finish()
    }
}

impl RegistryClient {
    /// Client for a registry base URL (e.g. `https://zz-lang.pages.dev`).
    pub fn new(base: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            timeout: Duration::from_secs(60),
        }
    }

    /// Override the per-request timeout (downloads of large native
    /// tarballs may need more than the 60 s default).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn agent(&self) -> ureq::Agent {
        ureq::Agent::new_with_config(
            ureq::config::Config::builder()
                .timeout_global(Some(self.timeout))
                .user_agent(format!("zzpm/{}", env!("CARGO_PKG_VERSION")))
                .build(),
        )
    }

    /// `GET /api/pkg/search?q=&limit=` (public).
    pub fn search(&self, query: &str, limit: u32) -> Result<Vec<SearchHit>, RemoteError> {
        let url = format!("{}/api/pkg/search", self.base);
        let mut resp = self
            .agent()
            .get(&url)
            .query("q", query)
            .query("limit", limit.to_string())
            .call()
            .map_err(|e| map_ureq(e, "search failed"))?;
        let body = resp
            .body_mut()
            .read_to_string()
            .map_err(|e| RemoteError::Network(format!("search failed: {e}")))?;
        serde_json::from_str(&body)
            .map_err(|e| RemoteError::InvalidResponse(format!("search: {e}")))
    }

    /// `GET /api/pkg/{name}` (public). 404 maps to `NotFound`.
    pub fn fetch_metadata(&self, name: &str) -> Result<PackageInfo, RemoteError> {
        let url = format!("{}/api/pkg/{name}", self.base);
        let mut resp = self.agent().get(&url).call().map_err(|e| match e {
            ureq::Error::StatusCode(404) => RemoteError::NotFound(name.to_string()),
            other => map_ureq(other, &format!("cannot fetch metadata for `{name}`")),
        })?;
        let body = resp.body_mut().read_to_string().map_err(|e| {
            RemoteError::Network(format!("cannot fetch metadata for `{name}`: {e}"))
        })?;
        serde_json::from_str(&body)
            .map_err(|e| RemoteError::InvalidResponse(format!("metadata for `{name}`: {e}")))
    }

    /// `GET /api/pkg/{name}/{version}.tgz` (public). 404 maps to `NotFound`.
    pub fn download_tarball(&self, name: &str, version: &str) -> Result<Vec<u8>, RemoteError> {
        let url = format!("{}/api/pkg/{name}/{version}.tgz", self.base);
        let mut resp = self.agent().get(&url).call().map_err(|e| match e {
            ureq::Error::StatusCode(404) => {
                RemoteError::NotFound(format!("{name}@{version} (tarball)"))
            }
            other => map_ureq(other, &format!("cannot download `{name}@{version}`")),
        })?;
        resp.body_mut()
            .read_to_vec()
            .map_err(|e| RemoteError::Network(format!("cannot download `{name}@{version}`: {e}")))
    }

    /// Download, verify, and stage a package into the CAS.
    ///
    /// Mirrors `git::fetch_to_cas`: idempotent (existing entries are
    /// reused), content lives at `cas_entry(<tarball-sha256>)` so
    /// `link.rs` resolves it generically via the lockfile hash.
    ///
    /// Returns `(cas_dir, actual_sha256)`.
    pub fn fetch_to_cas(
        &self,
        name: &str,
        version: &str,
        expected_sha256: &str,
    ) -> Result<(PathBuf, String), RemoteError> {
        // Fast path: a verified entry is content-addressed, so an existing
        // dir at the expected hash needs no download (keeps `zz install`
        // cheap and offline-safe when warm).
        if !expected_sha256.is_empty() {
            let dir = crate::paths::cas_entry(expected_sha256);
            if dir.exists() {
                return Ok((dir, expected_sha256.to_string()));
            }
        }
        let bytes = self.download_tarball(name, version)?;
        let actual = hash::hash_bytes(&bytes);
        if !expected_sha256.is_empty() && actual != expected_sha256 {
            return Err(RemoteError::HashMismatch {
                name: name.to_string(),
                version: version.to_string(),
            });
        }
        let cas_dir = crate::paths::cas_entry(&actual);
        if cas_dir.exists() {
            return Ok((cas_dir, actual));
        }
        if let Some(parent) = cas_dir.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| RemoteError::Io(format!("cannot create CAS parent: {e}")))?;
        }
        // Extract to a unique temp dir first, then rename — a crashed
        // unpack never leaves a half-populated CAS entry behind.
        let tmp = unique_scratch_dir(name, version)?;
        extract_tarball(&bytes, &tmp)?;
        match std::fs::rename(&tmp, &cas_dir) {
            Ok(()) => {}
            Err(e) if cas_dir.exists() => {
                // Lost a staging race with another process: our bytes are
                // verified above, so the winner holds identical content.
                let _ = std::fs::remove_dir_all(&tmp);
                let _ = e;
            }
            Err(e) => {
                let _ = std::fs::remove_dir_all(&tmp);
                return Err(RemoteError::Io(format!("cannot stage CAS entry: {e}")));
            }
        }
        Ok((cas_dir, actual))
    }

    /// `POST /api/pkg/publish` (authenticated). Maps 401/409 precisely.
    pub fn publish(
        &self,
        token: &str,
        req: &PublishRequest,
    ) -> Result<PublishResponse, RemoteError> {
        let url = format!("{}/api/pkg/publish", self.base);
        let payload = serde_json::to_string(req)
            .map_err(|e| RemoteError::Io(format!("cannot encode publish payload: {e}")))?;
        let mut resp = self
            .agent()
            .post(&url)
            .header("Authorization", &format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .send(payload.as_bytes())
            .map_err(|e| match e {
                ureq::Error::StatusCode(401) => RemoteError::Unauthorized,
                ureq::Error::StatusCode(409) => {
                    RemoteError::Conflict(format!("{}@{}", req.name, req.version))
                }
                other => map_ureq(other, "publish failed"),
            })?;
        let body = resp
            .body_mut()
            .read_to_string()
            .map_err(|e| RemoteError::Network(format!("publish failed: {e}")))?;
        serde_json::from_str(&body)
            .map_err(|e| RemoteError::InvalidResponse(format!("publish: {e}")))
    }
}

/// One search result row.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchHit {
    pub name: String,
    pub latest: String,
    pub description: String,
    pub author: String,
}

/// Per-version integrity metadata (for pins older than `latest`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct VersionDetail {
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub license: String,
}

/// Registry metadata for a package (`GET /api/pkg/{name}`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PackageMetadata {
    pub name: String,
    pub latest: String,
    pub author: String,
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub versions: Vec<String>,
    /// Version → integrity/license detail. Absent on legacy servers.
    #[serde(default)]
    pub version_details: std::collections::HashMap<String, VersionDetail>,
    #[serde(default)]
    pub deps: std::collections::HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub tarball_sha256: String,
    #[serde(default)]
    pub download_url: String,
}

/// Full `GET /api/pkg/{name}` response body.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PackageInfo {
    pub metadata: PackageMetadata,
    #[serde(default)]
    pub readme: String,
}

/// `POST /api/pkg/publish` request body. The token travels in the
/// `Authorization` header, never in here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PublishRequest {
    pub name: String,
    pub version: String,
    /// Base64-encoded `.tar.gz` (what `publish::pack` produces).
    pub tarball_b64: String,
    /// Hex SHA-256 of the raw tarball bytes (server re-verifies).
    pub tarball_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readme_md: Option<String>,
    #[serde(default)]
    pub deps: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub license: String,
}

/// `POST /api/pkg/publish` success body.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PublishResponse {
    pub ok: bool,
    pub url: String,
    #[serde(default)]
    pub sha256: String,
}

/// Pick the highest published version satisfying `req`.
///
/// `req` accepts cargo-style ranges (`^1.2`, `>=1.0,<2.0`, `*`) and bare
/// exact versions (`1.2.3`). Unparseable versions in the list are skipped.
pub fn pick_version(versions: &[String], req: &str) -> Result<String, RemoteError> {
    let req_trim = req.trim();
    // `VersionReq::parse` rejects bare `1.2.3` — normalize to `=1.2.3`.
    let normalized = if semver::Version::parse(req_trim).is_ok() {
        format!("={req_trim}")
    } else {
        req_trim.to_string()
    };
    let range =
        semver::VersionReq::parse(&normalized).map_err(|_| RemoteError::VersionNoMatch {
            name: String::new(),
            req: req.to_string(),
        })?;
    let mut parsed: Vec<semver::Version> = versions
        .iter()
        .filter_map(|v| semver::Version::parse(v).ok())
        .collect();
    parsed.sort();
    parsed
        .iter()
        .rev()
        .find(|v| range.matches(v))
        .map(|v| v.to_string())
        .ok_or(RemoteError::VersionNoMatch {
            name: String::new(),
            req: req.to_string(),
        })
}

/// Expected tarball hash for `version`: per-version detail first, then
/// the `latest` hash when the pin equals latest, else empty (legacy
/// servers predate hash tracking — the client still records what it saw).
pub fn expected_sha(info: &PackageInfo, version: &str) -> String {
    if let Some(detail) = info.metadata.version_details.get(version) {
        if !detail.sha256.is_empty() {
            return detail.sha256.clone();
        }
    }
    if version == info.metadata.latest && !info.metadata.tarball_sha256.is_empty() {
        return info.metadata.tarball_sha256.clone();
    }
    String::new()
}

/// Unpack `.tar.gz` bytes into `dest`, refusing entries that would escape
/// it (absolute paths or `..` components). `dest` is created as needed.
pub fn extract_tarball(bytes: &[u8], dest: &Path) -> Result<(), RemoteError> {
    std::fs::create_dir_all(dest)
        .map_err(|e| RemoteError::Tarball(format!("cannot create {dest:?}: {e}")))?;
    let gz = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(gz);
    let entries = archive
        .entries()
        .map_err(|e| RemoteError::Tarball(format!("cannot read archive: {e}")))?;
    for entry in entries {
        let mut entry =
            entry.map_err(|e| RemoteError::Tarball(format!("cannot read entry: {e}")))?;
        let path = entry
            .path()
            .map_err(|e| RemoteError::Tarball(format!("bad entry path: {e}")))?
            .into_owned();
        // Reject escapes before joining: absolute paths and any `..`.
        if path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err(RemoteError::Tarball(format!(
                "entry escapes package root: {}",
                path.display()
            )));
        }
        let display = path.display().to_string();
        entry
            .unpack_in(dest)
            .map_err(|e| RemoteError::Tarball(format!("cannot unpack {display}: {e}")))?;
    }
    Ok(())
}

/// Split a `registry+{base}/{name}#{version}` lockfile source into
/// `(base, name, version)`. Returns `None` for any other shape.
pub fn parse_registry_source(source: &str) -> Option<(String, String, String)> {
    let rest = source.strip_prefix("registry+")?;
    let (left, version) = rest.rsplit_once('#')?;
    let (base, name) = left.rsplit_once('/')?;
    if base.is_empty() || name.is_empty() || version.is_empty() {
        return None;
    }
    Some((base.to_string(), name.to_string(), version.to_string()))
}

/// Unique scratch dir for staging a download before the CAS rename.
fn unique_scratch_dir(name: &str, version: &str) -> Result<PathBuf, RemoteError> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let pid = std::process::id();
    let digest = hash::hash_bytes(format!("{name}:{version}:{pid}:{nanos}").as_bytes());
    Ok(std::env::temp_dir().join(format!("zz_pm_dl_{}", &digest[..16])))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn registry_base_default_and_env() {
        std::env::remove_var("ZZ_REGISTRY");
        assert_eq!(registry_base(), DEFAULT_REGISTRY);
        std::env::set_var("ZZ_REGISTRY", "https://example.com/");
        assert_eq!(registry_base(), "https://example.com");
        std::env::remove_var("ZZ_REGISTRY");
    }

    #[test]
    fn pick_version_ranges() {
        let vs = vec![
            "0.9.0".to_string(),
            "1.2.0".to_string(),
            "1.4.1".to_string(),
            "2.0.0".to_string(),
        ];
        assert_eq!(pick_version(&vs, "^1.2").unwrap(), "1.4.1");
        assert_eq!(pick_version(&vs, "*").unwrap(), "2.0.0");
        assert_eq!(pick_version(&vs, "1.2.0").unwrap(), "1.2.0");
        assert_eq!(pick_version(&vs, ">=1.0,<2.0").unwrap(), "1.4.1");
        assert!(pick_version(&vs, "^3.0").is_err());
        // Garbage entries are skipped, not fatal.
        let mut with_junk = vs.clone();
        with_junk.push("not-a-version".to_string());
        assert_eq!(pick_version(&with_junk, "*").unwrap(), "2.0.0");
    }

    #[test]
    fn pick_version_bad_req_names_req() {
        let vs = vec!["1.0.0".to_string()];
        let err = pick_version(&vs, ">=2.0").unwrap_err();
        match err {
            RemoteError::VersionNoMatch { req, .. } => assert_eq!(req, ">=2.0"),
            other => panic!("wrong variant: {other:?}"),
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

    /// Build a `.tar.gz` with a raw entry name, bypassing `tar::Builder`'s
    /// own path validation so `extract_tarball`'s guard is what gets tested.
    fn make_raw_tarball(name: &str, content: &[u8]) -> Vec<u8> {
        let mut header = [0u8; 512];
        let name_bytes = name.as_bytes();
        assert!(name_bytes.len() < 100, "test name too long");
        header[..name_bytes.len()].copy_from_slice(name_bytes);
        // mode, uid, gid
        header[100..108].copy_from_slice(b"0000644\0");
        header[108..116].copy_from_slice(b"0000000\0");
        header[116..124].copy_from_slice(b"0000000\0");
        // size (octal, NUL-terminated)
        let size_field = format!("{:011o}\0", content.len());
        header[124..136].copy_from_slice(size_field.as_bytes());
        // mtime
        header[136..148].copy_from_slice(b"00000000000\0");
        // chksum: spaces for the computation, then octal digits
        header[148..156].copy_from_slice(b"        ");
        header[156] = b'0';
        header[257..262].copy_from_slice(b"ustar");
        let sum: u32 = header.iter().map(|b| *b as u32).sum();
        let cksum_field = format!("{sum:06o}\0 ");
        header[148..156].copy_from_slice(cksum_field.as_bytes());

        let mut tar = Vec::new();
        tar.extend_from_slice(&header);
        tar.extend_from_slice(content);
        tar.resize(tar.len() + (512 - content.len() % 512) % 512, 0);
        tar.extend_from_slice(&[0u8; 1024]); // end-of-archive markers

        let mut buf = Vec::new();
        {
            use std::io::Write;
            let mut enc = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::fast());
            enc.write_all(&tar).unwrap();
            enc.finish().unwrap();
        }
        buf
    }

    #[test]
    fn extract_round_trip() {
        let bytes = make_tarball(&[("zz.toml", "[package]\n"), ("src/a.zz", "x\n")]);
        let dest = std::env::temp_dir().join(format!("zz_rm_ext_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);
        extract_tarball(&bytes, &dest).unwrap();
        assert!(dest.join("zz.toml").exists());
        assert!(dest.join("src/a.zz").exists());
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn extract_rejects_parent_escape() {
        let bytes = make_raw_tarball("../evil.zz", b"x\n");
        let dest = std::env::temp_dir().join(format!("zz_rm_evil_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);
        let err = extract_tarball(&bytes, &dest).unwrap_err();
        assert!(
            matches!(err, RemoteError::Tarball(ref m) if m.contains("escapes package root")),
            "{err:?}"
        );
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn extract_rejects_absolute_path() {
        let bytes = make_raw_tarball("/tmp/evil.zz", b"x\n");
        let dest = std::env::temp_dir().join(format!("zz_rm_abs_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);
        let err = extract_tarball(&bytes, &dest).unwrap_err();
        assert!(
            matches!(err, RemoteError::Tarball(ref m) if m.contains("escapes package root")),
            "{err:?}"
        );
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn parse_registry_source_cases() {
        let (base, name, ver) =
            parse_registry_source("registry+https://example.com/foo#1.2.0").unwrap();
        assert_eq!(base, "https://example.com");
        assert_eq!(name, "foo");
        assert_eq!(ver, "1.2.0");
        assert!(parse_registry_source("git+https://example.com/foo#main").is_none());
        assert!(parse_registry_source("registry+no-version-sep").is_none());
        assert!(parse_registry_source("registry+https://h/#").is_none());
    }

    #[test]
    fn error_messages_never_carry_tokens() {
        // The client holds no token field; errors are static strings.
        let e = RemoteError::Unauthorized.to_string();
        assert!(!e.contains("zz_pat_"));
        let dbg = format!("{:?}", RegistryClient::new("https://example.com"));
        assert!(dbg.contains("example.com"));
    }

    // --- Minimal blocking HTTP harness (std only) for client tests. ---

    /// Serve `expect` requests on 127.0.0.1, routing by request path.
    /// Returns the base URL (caller must `join` the handle).
    ///
    /// Fails fast: if fewer than `expect` requests arrive within the
    /// deadline the thread panics instead of blocking `join` forever
    /// (a count mismatch means the client under test changed its
    /// request pattern — surface it, don't hang the suite).
    pub(crate) fn serve(
        expect: usize,
        route: impl Fn(&str) -> (u16, &'static str, Vec<u8>) + Send + 'static,
    ) -> (String, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            use std::io::{Read, Write};
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
            let mut served = 0;
            while served < expect {
                match listener.accept() {
                    Ok((stream, _)) => {
                        served += 1;
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
                        let mut stream = stream;
                        let mut buf = [0u8; 8192];
                        let n = stream.read(&mut buf).unwrap_or(0);
                        let head = String::from_utf8_lossy(&buf[..n]);
                        let path = head
                            .lines()
                            .next()
                            .unwrap_or("")
                            .split_whitespace()
                            .nth(1)
                            .unwrap_or("/")
                            .to_string();
                        // Strip query string for routing.
                        let route_path = path.split('?').next().unwrap_or("/").to_string();
                        let (code, ctype, body) = route(&route_path);
                        let reason = match code {
                            200 => "OK",
                            201 => "Created",
                            401 => "Unauthorized",
                            404 => "Not Found",
                            409 => "Conflict",
                            _ => "Error",
                        };
                        let head = format!(
                            "HTTP/1.1 {code} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(head.as_bytes());
                        let _ = stream.write_all(&body);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        if std::time::Instant::now() > deadline {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(e) => panic!("test server accept failed: {e}"),
                }
            }
            assert_eq!(served, expect, "test server got {served}/{expect} requests");
        });
        (format!("http://{addr}"), handle)
    }

    #[test]
    fn search_and_metadata_against_local_server() {
        let (base, handle) = serve(2, |path| {
            if path == "/api/pkg/search" {
                (
                    200,
                    "application/json",
                    br#"[{"name":"foo","latest":"1.0.0","description":"d","author":"a"}]"#.to_vec(),
                )
            } else if path == "/api/pkg/foo" {
                (
                    200,
                    "application/json",
                    br#"{"metadata":{"name":"foo","latest":"1.0.0","author":"a","versions":["1.0.0"],"deps":{}},"readme":"hi"}"#.to_vec(),
                )
            } else {
                (404, "application/json", br#"{"error":"x"}"#.to_vec())
            }
        });
        let client = RegistryClient::new(&base);
        let hits = client.search("foo", 20).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "foo");
        let info = client.fetch_metadata("foo").unwrap();
        assert_eq!(info.metadata.latest, "1.0.0");
        assert_eq!(info.readme, "hi");
        handle.join().unwrap();
    }

    #[test]
    fn not_found_maps_to_not_found() {
        let (base, handle) = serve(1, |_| {
            (404, "application/json", br#"{"error":"nope"}"#.to_vec())
        });
        let client = RegistryClient::new(&base);
        let err = client.fetch_metadata("missing").unwrap_err();
        assert!(matches!(err, RemoteError::NotFound(_)), "{err:?}");
        handle.join().unwrap();
    }

    #[test]
    fn download_and_stage_to_cas() {
        let _guard = crate::paths::test_sync::lock_env();
        let tgz = make_tarball(&[("zz.toml", "[package]\nname=\"w\"\n")]);
        let tgz_clone = tgz.clone();
        // Two downloads: the initial stage plus the wrong-hash probe.
        // The warm re-fetch in between hits the CAS fast path (no request).
        let (base, handle) = serve(2, move |path| {
            assert_eq!(path, "/api/pkg/w/1.0.0.tgz");
            (200, "application/gzip", tgz_clone.clone())
        });
        // Isolate CAS via ZZ_HOME so the test never touches ~/.zz.
        let home = std::env::temp_dir().join(format!("zz_rm_cas_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::env::set_var("ZZ_HOME", &home);
        let client = RegistryClient::new(&base);
        let (dir, sha) = client.fetch_to_cas("w", "1.0.0", "").unwrap();
        assert!(dir.join("zz.toml").exists());
        assert_eq!(sha.len(), 64);
        // Idempotent second fetch reuses the entry.
        let (dir2, sha2) = client.fetch_to_cas("w", "1.0.0", &sha).unwrap();
        assert_eq!(dir, dir2);
        assert_eq!(sha, sha2);
        // Wrong expectation is rejected.
        let bad = "0".repeat(64);
        assert!(client.fetch_to_cas("w", "1.0.0", &bad).is_err());
        std::env::remove_var("ZZ_HOME");
        let _ = std::fs::remove_dir_all(&home);
        handle.join().unwrap();
    }
}
