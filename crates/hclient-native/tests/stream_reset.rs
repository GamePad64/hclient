//! A server that answers without reading the request body, and three
//! servers that fail in its neighbourhood. **The set is the claim**: the
//! first test alone is passed by a client that tolerates everything, and
//! such a client turns a dead connection into a hang and a truncated body
//! into a short one.
//!
//! # The defect these were written for
//!
//! RFC 9113 §8.1: *"A server MAY request that the client abort
//! transmission of a request without error by sending a `RST_STREAM` with
//! an error code of `NO_ERROR` after sending a complete response … Clients
//! **MUST NOT** discard responses as a result of receiving such a
//! `RST_STREAM`."* This transport did, in two places at once:
//!
//! 1. `http2::poll_pump` turned every write failure on the request stream
//!    into an error, and `http2::exchange` returned on it **without ever
//!    polling `resp_fut`** — so the complete response, already decoded and
//!    sitting in h2's `pending_recv`, was thrown away;
//! 2. and had it not, the response *body* would still have failed: h2
//!    records end-of-stream as a state, and the `RST_STREAM` overwrites
//!    it, so the body ends with `Reset(_, NO_ERROR, Remote)` after
//!    delivering every byte. Measured, before the fix: `status=200`
//!    followed by exactly that error.
//!
//! Both halves are the same RFC sentence, and either one alone still
//! discards the response.
//!
//! # Why the request body is a megabyte
//!
//! To take the race out rather than to be large. HTTP/2's default
//! flow-control window is 65 535 bytes at both the stream and the
//! connection level, and these servers never read a request body, so they
//! never enlarge it. A body sixteen times that size **cannot** be written
//! before the server has answered, so the reset always lands mid-write and
//! the tests measure the decision instead of the scheduler.
//!
//! # Why this file carries its own TLS stub
//!
//! ALPN is the only route to HTTP/2 here and `TlsConnect` is what reports
//! it; `tests/http2.rs`'s module doc argues the technique at length. The
//! hazard of a second copy is that a mis-wired stub would silently test
//! HTTP/1.1 instead, so every test below asserts `Response::version()`,
//! which only the `h2` crate can set, or talks to a server that speaks
//! nothing else.
#![cfg(all(feature = "http2", not(target_family = "wasm")))]

use bytes::Bytes;
use hclient::Client;
use hclient_core::RequestBody;
use hclient_dns_system::SystemDns;
use hclient_native::Native;
use hclient_rt_tokio::Tokio;
use hclient_tls::{TlsConfigId, TlsConnect, TlsIdentity, TlsInfo, TlsRequest};
use std::future::poll_fn;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

/// Ceiling for anything that must not hang. Every failure mode these tests
/// are about is a hang if the client gets it wrong, so the bound is what
/// turns "wrong" into a named failure instead of a stuck run.
const BOUND: Duration = Duration::from_secs(30);

/// Sixteen times the 65 535-byte default flow-control window — see the
/// module doc.
const BODY: usize = 1024 * 1024;

/// Exactly one default flow-control window of response body, so that the
/// window is spent and the server's next reservation can only be granted
/// once the client has actually read some of it. `/reset-mid-body` uses
/// that as a synchronisation point.
const WINDOW: usize = 65_535;

struct Fixture {
    addr: SocketAddr,
    /// Requests the server decoded. Claims about what reached the far end
    /// are made from this, never from the client's own account of itself.
    requests: Arc<AtomicUsize>,
}

impl Fixture {
    fn url(&self, path: &str) -> String {
        format!("https://{}{}", self.addr, path)
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

fn spawn_server() -> Fixture {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&requests);

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
                let seen = Arc::clone(&seen);
                tokio::spawn(async move {
                    let _ = serve(tcp, seen).await;
                });
            }
        });
    });

    Fixture { addr, requests }
}

/// One connection's worth of HTTP/2, and nothing else — no HTTP/1
/// fallback, so a client that did not really speak h2 would get no answer
/// at all rather than a degraded one.
///
/// Handlers are spawned rather than awaited inline for the reason h2's own
/// server example gives: `Connection::accept` is what drives the
/// connection's IO, so a handler awaited inside the accept loop would
/// stall the very connection it needs flushed. `/reset-mid-body` depends
/// on that directly — it waits for a flow-control update from the client.
async fn serve(tcp: tokio::net::TcpStream, seen: Arc<AtomicUsize>) -> Result<(), h2::Error> {
    let mut conn = h2::server::handshake(tcp).await?;
    while let Some(accepted) = conn.accept().await {
        let (req, respond) = accepted?;
        seen.fetch_add(1, Ordering::SeqCst);
        let path = req.uri().path().to_owned();

        if path == "/die" {
            // Returning drops `conn`, and with it the socket, with the
            // request head read and nothing answered. Not a reset of one
            // stream: there is no response anywhere and none is coming.
            return Ok(());
        }

        let (_parts, body) = req.into_parts();
        tokio::spawn(async move {
            let _ = handle(path, respond, body).await;
        });
    }
    Ok(())
}

