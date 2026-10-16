use super::{ConnectionMode, SyncConnection};
use anyhow::{Context, Result};
use rustls::{ClientConfig, RootCertStore};
use std::{io::BufReader, sync::Arc};

/// Server chain and hostname verification apply to every TLS connection.
pub fn client_config(profile: &SyncConnection) -> Result<Arc<ClientConfig>> {
    profile.validate()?;
    let mut roots = RootCertStore::empty();
    if let Some(path) = &profile.ca_cert {
        let bytes = std::fs::read(path).context("cannot read sync CA certificate")?;
        for cert in rustls_pemfile::certs(&mut BufReader::new(bytes.as_slice())) {
            roots
                .add(cert.context("invalid sync CA certificate")?)
                .context("invalid sync CA certificate")?;
        }
    } else {
        let native = rustls_native_certs::load_native_certs();
        for cert in native.certs {
            let _ = roots.add(cert);
        }
    }
    anyhow::ensure!(!roots.is_empty(), "sync CA trust store is empty");
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots);
    let config = if profile.mode == ConnectionMode::Mtls {
        let bytes =
            std::fs::read(&profile.client_cert).context("cannot read sync client certificate")?;
        let certs = rustls_pemfile::certs(&mut BufReader::new(bytes.as_slice()))
            .collect::<std::io::Result<Vec<_>>>()
            .context("invalid sync client certificate")?;
        anyhow::ensure!(!certs.is_empty(), "sync client certificate chain is empty");
        let bytes =
            std::fs::read(&profile.client_key).context("cannot read sync client private key")?;
        let key = rustls_pemfile::private_key(&mut BufReader::new(bytes.as_slice()))
            .context("invalid sync private key")?
            .context("sync private key is missing")?;
        builder
            .with_client_auth_cert(certs, key)
            .context("sync certificate and private key do not match")?
    } else {
        builder.with_no_client_auth()
    };
    Ok(Arc::new(config))
}
