//! Proxying, watched from the proxy's own side of the wire.
//!
//! Every assertion here is about bytes a fixture actually received, not
//! about what the client believes it sent: the two claims that matter —
//! the request line changes shape, and the origin travels **by name** —
//! are both invisible from the caller's end.

#![cfg(feature = "proxy")]

use hclient::Client;
use hclient_dns::IpLiteralOnly;
use hclient_native::{HttpConnect, Native, Proxy, Socks4, Socks5};
use hclient_rt_tokio::Tokio;
use hclient_tls::NoTls;
use std::error::Error as StdError;
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

async fn get(client: &Client, url: &str) -> Result<u16, hclient_core::Error> {
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
    use hclient_core::unversioned::Transport;
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
/// Three claims in one run: the `CONNECT` names
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
    let tls = hclient_tls_rustls::Rustls::from_config(Arc::new(
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
        Native::new(Tokio, hclient_tls::NoTls, IpLiteralOnly).proxy(Proxy::new(
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
    assert_eq!(*err.kind(), hclient_core::ErrorKind::Connect);
    // The source type, not the message: `ProxySpokeFirst(8)` names both
    // the defect and how many bytes were invented, and a test reading the
    // rendered string would pass for any wording.
    let spoke = StdError::source(&err)
        .and_then(|s| s.downcast_ref::<hclient_native::ProxySpokeFirst>())
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
    let tls = hclient_tls_rustls::Rustls::from_config(Arc::new(
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
    let tls = hclient_tls_rustls::Rustls::from_config(Arc::new(
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
    assert_eq!(*err.kind(), hclient_core::ErrorKind::Tls, "{err:?}");
}

/// **A bypassed origin goes direct, and the same client still proxies
/// everything else.** Both halves in one run, because a bypass list that
/// matched everything would pass the first assertion on its own and a
/// client that never bypassed would pass the second.
///
/// The direct half also checks the *shape* of the request, not only where
/// it landed: a bypassed request written in absolute-form would reach an
/// origin server that never agreed to act as a proxy, which is the
/// quieter half of getting this wrong.
#[tokio::test(flavor = "multi_thread")]
async fn a_bypassed_origin_goes_direct_and_in_origin_form() {
    let (proxy, proxied) = http_proxy("200");
    let (origin, direct) = http_proxy("200");

    let client = Client::builder(
        Native::new(Tokio, hclient_tls::NoTls, IpLiteralOnly)
            .proxy(Proxy::new(HttpConnect::new(), "127.0.0.1", proxy.port()).bypass(["127.0.0.1"])),
    )
    .build()
    .expect("build");

    let url = format!("http://127.0.0.1:{}/direct", origin.port());
    let status = tokio::time::timeout(BOUND, get(&client, &url))
        .await
        .expect("must not hang")
        .expect("the bypassed origin answers");
    assert_eq!(status, 200);
    let head = direct
        .recv_timeout(BOUND)
        .expect("the bypassed origin was reached directly");
    assert!(
        head.starts_with("GET /direct HTTP/1.1\r\n"),
        "origin-form, got: {:?}",
        head.lines().next()
    );

    let status = tokio::time::timeout(BOUND, get(&client, "http://example.invalid/proxied"))
        .await
        .expect("must not hang")
        .expect("the proxy answers");
    assert_eq!(status, 200);
    let head = proxied
        .recv_timeout(BOUND)
        .expect("everything else still goes through the proxy");
    assert!(
        head.starts_with("GET http://example.invalid/proxied HTTP/1.1\r\n"),
        "absolute-form, got: {:?}",
        head.lines().next()
    );
}

/// A SOCKS5 proxy that gets as far as `refuse_with` and stops there.
///
/// `None` means it refuses the *greeting* with RFC 1928 §3's `0xFF`;
/// `Some(rep)` means it accepts the greeting and refuses the CONNECT with
/// that `REP`. The two are different failures — one is "we will not talk
/// to you", the other "we talked, and your origin is unreachable" — and
/// the point of this fixture is that they arrive as different errors.
fn socks5_refusing(refuse_with: Option<u8>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut s) = conn else { break };
            let mut hdr = [0u8; 2];
            if s.read_exact(&mut hdr).is_err() {
                continue;
            }
            let mut methods = vec![0u8; usize::from(hdr[1])];
            let _ = s.read_exact(&mut methods);
            let Some(rep) = refuse_with else {
                let _ = s.write_all(&[0x05, 0xFF]);
                let _ = s.flush();
                continue;
            };
            let _ = s.write_all(&[0x05, 0x00]);
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
            let _ = s.write_all(&[0x05, rep, 0x00, 0x01, 0, 0, 0, 0, 0, 0]);
            let _ = s.flush();
        }
    });
    addr
}

/// The proxy protocol is part of the transport's type, which is the
/// visible cost of `P` being a parameter rather than a `Box<dyn ..>` —
/// and the reason it is one is in `proxy`'s module doc.
fn socks5_client(proxy: SocketAddr) -> Client {
    Client::builder(
        Native::new(Tokio, hclient_tls::NoTls, IpLiteralOnly).proxy(Proxy::new(
            Socks5::new(),
            "127.0.0.1",
            proxy.port(),
        )),
    )
    .build()
    .expect("build")
}

/// **The proxy's own upstream failing is a connect error carrying the
/// `REP` byte**, which is the last thing about SOCKS5 this suite reached
/// only through `0x00`.
///
/// Three codes rather than one, because a client that reported every
/// refusal as the same value would pass a single-row test: `0x05`
/// connection refused, `0x04` host unreachable, `0x02` not allowed by
/// ruleset.
#[tokio::test(flavor = "multi_thread")]
async fn a_socks5_upstream_failure_arrives_with_its_own_reply_code() {
    for rep in [0x05u8, 0x04, 0x02] {
        let client = socks5_client(socks5_refusing(Some(rep)));
        let err = tokio::time::timeout(BOUND, get(&client, "http://example.invalid/x"))
            .await
            .expect("must not hang")
            .expect_err("a refused CONNECT is not a response");
        assert_eq!(*err.kind(), hclient_core::ErrorKind::Connect);
        let refused = StdError::source(&err)
            .and_then(|s| s.downcast_ref::<hclient_native::Socks5Refused>())
            .unwrap_or_else(|| panic!("REP={rep:#04x} must be readable off the error: {err:?}"));
        assert_eq!(refused.rep, rep);
    }
}

/// **Refusing the greeting is a different error from refusing the
/// CONNECT**, and the pair is the assertion: `0xFF` never reaches the
/// request stage at all, so reporting it as a `REP` would name a byte the
/// proxy never sent.
#[tokio::test(flavor = "multi_thread")]
async fn a_socks5_proxy_that_refuses_every_method_says_so_and_not_a_reply_code() {
    let client = socks5_client(socks5_refusing(None));
    let err = tokio::time::timeout(BOUND, get(&client, "http://example.invalid/x"))
        .await
        .expect("must not hang")
        .expect_err("no acceptable methods is a refusal");
    assert_eq!(*err.kind(), hclient_core::ErrorKind::Connect);
    let source = StdError::source(&err).expect("a source");
    assert!(
        source
            .downcast_ref::<hclient_native::Socks5HandshakeError>()
            .is_some_and(|e| matches!(
                e,
                hclient_native::Socks5HandshakeError::NoAcceptableMethods
            )),
        "the handshake failed, not the CONNECT: {err:?}"
    );
    assert!(
        source
            .downcast_ref::<hclient_native::Socks5Refused>()
            .is_none(),
        "and it must not be reported as a reply code the proxy never sent"
    );
}

// ── the per-scheme rule ─────────────────────────────────────────────────

/// **Two proxies, one transport, and the scheme decides which sees the
/// request.** The ordinary corporate setup — an `HTTP_PROXY` and an
/// `HTTPS_PROXY` at different hosts — which one `Option` could not hold.
///
/// Asserted from **both proxies' side of the wire**, and both halves are
/// needed: a transport that always used the first would pass the `http`
/// half alone, and one that always used the last would pass the `https`
/// half alone.
///
/// The `https` arm is expected to fail, and that is not a weakness: the
/// fixture answers `CONNECT` with `200` and then speaks no TLS, so the
/// handshake fails **after** the tunnel. What is being asserted is which
/// proxy received the `CONNECT`, which is the routing question.
#[tokio::test]
async fn two_proxies_route_by_scheme_and_each_one_sees_only_its_own() {
    use hclient_native::ProxyScheme;

    let (secure, secure_seen) = http_proxy("200");
    let (plain, plain_seen) = http_proxy("200");

    let transport = Native::new(Tokio, NoTls, IpLiteralOnly)
        .proxy(
            Proxy::new(HttpConnect::new(), secure.ip().to_string(), secure.port())
                .only_for(ProxyScheme::Https),
        )
        .and_proxy(Proxy::new(
            HttpConnect::new(),
            plain.ip().to_string(),
            plain.port(),
        ));
    let client = Client::builder(transport).build().expect("build");

    // `http://` — absolute-form to the second proxy, which answers it.
    let text = tokio::time::timeout(BOUND, async {
        client
            .get("http://198.51.100.7/one")
            .send()
            .await?
            .collect()
            .await
    })
    .await
    .expect("must not hang")
    .expect("the plain proxy answers")
    .text()
    .expect("utf-8");
    assert_eq!(text, "hi");

    let head = plain_seen
        .recv_timeout(BOUND)
        .expect("the plain proxy saw it");
    assert!(
        head.starts_with("GET http://198.51.100.7/one HTTP/1.1\r\n"),
        "absolute-form to the proxy that serves http: {head}"
    );
    assert!(
        secure_seen
            .recv_timeout(Duration::from_millis(300))
            .is_err(),
        "and the https-only proxy saw nothing at all"
    );

    // `https://` — a CONNECT to the first proxy. `NoTls` refuses the
    // scheme before any of this, so the transport is rebuilt with a real
    // one for this half.
    // Any real TLS backend: the handshake is expected to fail, since the
    // fixture answers `CONNECT` and then speaks HTTP. What matters is that
    // the `CONNECT` went to the right proxy, which happens first.
    let transport = Native::new(
        Tokio,
        hclient_tls_rustls::Rustls::with_webpki_roots(),
        IpLiteralOnly,
    )
    .proxy(
        Proxy::new(HttpConnect::new(), secure.ip().to_string(), secure.port())
            .only_for(ProxyScheme::Https),
    )
    .and_proxy(Proxy::new(
        HttpConnect::new(),
        plain.ip().to_string(),
        plain.port(),
    ));
    let client = Client::builder(transport).build().expect("build");
    let _ = tokio::time::timeout(BOUND, client.get("https://198.51.100.7/two").send())
        .await
        .expect("must not hang");

    let head = secure_seen
        .recv_timeout(BOUND)
        .expect("the https-only proxy saw it");
    assert!(
        head.starts_with("CONNECT 198.51.100.7:443 HTTP/1.1\r\n"),
        "a tunnel through the proxy that serves https: {head}"
    );
    assert!(
        plain_seen.recv_timeout(Duration::from_millis(300)).is_err(),
        "and the plain proxy saw nothing this time"
    );
}

/// **The first entry that serves a request wins**, which is a rule about
/// order rather than about specificity — so an unrestricted proxy placed
/// first shadows a narrower one after it, and the shadowing is visible at
/// the call site.
#[tokio::test]
async fn an_unrestricted_proxy_placed_first_shadows_the_one_after_it() {
    use hclient_native::ProxyScheme;

    let (first, first_seen) = http_proxy("200");
    let (second, second_seen) = http_proxy("200");

    let transport = Native::new(Tokio, NoTls, IpLiteralOnly)
        .proxy(Proxy::new(
            HttpConnect::new(),
            first.ip().to_string(),
            first.port(),
        ))
        .and_proxy(
            Proxy::new(HttpConnect::new(), second.ip().to_string(), second.port())
                .only_for(ProxyScheme::Http),
        );
    let client = Client::builder(transport).build().expect("build");
    let _ = tokio::time::timeout(BOUND, client.get("http://198.51.100.7/x").send())
        .await
        .expect("must not hang");

    assert!(first_seen.recv_timeout(BOUND).is_ok());
    assert!(
        second_seen
            .recv_timeout(Duration::from_millis(300))
            .is_err(),
        "the narrower proxy is after the broad one and never gets a turn"
    );
}

/// **A bypass is a property of the proxy that carries it**, so a
/// bypassed host falls through to the next proxy in the list and only
/// goes direct when the list runs out.
///
/// That is a decision, and the alternative — a bypass anywhere meaning
/// direct everywhere, which is `NO_PROXY`'s semantics — is worse *because*
/// the list exists: a host bypassed on an `https`-only proxy would take an
/// `http://` request direct, past an `http` proxy that was never in the
/// running and never mentioned it. With one proxy, which is the
/// overwhelming majority, the two rules coincide exactly. A caller who
/// wants the global rule writes the list on each proxy, which is honest
/// because they wrote it.
///
/// Both halves, because either alone is satisfied by the wrong rule: with
/// a second proxy the request reaches it, and with only the bypassing
/// proxy it reaches nobody.
#[tokio::test]
async fn a_bypass_belongs_to_its_own_proxy_and_falls_through_to_the_next() {
    let (first, first_seen) = http_proxy("200");
    let (second, second_seen) = http_proxy("200");

    let bypassing = |addr: SocketAddr| {
        Proxy::new(HttpConnect::new(), addr.ip().to_string(), addr.port()).bypass(["198.51.100.7"])
    };

    let client = Client::builder(
        Native::new(Tokio, NoTls, IpLiteralOnly)
            .proxy(bypassing(first))
            .and_proxy(Proxy::new(
                HttpConnect::new(),
                second.ip().to_string(),
                second.port(),
            )),
    )
    .build()
    .expect("build");
    let _ = tokio::time::timeout(BOUND, client.get("http://198.51.100.7/x").send()).await;

    assert!(
        first_seen.recv_timeout(Duration::from_millis(300)).is_err(),
        "the host is on this proxy's bypass list"
    );
    assert!(
        second_seen.recv_timeout(BOUND).is_ok(),
        "and the next proxy, which does not bypass it, serves it"
    );

    // The other half: with nothing after it, the same bypass is direct.
    // 198.51.100.7 is TEST-NET-2 and answers nothing, so the attempt
    // fails — which is the assertion, since a proxied attempt would have
    // reached the fixture instead.
    let (only, only_seen) = http_proxy("200");
    let client = Client::builder(Native::new(Tokio, NoTls, IpLiteralOnly).proxy(bypassing(only)))
        .build()
        .expect("build");
    let _ = tokio::time::timeout(
        Duration::from_secs(2),
        client.get("http://198.51.100.7/x").send(),
    )
    .await;
    assert!(
        only_seen.recv_timeout(Duration::from_millis(300)).is_err(),
        "a list that runs out is direct"
    );
}

// ── SOCKS4 / SOCKS4a ────────────────────────────────────────────────────

/// A SOCKS4a proxy that reports the request it decoded and then answers
/// HTTP itself, in `socks5_proxy`'s shape and for its reason: the origin's
/// own behaviour is not what these tests are about.
///
/// `cd` is what it replies with, so a test can drive the refusal path.
fn socks4_proxy(cd: u8) -> (SocketAddr, mpsc::Receiver<(String, String, u16)>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut s) = conn else { break };
            // VN, CD, DSTPORT(2), DSTIP(4) — fixed, then two
            // NUL-terminated strings.
            let mut head = [0u8; 8];
            if s.read_exact(&mut head).is_err() {
                continue;
            }
            let read_cstr = |s: &mut TcpStream| {
                let mut out = Vec::new();
                let mut b = [0u8; 1];
                while s.read_exact(&mut b).is_ok() && b[0] != 0 {
                    out.push(b[0]);
                }
                String::from_utf8_lossy(&out).into_owned()
            };
            let userid = read_cstr(&mut s);
            // Only a SOCKS4a request has a hostname, and `0.0.0.x` with a
            // non-zero last byte is exactly how the wire says so.
            let host = if head[4..8] == [0, 0, 0, 1] {
                read_cstr(&mut s)
            } else {
                String::new()
            };
            let port = u16::from_be_bytes([head[2], head[3]]);
            let _ = tx.send((userid, host, port));

            // VN=0 — zero, not four, which is the detail every
            // implementation gets wrong once.
            let _ = s.write_all(&[0x00, cd, 0, 0, 0, 0, 0, 0]);
            if cd == 90 {
                // Read the request before answering, as `socks5_proxy`
                // does: a fixture that writes a response and drops the
                // socket while the client is still writing gets an RST,
                // and the RST discards the buffered response — which
                // arrives as `ConnectionWentAwayBeforeTheRequest` and
                // looks like a defect in the tunnel.
                let _ = read_head(&mut s);
                let _ = s.write_all(ok_response());
            }
            let _ = s.flush();
        }
    });
    (addr, rx)
}

