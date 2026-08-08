//! HTTP/2 (v0.2 W3), observed from outside the client.
//!
//! # The observer is a server that only speaks HTTP/2
//!
//! `cargo build --features http2` proves nothing at all, and neither does
//! a client-side variable saying "h2". Every test below therefore talks to
//! a **real `h2::server`** on a real socket: it performs the HTTP/2
//! connection preface, exchanges `SETTINGS`, decodes HPACK and reports the
//! pseudo-headers it decoded. A client that spoke HTTP/1.1 into it would
//! not get a slow answer or a degraded one — it would get no answer at
//! all, because the first bytes it sent would not be `PRI * HTTP/2.0`.
//!
//! On top of that the client's own `Response::version()` is asserted,
//! which is `http::Version::HTTP_2` only because the `h2` crate put it
//! there while decoding the response headers. The two together mean the
//! protocol was negotiated and spoken, not merely compiled in.
//!
//! # Why the TLS backend here is a stub, and what that does not weaken
//!
//! ALPN is the only way this transport reaches HTTP/2, and ALPN is
//! reported by the `TlsConnect` backend. The stub below performs no
//! encryption and hands the stream straight through — so the bytes on the
//! wire really are the HTTP/2 the fixture server reads — while reporting a
//! negotiated protocol of the test's choosing and recording the list it
//! was *offered*. That is exactly the input the transport's decision is
//! made from, so it is exactly the input a test of that decision should
//! control. The same technique, for the same reason, as `tests/pool.rs`'s
//! `alpn_guard::ReportsAlpn`.
//!
//! What it means is that these tests pin the transport's behaviour given a
//! negotiated ALPN, not rustls's ability to negotiate one. That half is
//! `http-ng-tls-rustls`'s own, where it belongs.
#![cfg(all(feature = "http2", not(target_family = "wasm")))]

use bytes::Bytes;
use http_ng::Client;
use http_ng_dns_system::SystemDns;
use http_ng_native::Native;
use http_ng_rt_tokio::Tokio;
use http_ng_tls::{TlsConfigId, TlsConnect, TlsInfo, TlsRequest};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Ceiling for anything that must not hang.
const BOUND: Duration = Duration::from_secs(30);

/// One chunk of `/big`'s response, and how many of them. Four times the
/// default HTTP/2 initial window (65535 bytes), so the exchange only
/// finishes if receive capacity is released as the body is read.
const BIG_CHUNK: usize = 64 * 1024;
const BIG_CHUNKS: usize = 4;

/// A request as the HTTP/2 server decoded it — pseudo-headers included,
/// because `:authority` and `:path` are the two things an HTTP/1 request
/// does not have and are therefore the proof that this went out as h2.
#[derive(Debug, Clone)]
struct Seen {
    method: http::Method,
    path: String,
    authority: Option<String>,
    body_len: usize,
}

struct Fixture {
    addr: SocketAddr,
    /// TCP connections accepted. The pool's claims are this number, and
    /// nothing the client says about itself.
    accepted: Arc<AtomicUsize>,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Fixture {
    fn url(&self, path: &str) -> String {
        format!("https://{}{}", self.addr, path)
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

/// An HTTP/2 server, and nothing else: no HTTP/1 fallback, no upgrade
/// path. `/slow` answers after a delay long enough for a test to drop a
/// request that is waiting on it; everything else answers `ok` at once.
fn spawn_h2_server() -> Fixture {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let accepted = Arc::new(AtomicUsize::new(0));
    let seen: Arc<Mutex<Vec<Seen>>> = Arc::new(Mutex::new(Vec::new()));

    let accepted_for_thread = Arc::clone(&accepted);
    let seen_for_thread = Arc::clone(&seen);
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
                accepted_for_thread.fetch_add(1, Ordering::SeqCst);
                let seen = Arc::clone(&seen_for_thread);
                tokio::spawn(async move {
                    let _ = serve(tcp, seen).await;
                });
            }
        });
    });

    Fixture {
        addr,
        accepted,
        seen,
    }
}

