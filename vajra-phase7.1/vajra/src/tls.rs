//! TLS configuration (rustls).
//!
//! rustls is a sans-I/O state machine: the worker feeds it ciphertext read
//! from the socket and sends the ciphertext it produces, so it composes
//! naturally with io_uring (no `AsyncRead`/`AsyncWrite` adapters).
//!
//! Each worker builds **its own** `ServerConfig`, so nothing TLS-related is
//! shared across cores. The trade-off: session tickets and the session cache
//! are per core, so a reconnecting client lands on a random core and usually
//! does a full handshake. Sharing ticket keys by message passing is a later
//! optimisation.

use rustls::crypto::ring as provider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use rustls::ServerConfig;
use rustls::server::ServerSessionMemoryCache;
use std::collections::{HashMap, HashSet};
use rustls_acme::ResolvesServerCertAcme;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;

use crate::config::SiteSettings;

/// ALPN protocols offered, in preference order.
pub const ALPN: &[&[u8]] = &[b"h2", b"http/1.1"];

fn load_pems(
    cert: &Path,
    key: &Path,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>), String> {
    let cert_file = File::open(cert).map_err(|e| format!("tls.cert {}: {e}", cert.display()))?;
    let certs = rustls_pemfile::certs(&mut BufReader::new(cert_file))
        .collect::<Result<Vec<CertificateDer<'static>>, _>>()
        .map_err(|e| format!("tls.cert {}: {e}", cert.display()))?;
    if certs.is_empty() {
        return Err(format!(
            "tls.cert {}: no certificates found",
            cert.display()
        ));
    }

    let key_file = File::open(key).map_err(|e| format!("tls.key {}: {e}", key.display()))?;
    let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut BufReader::new(key_file))
        .map_err(|e| format!("tls.key {}: {e}", key.display()))?
        .ok_or_else(|| format!("tls.key {}: no private key found", key.display()))?;
    Ok((certs, key))
}

/// TLS 1.3 only, ALPN `h3`: the configuration QUIC requires (RFC 9001).
/// Wrapped into a QUIC server configuration by [`crate::quic`].
pub fn build_quic_tls(cert: &Path, key: &Path) -> Result<ServerConfig, String> {
    let (certs, key) = load_pems(cert, key)?;
    let mut cfg = ServerConfig::builder_with_provider(Arc::new(provider::default_provider()))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| format!("tls: {e}"))?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| format!("tls: {e}"))?;
    cfg.alpn_protocols = vec![b"h3".to_vec()];
    // 0-RTT enabled for QUIC (Early Data)
    Ok(cfg)
}

pub fn build_server_config(cert: &Path, key: &Path) -> Result<Arc<ServerConfig>, String> {
    let (certs, key) = load_pems(cert, key)?;
    let mut cfg = ServerConfig::builder_with_provider(Arc::new(provider::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("tls: {e}"))?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| format!("tls: {e}"))?;

    cfg.alpn_protocols = ALPN.iter().map(|p| p.to_vec()).collect();
    // Stateless session tickets (per-worker keys, see module docs).
    cfg.ticketer = provider::Ticketer::new().map_err(|e| format!("tls ticketer: {e}"))?;
    cfg.session_storage = ServerSessionMemoryCache::new(20480);
    cfg.max_early_data_size = 16384;
    Ok(Arc::new(cfg))
}

/// Load a PEM cert chain + private key into a [`CertifiedKey`] that
/// rustls can hand out during SNI resolution.
fn load_certified_key(cert: &Path, key: &Path) -> Result<Arc<CertifiedKey>, String> {
    let (certs, key) = load_pems(cert, key)?;
    let signing_key = provider::default_provider()
        .key_provider
        .load_private_key(key)
        .map_err(|e| format!("tls: signing key {}: {e}", cert.display()))?;
    Ok(Arc::new(CertifiedKey::new(certs, signing_key)))
}

/// SNI-based certificate resolver.
///
/// Maps each site's `host` (and its `aliases`, lowercased) to that
/// site's [`CertifiedKey`]. Falls back to `default` when the
/// ClientHello carries no SNI or names an unknown host, so existing
/// single-cert deployments keep working when no site sets per-site
/// TLS fields.
#[derive(Debug)]
pub struct SniResolver {
    by_host: HashMap<String, Arc<CertifiedKey>>,
    default: Arc<CertifiedKey>,
    pub acme: Option<Arc<ResolvesServerCertAcme>>,
    pub acme_domains: HashSet<String>,
}

