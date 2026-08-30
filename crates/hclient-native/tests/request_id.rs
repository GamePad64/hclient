//! The identity on the events: which request an event belongs to.
//!
//! # What a request id is *for*, and why nothing here asserts one alone
//!
//! A `ConnectionId` answers a different question and neither replaces the
//! other: one multiplexed connection carries many requests, and one
//! operation may span several connections. So the claim worth pinning is
//! a **join** — every event of one exchange names the same request — and
//! that is what each test below asserts, against an id the test itself
//! put on the wire rather than one it read back out of the events.
//!
//! # The pairs
//!
//! Every positive test has a control differing in one thing, for
//! `tests/hooks.rs`'s reason. *The events name the request* is paired
//! with a transport driven with no `Attempt` in the extensions at all,
//! which must report `UNIDENTIFIED` — because an implementation that
//! stamped some constant on everything would pass the first test and fail
//! the second. And *the client's two requests get two identities* is what
//! says the id is minted per operation rather than per process.
#![cfg(not(target_family = "wasm"))]

use hclient::Client;
use hclient_core::RequestBody;
use hclient_core::unversioned::{Attempt, Event, Hooks, RequestId, Transport};
use hclient_dns_system::SystemDns;
use hclient_native::{Native, Prepared, StagedConnect};
use hclient_rt_tokio::Tokio;
use hclient_tls_rustls::Rustls;
use http_body_util::BodyExt as _;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const BOUND: Duration = Duration::from_secs(30);

// ── the recorder ────────────────────────────────────────────────────────

/// One event, flattened to *which kind* and *which request* — which is
/// the whole of what this file is about. The connection id rides along
/// because one test is about the two disagreeing on purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Line {
    kind: Kind,
    request: RequestId,
    connection: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Connected,
    Reused,
    Head,
    Informational,
    Progress,
}

#[derive(Clone, Default)]
struct Recorder {
    seen: Arc<Mutex<Vec<Line>>>,
}

impl Recorder {
    fn lines(&self) -> Vec<Line> {
        self.seen.lock().expect("recorder").clone()
    }

    /// The kinds seen, in order — asserted on so that a test which
    /// expects five events cannot pass because only one fired.
    fn kinds(&self) -> Vec<Kind> {
        self.lines().into_iter().map(|l| l.kind).collect()
    }

    /// Every distinct request id reported, in first-seen order.
    fn requests(&self) -> Vec<RequestId> {
        let mut out: Vec<RequestId> = Vec::new();
        for l in self.lines() {
            if !out.contains(&l.request) {
                out.push(l.request);
            }
        }
        out
    }

    fn requests_for(&self, kind: Kind) -> Vec<RequestId> {
        self.lines()
            .into_iter()
            .filter(|l| l.kind == kind)
            .map(|l| l.request)
            .collect()
    }
}

impl Hooks for Recorder {
    fn on(&self, event: &Event<'_>) {
        let line = match event {
            Event::Connected(e) => Line {
                kind: Kind::Connected,
                request: e.request,
                connection: e.id.get(),
            },
            Event::Reused(e) => Line {
                kind: Kind::Reused,
                request: e.request,
                connection: e.id.get(),
            },
            Event::Head(e) => Line {
                kind: Kind::Head,
                request: e.request,
                connection: e.id.get(),
            },
            Event::Informational(e) => Line {
                kind: Kind::Informational,
                request: e.request,
                connection: e.id.get(),
            },
            Event::Progress(e) => Line {
                kind: Kind::Progress,
                request: e.request,
                connection: e.id.get(),
            },
            // `Closed` carries no request id, deliberately — a connection
            // ending is not a fact about one request, and on a shared
            // connection it is emphatically not. See `Closed`'s own doc.
            // Recording nothing for it is the same discipline
            // `tests/hooks.rs` states: a `Line` invented here would be a
            // fact this test never observed.
            _ => return,
        };
        self.seen.lock().expect("recorder").push(line);
    }
}

// ── the server ──────────────────────────────────────────────────────────

