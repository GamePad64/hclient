//! What one egress filter makes possible that the old proxy parameter did
//! not, watched from the fixtures' side of the wire: rules of different
//! protocols on one transport, a SOCKS5 proxy listening on a Unix socket,
//! a tunnel reused from the pool, and the caller's connect bound holding
//! through a proxy that never answers.
//!
//! Every fixture here is loopback and every proxy is addressed by an IP
//! literal, so `IpLiteralOnly` suffices and nothing resolves an origin's
//! name locally — which is itself part of what a proxy promises.
#![cfg(all(feature = "proxy", unix, not(target_family = "wasm")))]

use hclient_core::body::RequestBody;
use hclient_core::error::ErrorKind;
use hclient_core::transport::Transport;
use hclient_dns::IpLiteralOnly;
use hclient_native::Native;
use hclient_native::proxy::{HttpConnect, Proxy, Socks5};
use hclient_rt_tokio::Tokio;
use hclient_tls::NoTls;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

/// Never an assertion — it turns a mutation that hangs into a red test.
const BOUND: Duration = Duration::from_secs(10);

/// Reads a request head off `s`, up to and including the blank line.
fn read_head<S: Read>(s: &mut S) -> String {
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

/// A keep-alive answer: two bytes, and the connection stays open.
const KEEP_ALIVE: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi";

/// Serve RFC 1928 §3–§4 on `s`, report the name it was asked for, then
/// answer every request that follows on the same connection — so a pooled
/// tunnel can be seen being reused.
fn serve_socks5<S: Read + Write>(mut s: S, asked: &mpsc::Sender<String>) {
    let mut hdr = [0u8; 2];
    if s.read_exact(&mut hdr).is_err() {
        return;
    }
    let mut methods = vec![0u8; usize::from(hdr[1])];
    let _ = s.read_exact(&mut methods);
    let _ = s.write_all(&[0x05, 0x00]);
    let mut req = [0u8; 4];
    if s.read_exact(&mut req).is_err() {
        return;
    }
    let mut len = [0u8; 1];
    let _ = s.read_exact(&mut len);
    let mut host = vec![0u8; usize::from(len[0])];
    let _ = s.read_exact(&mut host);
    let mut port = [0u8; 2];
    let _ = s.read_exact(&mut port);
    let _ = asked.send(String::from_utf8_lossy(&host).into_owned());
    let _ = s.write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]);
    loop {
        let head = read_head(&mut s);
        if head.is_empty() {
            return;
        }
        if s.write_all(KEEP_ALIVE).and_then(|()| s.flush()).is_err() {
            return;
        }
    }
}

/// A SOCKS5 proxy on TCP that answers HTTP itself; counts connections.
fn socks5_tcp() -> (SocketAddr, mpsc::Receiver<String>, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = mpsc::channel();
    let accepted = Arc::new(AtomicUsize::new(0));
    let count = accepted.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(s) = conn else { break };
            count.fetch_add(1, Ordering::SeqCst);
            let tx = tx.clone();
            std::thread::spawn(move || serve_socks5(s, &tx));
        }
    });
    (addr, rx, accepted)
}

/// An HTTP proxy answering absolute-form requests itself; reports each
/// request line.
fn http_proxy() -> (SocketAddr, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut s) = conn else { break };
            let head = read_head(&mut s);
            let _ = tx.send(head.lines().next().unwrap_or_default().to_owned());
            let _ =
                s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi");
            let _ = s.flush();
        }
    });
    (addr, rx)
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime")
}

fn get(
    t: &Native<Tokio, NoTls, IpLiteralOnly>,
    rt: &tokio::runtime::Runtime,
    uri: &str,
) -> Result<u16, hclient_core::error::Error> {
    get_with(t, rt, uri, None)
}

fn get_with(
    t: &Native<Tokio, NoTls, IpLiteralOnly>,
    rt: &tokio::runtime::Runtime,
    uri: &str,
    connect: Option<Duration>,
) -> Result<u16, hclient_core::error::Error> {
    let mut req = http::Request::builder()
        .uri(uri)
        .body(RequestBody::Empty)
        .expect("request");
    if let Some(c) = connect {
        req.extensions_mut()
            .insert(hclient_core::req::Timeouts::new().with_connect(c));
    }
    rt.block_on(async {
        let resp = tokio::time::timeout(BOUND, t.execute(req))
            .await
            .expect("must not hang")?;
        let status = resp.status().as_u16();
        // Drained, so a keep-alive connection goes back to the pool.
        let _ = http_body_util::BodyExt::collect(resp.into_body()).await;
        Ok(status)
    })
}

