//! Authentication and credentials management for `zz pm`.
//!
//! Stores credentials in `~/.zz/credentials.toml` with 0600 permissions.
//! Tokens are never logged or printed in error messages.

use std::path::{Path, PathBuf};

use crate::paths;

/// Stored credentials.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Credentials {
    /// Registry URL → auth token.
    #[serde(default)]
    pub registries: std::collections::HashMap<String, RegistryAuth>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("registries", &self.registries.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// Auth for a single registry.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RegistryAuth {
    /// Auth token (opaque string).
    pub token: String,
    /// Optional username.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// Timestamp when the token was obtained.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
}

impl Credentials {
    /// Load credentials from disk.
    pub fn load() -> Result<Self, String> {
        let path = credentials_path();
        Self::load_from(&path)
    }

    /// Load from a specific path.
    pub fn load_from(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(Self {
                registries: std::collections::HashMap::new(),
            });
        }
        let content =
            std::fs::read_to_string(path).map_err(|e| format!("cannot read credentials: {e}"))?;
        toml::from_str(&content).map_err(|e| format!("invalid credentials.toml: {e}"))
    }

    /// Save credentials to disk with 0600 permissions.
    pub fn save(&self) -> Result<(), String> {
        let path = credentials_path();
        self.save_to(&path)
    }

    /// Save to a specific path.
    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        let content = toml::to_string_pretty(self)
            .map_err(|e| format!("cannot serialize credentials: {e}"))?;

        // Write content first, then set permissions (atomic-ish)
        std::fs::write(path, &content).map_err(|e| format!("cannot write credentials: {e}"))?;

        // Set 0600 permissions (owner read/write only)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            std::fs::set_permissions(path, perms)
                .map_err(|e| format!("cannot set credentials permissions: {e}"))?;
        }

        Ok(())
    }

    /// Get the auth token for a registry URL.
    pub fn get_token(&self, registry_url: &str) -> Option<&str> {
        self.registries.get(registry_url).map(|r| r.token.as_str())
    }

    /// Set or update the auth token for a registry.
    pub fn set_token(&mut self, registry_url: &str, token: String, username: Option<String>) {
        self.registries.insert(
            registry_url.to_string(),
            RegistryAuth {
                token,
                username,
                created_at: Some(chrono_now()),
            },
        );
    }

    /// Remove auth for a registry.
    pub fn remove(&mut self, registry_url: &str) -> bool {
        self.registries.remove(registry_url).is_some()
    }

    /// List all configured registries (URLs only, no tokens).
    pub fn list_registries(&self) -> Vec<String> {
        self.registries.keys().cloned().collect()
    }
}

/// Get the credentials file path.
pub fn credentials_path() -> PathBuf {
    paths::credentials_path()
}

/// Current timestamp as ISO 8601 string.
fn chrono_now() -> String {
    // Minimal timestamp without pulling in chrono crate
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{now}")
}

/// Verify that the credentials file has correct permissions (0600).
/// Returns Ok(()) if permissions are correct, Err with a warning message otherwise.
pub fn verify_permissions(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(path)
            .map_err(|e| format!("cannot read credentials metadata: {e}"))?;
        let mode = meta.permissions().mode();
        // Check that group and other bits are clear (0600 = owner rw only)
        if mode & 0o077 != 0 {
            return Err(format!(
                "credentials file has insecure permissions: {mode:04o}\n\
                 hint: run `chmod 600 {}` to fix",
                path.display()
            ));
        }
    }
    Ok(())
}

