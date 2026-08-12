//! The staged connect, watched from the server's side of the wire.
//!
//! `docs/connect-only-seam.md` decided the shape — `connect` -> an opaque
//! handle -> `exchange` — and three of its claims are only claims until a
//! peer says otherwise. All three are counted by the **server**:
//!
//! - a staged connect opens a connection and sends **no request** (the
//!   whole point: a caller can find out whether an origin is reachable
//!   without the origin hearing the request);
//! - a handle nobody spends leaves a **warm** connection rather than a
//!   closed socket, with `without_pool()` as the control that attributes
//!   the reuse to the pool and not to the fixture;
//! - a staged connect **finds** a pooled connection rather than always
//!   dialling, which is §7's *"it must be allowed to answer: I already had
//!   one"*.
//!
//! The counters are the server's own, in the shape `tests/pool.rs`
//! established: a client-side variable incremented in the wrong place could
//! produce any of these numbers, and a socket the server accepted could
//! not.
#![cfg(not(target_family = "wasm"))]

use http_body_util::BodyExt;
use http_ng_core::unversioned::Transport;
use http_ng_core::{ErrorKind, RequestBody};
use http_ng_dns_system::SystemDns;
use http_ng_native::{Native, Prepared, StagedConnect};
use http_ng_rt_tokio::Tokio;
use http_ng_tls::NoTls;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// Never an assertion — it turns a mutation that hangs into a red test
/// rather than an eternal one.
const BOUND: Duration = Duration::from_secs(30);

/// What the fixture reports, and none of it is written by the client.
#[derive(Clone)]
struct Counters {
    /// TCP connections the server accepted.
    accepted: Arc<AtomicUsize>,
    /// Complete request heads the server read.
    heads: Arc<AtomicUsize>,
}

/// A server that counts the connections it accepts and the request heads it
/// reads, and answers each head on the same connection.
fn counting_server() -> (SocketAddr, Counters) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let counters = Counters {
        accepted: Arc::new(AtomicUsize::new(0)),
        heads: Arc::new(AtomicUsize::new(0)),
    };
    let c = counters.clone();
    std::thread::spawn(move || {
        for sock in listener.incoming() {
            let Ok(sock) = sock else { continue };
            c.accepted.fetch_add(1, Ordering::SeqCst);
            let heads = c.heads.clone();
            std::thread::spawn(move || serve(sock, heads));
        }
    });
    (addr, counters)
}

fn serve(mut sock: std::net::TcpStream, heads: Arc<AtomicUsize>) {
    sock.set_read_timeout(Some(BOUND)).expect("read timeout");
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let head_end = loop {
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
            match sock.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        };
        buf.drain(..head_end);
        // Counted before the answer is written, so a test that has seen a
        // response has certainly seen this increment.
        heads.fetch_add(1, Ordering::SeqCst);
        if sock
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
            .is_err()
        {
            return;
        }
    }
}

