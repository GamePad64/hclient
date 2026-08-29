//! The handshake's own facts reach a hook.
//!
//! `TlsInfo` has carried the protocol version, the cipher suite and the
//! negotiated ALPN since TLS became a seam here, and `hclient-native` has
//! had all three in hand at the line where it emits `Connected` — three
//! lines above it, in a local it already reads for protocol selection.
//! Nothing above this crate could see any of them, which is the shape
//! this workspace records twice already: *the limitation belongs to the
//! wrapper, and the layer beneath has the thing.*
//!
//! What these tests pin is the whole path — a real rustls server, a real
//! handshake, and the values read off the event rather than off the
//! backend.
#![cfg(not(target_family = "wasm"))]

use hclient_core::RequestBody;
use hclient_core::unversioned::Transport;
use hclient_native::Native;
use hclient_rt_tokio::Tokio;
use hclient_tls_rustls::Rustls;
use http_body_util::BodyExt;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::net::SocketAddr;
use std::sync::Arc;

/// One self-signed certificate covering every name these tests dial.
///
/// `rcgen` turns an IP-shaped SAN into an **IP** SAN and everything else
/// into a DNS SAN, which is exactly the distinction under test: a literal
/// is checked against `iPAddress`, a name against `dNSName`.
fn identity() -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    let cert = rcgen::generate_simple_self_signed(vec![
        "localhost".into(),
        "127.0.0.1".into(),
        "::1".into(),
    ])
    .expect("rcgen can always make a self-signed cert");
    (
        CertificateDer::from(cert.cert.der().to_vec()),
        PrivateKeyDer::try_from(cert.signing_key.serialize_der()).expect("pkcs8 from rcgen"),
    )
}

/// A TLS server on `bind` that answers every request with `200 ok`.
///
/// It speaks just enough HTTP/1.1 to be answered by a real client: read to
/// the end of the head, write a fixed response. The thing under test is the
/// handshake in front of it.
///
/// `None` when the address family is not available on this host — the v6
/// row uses it to skip rather than to fail, the same way
/// `an_endpoint_is_bound_in_the_peers_address_family` does in
/// `hclient-h3`.
fn spawn_tls(bind: SocketAddr) -> Option<(SocketAddr, CertificateDer<'static>)> {
    let (cert_der, key_der) = identity();
    let mut cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der.clone()], key_der)
        .expect("the cert and key were made together");
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(cfg));

    let listener = std::net::TcpListener::bind(bind).ok()?;
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    continue;
                };
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let Ok(mut tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    let mut head = Vec::new();
                    let mut byte = [0u8; 1];
                    while tls.read_exact(&mut byte).await.is_ok() {
                        head.push(byte[0]);
                        if head.ends_with(b"\r\n\r\n") {
                            break;
                        }
                    }
                    let _ = tls
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                        .await;
                    let _ = tls.flush().await;
                    // `close_notify`, not a bare drop: rustls reports a
                    // truncated stream as an error, and this crate's own
                    // `truncation_detection.rs` is why. A server that
                    // just dropped would make every row here fail for a
                    // reason that has nothing to do with the name.
                    let _ = tls.shutdown().await;
                });
            }
        });
    });

    Some((addr, cert_der))
}

/// A `Rustls` that trusts exactly this certificate and nothing else, so a
/// completed handshake is evidence about the name rather than about the
/// machine's trust store.
fn client_tls(cert: &CertificateDer<'static>) -> Rustls {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert.clone()).unwrap();
    let cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Rustls::from_config(Arc::new(cfg))
}

/// What one `Connected` said about its handshake.
///
/// A named struct rather than a tuple: `timed` and the three `Option`s
/// are four things a reader has to keep in order at every use site, and
/// the pair `(tls, tls_version)` is the whole point of the test.
#[derive(Debug, Clone, Default)]
struct Facts {
    version: Option<String>,
    cipher: Option<String>,
    alpn: Option<Vec<u8>>,
    /// Whether a handshake was **timed**, which is what separates *no
    /// TLS* from *TLS this backend will not describe*.
    timed: bool,
}

