//! TLS primitives for GoWay-compatible WSS.

use crate::ws::{pick_browser_profile_index, BROWSER_PROFILES};
use anyhow::{anyhow, Context, Result};
use rcgen::generate_simple_self_signed;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
use rustls::version::{TLS12, TLS13};
use rustls::{ClientConfig, CipherSuite, NamedGroup, RootCertStore, ServerConfig};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

type ProfileConfigCache = Mutex<HashMap<(usize, bool), Arc<ClientConfig>>>;
use tokio::net::TcpStream;
use tokio_rustls::{client::TlsStream, server::TlsAcceptor, TlsConnector};

pub type RushTlsStream = TlsStream<TcpStream>;

/// GoWay `tlsCiphersChrome136` order, restricted to suites rustls/ring
/// implements (RSA key exchange and CBC suites are unavailable and skipped).
const CHROMIUM_SUITE_ORDER: &[CipherSuite] = &[
    CipherSuite::TLS13_AES_128_GCM_SHA256,
    CipherSuite::TLS13_AES_256_GCM_SHA384,
    CipherSuite::TLS13_CHACHA20_POLY1305_SHA256,
    CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256,
    CipherSuite::TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256,
    CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384,
    CipherSuite::TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384,
    CipherSuite::TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256,
    CipherSuite::TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256,
];

/// GoWay `tlsCiphersFirefox138` order (same implementable subset).
const FIREFOX_SUITE_ORDER: &[CipherSuite] = &[
    CipherSuite::TLS13_AES_128_GCM_SHA256,
    CipherSuite::TLS13_CHACHA20_POLY1305_SHA256,
    CipherSuite::TLS13_AES_256_GCM_SHA384,
    CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256,
    CipherSuite::TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256,
    CipherSuite::TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256,
    CipherSuite::TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256,
    CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384,
    CipherSuite::TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384,
];

/// GoWay `curvePrefsChrome`/`curvePrefsFirefox` restricted to ring's key
/// exchange groups (ring has no P-521, so Firefox emits the shared subset).
const KX_GROUP_ORDER: &[NamedGroup] = &[NamedGroup::X25519, NamedGroup::secp256r1, NamedGroup::secp384r1];

fn roots() -> RootCertStore {
    static ROOTS: OnceLock<RootCertStore> = OnceLock::new();
    ROOTS
        .get_or_init(|| {
            let mut store = RootCertStore::empty();
            store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            store
        })
        .clone()
}

/// Reorders the ring provider's cipher suites / key-exchange groups to a
/// browser profile's preference, mirroring GoWay's per-profile
/// `tls.Config` (`CipherSuites` + `CurvePreferences`, ALPN `http/1.1`).
/// Suites the provider lacks keep their relative order at the end.
fn provider_for_profile(profile_index: usize) -> CryptoProvider {
    let profile = &BROWSER_PROFILES[profile_index % BROWSER_PROFILES.len()];
    let order = if profile.tls_chromium_order {
        CHROMIUM_SUITE_ORDER
    } else {
        FIREFOX_SUITE_ORDER
    };
    let mut provider = rustls::crypto::ring::default_provider();
    provider.cipher_suites.sort_by_key(|suite| {
        order
            .iter()
            .position(|want| *want == suite.suite())
            .unwrap_or(usize::MAX)
    });
    provider.kx_groups.sort_by_key(|group| {
        KX_GROUP_ORDER
            .iter()
            .position(|want| *want == group.name())
            .unwrap_or(usize::MAX)
    });
    provider
}

fn build_profile_config(profile_index: usize, verify_ssl: bool) -> Arc<ClientConfig> {
    let provider = provider_for_profile(profile_index);
    let builder = ClientConfig::builder_with_provider(provider.into())
        .with_protocol_versions(&[&TLS13, &TLS12])
        .expect("ring provider supports TLS 1.2/1.3");
    let mut config = if verify_ssl {
        builder.with_root_certificates(roots()).with_no_client_auth()
    } else {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoCertificateVerification))
            .with_no_client_auth()
    };
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Arc::new(config)
}

fn profile_config(profile_index: usize, verify_ssl: bool) -> Arc<ClientConfig> {
    static CACHE: OnceLock<ProfileConfigCache> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(config) = cache.lock().unwrap().get(&(profile_index, verify_ssl)) {
        return config.clone();
    }
    let config = build_profile_config(profile_index, verify_ssl);
    cache
        .lock()
        .unwrap()
        .insert((profile_index, verify_ssl), config.clone());
    config
}