/// **The whole SOCKS4a exchange**: the userid and the *unresolved host*
/// reach the proxy, and the response comes back through the tunnel.
///
/// The host is the assertion that matters. A connector that resolved
/// locally and sent an address would be leaking exactly the DNS a proxy
/// user is often there to hide, which is the reason a proxy is not a
/// `TcpConnect` decorator — and here it would also be sending
/// SOCKS4 rather than 4a.
#[tokio::test]
async fn a_socks4a_tunnel_carries_the_userid_and_the_unresolved_host() {
    let (proxy, seen) = socks4_proxy(90);
    let transport = Native::new(Tokio, NoTls, IpLiteralOnly).proxy(Proxy::new(
        Socks4::new().userid("alice").expect("no NUL"),
        proxy.ip().to_string(),
        proxy.port(),
    ));
    let client = Client::builder(transport).build().expect("build");

    let text = tokio::time::timeout(BOUND, async {
        client
            .get("http://origin.invalid:8080/x")
            .send()
            .await?
            .collect()
            .await
    })
    .await
    .expect("must not hang")
    .expect("the tunnel carries the exchange")
    .text()
    .expect("utf-8");
    assert_eq!(text, "hi");

    let (userid, host, port) = seen.recv_timeout(BOUND).expect("the proxy decoded it");
    assert_eq!(userid, "alice");
    assert_eq!(
        host, "origin.invalid",
        "by name — a resolved address here would be a DNS leak and a \
         SOCKS4 request where 4a was meant"
    );
    assert_eq!(port, 8080);
}