async fn handle(
    path: String,
    mut respond: h2::server::SendResponse<Bytes>,
    body: h2::RecvStream,
) -> Result<(), h2::Error> {
    let response = http::Response::builder().status(200).body(()).unwrap();
    let mut send = respond.send_response(response, false)?;

    if path == "/reset-mid-body" {
        send.send_data(Bytes::from(vec![b'y'; WINDOW]), false)?;
        // The window is now spent, so this reservation can only be granted
        // by a `WINDOW_UPDATE`, which the client only sends once it has
        // actually read part of the body. That makes "the response head
        // and some of its body have arrived" a fact rather than a guess,
        // and it matters: `RST_STREAM` with any reason but `NO_ERROR`
        // makes h2 discard whatever is still queued to send
        // (`h2-0.4.15/src/proto/streams/prioritize.rs:729-744`), the
        // response head included if it had not gone out yet.
        send.reserve_capacity(1);
        let _ = poll_fn(|cx| send.poll_capacity(cx)).await;
        send.send_reset(h2::Reason::INTERNAL_ERROR);
        return Ok(());
    }

    // The whole response, and not one byte of the request read. Dropping
    // the `RecvStream` below is what makes h2 schedule the
    // `RST_STREAM(NO_ERROR)`: server, send half closed, receive half still
    // streaming (`h2-0.4.15/src/proto/streams/streams.rs:1601-1618`).
    send.send_data(Bytes::from_static(b"answered"), true)?;
    drop(send);
    drop(respond);
    drop(body);
    Ok(())
}

/// A `TlsConnect` that encrypts nothing, hands the stream straight through
/// — so the bytes on the wire really are HTTP/2 — and reports `h2` as
/// negotiated. See this file's module doc for why a second copy of
/// `tests/http2.rs`'s stub is tolerable and what pins it honest.
#[derive(Clone)]
struct FakeTls {
    id: TlsConfigId,
}

impl FakeTls {
    fn new() -> Self {
        Self {
            id: TlsConfigId::new_unique(),
        }
    }
}

impl TlsIdentity for FakeTls {
    fn config_id(&self) -> TlsConfigId {
        self.id
    }
}

impl TlsConnect for FakeTls {
    type Stream<S>
        = S
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin;

    fn reports_alpn(&self) -> bool {
        true
    }

    type Handshake<'a, S>
        = std::future::Ready<Result<(S, TlsInfo), hclient_core::Error>>
    where
        Self: 'a,
        S: hyper::rt::Read + hyper::rt::Write + Unpin + 'a;

    fn connect<'a, S>(&'a self, io: S, _req: TlsRequest<'a>) -> Self::Handshake<'a, S>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin + 'a,
    {
        std::future::ready(Ok((io, TlsInfo::default().alpn(Some(b"h2".to_vec())))))
    }
}

fn client() -> Client {
    Client::builder(Native::new(Tokio, FakeTls::new(), SystemDns::new(Tokio)))
        .build()
        .unwrap()
}

fn megabyte() -> RequestBody {
    RequestBody::Full(Bytes::from(vec![0u8; BODY]))
}

/// **The acceptance.** A server that answers without reading the request
/// body still has its response read — head *and* body.
///
/// Both halves are asserted because the defect had two: fixing only
/// `exchange` returns a `200` whose body then fails with
/// `Reset(_, NO_ERROR, Remote)`, which is a response discarded by a
/// slower route.
#[tokio::test]
async fn a_server_that_stops_reading_the_body_still_gets_its_response_read() {
    let server = spawn_server();

    let resp = tokio::time::timeout(
        BOUND,
        client()
            .post(&server.url("/answer-without-reading"))
            .body(megabyte())
            .send(),
    )
    .await
    .expect("must not hang")
    .expect("RST_STREAM(NO_ERROR) acts on the request stream: the response was never affected");

    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.version(),
        http::Version::HTTP_2,
        "set by the h2 crate while decoding a real HEADERS frame — the \
         witness that this went out as HTTP/2 at all"
    );
    let body = tokio::time::timeout(BOUND, resp.collect())
        .await
        .expect("must not hang")
        .expect("and the body too, which the reset also has no claim on");
    assert_eq!(body.text().unwrap(), "answered");
    assert_eq!(server.requests(), 1);
}