/// Desired cipher-suite preference order for a profile (test hook).
#[cfg(test)]
pub fn suite_order_for_profile(profile_index: usize) -> &'static [CipherSuite] {
    if BROWSER_PROFILES[profile_index % BROWSER_PROFILES.len()].tls_chromium_order {
        CHROMIUM_SUITE_ORDER
    } else {
        FIREFOX_SUITE_ORDER
    }
}

/// Connect to an upstream TLS endpoint with a random browser-profile TLS
/// fingerprint (GoWay draws a random profile per session).
pub async fn connect(stream: TcpStream, host: &str, verify_ssl: bool) -> Result<RushTlsStream> {
    connect_with_profile(stream, host, verify_ssl, pick_browser_profile_index()).await
}

/// Connect with an explicit profile so callers can correlate the TLS
/// fingerprint with the HTTP handshake identity.
pub async fn connect_with_profile(
    stream: TcpStream,
    host: &str,
    verify_ssl: bool,
    profile_index: usize,
) -> Result<RushTlsStream> {
    let config = profile_config(profile_index, verify_ssl);
    let server_name = ServerName::try_from(host.to_owned())
        .map_err(|_| anyhow!("invalid TLS server name: {host}"))?;
    let connector = TlsConnector::from(config);
    connector
        .connect(server_name, stream)
        .await
        .context("TLS handshake failed")
}

/// Build a self-signed HTTP/1.1 TLS acceptor for standalone WSS server mode.
pub fn standalone_server_acceptor() -> Result<TlsAcceptor> {
    let cert = generate_simple_self_signed(vec!["localhost".into()])
        .context("generate WSS server certificate")?;
    let cert_der: CertificateDer<'static> = cert.cert.der().clone();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()));
    let mut config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key)
        .context("build WSS server TLS config")?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(TlsAcceptor::from(Arc::new(config)))
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
            rustls::SignatureScheme::ECDSA_NISTP521_SHA512,
            rustls::SignatureScheme::ED25519,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
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

    #[test]
    fn cached_configs_are_reused() {
        assert!(Arc::ptr_eq(&profile_config(0, true), &profile_config(0, true)));
        assert!(Arc::ptr_eq(&profile_config(0, false), &profile_config(0, false)));
        assert!(!Arc::ptr_eq(&profile_config(0, true), &profile_config(0, false)));
    }

    #[test]
    fn profile_suite_orders_match_goway() {
        // Chromium puts AES-256-GCM second; Firefox puts ChaCha20 second.
        assert_eq!(
            suite_order_for_profile(0)[1],
            CipherSuite::TLS13_AES_256_GCM_SHA384
        );
        let firefox = BROWSER_PROFILES.iter().position(|p| !p.is_chromium).unwrap();
        assert_eq!(
            suite_order_for_profile(firefox)[1],
            CipherSuite::TLS13_CHACHA20_POLY1305_SHA256
        );
    }

    #[test]
    fn provider_starts_with_profile_suite() {
        for idx in 0..BROWSER_PROFILES.len() {
            let provider = provider_for_profile(idx);
            let want = suite_order_for_profile(idx)[0];
            assert_eq!(provider.cipher_suites[0].suite(), want);
            // Key-exchange preference leads with X25519 (GoWay parity).
            assert_eq!(provider.kx_groups[0].name(), NamedGroup::X25519);
        }
    }

    #[test]
    fn standalone_server_acceptor_is_constructible() {
        assert!(standalone_server_acceptor().is_ok());
    }

    /// Minimal repro probe for the WSS second-session stall: two sequential
    /// bare TLS handshakes against one acceptor, second while the first is
    /// still open. Times out loudly instead of hanging the suite.
    #[tokio::test]
    async fn two_sequential_tls_handshakes() {
        let acceptor = standalone_server_acceptor().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let srv = tokio::spawn(async move {
            for _ in 0..2 {
                let (stream, _) = listener.accept().await.unwrap();
                tokio::time::timeout(std::time::Duration::from_secs(10), acceptor.accept(stream))
                    .await
                    .expect("server accept timeout")
                    .expect("server accept failed");
            }
        });
        let mut first = None;
        for _ in 0..2 {
            let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .unwrap();
            let tls = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                connect(tcp, "localhost", false),
            )
            .await
            .expect("client handshake timeout")
            .expect("client handshake failed");
            first = Some(tls);
        }
        drop(first);
        srv.await.unwrap();
    }
}
