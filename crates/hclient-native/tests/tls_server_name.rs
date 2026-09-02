//! What name this transport asks TLS to verify, watched from a real
//! handshake rather than from the argument it passes.
//!
//! `http::Uri::host()` returns an IPv6 literal **with its brackets** —
//! `[::1]`, not `::1` — because the brackets are URI syntax (RFC 3986
//! §3.2.2), part of the authority rather than part of the host. Everything
//! downstream of a URI wants the host without them:
//! `rustls_pki_types::ServerName::try_from` accepts `::1` and rejects
//! `[::1]` as neither a DNS name nor an address, and every resolver in this
//! workspace strips before parsing. `connect.rs` is where the URI ends and
//! a TLS name begins, so `connect.rs` is where the strip belongs — see
//! `hclient_core::bare_host` and `hclient_tls::TlsRequest::server_name`.
//!
//! **The assertion is that the handshake completes**, against a certificate
//! that carries the name being dialled — an IP SAN for the two literals, a
//! DNS SAN for the name. Checking `ServerName::try_from` in isolation would
//! pin rustls's behaviour and say nothing about ours; a server that
//! finished a handshake is only reachable if the name we sent was the name
//! the certificate covers.
//!
//! Three rows, and the third is not decoration: a strip that fires
//! unconditionally (`&s[1..s.len() - 1]`, or a `trim_matches`) turns
//! `localhost` into `ocalhos`, which is still a perfectly valid DNS name
//! and fails only against the certificate. Without that row, the fix's
//! most obvious mutation lives.
#![cfg(not(target_family = "wasm"))]

use futures_util::stream;
use hclient_core::unversioned::Transport;
use hclient_core::{Error, RequestBody};
use hclient_dns::{IpLiteralOnly, RData, Record, Resolve, rtype};
use hclient_native::Native;
use hclient_rt_tokio::Tokio;
use hclient_tls_rustls::Rustls;
use http_body_util::BodyExt;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::net::{IpAddr, SocketAddr};
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

/// A resolver that answers every name with one address.
///
/// Needed for the named row: `IpLiteralOnly` refuses a name, and
/// `SystemDns` would make the test depend on what `localhost` resolves to
/// on the machine running it. The name under test is the one in the
/// certificate, and it must reach a server whose address nobody looked up.
#[derive(Debug, Clone, Copy)]
struct Pointing(IpAddr);

impl Pointing {
    fn answer(self, this_family: bool) -> Vec<Result<Record, Error>> {
        if this_family {
            vec![Ok(Record::new(RData::from(self.0)))]
        } else {
            // The other family has no answer, which is not an error — the
            // rule `IpLiteralOnly` documents for the same case.
            vec![]
        }
    }
}

impl Resolve for Pointing {
    type Records<'a>
        = std::pin::Pin<Box<dyn futures_core::Stream<Item = Result<Record, Error>> + Send + 'a>>
    where
        Self: 'a;

    fn supports(&self, rtype: u16) -> bool {
        matches!(rtype, rtype::A | rtype::AAAA)
    }

    fn lookup<'a>(&'a self, name: &str, rtype: u16) -> Self::Records<'a> {
        let _ = name;
        match rtype {
            rtype::A => Box::pin(stream::iter(self.answer(self.0.is_ipv4()))),
            rtype::AAAA => Box::pin(stream::iter(self.answer(self.0.is_ipv6()))),
            _ => Box::pin(futures_util::stream::empty()),
        }
    }
}

/// One request over TLS, returning the body the server sent.
async fn get<D: Resolve>(uri: &str, cert: &CertificateDer<'static>, dns: D) -> String {
    let t = Native::new(Tokio, client_tls(cert), dns);
    let req = http::Request::builder()
        .uri(uri)
        .body(RequestBody::Empty)
        .unwrap();
    let resp = t
        .execute(req)
        .await
        .unwrap_or_else(|e| panic!("{uri}: the handshake had to complete, got {e:?} ({e})"));
    assert_eq!(resp.status(), 200);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// The defect this file was written for: `https://[::1]:port/`.
#[tokio::test(flavor = "multi_thread")]
async fn an_ipv6_literal_authority_completes_the_handshake() {
    let Some((addr, cert)) = spawn_tls("[::1]:0".parse().unwrap()) else {
        eprintln!("skipped: this host has no IPv6 loopback");
        return;
    };
    // `{addr}` on a v6 `SocketAddr` is `[::1]:port` — the bracketed
    // authority a caller writes, and the one `Uri::host()` hands back with
    // its brackets still on.
    assert_eq!(
        get(&format!("https://{addr}/"), &cert, IpLiteralOnly).await,
        "ok"
    );
}

/// The control that says the strip did not break the unbracketed literal
/// it was never meant to touch.
#[tokio::test(flavor = "multi_thread")]
async fn an_ipv4_literal_authority_completes_the_handshake() {
    let (addr, cert) = spawn_tls("127.0.0.1:0".parse().unwrap()).expect("v4 loopback");
    assert_eq!(
        get(&format!("https://{addr}/"), &cert, IpLiteralOnly).await,
        "ok"
    );
}

/// A named authority must reach TLS byte for byte. A strip that fires on
/// every host sends `ocalhos`, which is a valid DNS name and a certificate
/// mismatch.
#[tokio::test(flavor = "multi_thread")]
async fn a_named_authority_reaches_tls_unchanged() {
    let (addr, cert) = spawn_tls("127.0.0.1:0".parse().unwrap()).expect("v4 loopback");
    let dns = Pointing(addr.ip());
    assert_eq!(
        get(&format!("https://localhost:{}/", addr.port()), &cert, dns).await,
        "ok"
    );
}