/// A hook that keeps the first `Connected` it sees.
#[derive(Debug, Clone, Default)]
struct Seen(std::sync::Arc<std::sync::Mutex<Option<Facts>>>);

impl Seen {
    fn facts(&self) -> Facts {
        self.0
            .lock()
            .unwrap()
            .clone()
            .expect("a Connected was emitted")
    }
}

impl hclient_core::unversioned::Hooks for Seen {
    fn on(&self, event: &hclient_core::unversioned::Event<'_>) {
        if let hclient_core::unversioned::Event::Connected(c) = event {
            let mut g = self.0.lock().unwrap();
            if g.is_none() {
                *g = Some(Facts {
                    version: c.tls_version.map(ToOwned::to_owned),
                    cipher: c.tls_cipher.map(ToOwned::to_owned),
                    alpn: c.alpn.map(<[u8]>::to_vec),
                    timed: c.timing.tls.is_some(),
                });
            }
        }
    }
}

async fn fetch(uri: &str, tls: Rustls, seen: Seen) {
    let t = Native::new(Tokio, tls, hclient_dns::IpLiteralOnly).hooks(seen);
    let req = http::Request::get(uri)
        .body(RequestBody::Empty)
        .expect("a well-formed request");
    let resp = t.execute(req).await.expect("the exchange completes");
    let body = resp
        .into_body()
        .collect()
        .await
        .expect("the body")
        .to_bytes();
    assert_eq!(&body[..], b"ok");
}

/// Over TLS, all three arrive and they are the real negotiated values —
/// not defaults, and not the client's offer.
#[tokio::test]
async fn a_handshake_reports_its_version_suite_and_alpn_to_a_hook() {
    let Some((addr, cert)) = spawn_tls("127.0.0.1:0".parse().unwrap()) else {
        return;
    };
    let seen = Seen::default();
    fetch(
        &format!("https://{addr}/x"),
        client_tls(&cert),
        seen.clone(),
    )
    .await;

    let f = seen.facts();
    assert_eq!(
        f.version.as_deref(),
        Some("TLSv1.3"),
        "the dotted spelling curl prints, not rustls' enum name"
    );
    let cipher = f.cipher.expect("rustls reports the suite");
    assert!(
        cipher.starts_with("TLS_") && cipher.contains("_SHA"),
        "an IANA registry name, not a debug rendering: {cipher}"
    );
    // The server above offers `http/1.1` alone, so this is what the peer
    // *selected* rather than what the client listed — which is the whole
    // reason the field is not a second statement of `version`.
    assert_eq!(f.alpn.as_deref(), Some(b"http/1.1".as_slice()));
    assert!(
        f.timed,
        "and the handshake was timed, so `None` here would mean unreported"
    );
}

/// Over plaintext there is no handshake, so all three are absent — and
/// `timing.tls` is absent with them, which is the pair that separates
/// *no TLS* from *TLS this backend will not describe*.
#[tokio::test]
async fn a_plaintext_connection_reports_no_tls_facts_and_no_tls_timing() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        for s in listener.incoming() {
            let Ok(mut s) = s else { break };
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf);
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
            let _ = s.flush();
        }
    });

    let seen = Seen::default();
    let t = Native::new(Tokio, hclient_tls::NoTls, hclient_dns::IpLiteralOnly).hooks(seen.clone());
    let req = http::Request::get(format!("http://{addr}/x"))
        .body(RequestBody::Empty)
        .expect("a well-formed request");
    let resp = t.execute(req).await.expect("the exchange completes");
    let _ = resp.into_body().collect().await.expect("the body");

    let f = seen.facts();
    assert_eq!(f.version, None);
    assert_eq!(f.cipher, None);
    assert_eq!(f.alpn, None);
    assert!(!f.timed, "no handshake was timed either");
}