/// One connection's worth of HTTP/2.
///
/// The request handler is spawned rather than awaited inline, and that is
/// not tidiness: in `h2`'s server API it is `Connection::accept` that
/// drives the connection's IO, so a handler awaited inside the accept loop
/// would stall the very connection it is reading a body from. The same
/// shape as h2's own server example.
async fn serve(tcp: tokio::net::TcpStream, seen: Arc<Mutex<Vec<Seen>>>) -> Result<(), h2::Error> {
    let mut conn = h2::server::handshake(tcp).await?;
    while let Some(accepted) = conn.accept().await {
        let (req, mut respond) = accepted?;
        let seen = Arc::clone(&seen);
        tokio::spawn(async move {
            let (parts, mut body) = req.into_parts();
            let mut body_len = 0usize;
            while let Some(chunk) = body.data().await {
                let Ok(chunk) = chunk else { return };
                body_len += chunk.len();
                let _ = body.flow_control().release_capacity(chunk.len());
            }
            let path = parts.uri.path().to_owned();
            seen.lock().unwrap().push(Seen {
                method: parts.method.clone(),
                path: path.clone(),
                authority: parts.uri.authority().map(|a| a.to_string()),
                body_len,
            });
            if path == "/slow" {
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
            let response = http::Response::builder().status(200).body(()).unwrap();
            let Ok(mut send) = respond.send_response(response, false) else {
                return;
            };
            if path == "/big" {
                // More than one flow-control window's worth (the default
                // initial window is 65535), in chunks, so a client that
                // never releases receive capacity stops after the first
                // window instead of finishing.
                for _ in 0..BIG_CHUNKS {
                    if send
                        .send_data(Bytes::from(vec![b'y'; BIG_CHUNK]), false)
                        .is_err()
                    {
                        return;
                    }
                }
                let _ = send.send_data(Bytes::new(), true);
                return;
            }
            let _ = send.send_data(Bytes::from_static(b"ok"), true);
        });
    }
    Ok(())
}

/// A `TlsConnect` that encrypts nothing, reports the ALPN a test chooses,
/// and records the ALPN list it was offered.
///
/// Both of the last two are inputs to the decision under test:
/// `reports_alpn` is what decides whether `h2` may be *offered* at all,
/// and the negotiated value is what decides which protocol is then
/// *spoken*.
#[derive(Clone)]
struct FakeTls {
    negotiated: Option<&'static [u8]>,
    reports_alpn: bool,
    offered: Arc<Mutex<Vec<Vec<Vec<u8>>>>>,
    id: TlsConfigId,
}

impl FakeTls {
    fn new(negotiated: Option<&'static [u8]>, reports_alpn: bool) -> Self {
        Self {
            negotiated,
            reports_alpn,
            offered: Arc::new(Mutex::new(Vec::new())),
            id: TlsConfigId::new_unique(),
        }
    }

    /// Reports `h2` and admits it can — the ordinary configuration for a
    /// backend equivalent to `http-ng-tls-rustls`.
    fn negotiating_h2() -> Self {
        Self::new(Some(b"h2"), true)
    }

    fn offered(&self) -> Vec<Vec<Vec<u8>>> {
        self.offered.lock().unwrap().clone()
    }
}

impl TlsConnect for FakeTls {
    type Stream<S>
        = S
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin;

    fn config_id(&self) -> TlsConfigId {
        self.id
    }

    fn reports_alpn(&self) -> bool {
        self.reports_alpn
    }

    async fn connect<S>(
        &self,
        io: S,
        req: TlsRequest<'_>,
    ) -> Result<(S, TlsInfo), http_ng_core::Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin,
    {
        self.offered
            .lock()
            .unwrap()
            .push(req.alpn.iter().map(|p| p.to_vec()).collect());
        Ok((
            io,
            TlsInfo {
                alpn: self.negotiated.map(|p| p.to_vec()),
                ..Default::default()
            },
        ))
    }
}

fn client(tls: FakeTls) -> Client<Native<Tokio, FakeTls, SystemDns<Tokio>>> {
    Client::builder(Native::new(Tokio, tls, SystemDns::new(Tokio)))
        .build()
        .unwrap()
}

/// The acceptance: a live exchange with a server that speaks nothing but
/// HTTP/2, and a client that reports HTTP/2 for it.
///
/// Four independent witnesses, so that no single one carrying the test can
/// be true for the wrong reason: the server accepted the connection and
/// answered (it could not have, over HTTP/1.1); the response body is what
/// the server sent; `Response::version()` is `HTTP_2`, which the `h2`
/// crate set while decoding real HEADERS frames; and the server decoded a
/// `:path` and an `:authority`, which are HTTP/2 pseudo-headers and do not
/// exist in an HTTP/1 request line.
#[tokio::test]
async fn a_live_exchange_with_an_http2_server_is_reported_as_http2() {
    let server = spawn_h2_server();
    let tls = FakeTls::negotiating_h2();
    let client = client(tls.clone());

    let resp = tokio::time::timeout(BOUND, client.get(&server.url("/hello")).send())
        .await
        .expect("must not hang")
        .expect("the request must succeed against an HTTP/2-only server");

    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.version(),
        http::Version::HTTP_2,
        "the response was decoded by the h2 crate, which is what sets this"
    );
    assert_eq!(resp.collect().await.unwrap().text().unwrap(), "ok");

    let seen = server.seen();
    assert_eq!(seen.len(), 1, "exactly one request reached the server");
    assert_eq!(seen[0].method, http::Method::GET);
    assert_eq!(seen[0].path, "/hello");
    assert_eq!(
        seen[0].authority.as_deref(),
        Some(server.addr.to_string().as_str()),
        "`:authority` is built from the absolute URI, which is why the HTTP/1 \
         origin-form rewrite must not run on this path"
    );
    assert_eq!(server.accepted.load(Ordering::SeqCst), 1);
}

