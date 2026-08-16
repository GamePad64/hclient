//! Proxying, watched from the proxy's own side of the wire.
//!
//! Every assertion here is about bytes a fixture actually received, not
//! about what the client believes it sent: the two claims that matter —
//! the request line changes shape, and the origin travels **by name** —
//! are both invisible from the caller's end.

#![cfg(feature = "proxy")]

use http_ng::Client;
use http_ng_dns::IpLiteralOnly;
use http_ng_native::{HttpConnect, Native, Proxy, Socks5};
use http_ng_rt_tokio::Tokio;
use http_ng_tls::NoTls;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc;
use std::time::Duration;

const BOUND: Duration = Duration::from_secs(5);

/// Reads a request head off `s`, up to and including the blank line.
fn read_head(s: &mut TcpStream) -> String {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match s.read(&mut byte) {
            Ok(0) | Err(_) => break,
            Ok(_) => head.push(byte[0]),
        }
    }
    String::from_utf8_lossy(&head).into_owned()
}

fn ok_response() -> &'static [u8] {
    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi"
}

/// An HTTP proxy that answers `http://` requests itself, and reports the
/// request line it was given.
fn http_proxy(status: &'static str) -> (SocketAddr, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut s) = conn else { break };
            let head = read_head(&mut s);
            let _ = tx.send(head);
            if status == "200" {
                let _ = s.write_all(ok_response());
            } else {
                let _ = s.write_all(
                    b"HTTP/1.1 407 Proxy Authentication Required\r\n\
                      Proxy-Authenticate: Basic realm=\"x\"\r\n\
                      Content-Length: 0\r\nConnection: close\r\n\r\n",
                );
            }
            let _ = s.flush();
        }
    });
    (addr, rx)
}

/// A SOCKS5 proxy that performs RFC 1928 §3 and §4, reports the address it
/// was asked for, and then answers HTTP itself rather than connecting
/// anywhere — the origin's own behaviour is not what these tests are about.
fn socks5_proxy(want_auth: bool) -> (SocketAddr, mpsc::Receiver<(u8, String, u16)>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut s) = conn else { break };
            let mut hdr = [0u8; 2];
            if s.read_exact(&mut hdr).is_err() {
                continue;
            }
            let mut methods = vec![0u8; usize::from(hdr[1])];
            let _ = s.read_exact(&mut methods);
            let chosen = if want_auth { 0x02u8 } else { 0x00u8 };
            let _ = s.write_all(&[0x05, chosen]);
            if want_auth {
                let mut v = [0u8; 1];
                let _ = s.read_exact(&mut v);
                let mut n = [0u8; 1];
                let _ = s.read_exact(&mut n);
                let mut user = vec![0u8; usize::from(n[0])];
                let _ = s.read_exact(&mut user);
                let _ = s.read_exact(&mut n);
                let mut pass = vec![0u8; usize::from(n[0])];
                let _ = s.read_exact(&mut pass);
                let _ = s.write_all(&[0x01, 0x00]);
            }
            let mut req = [0u8; 4];
            if s.read_exact(&mut req).is_err() {
                continue;
            }
            let mut len = [0u8; 1];
            let _ = s.read_exact(&mut len);
            let mut host = vec![0u8; usize::from(len[0])];
            let _ = s.read_exact(&mut host);
            let mut port = [0u8; 2];
            let _ = s.read_exact(&mut port);
            let _ = tx.send((
                req[3],
                String::from_utf8_lossy(&host).into_owned(),
                u16::from_be_bytes(port),
            ));
            // §6: granted, bound to 0.0.0.0:0.
            let _ = s.write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]);
            let _ = read_head(&mut s);
            let _ = s.write_all(ok_response());
            let _ = s.flush();
        }
    });
    (addr, rx)
}

async fn get<T>(client: &Client<T>, url: &str) -> Result<u16, http_ng_core::Error>
where
    T: http_ng_core::unversioned::Transport<Error = http_ng_core::Error>,
{
    // The head is the whole assertion here — every fixture answers with
    // `Connection: close` and a two-byte body, so draining it would prove
    // nothing these tests are about.
    let resp = client.get(url).send().await?;
    Ok(resp.status().as_u16())
}

