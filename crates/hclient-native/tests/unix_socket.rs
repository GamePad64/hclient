//! `Native::unix_socket`, against a real `AF_UNIX` server.
//!
//! The capability curl has as `--unix-socket` and nobody in Rust does:
//! reaching a local daemon that speaks HTTP over a socket rather than a
//! port. The URI still carries a host, because HTTP needs one for `Host:`
//! and for the pool — `http://localhost/x` over `/run/thing.sock` is the
//! shape.
#![cfg(unix)]

use hclient_core::RequestBody;
use hclient_core::unversioned::Transport;
use hclient_dns::IpLiteralOnly;
use hclient_native::Native;
use hclient_rt_tokio::Tokio;
use hclient_tls::NoTls;
use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::Duration;

const BOUND: Duration = Duration::from_secs(5);

/// A `SocketAddr`-less server: reads a request head, reports it, answers.
///
/// The path lives in a fresh temporary directory rather than beside the
/// test binary, because a stale socket file is `EADDRINUSE` and a test
/// that fails on its own leftovers is a test nobody trusts.
fn unix_server() -> (std::path::PathBuf, mpsc::Receiver<String>) {
    let dir = std::env::temp_dir().join(format!(
        "hclient-uds-{}-{}",
        std::process::id(),
        // Two tests in one binary must not collide, and the address of a
        // fresh allocation is the cheapest distinct number available
        // without a clock or a counter.
        &format!("{:p}", Box::into_raw(Box::new(0u8)))[2..]
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("s.sock");
    let l = std::os::unix::net::UnixListener::bind(&path).expect("bind");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { break };
            let mut head = Vec::new();
            let mut b = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                match s.read(&mut b) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => head.push(b[0]),
                }
            }
            let _ = tx.send(String::from_utf8_lossy(&head).into_owned());
            let _ = s.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nlocal",
            );
            let _ = s.flush();
        }
    });
    (path, rx)
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

/// **The whole exchange over a socket**, and the head that reaches the
/// server is an ordinary origin-form request — nothing is tunnelled and
/// nothing is rewritten, which is what separates this from a proxy.
#[test]
fn a_request_goes_over_the_socket_and_the_head_is_ordinary() {
    let (path, seen) = unix_server();
    let t = Native::new(Tokio, NoTls, IpLiteralOnly)
        .unix_socket(&path)
        .expect("tokio on unix supports this");

    let resp = rt()
        .block_on(async {
            tokio::time::timeout(
                BOUND,
                t.execute(
                    http::Request::builder()
                        // A host that resolves to nothing and a port that
                        // is listening on nothing: if any of the resolve →
                        // Happy Eyeballs → connect block still ran, this
                        // could not succeed.
                        .uri("http://not-a-real-host.invalid:9/v1/version")
                        .body(RequestBody::Empty)
                        .expect("request"),
                ),
            )
            .await
            .expect("must not hang")
        })
        .expect("the socket answers");
    assert_eq!(resp.status(), 200);

    let head = seen.recv_timeout(BOUND).expect("the server saw it");
    assert!(
        head.starts_with("GET /v1/version HTTP/1.1\r\n"),
        "origin-form, not absolute-form — this is not a proxy: {head}"
    );
    assert!(
        head.to_ascii_lowercase()
            .contains("host: not-a-real-host.invalid:9"),
        "the URI's authority is still what `Host:` carries: {head}"
    );
}