impl SniResolver {
    pub fn new(sites: &[SiteSettings], default: Arc<CertifiedKey>) -> Self {
        let mut by_host: HashMap<String, Arc<CertifiedKey>> = HashMap::new();
        for site in sites {
            let (Some(cert_path), Some(key_path)) = (&site.tls_cert, &site.tls_key) else {
                continue;
            };
            let ck = match load_certified_key(cert_path, key_path) {
                Ok(k) => k,
                Err(e) => {
                    eprintln!(
                        "vajra: SNI: site {}: skipping cert {}: {e}",
                        site.host,
                        cert_path.display()
                    );
                    continue;
                }
            };
            by_host.insert(site.host.to_lowercase(), ck.clone());
            for alias in &site.aliases {
                by_host.insert(alias.to_lowercase(), ck.clone());
            }
        }
        Self { by_host, default, acme: None, acme_domains: HashSet::new() }
    }
}


impl SniResolver {
    pub fn with_acme(
        mut self,
        acme: Option<Arc<ResolvesServerCertAcme>>,
        domains: Vec<String>,
    ) -> Self {
        self.acme = acme;
        self.acme_domains = domains.into_iter().map(|d| d.to_lowercase()).collect();
        self
    }
}

impl ResolvesServerCert for SniResolver {
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let sni_lc: Option<String> = hello.server_name().map(|s| s.to_lowercase());

        // ACME-managed domain: hand to ACME resolver
        // (handles both acme-tls/1 challenge cert and normal SAN cert)
        if let Some(name) = &sni_lc {
            if self.acme_domains.contains(name) {
                if let Some(acme) = &self.acme {
                    return acme.resolve(hello);
                }
            }
        }

        // Static cert for known SNI (certbot / acme.sh)
        if let Some(name) = sni_lc {
            if let Some(k) = self.by_host.get(&name) {
                return Some(k.clone());
            }
        }

        // Default fallback (first static cert)
        Some(self.default.clone())
    }
}

/// Build a `ServerConfig` that dispatches by SNI over `sites`, using
/// `(default_cert, default_key)` for any ClientHello that does not
/// match a configured per-site cert.
///
/// The default pair is usually the top-level `[tls]` cert from
/// `globals.toml`; sites without `tls_cert`/`tls_key` simply don't
/// contribute an entry and rely on the fallback.
pub fn build_server_config_with_sni(
    sites: &[SiteSettings],
    default_cert: &Path,
    default_key: &Path,
) -> Result<Arc<ServerConfig>, String> {
    let default = load_certified_key(default_cert, default_key)?;
    let resolver = Arc::new(SniResolver::new(sites, default));

    let mut cfg = ServerConfig::builder_with_provider(Arc::new(provider::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("tls: {e}"))?
        .with_no_client_auth()
        .with_cert_resolver(resolver);

    cfg.alpn_protocols = ALPN.iter().map(|p| p.to_vec()).collect();
    cfg.ticketer = provider::Ticketer::new().map_err(|e| format!("tls ticketer: {e}"))?;
    cfg.session_storage = ServerSessionMemoryCache::new(20480);
    cfg.max_early_data_size = 16384;
    Ok(Arc::new(cfg))
}

pub fn build_server_config_with_sni_and_acme(
    sites: &[SiteSettings],
    cert: &Path,
    key: &Path,
    acme: Option<Arc<ResolvesServerCertAcme>>,
    acme_domains: Vec<String>,
) -> Result<Arc<ServerConfig>, String> {
    let default = load_certified_key(cert, key)?;
    let resolver = SniResolver::new(sites, default).with_acme(acme, acme_domains);

    let mut cfg = ServerConfig::builder_with_provider(
        Arc::new(rustls::crypto::ring::default_provider()),
    )
    .with_safe_default_protocol_versions()
    .map_err(|e| format!("tls: {e}"))?
    .with_no_client_auth()
    .with_cert_resolver(Arc::new(resolver));

    // ALPN: standard + ACME challenge protocol
    cfg.alpn_protocols = vec![
        b"h2".to_vec(),
        b"http/1.1".to_vec(),
        crate::acme::ACME_TLS_ALPN.to_vec(),
    ];
    cfg.ticketer = rustls::crypto::ring::Ticketer::new()
        .map_err(|e| format!("tls ticketer: {e}"))?;
    cfg.session_storage = ServerSessionMemoryCache::new(20480);
    cfg.max_early_data_size = 16384;

    Ok(Arc::new(cfg))
}