/// Waits for a counter to reach `n`, bounded. The bound is a failure rather
/// than a `return`: a test whose premise never arrived has not passed.
async fn reaches(counter: &AtomicUsize, n: usize, what: &str) {
    tokio::time::timeout(BOUND, async {
        while counter.load(Ordering::SeqCst) < n {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the server never reached {n} {what}"));
}

/// `NoTls` and `http://`, so the exchange under test is HTTP/1.1 over a
/// plain socket and nothing here depends on a TLS backend or on whether
/// the `http2` feature happens to be on in this build.
fn native() -> Native<Tokio, NoTls, SystemDns<Tokio>> {
    Native::new(Tokio, NoTls, SystemDns::new(Tokio))
}

fn get(addr: SocketAddr) -> Prepared {
    Prepared::new(
        http::Request::builder()
            .uri(format!("http://{addr}/hello"))
            .body(RequestBody::Empty)
            .expect("a well-formed request"),
    )
}

async fn body_of<B>(resp: http::Response<B>) -> String
where
    B: http_body::Body<Data = bytes::Bytes, Error = http_ng_core::Error>,
{
    assert_eq!(resp.status(), 200);
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("a complete body")
        .to_bytes();
    String::from_utf8(bytes.to_vec()).expect("utf-8")
}

// --- the deliverable ----------------------------------------------------

/// **The whole point.** A staged connect reaches the origin and the origin
/// hears no request; the request arrives only when the handle is spent.
///
/// The absence is asserted after waiting for the accept, so the server
/// thread is known to have run past `accept` — what it cannot rule out is
/// a write that the server has not yet read, which is why the second half
/// matters: the same counter that stays at zero across the connect reaches
/// one across the exchange, so it is a counter that works.
#[tokio::test]
async fn a_staged_connect_opens_a_connection_and_sends_no_request() {
    let (addr, c) = counting_server();
    let t = native();

    let staged = t.connect(get(addr)).await.expect("the server is listening");
    reaches(&c.accepted, 1, "accepted connections").await;
    assert_eq!(
        c.heads.load(Ordering::SeqCst),
        0,
        "a staged connect must not put a request on the wire"
    );

    let resp = t.exchange(staged).await.expect("the exchange succeeds");
    assert_eq!(body_of(resp).await, "ok");
    assert_eq!(
        (
            c.accepted.load(Ordering::SeqCst),
            c.heads.load(Ordering::SeqCst)
        ),
        (1, 1),
        "one connection, one request, and the request came from the exchange"
    );
}

/// §7's *"it must be allowed to answer: I already had one"*. A staged
/// connect at an origin the pool is already serving costs no connection.
#[tokio::test]
async fn a_staged_connect_finds_a_pooled_connection_rather_than_dialling() {
    let (addr, c) = counting_server();
    let t = native();

    // An ordinary exchange first, to leave something in the pool.
    let resp = t
        .execute(get(addr).into_request())
        .await
        .expect("request 1");
    assert_eq!(body_of(resp).await, "ok");

    let staged = t.connect(get(addr)).await.expect("request 2 connects");
    let resp = t.exchange(staged).await.expect("request 2 exchanges");
    assert_eq!(body_of(resp).await, "ok");

    assert_eq!(
        (
            c.accepted.load(Ordering::SeqCst),
            c.heads.load(Ordering::SeqCst)
        ),
        (1, 2),
        "the staged connect must have found the pooled connection, not dialled a second one"
    );
}

/// **The check-in the handle carries is the one the exchange uses.** A
/// staged exchange returns its connection to the pool exactly as
/// `Transport::execute` does, so the request after it finds it there.
///
/// The mutation this exists to fail is a handle minted with no way home:
/// [`a_staged_connect_finds_a_pooled_connection_rather_than_dialling`]
/// above passes with one, because it never asks what happened to the
/// connection *afterwards*. Both arms are here rather than one, because
/// the check-in is minted at two sites — the pooled branch and the fresh
/// one — and a test covering one of them says nothing about the other.
#[tokio::test]
async fn a_staged_exchange_returns_its_connection_to_the_pool() {
    for warm in [false, true] {
        let (addr, c) = counting_server();
        let t = native();
        let mut expected_heads = 0;

        if warm {
            // The staged connect then comes off the pooled branch.
            let resp = t.execute(get(addr).into_request()).await.expect("warming");
            assert_eq!(body_of(resp).await, "ok");
            expected_heads += 1;
        }

        let staged = t.connect(get(addr)).await.expect("connects");
        let resp = t.exchange(staged).await.expect("exchanges");
        assert_eq!(body_of(resp).await, "ok");
        expected_heads += 1;

        let resp = t.execute(get(addr).into_request()).await.expect("after");
        assert_eq!(body_of(resp).await, "ok");
        expected_heads += 1;

        assert_eq!(
            (
                c.accepted.load(Ordering::SeqCst),
                c.heads.load(Ordering::SeqCst)
            ),
            (1, expected_heads),
            "warm={warm}: the request after a staged exchange must find that \
             exchange's connection in the pool"
        );
    }
}

/// `docs/connect-only-seam.md` §9 left open what happens to a connection
/// whose caller decided not to use it. This is the answer: it goes back to
/// the pool, so the next request finds it.
#[tokio::test]
async fn a_handle_nobody_spends_leaves_a_warm_connection() {
    let (addr, c) = counting_server();
    let t = native();

    let staged = t.connect(get(addr)).await.expect("the server is listening");
    reaches(&c.accepted, 1, "accepted connections").await;
    drop(staged);

    let resp = t.execute(get(addr).into_request()).await.expect("request");
    assert_eq!(body_of(resp).await, "ok");
    assert_eq!(
        (
            c.accepted.load(Ordering::SeqCst),
            c.heads.load(Ordering::SeqCst)
        ),
        (1, 1),
        "the dropped handle's connection must be the one the request used"
    );
}

/// The control for the test above, and the pair is what attributes the
/// single connection to the pool rather than to anything about the
/// fixture: with reuse off there is no check-in, and the drop closes the
/// socket.
#[tokio::test]
async fn without_a_pool_a_dropped_handle_closes_its_connection() {
    let (addr, c) = counting_server();
    let t = native().without_pool();

    let staged = t.connect(get(addr)).await.expect("the server is listening");
    reaches(&c.accepted, 1, "accepted connections").await;
    drop(staged);

    let resp = t.execute(get(addr).into_request()).await.expect("request");
    assert_eq!(body_of(resp).await, "ok");
    assert_eq!(
        c.accepted.load(Ordering::SeqCst),
        2,
        "without a pool the dropped handle has nowhere to go and the request dials again"
    );
}

/// A failed connect hands the request back, untouched — the property the
/// whole seam exists for, since a caller that is going to send this request
/// somewhere else has to still have it.
#[tokio::test]
async fn a_refused_connect_hands_the_request_back() {
    // A port nothing is listening on: bound, then dropped, so the kernel
    // answers RST rather than leaving the connect to time out.
    let addr = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        l.local_addr().expect("local_addr")
    };
    let t = native();

    let refused = t
        .connect(get(addr))
        .await
        .map(|_| ())
        .expect_err("nothing is listening there");
    assert_eq!(*refused.error().kind(), ErrorKind::Connect);

    let (_, request) = refused.into_parts();
    assert_eq!(request.uri().to_string(), format!("http://{addr}/hello"));
    assert_eq!(request.method(), http::Method::GET);
}

/// The handle is spendable exactly once, which is what makes the pairing of
/// a connection with its request a thing the type system holds rather than
/// a thing a caller remembers: `exchange` consumes it, so there is no
/// second request to give the same connection to.
///
/// A compile-time property, asserted here as a comment with the code that
/// would not compile beside it, because a test cannot observe a move:
///
/// ```compile_fail
/// # use http_ng_native::StagedConnect;
/// # async fn f<T: StagedConnect>(t: &T, s: T::Staged) {
/// let _ = t.exchange(s).await;
/// let _ = t.exchange(s).await; // `s` moved
/// # }
/// ```
#[tokio::test]
async fn the_response_is_the_one_the_ordinary_exchange_would_have_returned() {
    let (addr, _) = counting_server();
    let t = native();

    let staged = t.connect(get(addr)).await.expect("connects");
    let staged_resp = t.exchange(staged).await.expect("exchanges");
    let direct = t.execute(get(addr).into_request()).await.expect("executes");

    assert_eq!(staged_resp.status(), direct.status());
    assert_eq!(staged_resp.version(), direct.version());
    assert_eq!(body_of(staged_resp).await, body_of(direct).await);
}
