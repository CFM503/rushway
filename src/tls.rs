//! TLS primitives for GoWay-compatible WSS.

use crate::ws::{pick_browser_profile_index, BROWSER_PROFILES};
use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use rcgen::generate_simple_self_signed;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{
    CertificateDer, PrivateKeyDer, PrivatePkcs1KeyDer, PrivatePkcs8KeyDer, PrivateSec1KeyDer,
    ServerName,
};
use rustls::version::{TLS12, TLS13};
use rustls::{CipherSuite, ClientConfig, NamedGroup, RootCertStore, ServerConfig};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
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
const KX_GROUP_ORDER: &[NamedGroup] = &[
    NamedGroup::X25519,
    NamedGroup::secp256r1,
    NamedGroup::secp384r1,
];

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
        builder
            .with_root_certificates(roots())
            .with_no_client_auth()
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
    if let Some(config) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(&(profile_index, verify_ssl)) {
        return config.clone();
    }
    let config = build_profile_config(profile_index, verify_ssl);
    cache
        .lock().unwrap_or_else(|e| e.into_inner())
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

/// Server identity supplied with `--cert` / `--key`.
///
/// Server modes otherwise generate a self-signed certificate at startup, which
/// no verifying client will accept. Holding it behind one `OnceLock` lets the
/// WSS acceptor and the QUIC server config share it without threading a new
/// parameter through every call site.
static SERVER_IDENTITY: OnceLock<Option<ServerIdentity>> = OnceLock::new();

struct ServerIdentity {
    certs: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
}

/// Load the PEM certificate chain and private key used by server modes.
///
/// Both paths must be supplied together: a half-configured identity is a
/// startup error rather than a silent fall back to self-signed, because a
/// silent fall back would leave the operator believing they had pinned a
/// certificate when they had not.
pub fn configure_server_identity(
    cert_path: Option<PathBuf>,
    key_path: Option<PathBuf>,
) -> Result<()> {
    let (cert_path, key_path) = match (cert_path, key_path) {
        (None, None) => return Ok(()),
        (Some(cert), Some(key)) => (cert, key),
        _ => bail!("--cert and --key must be given together"),
    };
    let identity = ServerIdentity {
        certs: load_certs(&cert_path)?,
        key: load_key(&key_path)?,
    };
    SERVER_IDENTITY
        .set(Some(identity))
        .map_err(|_| anyhow!("server certificate configured twice"))?;
    tracing::info!(
        cert = %cert_path.display(),
        key = %key_path.display(),
        "loaded server certificate (self-signed generation disabled)"
    );
    Ok(())
}

/// The configured server identity, or `None` when `--cert`/`--key` were not
/// given and server mode should generate a self-signed certificate.
///
/// Returns freshly cloned material because both the WSS acceptor and the QUIC
/// server config consume it independently.
pub fn server_identity() -> Option<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let identity = SERVER_IDENTITY.get_or_init(|| None).as_ref()?;
    Some((identity.certs.clone(), identity.key.clone_key()))
}

/// Minimal RFC 7468 reader: split on `-----BEGIN <label>-----` /
/// `-----END <label>-----` and base64-decode the body.
///
/// Hand-rolled rather than pulled in as a dependency for two files read once
/// at startup; the crate already depends on `base64`, and the body lines are
/// concatenated back together (PEM wraps at a multiple of four characters, so
/// the concatenation stays valid) with missing padding restored.
fn pem_blocks(pem: &str) -> Result<Vec<(String, Vec<u8>)>> {
    let mut blocks = Vec::new();
    let mut label: Option<String> = None;
    let mut body = String::new();
    for line in pem.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("-----BEGIN ") {
            if let Some(name) = rest.strip_suffix("-----") {
                label = Some(name.to_string());
                body.clear();
            }
        } else if line.starts_with("-----END ") {
            if let Some(name) = label.take() {
                let mut encoded = body.replace(' ', "");
                while encoded.len() % 4 != 0 {
                    encoded.push('=');
                }
                let der = STANDARD
                    .decode(encoded)
                    .with_context(|| format!("decode base64 body of the {name:?} PEM block"))?;
                if der.is_empty() {
                    bail!("the {name:?} PEM block is empty");
                }
                blocks.push((name, der));
            }
            body.clear();
        } else if label.is_some() {
            body.push_str(line);
        }
    }
    Ok(blocks)
}

const CERT_LABELS: &[&str] = &["CERTIFICATE", "X509 CERTIFICATE", "TRUSTED CERTIFICATE"];

fn load_certs(path: &Path) -> Result<Vec<CertificateDer<'static>>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read certificate file {}", path.display()))?;
    let certs: Vec<CertificateDer<'static>> = pem_blocks(&text)?
        .into_iter()
        .filter(|(label, _)| CERT_LABELS.contains(&label.as_str()))
        .map(|(_, der)| CertificateDer::from(der))
        .collect();
    if certs.is_empty() {
        bail!("{} contains no CERTIFICATE PEM block", path.display());
    }
    Ok(certs)
}