/// **A proxy and a Unix socket are refused together**, in both orders,
/// because both answer *where does this connection go* and a precedence
/// rule between them would be one nobody could guess.
#[cfg(feature = "proxy")]
#[test]
fn a_proxy_and_a_socket_cannot_both_be_configured() {
    use hclient_native::{HttpConnect, Proxy};

    let (path, _seen) = unix_server();
    let err = Native::new(Tokio, NoTls, IpLiteralOnly)
        .proxy(Proxy::new(HttpConnect::new(), "127.0.0.1", 1))
        .unix_socket(&path)
        .map(|_| ())
        .expect_err("both decide where the connection goes");
    assert_eq!(*err.kind(), hclient_core::ErrorKind::Unsupported, "{err:?}");
    assert!(
        std::error::Error::source(&err)
            .and_then(|s| s.downcast_ref::<hclient_native::ProxyAndUnixSocket>())
            .is_some(),
        "the typed refusal: {err:?}"
    );

    // The other order panics rather than returning, and that asymmetry is
    // documented on `proxy` — it changes `P`, so it cannot hand back a
    // `Result` without costing every caller who never touches a socket a
    // `?`. Asserted so the panic is a decision rather than a surprise.
    let (path2, _s2) = unix_server();
    let hit = std::panic::catch_unwind(move || {
        let _ = Native::new(Tokio, NoTls, IpLiteralOnly)
            .unix_socket(&path2)
            .expect("supported")
            .proxy(Proxy::new(HttpConnect::new(), "127.0.0.1", 1));
    });
    assert!(
        hit.is_err(),
        "`.unix_socket(..).proxy(..)` must not be silent"
    );
}

/// **Two transports over two sockets, one authority, and each socket sees
/// its own request.**
///
/// The name used to say *never one pooled connection*, and that was more
/// than this asserts: each `Native` has its own pool, so two of them
/// cannot share a connection whatever the key says. The path **is** in the
/// pool key, and that correctness is unobservable for the same structural
/// reason the proxy's is — `unix_socket` is constant within one `Native`,
/// so two requests through one transport can never disagree about it.
/// Removing it from the key passes the whole suite; it is written
/// correctly anyway for the moment a pool is shared between transports,
/// which is why the `proxy` field is in `PoolKey` at all.
#[test]
fn each_socket_sees_its_own_requests() {
    let (a, a_seen) = unix_server();
    let (b, b_seen) = unix_server();
    let uri = "http://same-authority.invalid/x";

    for (path, seen) in [(&a, &a_seen), (&b, &b_seen)] {
        let t = Native::new(Tokio, NoTls, IpLiteralOnly)
            .unix_socket(path)
            .expect("supported");
        let resp = rt()
            .block_on(async {
                tokio::time::timeout(
                    BOUND,
                    t.execute(
                        http::Request::builder()
                            .uri(uri)
                            .body(RequestBody::Empty)
                            .expect("request"),
                    ),
                )
                .await
                .expect("must not hang")
            })
            .expect("answers");
        assert_eq!(resp.status(), 200);
        assert!(
            seen.recv_timeout(BOUND).is_ok(),
            "each socket saw its own request"
        );
    }
}

/// **The `Connected` event reports no address**, which is the honest
/// absence rather than a fabricated `0.0.0.0:0` — and it is emitted at
/// all, because a `Closed` without it would announce the end of a
/// connection whose beginning was never announced.
#[test]
fn the_connected_event_carries_no_address_and_is_still_emitted() {
    use hclient_core::unversioned::{Event, Hooks};
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Rec(Arc<Mutex<Vec<Option<std::net::SocketAddr>>>>);
    impl Hooks for Rec {
        fn on(&self, event: Event<'_>) {
            if let Event::Connected(c) = event {
                self.0.lock().expect("lock").push(c.remote);
            }
        }
    }

    let (path, _seen) = unix_server();
    let rec = Rec::default();
    let t = Native::new(Tokio, NoTls, IpLiteralOnly)
        .hooks(rec.clone())
        .unix_socket(&path)
        .expect("supported");
    let _ = rt().block_on(async {
        tokio::time::timeout(
            BOUND,
            t.execute(
                http::Request::builder()
                    .uri("http://x.invalid/x")
                    .body(RequestBody::Empty)
                    .expect("request"),
            ),
        )
        .await
        .expect("must not hang")
    });

    let seen = rec.0.lock().expect("lock").clone();
    assert_eq!(seen.len(), 1, "exactly one Connected: {seen:?}");
    assert_eq!(
        seen[0], None,
        "there is no IP address, and `None` says so where `0.0.0.0:0` \
         would be a wrong answer a hook could not tell from a real one"
    );
}