/// **An HTTP proxy and a SOCKS5 proxy on one transport**, which the old
/// single proxy type parameter could not express: the first rule serves
/// every host but the one it bypasses, and that one falls through to the
/// second rule. Each fixture's log is the witness.
#[test]
fn http_and_socks5_rules_serve_one_transport() {
    let (http_addr, http_seen) = http_proxy();
    let (socks_addr, socks_seen, _) = socks5_tcp();
    let t = Native::new(Tokio, NoTls, IpLiteralOnly)
        .proxy(
            Proxy::new(
                HttpConnect::new(),
                http_addr.ip().to_string(),
                http_addr.port(),
            )
            .bypass(["b.test"]),
        )
        .and_proxy(Proxy::new(
            Socks5::new(),
            socks_addr.ip().to_string(),
            socks_addr.port(),
        ));
    let rt = rt();

    assert_eq!(
        get(&t, &rt, "http://a.test/one").expect("via the HTTP proxy"),
        200
    );
    assert_eq!(
        http_seen
            .recv_timeout(BOUND)
            .expect("the HTTP proxy saw it"),
        "GET http://a.test/one HTTP/1.1",
        "absolute-form, to the HTTP proxy"
    );

    assert_eq!(get(&t, &rt, "http://b.test/two").expect("via SOCKS5"), 200);
    assert_eq!(
        socks_seen
            .recv_timeout(BOUND)
            .expect("the SOCKS5 proxy saw it"),
        "b.test",
        "the origin's name, carried to the proxy rather than resolved here"
    );
    assert!(
        http_seen.try_recv().is_err(),
        "the bypassed host never reached the HTTP proxy"
    );
}

/// **A SOCKS5 proxy listening on a Unix socket** — Tor's
/// `SocksPort unix:/path` — reached through `proxy_over_ipc`. The old
/// transport could not say this: a proxy and a socket were exclusive.
#[test]
fn socks5_on_a_unix_socket_reaches_the_origin() {
    let dir = std::env::temp_dir().join(format!(
        "hc-eg-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("s");
    let _ = std::fs::remove_file(&path);
    let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
    let (tx, asked) = mpsc::channel();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(s) = conn else { break };
            let tx = tx.clone();
            std::thread::spawn(move || serve_socks5(s, &tx));
        }
    });

    let t = Native::new(Tokio, NoTls, IpLiteralOnly)
        .proxy_over_ipc(Proxy::over_ipc(
            Socks5::new(),
            hclient_rt::IpcAddr::unix(&path),
        ))
        .expect("tokio on unix opens same-machine connections");
    let rt = rt();

    assert_eq!(
        get(&t, &rt, "http://over-a-socket.test/").expect("through the socket"),
        200
    );
    assert_eq!(
        asked.recv_timeout(BOUND).expect("asked"),
        "over-a-socket.test"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// **A tunnel is pooled and reused through the same proxy**: two requests
/// to one origin through one SOCKS5 rule open one connection to the proxy.
///
/// The other half — two *different* proxies never sharing a connection —
/// is not observable inside one transport, because the route is a pure
/// function of the scheme, host and port the pool key already carries; it
/// is pinned where the key is made, in `hclient-proxy`'s `Rules` tests.
#[test]
fn a_tunnel_is_reused_through_the_same_proxy() {
    let (socks_addr, _asked, accepted) = socks5_tcp();
    let t = Native::new(Tokio, NoTls, IpLiteralOnly).proxy(Proxy::new(
        Socks5::new(),
        socks_addr.ip().to_string(),
        socks_addr.port(),
    ));
    let rt = rt();

    assert_eq!(get(&t, &rt, "http://pooled.test/1").expect("first"), 200);
    assert_eq!(get(&t, &rt, "http://pooled.test/2").expect("second"), 200);
    assert_eq!(accepted.load(Ordering::SeqCst), 1, "one tunnel, reused");
}

/// **The caller's connect bound holds through a proxy that never
/// answers.** The proxy accepts and then says nothing, so the SOCKS5
/// greeting waits for ever; the request must end at the caller's bound as
/// a timeout, and the hang guard around it must not be what ends it.
#[test]
fn a_black_holed_proxy_times_out_at_the_callers_bound() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        let mut held = Vec::new();
        // Held open and never answered.
        for s in listener.incoming().flatten() {
            held.push(s);
        }
    });
    let t = Native::new(Tokio, NoTls, IpLiteralOnly).proxy(Proxy::new(
        Socks5::new(),
        addr.ip().to_string(),
        addr.port(),
    ));
    let rt = rt();

    let began = std::time::Instant::now();
    let err = get_with(
        &t,
        &rt,
        "http://silent.test/",
        Some(Duration::from_millis(300)),
    )
    .expect_err("the proxy never answers");
    assert!(
        matches!(
            *err.kind(),
            ErrorKind::Timeout(hclient_core::error::Phase::Connect)
        ),
        "a connect timeout: {err:?}"
    );
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "ended by the caller's bound"
    );
}
