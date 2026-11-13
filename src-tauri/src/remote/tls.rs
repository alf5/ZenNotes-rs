//! One rustls client config for everything that talks to a ZenNotes server:
//! the HTTP transport and the change feed.
//!
//! Port of upstream's apps/desktop/src/main/system-trust.ts, which adds the
//! operating system's CA store to Node's bundled roots. A self-hosted server
//! behind a homelab or company CA is trusted when the machine trusts that CA,
//! and a machine whose store cannot be read still has the bundled roots.

use std::sync::{Arc, OnceLock};

static CONFIG: OnceLock<Result<Arc<rustls::ClientConfig>, String>> = OnceLock::new();

/// Mozilla's roots plus whatever the OS store holds.
fn root_store() -> rustls::RootCertStore {
    let mut roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let native = rustls_native_certs::load_native_certs();
    if !native.errors.is_empty() && native.certs.is_empty() {
        eprintln!("[remote] the system certificate store could not be read; using the bundled roots only");
    }
    roots.add_parsable_certificates(native.certs);
    roots
}

fn build() -> Result<Arc<rustls::ClientConfig>, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("TLS could not be set up: {e}"))?
        .with_root_certificates(root_store())
        .with_no_client_auth();
    Ok(Arc::new(config))
}

pub fn client_config() -> Result<Arc<rustls::ClientConfig>, String> {
    CONFIG.get_or_init(build).clone()
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_config_builds_and_holds_the_bundled_roots() {
        let config = super::client_config().unwrap();
        // ALPN is unset: the transport and the feed speak HTTP/1.1.
        assert!(config.alpn_protocols.is_empty());
        assert!(super::root_store().len() >= webpki_roots::TLS_SERVER_ROOTS.len());
    }
}
