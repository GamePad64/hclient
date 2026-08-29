//! Named client identities: selection, refusal, and isolation.
//!
//! The load-bearing test here is the last one. A label resolves to its
//! own `TlsConfigId`, which is a component of `hclient-native`'s pool
//! key, so two labels cannot share a connection **by construction** — and
//! the failure mode if that were wrong is presenting one tenant's
//! certificate on another tenant's behalf, which no error would report.
//! So it is asserted from the server's side of the wire: two accepts, not
//! one.

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

/// A `Rustls` that trusts exactly this certificate and nothing else, so a
/// completed handshake is evidence about the name rather than about the
/// machine's trust store.
fn client_config(cert: &CertificateDer<'static>) -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert.clone()).unwrap();
    Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

/// The identities here carry no client certificate, and that is on
/// purpose: what these tests pin is **selection and isolation**, which are
/// this workspace's, where whether a certificate satisfies a server is
/// rustls' and the server's. A test needing a real mTLS handshake would
/// be testing rustls.
fn client_tls(cert: &CertificateDer<'static>) -> Rustls {
    Rustls::from_config(client_config(cert))
}

/// Counts accepted connections, so isolation is asserted from the
/// server's side rather than from ours.
fn spawn_counting_tls(
    bind: SocketAddr,
) -> Option<(
    SocketAddr,
    CertificateDer<'static>,
    Arc<std::sync::atomic::AtomicUsize>,
)> {
    let accepts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
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
    let seen = accepts.clone();

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
                seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let Ok(mut tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    // **Keeps the connection open**, which the reuse
                    // test needs and which a one-shot server quietly makes
                    // untestable: without it the second request races the
                    // peer's FIN and the count reads one for the wrong
                    // reason. The Alt-Svc fixture defect this workspace
                    // already recorded, met from the other side.
                    loop {
                        let mut head = Vec::new();
                        let mut byte = [0u8; 1];
                        let mut got = false;
                        while tls.read_exact(&mut byte).await.is_ok() {
                            got = true;
                            head.push(byte[0]);
                            if head.ends_with(b"\r\n\r\n") {
                                break;
                            }
                        }
                        if !got
                            || tls
                                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                                .await
                                .is_err()
                        {
                            break;
                        }
                        let _ = tls.flush().await;
                    }
                });
            }
        });
    });

    Some((addr, cert_der, accepts))
}

fn get(
    t: &impl Transport,
    uri: &str,
    identity: Option<&'static str>,
) -> http::Request<RequestBody> {
    let mut req = http::Request::get(uri)
        .body(RequestBody::Empty)
        .expect("a well-formed request");
    if let Some(name) = identity {
        req.extensions_mut()
            .insert(hclient_core::ClientIdentity::new(name));
    }
    let _ = t;
    req
}

/// A name this backend has not got is **refused, by name**, and no socket
/// is opened. Connecting with the default identity instead is how one
/// tenant's certificate reaches another tenant's server, and nothing at
/// the call site would show it happened.
#[tokio::test]
async fn a_name_this_backend_has_not_got_is_refused_before_any_socket() {
    let Some((addr, cert, accepts)) = spawn_counting_tls("127.0.0.1:0".parse().unwrap()) else {
        return;
    };
    let t = Native::new(Tokio, client_tls(&cert), hclient_dns::IpLiteralOnly);
    let err = t
        .execute(get(&t, &format!("https://{addr}/x"), Some("nobody")))
        .await
        .expect_err("an unknown identity is refused");

    let msg = format!("{err:#}");
    assert!(msg.contains("nobody"), "it names the label: {msg}");
    assert_eq!(
        accepts.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "and no connection was opened"
    );
}

/// **The load-bearing one.** Two labels, one origin, two connections —
/// asserted from the server's side, because a shared connection is
/// exactly the failure nothing else would report.
#[tokio::test]
async fn two_identities_to_one_origin_cannot_share_a_connection() {
    let Some((addr, cert, accepts)) = spawn_counting_tls("127.0.0.1:0".parse().unwrap()) else {
        return;
    };
    let base = client_tls(&cert);
    let cfg = client_config(&cert);
    let tls = base
        .with_identity("tenant-a", cfg.clone())
        .with_identity("tenant-b", cfg);
    let t = Native::new(Tokio, tls, hclient_dns::IpLiteralOnly);
    let uri = format!("https://{addr}/x");

    for name in ["tenant-a", "tenant-b"] {
        let resp = t
            .execute(get(&t, &uri, Some(name)))
            .await
            .unwrap_or_else(|e| panic!("{name}: {e:#}"));
        let body = resp.into_body().collect().await.expect("body").to_bytes();
        assert_eq!(&body[..], b"ok");
    }

    assert_eq!(
        accepts.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "two labels, two connections — the pool key carries the identity"
    );
}

/// The control the test above needs to mean anything: the **same** label
/// twice reuses one connection, so the count is about the identity rather
/// than about the pool being broken.
#[tokio::test]
async fn one_identity_twice_reuses_its_connection() {
    let Some((addr, cert, accepts)) = spawn_counting_tls("127.0.0.1:0".parse().unwrap()) else {
        return;
    };
    let tls = client_tls(&cert).with_identity("tenant-a", client_config(&cert));
    let t = Native::new(Tokio, tls, hclient_dns::IpLiteralOnly);
    let uri = format!("https://{addr}/x");

    for _ in 0..2 {
        let resp = t
            .execute(get(&t, &uri, Some("tenant-a")))
            .await
            .expect("both succeed");
        let _ = resp.into_body().collect().await.expect("body");
    }

    assert_eq!(
        accepts.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "one label, one connection"
    );
}