#[derive(Clone, Copy, Default)]
struct Behaviour {
    /// Send a `103 Early Hints` ahead of the response, so the
    /// `Informational` event has a producer.
    early_hints: bool,
}

fn server(behaviour: Behaviour) -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&accepted);
    std::thread::spawn(move || {
        for sock in listener.incoming() {
            let Ok(sock) = sock else { continue };
            counter.fetch_add(1, Ordering::SeqCst);
            std::thread::spawn(move || serve(sock, behaviour));
        }
    });
    (addr, accepted)
}

fn serve(mut sock: std::net::TcpStream, behaviour: Behaviour) {
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
        if behaviour.early_hints
            && sock
                .write_all(b"HTTP/1.1 103 Early Hints\r\nLink: </s.css>; rel=preload\r\n\r\n")
                .is_err()
        {
            return;
        }
        if sock
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
            .is_err()
        {
            return;
        }
    }
}

// ── the client ──────────────────────────────────────────────────────────

type Watched = Native<Tokio, Rustls, SystemDns<Tokio>, Recorder>;

fn watched(rec: &Recorder) -> Watched {
    Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio)).hooks(rec.clone())
}

/// A request carrying the identity a `Client` would have put there — or,
/// where `attempt` is `None`, the one a transport driven directly gets.
fn get(addr: SocketAddr, attempt: Option<Attempt>) -> http::Request<RequestBody> {
    let mut req = http::Request::builder()
        .uri(format!("http://{addr}/"))
        .body(RequestBody::Empty)
        .expect("request");
    if let Some(a) = attempt {
        req.extensions_mut().insert(a);
    }
    req
}

/// Drives one exchange to the end of the body, because `Progress` is
/// reported from the body and a test that dropped it would assert on four
/// events out of five.
async fn drive(t: &Watched, req: http::Request<RequestBody>) {
    let resp = tokio::time::timeout(BOUND, t.execute(req))
        .await
        .expect("must not hang")
        .expect("request must succeed");
    assert_eq!(resp.status(), 200);
    let body = resp.into_body().collect().await.expect("body").to_bytes();
    assert_eq!(&body[..], b"ok");
}

// ── the id the test put on the wire ─────────────────────────────────────

/// **The claim: every event of one exchange names the request that was
/// sent.** The id is one the test minted and inserted itself, so this is
/// an equality against a known value rather than a check that the events
/// agree with each other — which they would also do if the transport
/// stamped a constant.
///
/// All five payloads that carry an id are exercised in one exchange:
/// `Connected` because the pool is cold, `Informational` because the
/// server sends a `103`, `Head`, `Progress` from the response body, and
/// `Reused` on the second request over the same connection.
#[tokio::test]
async fn every_event_of_one_exchange_names_the_request_that_was_sent() {
    let (addr, accepted) = server(Behaviour { early_hints: true });
    let rec = Recorder::default();
    let t = watched(&rec).watching_1xx();

    let first = Attempt::new(RequestId::next());
    drive(&t, get(addr, Some(first))).await;
    let second = Attempt::new(RequestId::next());
    drive(&t, get(addr, Some(second))).await;

    assert_eq!(
        accepted.load(Ordering::SeqCst),
        1,
        "premise: one connection served both requests, so there is a \
         `Reused` to assert on and one connection id covering two requests"
    );
    let kinds = rec.kinds();
    for expected in [
        Kind::Connected,
        Kind::Reused,
        Kind::Head,
        Kind::Informational,
        Kind::Progress,
    ] {
        assert!(
            kinds.contains(&expected),
            "premise: {expected:?} was reported at all — saw {kinds:?}",
        );
    }

    assert_eq!(
        rec.requests(),
        vec![first.id, second.id],
        "every event names the request it belongs to, and only those two",
    );
    assert_eq!(
        rec.requests_for(Kind::Connected),
        vec![first.id],
        "the connect was paid for by the first request",
    );
    assert_eq!(
        rec.requests_for(Kind::Reused),
        vec![second.id],
        "and the reuse is the second request's — not the one the \
         `Connected` with the same connection id names, which is the \
         whole reason both ids are carried",
    );
    assert_eq!(
        rec.requests_for(Kind::Head),
        vec![first.id, second.id],
        "one head each, in order",
    );
    assert_eq!(
        rec.requests_for(Kind::Informational),
        vec![first.id, second.id],
        "the `103` belongs to the request that provoked it",
    );

    let connections: Vec<u64> = rec.lines().into_iter().map(|l| l.connection).collect();
    assert!(
        connections.windows(2).all(|w| w[0] == w[1]),
        "premise: one connection id throughout, so the request id is the \
         only thing telling the two exchanges apart: {connections:?}",
    );
}