/// **The point of the whole `Capabilities` decision in §W3, as a test.**
///
/// With the `http2` feature ON — this file does not compile without it —
/// `capabilities()` still reports the value that holds on the WORST
/// protocol the transport might negotiate. `full_duplex` is the field that
/// matters: over-claiming it costs a caller a deadlock rather than a
/// degradation, and a library cannot know whether some other crate in the
/// build turned h2 on, so the answer has to be one that is safe either
/// way.
///
/// It is also not merely a declaration here: `http2::exchange` writes the
/// whole request body before it awaits the response, so `false` is
/// literally what the code does.
#[tokio::test]
async fn capabilities_report_the_floor_with_the_feature_on() {
    let transport = Native::new(Tokio, FakeTls::negotiating_h2(), SystemDns::new(Tokio));
    let caps = http_ng_core::unversioned::Transport::capabilities(&transport);

    assert!(
        !caps.full_duplex,
        "the floor: h2 permits duplex, HTTP/1.1 does not, and over-claiming \
         this one hangs a caller instead of slowing it down"
    );
    assert!(
        !caps.request_trailers && !caps.response_trailers,
        "the same floor, on the two fields h2 would otherwise let us raise"
    );
    assert!(
        caps.streaming_request_body,
        "unchanged, and true on both protocols — the floor is not blanket \
         conservatism, it is per field"
    );
}

/// `h2` is offered only when the backend can read the answer back.
///
/// Both halves matter and neither is checkable from the other. A backend
/// that reports ALPN gets `h2` first in the offer; a backend that does not
/// (`http-ng-tls-native-tls` is the real one) is offered `http/1.1` alone,
/// because it would answer `None` to "what was negotiated" whatever the
/// server chose — and this transport would then speak HTTP/1 into an
/// HTTP/2 connection.
#[tokio::test]
async fn h2_is_offered_only_to_a_backend_that_reports_alpn() {
    let server = spawn_h2_server();

    let reporting = FakeTls::new(Some(b"h2"), true);
    let _ = tokio::time::timeout(
        BOUND,
        client(reporting.clone()).get(&server.url("/a")).send(),
    )
    .await
    .expect("must not hang");
    assert_eq!(
        reporting.offered(),
        vec![vec![b"h2".to_vec(), b"http/1.1".to_vec()]],
        "a backend that reports ALPN is offered h2, and h2 first"
    );

    // Reports `h2` and admits it cannot tell — the combination that must
    // not produce an h2 exchange. The value it "negotiated" is deliberately
    // the tempting one.
    let silent = FakeTls::new(Some(b"h2"), false);
    let _ = tokio::time::timeout(BOUND, client(silent.clone()).get(&server.url("/b")).send()).await;
    assert_eq!(
        silent.offered(),
        vec![vec![b"http/1.1".to_vec()]],
        "a backend that cannot report ALPN must never be offered h2"
    );
}