/// The first control: same moment, same unwritten megabyte, and the only
/// difference is that the peer is *gone* rather than merely uninterested.
///
/// There is no response anywhere and none is coming, so a client that
/// tolerated everything on the write side would wait for one. This is what
/// makes the test above a statement about `RST_STREAM` rather than about
/// giving up on the request body.
#[tokio::test]
async fn a_connection_that_dies_mid_request_is_still_an_error() {
    let server = spawn_server();

    let result = tokio::time::timeout(
        BOUND,
        client().post(&server.url("/die")).body(megabyte()).send(),
    )
    .await
    .expect("must not hang");

    let err = result.expect_err("the connection is gone; there is no response to wait for");
    assert_eq!(*err.kind(), hclient_core::ErrorKind::Body);
    assert_eq!(server.requests(), 1, "the head did reach the server");
}

/// The second control, and the one that keeps the *body* side narrow: a
/// `RST_STREAM` whose reason is not `NO_ERROR` still fails the response
/// body, however much of it had already arrived.
///
/// `NO_ERROR` is the server's statement that the response was complete —
/// RFC 9113 §8.1 defines the code that way, and it is the only evidence
/// available, since the `END_STREAM` that would prove it has been
/// overwritten in h2's state by the time anything can look. Any other
/// reason carries no such statement, so a short body must not be handed
/// over as a whole one.
///
/// **No request body here.** It was forced when this was written —
/// An `exchange` that wrote the whole request body before waiting for the
/// response would leave a client still writing a megabyte never reaching
/// the read this server waits for — a deadlock. The h2 path is duplex, so
/// the constraint is gone; the test keeps its shape because the receive
/// side is the side it
/// is about, and `tests/http2_duplex.rs`'s
/// `a_reset_while_the_body_drives_the_pump_does_not_discard_the_response`
/// is where the same server behaviour is now met *with* an upload in
/// flight.
#[tokio::test]
async fn a_reset_that_is_not_no_error_still_fails_the_response_body() {
    let server = spawn_server();

    let resp = tokio::time::timeout(BOUND, client().get(&server.url("/reset-mid-body")).send())
        .await
        .expect("must not hang")
        .expect("the head is complete and was sent before the reset");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.version(), http::Version::HTTP_2);

    let err = tokio::time::timeout(BOUND, resp.collect())
        .await
        .expect("must not hang")
        .expect_err("a body cut short by INTERNAL_ERROR is not a body");
    assert_eq!(*err.kind(), hclient_core::ErrorKind::Body);
}

/// The write the `poll_capacity` fix alone does not reach.
///
/// A `RequestBody::Full` never returns `Pending` between its last chunk
/// and its end — `poll_pump` handles both in one pass — so on that body
/// the reset can only ever be met while waiting for capacity. A
/// `RequestBody::Streaming` that stalls puts the reset somewhere else
/// entirely: the pump is parked on the caller's body, and the writes it
/// wakes up to make are `send_data(Bytes::new(), true)` and
/// `send_trailers`, which fail with a `UserError::InactiveStreamId` that
/// no public h2 API can tell apart from an API misuse of ours.
///
/// That is why the question is asked of `SendStream::poll_reset` at the
/// top of the pump loop instead of being decoded from each write's error.
/// With the tolerance placed at `poll_capacity` instead, this test does
/// not fail with a wrong answer — it hangs, because a body that never ends
/// is never asked for capacity again.
#[tokio::test]
async fn a_stalled_streaming_body_does_not_hide_a_response_the_server_already_sent() {
    let server = spawn_server();

    let resp = tokio::time::timeout(
        BOUND,
        client()
            .post(&server.url("/answer-without-reading"))
            .body(RequestBody::Streaming(Box::new(StallsAfterOneChunk {
                sent: false,
            })))
            .send(),
    )
    .await
    .expect("a body that never ends must not become a response that never arrives")
    .expect("the server answered and stopped reading; that is the whole exchange");

    assert_eq!(resp.status(), 200);
    assert_eq!(resp.version(), http::Version::HTTP_2);
    let body = tokio::time::timeout(BOUND, resp.collect())
        .await
        .expect("must not hang")
        .expect("the body is unaffected by a reset of the request stream");
    assert_eq!(body.text().unwrap(), "answered");
}

/// One chunk, then `Pending` for ever and no waker registered — nothing
/// will ever poll it again, which is the point. A real streaming body
/// waiting on a source that has gone quiet behaves the same way; this one
/// merely makes the moment exact.
struct StallsAfterOneChunk {
    sent: bool,
}

impl http_body::Body for StallsAfterOneChunk {
    type Data = Bytes;
    type Error = hclient_core::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
        if self.sent {
            return Poll::Pending;
        }
        self.sent = true;
        Poll::Ready(Some(Ok(http_body::Frame::data(Bytes::from_static(
            b"the first and only chunk",
        )))))
    }
}
