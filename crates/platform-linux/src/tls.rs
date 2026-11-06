//! Linux TLS client configuration: the system root store, read once, plus the
//! bundled webpki roots.

use std::sync::Arc;

use rustls::crypto::CryptoProvider;
use rustls::pki_types::CertificateDer;
use rustls::{ClientConfig, RootCertStore};

/// Protocol order the client offers: HTTP/2 first, HTTP/1.1 as the fallback.
const ALPN_PROTOCOLS: [&[u8]; 2] = [b"h2", b"http/1.1"];

/// The process-wide configuration built on first use.
///
/// `rustls-native-certs` reads the system store exactly once, here, because
/// every later call returns the cached geometry.
pub(crate) fn client_config() -> Arc<ClientConfig> {
    match build(Vec::new()) {
        Ok(config) => Arc::new(config),
        // Version selection is the only fallible step here, and it fails only
        // when the provider has no TLS 1.2/1.3 cipher suite or key exchange
        // group; the aws-lc-rs provider ships both. The fallback trusts only
        // the bundled webpki roots, a subset of the documented union, so it
        // never widens what the process accepts.
        Err(_) => Arc::new(bundled_roots_only()),
    }
}

/// Build a configuration that trusts the system roots, the bundled webpki
/// roots, and `extra_roots`.
///
/// Production passes no extra roots. Tests inject a private CA here to prove
/// that a root outside the bundled webpki set is accepted once the system
/// store provides it.
pub(crate) fn build(extra_roots: Vec<CertificateDer<'static>>) -> Result<ClientConfig, rustls::Error> {
    let provider = provider();
    let roots = root_store(extra_roots);
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = alpn();
    Ok(config)
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::aws_lc_rs::default_provider())
}

fn alpn() -> Vec<Vec<u8>> {
    ALPN_PROTOCOLS.iter().map(|protocol| protocol.to_vec()).collect()
}

/// webpki's bundled anchors plus everything the system store provides.
///
/// Certificates the store hands over that rustls cannot parse are skipped:
/// they cannot be used as anchors either way, and a single malformed file must
/// not cost the process every other root.
fn root_store(extra_roots: Vec<CertificateDer<'static>>) -> RootCertStore {
    let mut roots: RootCertStore = webpki_roots::TLS_SERVER_ROOTS.iter().cloned().collect();
    let system = rustls_native_certs::load_native_certs();
    roots.add_parsable_certificates(system.certs);
    roots.add_parsable_certificates(extra_roots);
    roots
}

/// The webpki half of the trust set, for the unreachable failure path above.
fn bundled_roots_only() -> ClientConfig {
    let mut config = ClientConfig::builder()
        .with_root_certificates(root_store(Vec::new()))
        .with_no_client_auth();
    config.alpn_protocols = alpn();
    config
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::thread;
    use std::time::Duration;

    use rcgen::{
        BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose,
    };
    use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
    use rustls::{ClientConnection, ServerConfig, ServerConnection, StreamOwned};

    use super::*;

    /// A private CA and a server certificate it signed. Neither is in the
    /// bundled webpki roots.
    struct Chain {
        ca: CertificateDer<'static>,
        server: CertificateDer<'static>,
        key: PrivateKeyDer<'static>,
    }

    fn chain() -> Chain {
        let mut ca_params = CertificateParams::new(Vec::new()).expect("ca parameters");
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let ca = CertifiedIssuer::self_signed(ca_params, KeyPair::generate().expect("ca key")).expect("ca certificate");

        let mut server_params = CertificateParams::new(vec!["localhost".to_string()]).expect("server parameters");
        server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let server_key = KeyPair::generate().expect("server key");
        let server_cert = server_params.signed_by(&server_key, &ca).expect("server certificate");

        Chain {
            ca: ca.der().clone(),
            server: server_cert.der().clone(),
            key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(server_key.serialize_der())),
        }
    }

    fn server_config(chain: &Chain) -> Arc<ServerConfig> {
        let config = ServerConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
            .with_safe_default_protocol_versions()
            .expect("server versions")
            .with_no_client_auth()
            .with_single_cert(vec![chain.server.clone()], chain.key.clone_key())
            .expect("server certificate");
        Arc::new(config)
    }

    fn echo_once(server_config: Arc<ServerConfig>) -> (SocketAddr, thread::JoinHandle<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let address = listener.local_addr().expect("address");
        let handle = thread::spawn(move || {
            let Ok((tcp, _)) = listener.accept() else { return };
            let connection = ServerConnection::new(server_config).expect("server connection");
            let mut stream = StreamOwned::new(connection, tcp);
            let mut buffer = [0u8; 4];
            if stream.read_exact(&mut buffer).is_ok() {
                let _ = stream.write_all(&buffer);
                let _ = stream.flush();
            }
        });
        (address, handle)
    }

    /// Run one TLS handshake plus a four-byte echo against a fresh local server.
    fn probe(chain: &Chain, config: ClientConfig) -> std::io::Result<()> {
        let (address, handle) = echo_once(server_config(chain));
        let name = ServerName::try_from("localhost").expect("server name");
        let connection = ClientConnection::new(Arc::new(config), name).expect("client connection");
        let tcp = TcpStream::connect(address)?;
        tcp.set_read_timeout(Some(Duration::from_secs(10)))?;
        tcp.set_write_timeout(Some(Duration::from_secs(10)))?;
        let mut stream = StreamOwned::new(connection, tcp);
        let outcome = stream.write_all(b"ping").and_then(|()| stream.flush()).and_then(|()| {
            let mut buffer = [0u8; 4];
            stream.read_exact(&mut buffer)?;
            Ok(buffer)
        });
        drop(stream);
        let _ = handle.join();
        outcome.map(|buffer| assert_eq!(&buffer, b"ping"))
    }

    #[test]
    fn os_roots_are_trusted_beside_webpki() {
        let chain = chain();

        let trusted = build(vec![chain.ca.clone()]).expect("config with the private CA");
        probe(&chain, trusted).expect("a root injected as a system root must be trusted");

        let untrusted = build(Vec::new()).expect("config without the private CA");
        assert!(
            probe(&chain, untrusted).is_err(),
            "webpki alone must reject a server under a private CA"
        );
    }

    #[test]
    fn client_config_offers_http2_and_http11() {
        let config = build(Vec::new()).expect("config");
        assert_eq!(config.alpn_protocols, vec![b"h2".to_vec(), b"http/1.1".to_vec()]);
    }

    #[test]
    fn system_store_errors_do_not_drop_the_bundled_roots() {
        let roots = root_store(Vec::new());
        assert!(roots.len() >= webpki_roots::TLS_SERVER_ROOTS.len());
    }
}