/// The other half of the same rule, from the far end: a transport must not
/// speak a protocol it did not propose, whatever a backend claims was
/// negotiated.
///
/// `FakeTls` here reports `h2` while answering `false` to `reports_alpn`,
/// so `h2` never went out on the wire. If `negotiated_protocol` trusted
/// the report alone, the client would send an HTTP/2 preface to a server
/// that was told to expect HTTP/1.1 — here, an HTTP/2-only server, which
/// makes the mistake visible as a failure rather than as a subtle one. The
/// request must fail (the server speaks no HTTP/1.1) **and** the server
/// must have seen no request at all.
#[tokio::test]
async fn a_protocol_that_was_never_offered_is_never_spoken() {
    let server = spawn_h2_server();
    let silent = FakeTls::new(Some(b"h2"), false);

    let result = tokio::time::timeout(BOUND, client(silent).get(&server.url("/c")).send()).await;

    match result {
        Err(_) => panic!("must not hang"),
        Ok(Ok(resp)) => {
            let _ = resp;
            panic!("an HTTP/1.1 request cannot be answered by an HTTP/2-only server")
        }
        Ok(Err(_)) => {}
    }
    assert!(
        server.seen().is_empty(),
        "the server decoded no HTTP/2 request, because none was sent"
    );
}

/// Reuse works on HTTP/2 too, and it is the server that says so.
///
/// Two sequential requests, one accepted connection. The control is the
/// same pair `tests/pool.rs` uses for HTTP/1: `Native::without_pool` must
/// take two.
#[tokio::test]
async fn two_requests_travel_over_one_http2_connection() {
    let server = spawn_h2_server();
    let client = client(FakeTls::negotiating_h2());

    for path in ["/one", "/two"] {
        let resp = tokio::time::timeout(BOUND, client.get(&server.url(path)).send())
            .await
            .expect("must not hang")
            .expect("request must succeed");
        assert_eq!(resp.version(), http::Version::HTTP_2);
        assert_eq!(resp.collect().await.unwrap().text().unwrap(), "ok");
    }

    assert_eq!(
        server.accepted.load(Ordering::SeqCst),
        1,
        "the second request must have reused the first request's connection"
    );
    assert_eq!(server.seen().len(), 2);
}

/// The control for the test above: without a pool, two connections.
///
/// Without it, `two_requests_travel_over_one_http2_connection` would also
/// pass against a server that only ever managed to accept once.
#[tokio::test]
async fn without_a_pool_each_http2_request_gets_its_own_connection() {
    let server = spawn_h2_server();
    let transport =
        Native::new(Tokio, FakeTls::negotiating_h2(), SystemDns::new(Tokio)).without_pool();
    let client = Client::builder(transport).build().unwrap();

    for path in ["/one", "/two"] {
        let resp = tokio::time::timeout(BOUND, client.get(&server.url(path)).send())
            .await
            .expect("must not hang")
            .expect("request must succeed");
        assert_eq!(resp.collect().await.unwrap().text().unwrap(), "ok");
    }

    assert_eq!(server.accepted.load(Ordering::SeqCst), 2);
}