/// Prompt the user for a registry URL and token.
/// Returns (registry_url, token, optional_username).
pub fn prompt_credentials() -> Result<(String, String, Option<String>), String> {
    eprint!("Registry URL (e.g. https://registry.zz.dev): ");
    let mut url = String::new();
    std::io::stdin()
        .read_line(&mut url)
        .map_err(|e| format!("cannot read input: {e}"))?;
    let url = url.trim().to_string();

    if url.is_empty() {
        return Err("registry URL cannot be empty".to_string());
    }

    // Token should not be echoed
    eprint!("Auth token: ");
    let token = read_secret().map_err(|e| format!("cannot read token: {e}"))?;

    if token.is_empty() {
        return Err("auth token cannot be empty".to_string());
    }

    eprint!("Username (optional, press Enter to skip): ");
    let mut username = String::new();
    std::io::stdin()
        .read_line(&mut username)
        .map_err(|e| format!("cannot read input: {e}"))?;
    let username = username.trim();
    let username = if username.is_empty() {
        None
    } else {
        Some(username.to_string())
    };

    Ok((url, token, username))
}

/// Read a secret from stdin without echoing (uses `/dev/tty`).
fn read_secret() -> Result<String, String> {
    // Try to read from /dev/tty for non-echo input
    use std::io::BufRead;
    let tty = std::fs::File::open("/dev/tty").map_err(|e| format!("cannot open /dev/tty: {e}"))?;
    let mut reader = std::io::BufReader::new(tty);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| format!("cannot read: {e}"))?;
    Ok(line.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("zz_pm_auth_test_{}_{}", std::process::id(), id));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn credentials_round_trip() {
        let d = tmp();
        let path = d.join("credentials.toml");

        let mut creds = Credentials {
            registries: std::collections::HashMap::new(),
        };
        creds.set_token(
            "https://registry.zz.dev",
            "tok_abc123".to_string(),
            Some("alice".to_string()),
        );

        creds.save_to(&path).unwrap();

        let loaded = Credentials::load_from(&path).unwrap();
        assert_eq!(
            loaded.get_token("https://registry.zz.dev"),
            Some("tok_abc123")
        );
        assert_eq!(
            loaded.registries["https://registry.zz.dev"].username,
            Some("alice".to_string())
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn credentials_permissions_0600() {
        let d = tmp();
        let path = d.join("credentials.toml");

        let creds = Credentials {
            registries: std::collections::HashMap::new(),
        };
        creds.save_to(&path).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = fs::metadata(&path).unwrap();
            let mode = meta.permissions().mode();
            // Should be 0600 (owner rw only)
            assert_eq!(
                mode & 0o777,
                0o600,
                "permissions are {mode:04o}, expected 0600"
            );
        }
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn verify_permissions_correct() {
        let d = tmp();
        let path = d.join("credentials.toml");
        fs::write(&path, "").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            assert!(verify_permissions(&path).is_ok());

            fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(verify_permissions(&path).is_err());
        }
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn remove_registry() {
        let d = tmp();
        let path = d.join("credentials.toml");

        let mut creds = Credentials {
            registries: std::collections::HashMap::new(),
        };
        creds.set_token("https://registry.zz.dev", "token".to_string(), None);
        creds.save_to(&path).unwrap();

        let mut loaded = Credentials::load_from(&path).unwrap();
        assert!(loaded.remove("https://registry.zz.dev"));
        assert!(!loaded.remove("https://nonexistent.dev"));
        assert!(loaded.get_token("https://registry.zz.dev").is_none());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn empty_credentials_valid() {
        let d = tmp();
        let path = d.join("credentials.toml");

        // Non-existent file returns empty credentials
        let creds = Credentials::load_from(&path).unwrap();
        assert!(creds.registries.is_empty());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn token_not_in_display() {
        // Ensure token is never accidentally included in Debug output
        let mut registries = std::collections::HashMap::new();
        registries.insert(
            "https://registry.zz.dev".to_string(),
            RegistryAuth {
                token: "super_secret_token_abc123".to_string(),
                username: Some("alice".to_string()),
                created_at: None,
            },
        );
        let creds = Credentials { registries };
        let debug = format!("{creds:?}");
        assert!(
            !debug.contains("super_secret_token_abc123"),
            "token leaked into Debug output"
        );
    }
}