/// The control, and what makes the test above mean anything: the same
/// transport, the same server, one thing different — no `Attempt` in the
/// extensions, which is every transport driven with no `Client` above it.
///
/// `UNIDENTIFIED` is the understating answer and the only honest one:
/// there is no request identity to report, and a fabricated id would
/// attach an exchange to a request nobody made.
#[tokio::test]
async fn a_transport_driven_with_no_client_above_it_reports_unidentified() {
    let (addr, _accepted) = server(Behaviour { early_hints: true });
    let rec = Recorder::default();
    let t = watched(&rec).watching_1xx();

    drive(&t, get(addr, None)).await;

    let kinds = rec.kinds();
    assert!(
        kinds.contains(&Kind::Connected) && kinds.contains(&Kind::Head),
        "premise: events were reported at all — saw {kinds:?}",
    );
    assert_eq!(
        rec.requests(),
        vec![RequestId::UNIDENTIFIED],
        "every event says it names no request",
    );
}

// ── the id the client minted ────────────────────────────────────────────

/// The other half: `Client` mints the identity and the events carry it,
/// end to end, with nothing in the test inserting an extension.
///
/// The test cannot know the numbers — that is the point of them being
/// minted where the operation is — so it asserts the shape: one id per
/// operation, never `UNIDENTIFIED`, and two operations are two ids.
#[tokio::test]
async fn two_operations_through_a_client_get_two_identities() {
    let (addr, accepted) = server(Behaviour::default());
    let rec = Recorder::default();
    let client = Client::builder(watched(&rec)).build().unwrap();

    for _ in 0..2 {
        let resp = tokio::time::timeout(BOUND, client.get(format!("http://{addr}/")).send())
            .await
            .expect("must not hang")
            .expect("request must succeed");
        assert_eq!(
            resp.collect().await.expect("body").text().expect("text"),
            "ok"
        );
    }

    assert_eq!(
        accepted.load(Ordering::SeqCst),
        1,
        "premise: one connection, so a shared connection id cannot be \
         what separates the two operations"
    );
    let ids = rec.requests();
    assert_eq!(ids.len(), 2, "two operations, two identities: {ids:?}");
    assert!(
        ids.iter().all(|id| *id != RequestId::UNIDENTIFIED),
        "the client minted them, so none is the absent value: {ids:?}",
    );
    assert_eq!(
        rec.requests_for(Kind::Head),
        ids,
        "and the heads arrive in the order the operations were made",
    );
}

/// A request that carries something, so that the **outbound** direction
/// has octets to report.
fn post(addr: SocketAddr, attempt: Attempt) -> http::Request<RequestBody> {
    let mut req = http::Request::builder()
        .method(http::Method::POST)
        .uri(format!("http://{addr}/"))
        .body(RequestBody::Full(bytes::Bytes::from_static(
            b"a payload worth counting",
        )))
        .expect("request");
    req.extensions_mut().insert(attempt);
    req
}

