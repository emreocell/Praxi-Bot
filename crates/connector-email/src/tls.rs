//! TLS configuration (rustls + ring, Mozilla roots plus an optional private CA).

use std::sync::Arc;

use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::{CertificateDer, pem::PemObject};
use tokio_rustls::rustls::{ClientConfig, RootCertStore, crypto::ring};

use crate::error::EmailError;

/// Build a connector that verifies server certificates. There is no way to
/// disable verification.
///
/// # Errors
/// [`EmailError::Tls`] if the extra PEM cannot be parsed.
pub fn connector(extra_root_pem: Option<&str>) -> Result<TlsConnector, EmailError> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(pem) = extra_root_pem {
        let mut added = 0;
        for cert in CertificateDer::pem_slice_iter(pem.as_bytes()) {
            let cert = cert.map_err(|e| EmailError::Tls(format!("bad root certificate: {e}")))?;
            roots
                .add(cert)
                .map_err(|e| EmailError::Tls(format!("bad root certificate: {e}")))?;
            added += 1;
        }
        if added == 0 {
            return Err(EmailError::Tls("extra_root_pem has no certificate".into()));
        }
    }
    let cfg = ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| EmailError::Tls(e.to_string()))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(cfg)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extra_root_pem_parses_with_pki_types() {
        let rcgen::CertifiedKey { cert, .. } =
            rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        assert!(connector(Some(&cert.pem())).is_ok());
        assert!(connector(None).is_ok());
        assert!(connector(Some("not-a-certificate")).is_err());
    }
}