/// **`http://` through an HTTP proxy takes absolute-form**, RFC 9112
/// §3.2.2, and the control is the same client without the proxy — whose
/// line is origin-form. Either alone would pass for a client that always
/// wrote one of the two.
#[tokio::test(flavor = "multi_thread")]
async fn an_http_proxy_gets_absolute_form_and_a_direct_client_does_not() {
    let (proxy, lines) = http_proxy("200");
    let (origin, direct_lines) = http_proxy("200");

    let via = Client::builder(Native::new(Tokio, NoTls, IpLiteralOnly).proxy(Proxy::new(
        HttpConnect::new(),
        "127.0.0.1",
        proxy.port(),
    )))
    .build()
    .expect("build");
    let status = tokio::time::timeout(BOUND, get(&via, "http://example.invalid/thing"))
        .await
        .expect("must not hang")
        .expect("the proxy answers");
    assert_eq!(status, 200);
    let head = lines.recv_timeout(BOUND).expect("the proxy saw a request");
    assert!(
        head.starts_with("GET http://example.invalid/thing HTTP/1.1\r\n"),
        "absolute-form, got: {:?}",
        head.lines().next()
    );

    let plain = Client::builder(Native::new(Tokio, NoTls, IpLiteralOnly))
        .build()
        .expect("build");
    let url = format!("http://127.0.0.1:{}/thing", origin.port());
    let _ = tokio::time::timeout(BOUND, get(&plain, &url))
        .await
        .expect("must not hang");
    let head = direct_lines
        .recv_timeout(BOUND)
        .expect("the origin saw one");
    assert!(
        head.starts_with("GET /thing HTTP/1.1\r\n"),
        "origin-form, got: {:?}",
        head.lines().next()
    );
}

/// **`Proxy-Authorization` rides the absolute-form request**, and the
/// control is the run above, which carries none.
#[tokio::test(flavor = "multi_thread")]
async fn proxy_authorization_reaches_an_absolute_form_request() {
    let (proxy, lines) = http_proxy("200");
    let client = Client::builder(
        Native::new(Tokio, NoTls, IpLiteralOnly).proxy(Proxy::new(
            HttpConnect::new()
                .basic_auth("Aladdin", "open sesame")
                .expect("valid"),
            "127.0.0.1",
            proxy.port(),
        )),
    )
    .build()
    .expect("build");
    let _ = tokio::time::timeout(BOUND, get(&client, "http://example.invalid/x"))
        .await
        .expect("must not hang");
    let head = lines.recv_timeout(BOUND).expect("saw a request");
    assert!(
        head.contains("proxy-authorization: Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==")
            || head.contains("Proxy-Authorization: Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ=="),
        "the header must reach the proxy, got:\n{head}"
    );
}

/// **The origin travels by name**, `ATYP=0x03`, and is never resolved
/// here — which is the whole reason this is not a wrapper over
/// `TcpConnect`, whose `connect` takes a `SocketAddr` and could not carry
/// one. The resolver is `IpLiteralOnly`, so a client that tried to resolve
/// `example.invalid` itself would fail rather than reach the proxy.
#[tokio::test(flavor = "multi_thread")]
async fn socks5_sends_the_origin_as_a_name_and_never_resolves_it() {
    let (proxy, asked) = socks5_proxy(false);
    let client = Client::builder(Native::new(Tokio, NoTls, IpLiteralOnly).proxy(Proxy::new(
        Socks5::new(),
        "127.0.0.1",
        proxy.port(),
    )))
    .build()
    .expect("build");
    let status = tokio::time::timeout(BOUND, get(&client, "http://example.invalid:8080/x"))
        .await
        .expect("must not hang")
        .expect("the proxy answers");
    assert_eq!(status, 200);

    let (atyp, host, port) = asked.recv_timeout(BOUND).expect("the proxy was asked");
    assert_eq!(atyp, 0x03, "DOMAINNAME, not an address");
    assert_eq!(host, "example.invalid");
    assert_eq!(port, 8080);
}

/// RFC 1929's sub-negotiation happens when credentials are configured, and
/// the request still arrives — the control being the test above, which
/// offers no-auth and is answered with it.
#[tokio::test(flavor = "multi_thread")]
async fn socks5_password_auth_is_negotiated_and_the_request_still_arrives() {
    let (proxy, asked) = socks5_proxy(true);
    let client = Client::builder(Native::new(Tokio, NoTls, IpLiteralOnly).proxy(Proxy::new(
        Socks5::new().password_auth("u", "p").expect("valid"),
        "127.0.0.1",
        proxy.port(),
    )))
    .build()
    .expect("build");
    let status = tokio::time::timeout(BOUND, get(&client, "http://example.invalid/x"))
        .await
        .expect("must not hang")
        .expect("the proxy answers");
    assert_eq!(status, 200);
    let (_, host, port) = asked.recv_timeout(BOUND).expect("asked");
    assert_eq!((host.as_str(), port), ("example.invalid", 80));
}

