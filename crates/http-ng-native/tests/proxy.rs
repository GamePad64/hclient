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
