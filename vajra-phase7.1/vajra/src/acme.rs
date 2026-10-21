//! ACME (Let's Encrypt) integration via rustls-acme.
//!
//! Runs `AcmeState` as a Stream on a background Tokio thread.
//! The resolver it provides serves both TLS-ALPN-01 challenge certs
//! (during issuance) and the real SAN cert (after issuance).
//!
//! Vajra io_uring loop untouched — TLS handshake asks resolver for
//! cert, resolver handles both challenge and normal traffic.

use std::sync::Arc;

use rustls_acme::{AcmeConfig, caches::DirCache, ResolvesServerCertAcme};
use tokio::runtime::Runtime;

use crate::config::AcmeSettings;

/// ALPN identifier used by Let's Encrypt for TLS-ALPN-01 challenges.
pub const ACME_TLS_ALPN: &[u8] = b"acme-tls/1";

pub struct AcmeRuntime {
    /// Shared with Vajra's SniResolver. Internally Mutex-protected,
    /// updated by the background state machine.
    pub resolver: Arc<ResolvesServerCertAcme>,
    /// List of domains this runtime manages (lowercase).
    pub domains: Vec<String>,
    _rt: Runtime,
}

impl AcmeRuntime {
    pub fn spawn(domains: Vec<String>, cfg: &AcmeSettings) -> Result<Self, String> {
        if domains.is_empty() {
            return Err("acme: no domains to manage".into());
        }
        std::fs::create_dir_all(&cfg.cache_dir)
            .map_err(|e| format!("acme: cache dir {}: {e}", cfg.cache_dir.display()))?;

        // Build state machine (actual 0.12.1 API).
        let mut state = AcmeConfig::new(domains.clone())
            .contact_push(format!("mailto:{}", cfg.email))
            .cache_option(Some(DirCache::new(cfg.cache_dir.clone())))
            .directory_lets_encrypt(!cfg.is_staging())
            .state();

        let resolver = state.resolver();

        // Dedicated Tokio runtime on its own thread.
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("vajra-acme")
            .enable_all()
            .build()
            .map_err(|e| format!("acme: tokio runtime: {e}"))?;

        // Drive ACME by polling the state Stream. Every poll advances
        // the state machine: account registration, order creation,
        // challenge storage, validation polling, cert fetch.
        rt.spawn(async move {
            use futures::StreamExt;
            eprintln!("vajra-acme: state machine running");
            while let Some(event) = state.next().await {
                eprintln!("vajra-acme: event: {:?}", event);
            }
            eprintln!("vajra-acme: state stream ENDED (unexpected)");
        });

        eprintln!(
            "vajra: acme started ({} domain(s), staging={}, cache={})",
            domains.len(),
            cfg.is_staging(),
            cfg.cache_dir.display(),
        );

        Ok(Self {
            resolver,
            domains: domains.into_iter().map(|d| d.to_lowercase()).collect(),
            _rt: rt,
        })
    }
}