/// **The capability is read from the transport that knows**, and both
/// directions are asserted because a constant would be right in one of
/// them — `client_certs`' lesson, one field over.
#[tokio::test(flavor = "multi_thread")]
async fn the_capability_follows_whether_a_proxy_was_configured() {
    use http_ng_core::unversioned::Transport;
    let direct = Native::new(Tokio, NoTls, IpLiteralOnly);
    assert!(!direct.capabilities().proxy);
    let via = Native::new(Tokio, NoTls, IpLiteralOnly).proxy(Proxy::new(
        Socks5::new(),
        "127.0.0.1",
        1080,
    ));
    assert!(via.capabilities().proxy);
}

/// **On an absolute-form request a `407` is a response**, from a server
/// acting as origin for that request, and is passed through untouched.
///
/// The other half of the pair — a `407` refusing a **tunnel**, which is
/// `ErrorKind::Connect` and reaches no caller as a response — is
/// `src/proxy.rs`'s `a_407_refusing_the_tunnel_is_a_connect_error`,
/// because `CONNECT` is only ever sent for `https://` and proving that
/// half here would need a TLS origin to prove nothing extra.
#[tokio::test(flavor = "multi_thread")]
async fn a_407_answering_an_absolute_form_request_is_a_response() {
    let (proxy, _lines) = http_proxy("407");

    let client = Client::builder(Native::new(Tokio, NoTls, IpLiteralOnly).proxy(Proxy::new(
        HttpConnect::new(),
        "127.0.0.1",
        proxy.port(),
    )))
    .build()
    .expect("build");

    // Absolute-form: the proxy answered the request, and `407` is that
    // answer.
    let status = tokio::time::timeout(BOUND, get(&client, "http://example.invalid/x"))
        .await
        .expect("must not hang")
        .expect("a 407 here is a response, not an error");
    assert_eq!(status, 407);
}

// --- TLS over a tunnel --------------------------------------------------

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::sync::Arc;

fn identity() -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()])
        .expect("rcgen can always make a self-signed cert");
    (
        CertificateDer::from(cert.cert.der().to_vec()),
        PrivateKeyDer::try_from(cert.signing_key.serialize_der()).expect("pkcs8 from rcgen"),
    )
}

/// A TLS origin that reports **the server name it was greeted with**.
///
/// That is the whole assertion: over a tunnel the certificate is still the
/// origin's, so a client that sent the proxy's name would fail the
/// handshake — but it would also fail if it sent nothing, and the two are
/// different defects. The name is read off the accepted connection rather
/// than inferred from the request succeeding.
fn tls_origin() -> (SocketAddr, CertificateDer<'static>, mpsc::Receiver<String>) {
    let (cert_der, key_der) = identity();
    let mut cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der.clone()], key_der)
        .expect("the cert and key were made together");
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(cfg));

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async move {
            listener.set_nonblocking(true).expect("nonblocking");
            let listener = tokio::net::TcpListener::from_std(listener).expect("adopt");
            while let Ok((tcp, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    let Ok(mut tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    let sni = tls.get_ref().1.server_name().unwrap_or("<none>").to_owned();
                    let _ = tx.send(sni);
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8; 1024];
                    let _ = tls.read(&mut buf).await;
                    let _ = tls.write_all(ok_response()).await;
                    let _ = tls.flush().await;
                });
            }
        });
    });
    (addr, cert_der, rx)
}

/// An HTTP proxy that tunnels: `200`, then bytes both ways, and it reports
/// the authority it was asked for.
fn tunnelling_proxy(origin: SocketAddr) -> (SocketAddr, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut client) = conn else { break };
            let head = read_head(&mut client);
            let target = head
                .lines()
                .next()
                .and_then(|l| l.split_whitespace().nth(1))
                .unwrap_or_default()
                .to_owned();
            let _ = tx.send(target);
            let Ok(mut upstream) = TcpStream::connect(origin) else {
                continue;
            };
            if client
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .is_err()
            {
                continue;
            }
            let (mut c2, mut u2) = (
                client.try_clone().expect("clone"),
                upstream.try_clone().expect("clone"),
            );
            std::thread::spawn(move || {
                let _ = std::io::copy(&mut c2, &mut u2);
            });
            std::thread::spawn(move || {
                let _ = std::io::copy(&mut upstream, &mut client);
            });
        }
    });
    (addr, rx)
}