/// A refusal is a typed `Connect` error naming the `CD`, never a response.
/// `91` is *rejected or failed*, the value a proxy sends when it will not
/// or cannot reach the origin.
#[tokio::test]
async fn a_socks4_refusal_is_a_typed_connect_error() {
    use hclient_native::Socks4Refused;

    let (proxy, _seen) = socks4_proxy(91);
    let transport = Native::new(Tokio, NoTls, IpLiteralOnly).proxy(Proxy::new(
        Socks4::new(),
        proxy.ip().to_string(),
        proxy.port(),
    ));
    let client = Client::builder(transport).build().expect("build");

    let err = tokio::time::timeout(BOUND, client.get("http://origin.invalid/x").send())
        .await
        .expect("must not hang")
        .expect_err("the proxy refused");
    assert_eq!(*err.kind(), hclient_core::ErrorKind::Connect, "{err:?}");
    let refused = StdError::source(&err)
        .and_then(|s| s.downcast_ref::<Socks4Refused>())
        .unwrap_or_else(|| panic!("the typed refusal carrying CD: {err:?}"));
    assert_eq!(refused.cd, 91);
}

/// **The reply's version byte is zero, not four**, and a proxy that sends
/// `4` there is refused by name rather than being read as a grant.
#[tokio::test]
async fn a_reply_version_of_four_is_refused_rather_than_read_as_a_grant() {
    use hclient_native::Socks4HandshakeError;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut s) = conn else { break };
            let mut sink = [0u8; 64];
            let _ = s.read(&mut sink);
            // VN=4 where the protocol says 0, and CD=90 — a grant in every
            // other respect, so only the version check can catch it.
            let _ = s.write_all(&[0x04, 90, 0, 0, 0, 0, 0, 0]);
            let _ = s.write_all(ok_response());
            let _ = s.flush();
            // Held open: the client must fail on the version byte rather
            // than on a closed socket, or the assertion below would pass
            // for the wrong reason.
            std::thread::sleep(Duration::from_millis(500));
        }
    });

    let transport = Native::new(Tokio, NoTls, IpLiteralOnly).proxy(Proxy::new(
        Socks4::new(),
        addr.ip().to_string(),
        addr.port(),
    ));
    let client = Client::builder(transport).build().expect("build");
    let err = tokio::time::timeout(BOUND, client.get("http://origin.invalid/x").send())
        .await
        .expect("must not hang")
        .expect_err("VN must be 0");
    assert_eq!(
        StdError::source(&err).and_then(|s| s.downcast_ref::<Socks4HandshakeError>()),
        Some(&Socks4HandshakeError::BadReplyVersion(4)),
        "{err:?}"
    );
}

/// A `NUL` in the userid is refused at configuration rather than
/// truncating the field on the wire — where the bytes after it would be
/// read as the hostname.
#[test]
fn a_nul_in_the_userid_is_refused_where_it_is_written() {
    use hclient_native::Socks4HandshakeError;
    assert_eq!(
        Socks4::new().userid("al\0ice").map(|_| ()),
        Err(Socks4HandshakeError::NulInUserid)
    );
    assert!(Socks4::new().userid("alice").is_ok());
}