fn load_key(path: &Path) -> Result<PrivateKeyDer<'static>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read private key file {}", path.display()))?;
    for (label, der) in pem_blocks(&text)? {
        match label.as_str() {
            "PRIVATE KEY" => return Ok(PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(der))),
            "EC PRIVATE KEY" => return Ok(PrivateKeyDer::Sec1(PrivateSec1KeyDer::from(der))),
            "RSA PRIVATE KEY" => return Ok(PrivateKeyDer::Pkcs1(PrivatePkcs1KeyDer::from(der))),
            _ if CERT_LABELS.contains(&label.as_str()) => {}
            other => bail!(
                "{}: unsupported private key PEM label {other:?}; expected PRIVATE KEY, \
                 EC PRIVATE KEY or RSA PRIVATE KEY (re-encode with \
                 `openssl pkcs8 -topk8 -nocrypt`)",
                path.display()
            ),
        }
    }
    bail!(
        "{} contains no private key PEM block (expected PRIVATE KEY, EC PRIVATE KEY or RSA PRIVATE KEY)",
        path.display()
    )
}

/// Build the HTTP/1.1 TLS acceptor for standalone WSS server mode.
///
/// Uses the `--cert`/`--key` identity when one was configured, otherwise falls
/// back to a freshly generated self-signed certificate — which only works if
/// the client runs with `--no-verify-ssl`.
pub fn standalone_server_acceptor() -> Result<TlsAcceptor> {
    let (certs, key) = match server_identity() {
        Some(identity) => identity,
        None => {
            let cert = generate_simple_self_signed(vec!["localhost".into()])
                .context("generate WSS server certificate")?;
            let cert_der: CertificateDer<'static> = cert.cert.der().clone();
            let key =
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()));
            (vec![cert_der], key)
        }
    };
    let mut config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
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
    fn pem_blocks_decodes_wrapped_and_unpadded_bodies() {
        // Windows line endings plus a body wrapped across two lines: the
        // reader has to concatenate the base64 back into one stream.
        let wrapped = concat!(
            "-----BEGIN CERTIFICATE-----\r\n",
            "YWJj\r\n",
            "ZA==\r\n",
            "-----END CERTIFICATE-----\r\n",
            "-----BEGIN PRIVATE KEY-----\n",
            "c2VjcmV0\n",
            "-----END PRIVATE KEY-----\n",
        );
        let blocks = pem_blocks(wrapped).unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].0, "CERTIFICATE");
        assert_eq!(&blocks[0].1[..], &b"abcd"[..]);
        assert_eq!(blocks[1].0, "PRIVATE KEY");
        assert_eq!(&blocks[1].1[..], &b"secret"[..]);

        // Six base64 characters: the missing "==" padding is restored.
        let unpadded = "-----BEGIN CERTIFICATE-----\nYWJjZA\n-----END CERTIFICATE-----\n";
        let blocks = pem_blocks(unpadded).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(&blocks[0].1[..], &b"abcd"[..]);
    }

    #[test]
    fn loads_cert_and_key_from_disk_and_reports_bad_input() {
        let dir = tempfile::tempdir().unwrap();
        let cert_path = dir.path().join("cert.pem");
        let key_path = dir.path().join("key.pem");
        let bad_path = dir.path().join("bad.pem");

        std::fs::write(
            &cert_path,
            "-----BEGIN CERTIFICATE-----\nYWJjZA==\n-----END CERTIFICATE-----\n",
        )
        .unwrap();
        std::fs::write(
            &key_path,
            "-----BEGIN PRIVATE KEY-----\nc2VjcmV0\n-----END PRIVATE KEY-----\n",
        )
        .unwrap();
        std::fs::write(&bad_path, "this is not a PEM file").unwrap();

        let certs = load_certs(&cert_path).unwrap();
        assert_eq!(certs.len(), 1);
        assert_eq!(certs[0].as_ref(), &b"abcd"[..]);
        assert!(matches!(load_key(&key_path), Ok(PrivateKeyDer::Pkcs8(_))));

        let err = load_certs(&bad_path).unwrap_err().to_string();
        assert!(err.contains("no CERTIFICATE PEM block"), "{err}");
    }

    #[test]
    fn cached_configs_are_reused() {
        assert!(Arc::ptr_eq(
            &profile_config(0, true),
            &profile_config(0, true)
        ));
        assert!(Arc::ptr_eq(
            &profile_config(0, false),
            &profile_config(0, false)
        ));
        assert!(!Arc::ptr_eq(
            &profile_config(0, true),
            &profile_config(0, false)
        ));
    }

    #[test]
    fn profile_suite_orders_match_goway() {
        // Chromium puts AES-256-GCM second; Firefox puts ChaCha20 second.
        assert_eq!(
            suite_order_for_profile(0)[1],
            CipherSuite::TLS13_AES_256_GCM_SHA384
        );
        let firefox = BROWSER_PROFILES
            .iter()
            .position(|p| !p.is_chromium)
            .unwrap();
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