/// **The TLS handshake rides the tunnel and carries the ORIGIN's name.**
///
/// The gap `docs/proxy-design.md` §8 named as the first thing a second
/// pass should add, and it is three claims in one run: the `CONNECT` names
/// the origin's authority, the origin's certificate validates (so the
/// stream really is end to end rather than terminated at the proxy), and
/// the SNI the origin was greeted with is `localhost` — **not**
/// `127.0.0.1`, which is the proxy's own host and the value a connector
/// that took its name from the socket would have sent.
///
/// The resolver is `IpLiteralOnly`, so `localhost` is a name this client
/// **cannot** resolve: reaching the origin at all proves the name was
/// carried rather than looked up.
#[tokio::test(flavor = "multi_thread")]
async fn tls_rides_the_tunnel_and_the_origin_is_greeted_with_its_own_name() {
    let (origin, cert, sni) = tls_origin();
    let (proxy, asked) = tunnelling_proxy(origin);

    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert).expect("a DER certificate");
    let tls = http_ng_tls_rustls::Rustls::from_config(Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    ));

    let client = Client::builder(Native::new(Tokio, tls, IpLiteralOnly).proxy(Proxy::new(
        HttpConnect::new(),
        "127.0.0.1",
        proxy.port(),
    )))
    .build()
    .expect("build");

    let url = format!("https://localhost:{}/x", origin.port());
    let status = tokio::time::timeout(BOUND, get(&client, &url))
        .await
        .expect("must not hang")
        .expect("the tunnelled request must succeed");
    assert_eq!(status, 200);

    assert_eq!(
        asked.recv_timeout(BOUND).expect("the proxy was asked"),
        format!("localhost:{}", origin.port()),
        "the CONNECT names the origin's authority, not the proxy's"
    );
    assert_eq!(
        sni.recv_timeout(BOUND).expect("the origin was greeted"),
        "localhost",
        "the SNI is the origin's name; the proxy's host is 127.0.0.1"
    );
}

/// **A proxy that speaks past its own handshake is refused, not rewound.**
///
/// Nothing the origin might say can have arrived — the client has not
/// written to it — so those bytes are the proxy's. Carrying them on would
/// feed them to the TLS handshake as if the origin had sent them, which is
/// the quieter of the two failures and the worse one.
///
/// The control is the test above: the same fixture, differing only in the
/// trailing bytes, completes a handshake.
#[tokio::test(flavor = "multi_thread")]
async fn a_proxy_that_speaks_first_is_refused() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let proxy = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut s) = conn else { break };
            let _ = read_head(&mut s);
            // The `200`, and then eight bytes nobody asked for.
            let _ = s.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\nSURPRISE");
            let _ = s.flush();
            std::thread::sleep(Duration::from_millis(200));
        }
    });

    let client = Client::builder(
        Native::new(Tokio, http_ng_tls::NoTls, IpLiteralOnly).proxy(Proxy::new(
            HttpConnect::new(),
            "127.0.0.1",
            proxy.port(),
        )),
    )
    .build()
    .expect("build");

    let err = tokio::time::timeout(BOUND, get(&client, "https://example.invalid/x"))
        .await
        .expect("must not hang")
        .expect_err("the tunnel must be refused");
    assert_eq!(*err.kind(), http_ng_core::ErrorKind::Connect);
    // The source type, not the message: `ProxySpokeFirst(8)` names both
    // the defect and how many bytes were invented, and a test reading the
    // rendered string would pass for any wording.
    let spoke = std::error::Error::source(&err)
        .and_then(|s| s.downcast_ref::<http_ng_native::ProxySpokeFirst>())
        .expect("the defect must be readable off the error");
    assert_eq!(spoke.0, 8, "the eight bytes the fixture invented");
}