/// **The upload's octets name the request too**, and that direction needs
/// its own test: it is reported from `Reporting` — the exchange future —
/// where the download is reported from `Counting`, the response body.
/// Neither wrapper covers for the other, and a request with no body
/// reports nothing outbound at all, which is why every other test here is
/// blind to it.
///
/// Two uploads rather than one, because the fresh-connection arm and the
/// pooled arm of `Native::run` each build their own reporter: a mutation
/// in either survives a single request over the other.
#[tokio::test]
async fn the_uploads_octets_name_the_request_that_sent_them() {
    let (addr, accepted) = server(Behaviour::default());
    let rec = Recorder::default();
    let t = watched(&rec);

    let fresh = Attempt::new(RequestId::next());
    drive(&t, post(addr, fresh)).await;
    let pooled = Attempt::new(RequestId::next());
    drive(&t, post(addr, pooled)).await;

    assert_eq!(
        accepted.load(Ordering::SeqCst),
        1,
        "premise: the second upload went over the pooled connection, so \
         the two arms of `run` were both exercised"
    );
    let uploads: Vec<RequestId> = rec
        .lines()
        .into_iter()
        .filter(|l| l.kind == Kind::Progress)
        .map(|l| l.request)
        .collect();
    assert!(
        !uploads.is_empty(),
        "premise: octets were reported at all — saw {:?}",
        rec.kinds(),
    );
    assert_eq!(
        rec.requests(),
        vec![fresh.id, pooled.id],
        "both directions of both exchanges name the request they belong \
         to, and nothing names anything else: {uploads:?}",
    );
}

// ── the staged connect ──────────────────────────────────────────────────

/// **The staged pair carries the identity too**, and it is asserted
/// separately because `connect`/`exchange` is a second code path rather
/// than a second entry point into the first: it emits its own `Connected`
/// and `Reused` and reads the identity twice — once off the request it is
/// holding, once off the request it is about to consume.
///
/// The pool is what makes both events reachable in one test, exactly as
/// above: the first pair dials and the second finds.
#[tokio::test]
async fn a_staged_connect_and_its_exchange_name_the_request_too() {
    let (addr, accepted) = server(Behaviour::default());
    let rec = Recorder::default();
    let t = watched(&rec);

    let first = Attempt::new(RequestId::next());
    let staged = t
        .connect(Prepared::new(get(addr, Some(first))))
        .await
        .expect("the server is listening");
    assert_eq!(
        rec.requests_for(Kind::Connected),
        vec![first.id],
        "the connect names the request it was made for, before anything \
         has been spoken on the connection",
    );
    let resp = t.exchange(staged).await.expect("the exchange succeeds");
    assert_eq!(resp.status(), 200);
    drop(resp.into_body().collect().await.expect("body"));

    // A body on the second, so the staged exchange's own upload reporter
    // is exercised: it is a different one from `Native::run`'s, and a
    // mutation in it survives an empty request.
    let second = Attempt::new(RequestId::next());
    let staged = t
        .connect(Prepared::new(post(addr, second)))
        .await
        .expect("the pooled connection is found");
    let resp = t.exchange(staged).await.expect("the exchange succeeds");
    assert_eq!(resp.status(), 200);
    drop(resp.into_body().collect().await.expect("body"));

    assert_eq!(
        accepted.load(Ordering::SeqCst),
        1,
        "premise: the second staged connect found the pooled connection, \
         so there is a `Reused` to assert on"
    );
    assert_eq!(
        rec.requests_for(Kind::Reused),
        vec![second.id],
        "and the reuse is the second request's",
    );
    assert_eq!(
        rec.requests_for(Kind::Head),
        vec![first.id, second.id],
        "one head each, from the staged exchange",
    );
    assert_eq!(
        rec.requests(),
        vec![first.id, second.id],
        "and no event on this path names anything else",
    );
}

/// The control for the pair above: with no `Attempt` to read, the staged
/// path under-reports rather than inventing an identity.
#[tokio::test]
async fn a_staged_connect_with_no_attempt_reports_unidentified() {
    let (addr, _accepted) = server(Behaviour::default());
    let rec = Recorder::default();
    let t = watched(&rec);

    let staged = t
        .connect(Prepared::new(get(addr, None)))
        .await
        .expect("the server is listening");
    let resp = t.exchange(staged).await.expect("the exchange succeeds");
    drop(resp.into_body().collect().await.expect("body"));

    let kinds = rec.kinds();
    assert!(
        kinds.contains(&Kind::Connected) && kinds.contains(&Kind::Head),
        "premise: events were reported at all — saw {kinds:?}",
    );
    assert_eq!(rec.requests(), vec![RequestId::UNIDENTIFIED]);
}
