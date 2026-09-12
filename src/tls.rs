//! TLS client transport primitives for GoWay-compatible WSS.
//!
//! GoWay v1.8.4 uses TLS 1.2-1.3 for WSS and defaults to insecure certificate
//! verification unless `-verify-ssl` is enabled. This module deliberately keeps
//! certificate policy separate from the WebSocket framing layer so the existing
//! plain `ws://` path is unchanged while WSS integration is completed.

use anyhow::{anyhow, Context, Result};
use rustls::{ClientConfig, RootCertStore, ServerName};
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio_rustls::{client::TlsStream, TlsConnector};

pub type RushTlsStream = TlsStream<TcpStream>;

fn roots() -> RootCertStore {
    let mut store = RootCertStore::empty();
    store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    store
}

/// Connect to an upstream TLS endpoint.
///
/// `verify_ssl=false` matches GoWay's v1.8.4 default (`InsecureSkipVerify`).
/// `verify_ssl=true` uses the platform-independent WebPKI root set.
pub async fn connect(
    stream: TcpStream,
    host: &str,
    verify_ssl: bool,
) -> Result<RushTlsStream> {
    let mut config = if verify_ssl {
        ClientConfig::builder()
            .with_root_certificates(roots())
            .with_no_client_auth()
    } else {
        ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoCertificateVerification))
            .with_no_client_auth()
    };
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let server_name = ServerName::try_from(host.to_owned())
        .map_err(|_| anyhow!("invalid TLS server name: {host}"))?;
    let connector = TlsConnector::from(Arc::new(config));
    connector
        .connect(server_name, stream)
        .await
        .context("TLS handshake failed")
}

#[derive(Debug)]
struct NoCertificateVerification;

impl rustls::client::danger::ServerCertVerifier for NoCertificateVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::ED25519,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_store_is_constructible() {
        let _ = roots();
    }
}