/// **W1 on HTTP/2: dropping one exchange must not take a neighbour with
/// it.**
///
/// Two requests in flight at once, to the same origin. One is waiting on
/// `/slow` and its future is dropped by a timeout; the other must still
/// get its response.
///
/// The assertion on `accepted` is the load-bearing half, and it is the one
/// that would change if the check-out policy did. Two concurrent requests
/// take two connections here — a connection is handed out exclusively,
/// which is why a dropped exchange has no neighbour on its connection to
/// tear down (see `pool.rs`'s "What an h2 connection is checked out for").
/// The day that number becomes 1, this test's *other* assertion is what
/// has to keep holding, and it will not hold for free.
#[tokio::test]
async fn dropping_one_exchange_leaves_a_concurrent_one_alone() {
    let server = spawn_h2_server();
    let client = client(FakeTls::negotiating_h2());

    let cancelled = tokio::time::timeout(
        Duration::from_millis(500),
        client.get(&server.url("/slow")).send(),
    );
    let survivor = client.get(&server.url("/fast")).send();
    let (cancelled, survivor) = tokio::join!(cancelled, survivor);

    assert!(
        cancelled.is_err(),
        "the slow request must still have been waiting when its future was dropped"
    );
    let survivor = survivor.expect("the concurrent request must be unaffected by the cancellation");
    assert_eq!(survivor.status(), 200);
    assert_eq!(survivor.version(), http::Version::HTTP_2);
    assert_eq!(survivor.collect().await.unwrap().text().unwrap(), "ok");

    assert_eq!(
        server.accepted.load(Ordering::SeqCst),
        2,
        "an h2 connection is checked out exclusively, so two concurrent \
         requests are two connections — which is what makes the survivor's \
         connection unreachable from the cancelled request"
    );
}

/// A request body goes out over h2 as DATA frames the server actually
/// counts — the flow-control loop in `http2::poll_pump`, exercised with
/// more bytes than one frame.
#[tokio::test]
async fn a_request_body_reaches_the_server_over_http2() {
    let server = spawn_h2_server();
    let client = client(FakeTls::negotiating_h2());

    let payload = vec![b'x'; 128 * 1024];
    let resp = tokio::time::timeout(
        BOUND,
        client
            .post(&server.url("/upload"))
            .body(http_ng_core::RequestBody::Full(payload.clone().into()))
            .send(),
    )
    .await
    .expect("must not hang")
    .expect("request must succeed");

    assert_eq!(resp.status(), 200);
    assert_eq!(resp.collect().await.unwrap().text().unwrap(), "ok");

    let seen = server.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].method, http::Method::POST);
    assert_eq!(
        seen[0].body_len,
        payload.len(),
        "every byte of the body must have crossed, which needs more than one \
         DATA frame and therefore a working capacity loop"
    );
}

/// A response body larger than one flow-control window arrives whole.
///
/// HTTP/2 receive flow control is not optional bookkeeping: the peer may
/// only send as much as the window allows, and the window only reopens
/// when the reader releases what it has consumed. A body reader that
/// forgot to would deliver the first 64 KiB and then wait forever — which
/// is why this test has a ceiling and fails by name instead of wedging.
#[tokio::test]
async fn a_large_response_body_crosses_the_flow_control_window() {
    let server = spawn_h2_server();
    let client = client(FakeTls::negotiating_h2());

    let resp = tokio::time::timeout(BOUND, client.get(&server.url("/big")).send())
        .await
        .expect("must not hang")
        .expect("request must succeed");
    assert_eq!(resp.version(), http::Version::HTTP_2);

    let body = tokio::time::timeout(BOUND, resp.collect())
        .await
        .expect("the body must not stall once the first window is spent")
        .expect("the body must arrive whole");
    assert_eq!(body.bytes().len(), BIG_CHUNK * BIG_CHUNKS);
}

/// A header HTTP/2 forbids must not turn into a protocol error just
/// because ALPN happened to pick h2 — a choice the caller did not make.
///
/// `Connection: close` is legal on the HTTP/1 path this same client takes
/// against a different origin, so the caller cannot be expected to know
/// it is illegal here. The server would reject the stream if it arrived,
/// so a successful exchange is the assertion.
#[tokio::test]
async fn connection_specific_headers_are_stripped_rather_than_sent() {
    let server = spawn_h2_server();
    let client = client(FakeTls::negotiating_h2());

    let mut headers = http::HeaderMap::new();
    headers.insert(http::header::CONNECTION, "close".parse().unwrap());
    headers.insert(
        http::HeaderName::from_static("keep-alive"),
        "timeout=5".parse().unwrap(),
    );

    let resp = tokio::time::timeout(
        BOUND,
        client.get(&server.url("/strip")).headers(headers).send(),
    )
    .await
    .expect("must not hang")
    .expect("a connection-specific header must be removed, not forwarded");

    assert_eq!(resp.status(), 200);
    assert_eq!(resp.collect().await.unwrap().text().unwrap(), "ok");
}