/// A SOCKS5 proxy that really connects and really pipes, unlike
/// [`socks5_proxy`], which answers HTTP itself. Needed for the TLS row:
/// a handshake cannot be faked by a fixture that never dials.
fn socks5_tunnel(origin: SocketAddr) -> (SocketAddr, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut client) = conn else { break };
            let mut hdr = [0u8; 2];
            if client.read_exact(&mut hdr).is_err() {
                continue;
            }
            let mut methods = vec![0u8; usize::from(hdr[1])];
            let _ = client.read_exact(&mut methods);
            let _ = client.write_all(&[0x05, 0x00]);
            let mut req = [0u8; 4];
            if client.read_exact(&mut req).is_err() {
                continue;
            }
            let mut len = [0u8; 1];
            let _ = client.read_exact(&mut len);
            let mut host = vec![0u8; usize::from(len[0])];
            let _ = client.read_exact(&mut host);
            let mut port = [0u8; 2];
            let _ = client.read_exact(&mut port);
            let _ = tx.send(format!(
                "{}:{}",
                String::from_utf8_lossy(&host),
                u16::from_be_bytes(port)
            ));
            let Ok(mut upstream) = TcpStream::connect(origin) else {
                continue;
            };
            let _ = client.write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]);
            let (mut c2, mut u2) = (
                client.try_clone().expect("clone"),
                upstream.try_clone().expect("clone"),
            );
            std::thread::spawn(move || {
                let _ = std::io::copy(&mut c2, &mut u2);
            });
            std::thread::spawn(move || {
                let _ = std::io::copy(&mut upstream, &mut client);
            });
        }
    });
    (addr, rx)
}

/// **The same three claims as the `CONNECT` row, over a protocol that
/// shares no bytes with it.** SOCKS5 tunnels either scheme, so this is the
/// half the first pass left out — and it is the strongest evidence the
/// seam is in the right place: the TLS handshake, the origin's
/// certificate and the origin's SNI are the transport's business, and
/// nothing about them changes with the protocol that carried the bytes.
#[tokio::test(flavor = "multi_thread")]
async fn tls_rides_a_socks5_tunnel_with_the_origin_name_too() {
    let (origin, cert, sni) = tls_origin();
    let (proxy, asked) = socks5_tunnel(origin);

    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert).expect("a DER certificate");
    let tls = http_ng_tls_rustls::Rustls::from_config(Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    ));

    let client = Client::builder(Native::new(Tokio, tls, IpLiteralOnly).proxy(Proxy::new(
        Socks5::new(),
        "127.0.0.1",
        proxy.port(),
    )))
    .build()
    .expect("build");

    let url = format!("https://localhost:{}/x", origin.port());
    let status = tokio::time::timeout(BOUND, get(&client, &url))
        .await
        .expect("must not hang")
        .expect("the tunnelled request must succeed");
    assert_eq!(status, 200);
    assert_eq!(
        asked.recv_timeout(BOUND).expect("asked"),
        format!("localhost:{}", origin.port())
    );
    assert_eq!(
        sni.recv_timeout(BOUND).expect("greeted"),
        "localhost",
        "the SNI is the origin's name over SOCKS5 exactly as it is over CONNECT"
    );
}

/// **A proxy that agrees and then vanishes is a failure with a name, not
/// a hang.** The refusal cases above all decline up front; this one
/// establishes the tunnel and drops it before the origin can answer,
/// which is the shape a real proxy takes when its own upstream dies.
///
/// The TLS backend is the **real** one, with the origin's certificate
/// trusted, and that is the whole point: with `NoTls` an `https://`
/// request fails the same way whether the tunnel is alive or dead, so
/// such a test would pass for a client that never noticed. The control is
/// `tls_rides_the_tunnel_and_the_origin_is_greeted_with_its_own_name`,
/// which differs in one thing — its proxy pipes instead of dropping — and
/// succeeds.
#[tokio::test(flavor = "multi_thread")]
async fn a_tunnel_that_dies_after_it_is_established_fails_rather_than_hangs() {
    let (_origin, cert, _sni) = tls_origin();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let proxy = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut s) = conn else { break };
            let _ = read_head(&mut s);
            let _ = s.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n");
            let _ = s.flush();
            // Established, and then gone: the client's ClientHello goes
            // into a socket nobody will answer.
            drop(s);
        }
    });

    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert).expect("a DER certificate");
    let tls = http_ng_tls_rustls::Rustls::from_config(Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    ));

    let client = Client::builder(Native::new(Tokio, tls, IpLiteralOnly).proxy(Proxy::new(
        HttpConnect::new(),
        "127.0.0.1",
        proxy.port(),
    )))
    .build()
    .expect("build");

    let err = tokio::time::timeout(BOUND, get(&client, "https://localhost:1/x"))
        .await
        .expect("must not hang")
        .expect_err("a dead tunnel cannot complete a handshake");
    // The handshake is where it dies, because that is the first thing
    // written into the tunnel. What is asserted is that it dies at all
    // and says so.
    assert_eq!(*err.kind(), http_ng_core::ErrorKind::Tls, "{err:?}");
}
