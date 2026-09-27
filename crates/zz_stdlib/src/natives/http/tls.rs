//! TLS listener support (Phase 3.1): rustls server side.
//!
//! - `ring` provider, TLS 1.3 + 1.2, no client auth, no OpenSSL dependency.
//! - ALPN advertises `http/1.1` only until the H2 leg lands (3.2); clients
//!   must not negotiate a protocol we cannot serve.
//! - PEM cert chains (leaf first) + PKCS#8 / PKCS#1 / SEC1 keys.

use std::sync::Arc;

use zz_runtime::{EvalError, Span};

/// Load a `ServerConfig` from PEM files. Every failure is a loud
/// `EvalError` naming the file — a half-configured TLS listener must never
/// silently fall back to cleartext.
pub(crate) fn load_server_config(
    cert_path: &str,
    key_path: &str,
    span: Span,
) -> Result<Arc<rustls::ServerConfig>, EvalError> {
    let fail = |msg: String| EvalError::new(msg, span);

    let cert_file = std::fs::File::open(cert_path)
        .map_err(|e| fail(format!("TLS cert `{cert_path}` unreadable: {e}")))?;
    let mut cert_reader = std::io::BufReader::new(cert_file);
    let certs: Vec<rustls::pki_types::CertificateDer<'static>> =
        rustls_pemfile::certs(&mut cert_reader)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| fail(format!("TLS cert `{cert_path}` is not valid PEM: {e}")))?;
    if certs.is_empty() {
        return Err(fail(format!(
            "TLS cert `{cert_path}` contains no certificates"
        )));
    }

    let key_file = std::fs::File::open(key_path)
        .map_err(|e| fail(format!("TLS key `{key_path}` unreadable: {e}")))?;
    let mut key_reader = std::io::BufReader::new(key_file);
    // Accept any of the three common PEM key encodings, PKCS#8 first.
    let key = rustls_pemfile::pkcs8_private_keys(&mut key_reader)
        .next()
        .transpose()
        .map_err(|e| fail(format!("TLS key `{key_path}` is not valid PEM: {e}")))?
        .map(rustls::pki_types::PrivateKeyDer::Pkcs8)
        .or_else(|| {
            let mut key_reader = reopen(key_path)?;
            let key = rustls_pemfile::rsa_private_keys(&mut key_reader)
                .next()?
                .ok()?;
            Some(rustls::pki_types::PrivateKeyDer::Pkcs1(key))
        })
        .or_else(|| {
            let mut key_reader = reopen(key_path)?;
            let key = rustls_pemfile::ec_private_keys(&mut key_reader)
                .next()?
                .ok()?;
            Some(rustls::pki_types::PrivateKeyDer::Sec1(key))
        })
        .ok_or_else(|| {
            fail(format!(
                "TLS key `{key_path}` has no PKCS#8, PKCS#1, or SEC1 private key"
            ))
        })?;

    let provider = rustls::crypto::ring::default_provider();
    let mut config = rustls::ServerConfig::builder_with_provider(provider.into())
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
        .map_err(|e| fail(format!("TLS provider error: {e}")))?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| fail(format!("TLS cert/key mismatch: {e}")))?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

fn reopen(path: &str) -> Option<std::io::BufReader<std::fs::File>> {
    std::fs::File::open(path).ok().map(std::io::BufReader::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_temp(name: &str, contents: &str) -> String {
        let path = std::env::temp_dir().join(format!("zz_tls_{name}.pem"));
        std::fs::write(&path, contents).unwrap();
        path.to_string_lossy().into_owned()
    }

    fn self_signed_pair() -> (String, String) {
        let key = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        let cert_pem = key.cert.pem();
        let key_pem = key.key_pair.serialize_pem();
        (write_temp("cert", &cert_pem), write_temp("key", &key_pem))
    }

    #[test]
    fn loads_rcgen_pair() {
        let (cert, key) = self_signed_pair();
        let cfg = load_server_config(&cert, &key, Span::new(0, 0)).unwrap();
        assert_eq!(cfg.alpn_protocols, vec![b"http/1.1".to_vec()]);
    }

    #[test]
    fn missing_files_are_loud_errors() {
        assert!(
            load_server_config("/no/such/cert.pem", "/no/such/key.pem", Span::new(0, 0)).is_err()
        );
    }

    #[test]
    fn garbage_pem_is_loud_error() {
        let cert = write_temp("bad-cert", "not pem at all\n");
        let key = write_temp("bad-key", "not pem at all\n");
        assert!(load_server_config(&cert, &key, Span::new(0, 0)).is_err());
    }

    #[test]
    fn key_without_cert_is_loud_error() {
        let (_, key) = self_signed_pair();
        let empty = write_temp("empty-cert", "");
        assert!(load_server_config(&empty, &key, Span::new(0, 0)).is_err());
    }
}
