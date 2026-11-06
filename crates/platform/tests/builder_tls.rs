//! A `reqwest` client that skips the platform adapter has no trusted roots.
//!
//! The dependency features are `rustls-tls-manual-roots`, so a plain builder
//! starts with an empty root store and cannot validate any HTTPS server. The
//! adapter supplies the trust set; this test proves both halves against a local
//! server whose private CA is in neither the webpki roots nor the system store.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose,
    SanType,
};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};

/// A private CA plus a server certificate it signed for `127.0.0.1`.
struct Chain {
    ca: rustls::pki_types::CertificateDer<'static>,
    server: rustls::pki_types::CertificateDer<'static>,
    key: PrivateKeyDer<'static>,
}

fn chain() -> Chain {
    let mut ca_params = CertificateParams::new(Vec::new()).expect("ca parameters");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca = CertifiedIssuer::self_signed(ca_params, KeyPair::generate().expect("ca key")).expect("ca certificate");

    let mut server_params = CertificateParams::new(Vec::new()).expect("server parameters");
    server_params
        .subject_alt_names
        .push(SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)));
    server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let server_key = KeyPair::generate().expect("server key");
    let server_cert = server_params.signed_by(&server_key, &ca).expect("server certificate");

    Chain {
        ca: ca.der().clone(),
        server: server_cert.der().clone(),
        key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(server_key.serialize_der())),
    }
}

/// Start a local HTTPS server that answers `connections` requests with a 200.
///
/// The listener thread is left detached: a failed assertion can end the test
/// before the last connection arrives, and a join would then hang the harness.
fn start_server(chain: &Chain, connections: usize) -> String {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
    let address = listener.local_addr().expect("server address");
    let config = Arc::new(
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
            .with_safe_default_protocol_versions()
            .expect("server versions")
            .with_no_client_auth()
            .with_single_cert(vec![chain.server.clone()], chain.key.clone_key())
            .expect("server certificate"),
    );
    thread::spawn(move || {
        for _ in 0..connections {
            let Ok((tcp, _)) = listener.accept() else { return };
            let Ok(connection) = ServerConnection::new(Arc::clone(&config)) else {
                return;
            };
            let mut stream = StreamOwned::new(connection, tcp);
            // `read` performs the handshake, then waits for the request line.
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok");
            let _ = stream.flush();
        }
    });
    format!("https://{}/", SocketAddr::from((Ipv4Addr::LOCALHOST, address.port())))
}

/// True when `error` is the client rejecting the server certificate.
///
/// `io::Error::source` skips the boxed error a custom IO error holds, so the
/// rustls failure is not reachable by downcast through reqwest's source chain;
/// the debug form of a connect error carries the handshake failure.
fn rejects_the_certificate(error: &reqwest::Error) -> bool {
    let debug = format!("{error:?}");
    error.is_connect() && (debug.contains("InvalidCertificate") || debug.contains("invalid peer certificate"))
}

#[test]
fn builder_without_adapter_has_no_roots() {
    let chain = chain();
    let url = start_server(&chain, 2);

    let plain = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("plain client");
    let error = plain
        .get(&url)
        .send()
        .expect_err("a plain builder must not trust the private CA");
    eprintln!("plain builder -> {error:?}");
    assert!(
        rejects_the_certificate(&error),
        "expected a certificate rejection without the adapter, got: {error:?}"
    );

    let config = horon_agent_platform::tls_client_config_with_extra_roots(vec![chain.ca.clone()])
        .expect("client config with the private CA");
    let routed = reqwest::blocking::Client::builder()
        .use_preconfigured_tls((*config).clone())
        .timeout(Duration::from_secs(10))
        .build()
        .expect("client with the platform TLS configuration");
    let response = routed.get(&url).send().expect("the injected root is trusted");
    assert_eq!(response.status(), 200);
    assert_eq!(response.text().expect("response body"), "ok");
}
