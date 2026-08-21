//! gRPC-over-HTTP/2 as an **external specification**, the way Autobahn was
//! used for WebSocket: `grpc/doc/PROTOCOL-HTTP2.md` says what a gRPC client
//! puts on the wire and what it must be able to read back, and this file
//! measures whether that shape is reachable through `hclient::Client` over
//! `hclient-native`.
//!
//! **Nothing here implements gRPC, and nothing here should.** There is no
//! framer, no status enum and no `grpc-*` helper: the five-byte
//! length-prefix, the status codes and the percent-decoding of
//! `grpc-message` are a caller's job and stay a caller's job. What is under
//! test is the eight things the *client* owes such a caller —
//!
//! 1. `te: trailers` reaches the wire, and the HTTP/1-only headers beside
//!    it do not (RFC 9113 §8.2.2, and the spec's *"used to detect
//!    incompatible proxies"*) — and **Custom-Metadata** rides along
//!    unchanged, repeated names and `-bin` values included, in the head,
//!    the response and the trailers alike.
//! 2. A **Trailers-Only** response — one HEADERS block with END_STREAM and
//!    no DATA at all — is a complete, empty-bodied response whose
//!    `grpc-status` is readable off the head.
//! 3. A response's **trailers** reach the caller, and DATA frame
//!    boundaries are handed over as they arrived (*"DATA frame boundaries
//!    have no relation to Length-Prefixed-Message boundaries"*).
//! 4. Messages flow **both ways on one stream**, indefinitely, with the
//!    caller's next request message depending on the previous response
//!    message.
//! 5. **Flow control**, in both directions: a response larger than the
//!    window arrives whole, and a request larger than the window is
//!    back-pressured rather than swallowed into memory.
//! 6. **Cancellation** ends the call at the server rather than leaking it.
//! 7. `Client`'s own stages — redirect, cookie jar, response
//!    decompression — leave an `application/grpc` exchange alone, and its
//!    timeouts stay out of the way of a stream that is idle for a long
//!    time, which for gRPC is normal rather than suspicious.
//! 8. **Connection management**: a call's trailers do not cost its
//!    connection, a `GOAWAY` does not cost the *next* call, a server's
//!    `PING` is answered by the next call rather than while the
//!    connection sits pooled, and two concurrent calls take two
//!    connections — the last of which is the real gap between this client
//!    and a gRPC one, and is measured here rather than argued.
//!
//! # Everything is read from the server
//!
//! The observer is a real `h2::server` on a real socket: it performs the
//! connection preface, decodes HPACK, and every claim below about what the
//! client sent is read from what that server decoded — never from the
//! client's own account of itself. A client that spoke HTTP/1.1 into it
//! would get no answer at all. The same technique, and the same TLS stub
//! for the same reason, as `tests/http2.rs`, whose module doc argues it at
//! length: ALPN is the only route to HTTP/2 here and `TlsConnect` is what
//! reports it, so the stub reports `h2` and hands the stream straight
//! through — the bytes on the wire really are HTTP/2.
//!
//! # Causal, not timed
//!
//! The bidirectional test's round *i + 1* does not exist until round *i*'s
//! echo has been read, so a transport that finished the request before
//! reading the response could not complete it at any speed. The
//! back-pressure test holds the server behind a gate the test opens itself,
//! so the bound on what the client pulled is flow control and not a race
//! with the server's scheduler. The only `Duration`s that decide anything
//! are the two in the idle-stream A/B, where the bound *is* the subject;
//! every other one is a ceiling that turns a hang into a named failure.
#![cfg(all(feature = "http2", not(target_family = "wasm")))]

use bytes::Bytes;
use hclient::Client;
use hclient_core::{RequestBody, Timeouts};
use hclient_dns_system::SystemDns;
use hclient_native::Native;
use hclient_rt_tokio::Tokio;
use hclient_tls::{TlsConfigId, TlsConnect, TlsIdentity, TlsInfo, TlsRequest};
use http_body::Body as _;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

/// Ceiling for anything that must not hang. Every failure mode in this
/// file is a hang if the client gets it wrong, so this is what turns
/// "wrong" into a named failure instead of a stuck run.
const BOUND: Duration = Duration::from_secs(30);

/// One HTTP/2 flow-control window (RFC 9113 §6.9.2's default), which is
/// what `/sink` never enlarges while its gate is shut.
const WINDOW: usize = 65_535;

// ── the wire, as the server decoded it ──────────────────────────────────

/// One request stream, as the HTTP/2 server saw it.
///
/// The pseudo-headers are kept apart from `headers` because that is where
/// they are on the wire: `:method`, `:scheme`, `:path` and `:authority`
/// are the four the gRPC spec's Call-Definition names, and none of them
/// exists in an HTTP/1 request line the way h2 hands them over.
#[derive(Debug, Clone, Default)]
struct Seen {
    method: String,
    scheme: Option<String>,
    path: String,
    authority: Option<String>,
    /// Every ordinary header field, lowercased, in the order h2 decoded
    /// them.
    headers: Vec<(String, String)>,
    /// The length of each DATA frame, in arrival order — the spec's
    /// *"implementations should make no assumptions about their
    /// alignment"*, from the other side.
    frames: Vec<usize>,
    body: Vec<u8>,
    /// The request stream ended with END_STREAM rather than being reset or
    /// cut off by the connection going away.
    complete: bool,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    fn has(&self, name: &str) -> bool {
        self.headers.iter().any(|(n, _)| n == name)
    }
}

/// What the server learned about how a call ended, for the cancellation
/// row. The three arms are three different statements and the whole point
/// of the test is which one arrives.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Ending {
    /// A `RST_STREAM` reached the server — the signal gRPC's §"Errors"
    /// describes for a client cancelling a call.
    Reset(String),
    /// The stream ended because the connection did: the client closed the
    /// socket under it.
    ConnectionGone,
    /// Neither, within the ceiling.
    Nothing,
}

/// What became of a server-initiated `PING`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Pinged {
    /// A `PONG` came back.
    Answered,
    /// The connection failed while the ping was outstanding.
    Failed,
}

/// Everything the connection tasks and the test both look at. One value
/// rather than five `Arc`s threaded through three functions, which is what
/// it was until the fifth arrived.
#[derive(Default)]
struct Shared {
    /// TCP connections accepted. Every claim about pooling and
    /// multiplexing is this number.
    accepted: AtomicUsize,
    /// Connections whose accept loop has ended — a server-side close,
    /// observed rather than waited out.
    closed: AtomicUsize,
    seen: Mutex<Vec<Seen>>,
    endings: Mutex<Vec<Ending>>,
    /// Shut until a test opens it. `/g.S/Sink` reads not one byte of the
    /// request body before this is `true`, which is what makes the
    /// back-pressure bound causal rather than a race.
    gate: AtomicBool,
    /// How many requests have arrived at `/g.S/Pair`, which answers none
    /// of them until two have.
    paired: AtomicUsize,
    /// `PING`s the server sent on a connection it had already answered on,
    /// and what came back.
    pongs: Mutex<Vec<Pinged>>,
}

struct Fixture {
    addr: SocketAddr,
    shared: Arc<Shared>,
}

impl Fixture {
    fn url(&self, path: &str) -> String {
        format!("https://{}{}", self.addr, path)
    }

    fn seen(&self) -> Vec<Seen> {
        self.shared.seen.lock().unwrap().clone()
    }

    fn accepted(&self) -> usize {
        self.shared.accepted.load(Ordering::SeqCst)
    }

    fn open_the_gate(&self) {
        self.shared.gate.store(true, Ordering::SeqCst);
    }

    /// Waits for the server to have finished with `n` request streams.
    /// Returning `false` rather than hanging is what makes a missing
    /// record a failed assertion instead of a stuck suite.
    async fn wait_for_seen(&self, n: usize, within: Duration) -> bool {
        self.wait_until(within, || self.seen().len() >= n).await
    }

    async fn wait_for_endings(&self, n: usize, within: Duration) -> bool {
        self.wait_until(within, || self.shared.endings.lock().unwrap().len() >= n)
            .await
    }

    async fn wait_for_pongs(&self, n: usize, within: Duration) -> bool {
        self.wait_until(within, || self.shared.pongs.lock().unwrap().len() >= n)
            .await
    }

    fn pongs(&self) -> Vec<Pinged> {
        self.shared.pongs.lock().unwrap().clone()
    }

    async fn wait_for_closed(&self, n: usize, within: Duration) -> bool {
        self.wait_until(within, || self.shared.closed.load(Ordering::SeqCst) >= n)
            .await
    }

    async fn wait_until(&self, within: Duration, mut done: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if done() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    fn endings(&self) -> Vec<Ending> {
        self.shared.endings.lock().unwrap().clone()
    }
}

/// A five-byte gRPC length prefix and its payload — written out here
/// rather than imported, because there is deliberately no framer in this
/// workspace to import it from. It exists so the fixture's bytes look like
/// the ones a real call would carry; nothing under test parses it.
fn framed(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(0); // Compressed-Flag: not compressed
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

fn grpc_trailers(status: &str) -> http::HeaderMap {
    let mut t = http::HeaderMap::new();
    t.insert("grpc-status", http::HeaderValue::from_str(status).unwrap());
    t
}

fn spawn_server() -> Fixture {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let shared = Arc::new(Shared::default());

    let shared_t = Arc::clone(&shared);
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
                shared_t.accepted.fetch_add(1, Ordering::SeqCst);
                let shared = Arc::clone(&shared_t);
                tokio::spawn(async move {
                    let _ = serve(tcp, Arc::clone(&shared)).await;
                    shared.closed.fetch_add(1, Ordering::SeqCst);
                });
            }
        });
    });

    Fixture { addr, shared }
}

/// One connection's worth of HTTP/2.
///
/// The handler is spawned rather than awaited inline for the reason
/// `tests/http2.rs` gives: in `h2`'s server API it is `Connection::accept`
/// that drives the connection's IO, so a handler awaited inside the accept
/// loop would stall the very connection it is reading a body from.
async fn serve(tcp: tokio::net::TcpStream, shared: Arc<Shared>) -> Result<(), h2::Error> {
    let mut conn = h2::server::handshake(tcp).await?;
    while let Some(accepted) = conn.accept().await {
        let (req, respond) = accepted?;
        // RFC 9113 §6.8 / the spec's §"GOAWAY Frame": *"servers should send
        // GOAWAY before terminating a connection to reliably inform
        // clients which work has been accepted"*. Sent after this stream
        // has been handed to its handler, so the stream it names is this
        // one and the response still goes out.
        let goaway = req.uri().path().ends_with("Goaway");
        // The spec's §"PING Frame": *"both clients and servers can send a
        // PING frame that the peer must respond to"*. Held until the test
        // opens the gate, so the ping provably goes out on a connection
        // the client has already finished with and handed back to its
        // pool. The `PingPong` handle is independent of the accept loop
        // below, which is what keeps driving this connection's IO.
        let ping = req.uri().path().ends_with("Ping").then(|| conn.ping_pong());
        let for_handler = Arc::clone(&shared);
        tokio::spawn(async move {
            handle(req, respond, for_handler).await;
        });
        if goaway {
            conn.graceful_shutdown();
        }
        if let Some(Some(mut pp)) = ping {
            let shared = Arc::clone(&shared);
            tokio::spawn(async move {
                while !shared.gate.load(Ordering::SeqCst) {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                if let Ok(r) = tokio::time::timeout(BOUND, pp.ping(h2::Ping::opaque())).await {
                    shared.pongs.lock().unwrap().push(match r {
                        Ok(_) => Pinged::Answered,
                        Err(_) => Pinged::Failed,
                    });
                }
            });
        }
    }
    Ok(())
}

fn head(content_type: &str) -> http::response::Builder {
    http::Response::builder()
        .status(200)
        .header("content-type", content_type)
}

async fn handle(
    req: http::Request<h2::RecvStream>,
    mut respond: h2::server::SendResponse<Bytes>,
    shared: Arc<Shared>,
) {
    let (parts, mut body) = req.into_parts();
    let mut record = Seen {
        method: parts.method.to_string(),
        scheme: parts.uri.scheme_str().map(str::to_owned),
        path: parts.uri.path().to_owned(),
        authority: parts.uri.authority().map(|a| a.to_string()),
        headers: parts
            .headers
            .iter()
            .map(|(n, v)| {
                (
                    n.as_str().to_owned(),
                    String::from_utf8_lossy(v.as_bytes()).into_owned(),
                )
            })
            .collect(),
        ..Default::default()
    };
    let path = record.path.clone();

    match path.as_str() {
        // ── Trailers-Only: HTTP-Status, Content-Type, Trailers, in ONE
        // HEADERS block with END_STREAM, and no DATA at all.
        "/g.S/TrailersOnly" => {
            drain(&mut body, &mut record).await;
            let resp = head("application/grpc+proto")
                .header("grpc-status", "5")
                .header("grpc-message", "it%20was%20not%20found")
                .body(())
                .unwrap();
            let _ = respond.send_response(resp, true);
            shared.seen.lock().unwrap().push(record);
        }

        // ── Response-Headers, *Length-Prefixed-Message, Trailers. The one
        // message is sent as TWO DATA frames that split it in the middle
        // of its own length prefix, which is exactly what the spec means
        // by "no relation to Length-Prefixed-Message boundaries".
        "/g.S/Unary" => {
            drain(&mut body, &mut record).await;
            let Ok(mut send) =
                respond.send_response(head("application/grpc+proto").body(()).unwrap(), false)
            else {
                return;
            };
            let message = framed(b"pong");
            let (a, b) = message.split_at(2);
            let _ = send.send_data(Bytes::copy_from_slice(a), false);
            let _ = send.send_data(Bytes::copy_from_slice(b), false);
            let _ = send.send_trailers(grpc_trailers("0"));
            shared.seen.lock().unwrap().push(record);
        }

        // ── Bidirectional: every request DATA frame is echoed back the
        // moment it arrives, and the trailers follow the request's own
        // end-of-stream. A server that read the whole request first would
        // deadlock against the caller in the test, which is the point.
        "/g.S/Echo" => {
            let Ok(mut send) =
                respond.send_response(head("application/grpc+proto").body(()).unwrap(), false)
            else {
                return;
            };
            while let Some(chunk) = body.data().await {
                let Ok(chunk) = chunk else {
                    record.complete = false;
                    shared.seen.lock().unwrap().push(record);
                    return;
                };
                record.frames.push(chunk.len());
                record.body.extend_from_slice(&chunk);
                let _ = body.flow_control().release_capacity(chunk.len());
                if send.send_data(chunk, false).is_err() {
                    shared.seen.lock().unwrap().push(record);
                    return;
                }
            }
            record.complete = true;
            let _ = send.send_trailers(grpc_trailers("0"));
            shared.seen.lock().unwrap().push(record);
        }

        // ── A response bigger than one flow-control window, ending in
        // trailers: the client only finishes this if it releases receive
        // capacity as it reads.
        "/g.S/Flood" => {
            drain(&mut body, &mut record).await;
            let Ok(mut send) =
                respond.send_response(head("application/grpc+proto").body(()).unwrap(), false)
            else {
                return;
            };
            for i in 0..8u8 {
                if send
                    .send_data(Bytes::from(framed(&vec![b'a' + i; 64 * 1024])), false)
                    .is_err()
                {
                    return;
                }
            }
            let _ = send.send_trailers(grpc_trailers("0"));
            shared.seen.lock().unwrap().push(record);
        }

        // ── The head goes out BEFORE a byte of the request is read, and
        // then nothing is read at all until the test opens the gate. What
        // the client managed to write in between is bounded by flow
        // control and by nothing else.
        "/g.S/Sink" => {
            let Ok(mut send) =
                respond.send_response(head("application/grpc+proto").body(()).unwrap(), false)
            else {
                return;
            };
            while !shared.gate.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            drain(&mut body, &mut record).await;
            let _ = send.send_data(Bytes::from(framed(b"ok")), false);
            let _ = send.send_trailers(grpc_trailers("0"));
            shared.seen.lock().unwrap().push(record);
        }

        // ── The head goes out and then nothing ever does. What this
        // records is HOW the call ended once the caller walked away.
        "/g.S/Cancel" => {
            let Ok(mut send) =
                respond.send_response(head("application/grpc+proto").body(()).unwrap(), false)
            else {
                return;
            };
            shared.seen.lock().unwrap().push(record);
            let ending = tokio::time::timeout(
                Duration::from_secs(5),
                std::future::poll_fn(|cx| send.poll_reset(cx)),
            )
            .await;
            shared.endings.lock().unwrap().push(match ending {
                Ok(Ok(reason)) => Ending::Reset(reason.to_string()),
                Ok(Err(_)) => Ending::ConnectionGone,
                Err(_) => Ending::Nothing,
            });
        }

        // ── Custom-Metadata in every position the spec puts it: repeated
        // names and `-bin` values on the way in, and on the way out in
        // both the response headers and the trailers.
        "/g.S/Metadata" => {
            drain(&mut body, &mut record).await;
            let resp = head("application/grpc+proto")
                .header("x-resp-md-bin", "AAEC")
                .header("x-resp-md-bin", "AwQF")
                .body(())
                .unwrap();
            let Ok(mut send) = respond.send_response(resp, false) else {
                return;
            };
            let _ = send.send_data(Bytes::from(framed(b"pong")), false);
            let mut trailers = grpc_trailers("2");
            trailers.insert(
                "grpc-message",
                http::HeaderValue::from_static("a%20message"),
            );
            trailers.insert(
                "grpc-status-details-bin",
                http::HeaderValue::from_static("CAIS"),
            );
            trailers.append("x-trailer-md", http::HeaderValue::from_static("one"));
            trailers.append("x-trailer-md", http::HeaderValue::from_static("two"));
            let _ = send.send_trailers(trailers);
            shared.seen.lock().unwrap().push(record);
        }

        // ── A barrier: nobody is answered until two requests are in
        // flight at once. If the client serialised them the first would
        // never be answered, so the test's ceiling is what would fail —
        // and how many CONNECTIONS the two took is then a fact about the
        // pool rather than about the scheduler.
        "/g.S/Pair" => {
            drain(&mut body, &mut record).await;
            shared.paired.fetch_add(1, Ordering::SeqCst);
            let deadline = Instant::now() + Duration::from_secs(10);
            while shared.paired.load(Ordering::SeqCst) < 2 && Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let Ok(mut send) =
                respond.send_response(head("application/grpc+proto").body(()).unwrap(), false)
            else {
                return;
            };
            let _ = send.send_data(Bytes::from(framed(b"pong")), false);
            let _ = send.send_trailers(grpc_trailers("0"));
            shared.seen.lock().unwrap().push(record);
        }

        // ── A stream that says nothing for a while and then finishes
        // normally: the shape of every long-lived gRPC call between two
        // messages.
        "/g.S/Idle" => {
            drain(&mut body, &mut record).await;
            let Ok(mut send) =
                respond.send_response(head("application/grpc+proto").body(()).unwrap(), false)
            else {
                return;
            };
            tokio::time::sleep(IDLE_GAP).await;
            let _ = send.send_data(Bytes::from(framed(b"late")), false);
            let _ = send.send_trailers(grpc_trailers("0"));
            shared.seen.lock().unwrap().push(record);
        }

        _ => {
            drain(&mut body, &mut record).await;
            let Ok(mut send) =
                respond.send_response(head("application/grpc+proto").body(()).unwrap(), false)
            else {
                return;
            };
            let _ = send.send_data(Bytes::from(framed(b"pong")), false);
            let _ = send.send_trailers(grpc_trailers("0"));
            shared.seen.lock().unwrap().push(record);
        }
    }
}

/// How long `/g.S/Idle` says nothing for. Longer than the `between_bytes`
/// bound the A/B's second arm sets, and the only place in this file where a
/// duration decides an outcome — because there the bound IS the subject.
const IDLE_GAP: Duration = Duration::from_millis(600);

async fn drain(body: &mut h2::RecvStream, record: &mut Seen) {
    while let Some(chunk) = body.data().await {
        let Ok(chunk) = chunk else { return };
        record.frames.push(chunk.len());
        record.body.extend_from_slice(&chunk);
        let _ = body.flow_control().release_capacity(chunk.len());
    }
    record.complete = true;
}

// ── the caller's side ───────────────────────────────────────────────────

/// A request body fed from a channel — a client-streaming or bidirectional
/// call's message queue, with nothing gRPC-shaped about it.
struct Feed {
    rx: tokio::sync::mpsc::UnboundedReceiver<Bytes>,
    pulled: Arc<AtomicUsize>,
}

impl http_body::Body for Feed {
    type Data = Bytes;
    type Error = hclient_core::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
        match self.rx.poll_recv(cx) {
            Poll::Ready(Some(b)) => {
                self.pulled.fetch_add(1, Ordering::SeqCst);
                Poll::Ready(Some(Ok(http_body::Frame::data(b))))
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

fn feed() -> (tokio::sync::mpsc::UnboundedSender<Bytes>, RequestBody) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let body = Feed {
        rx,
        pulled: Arc::new(AtomicUsize::new(0)),
    };
    (tx, RequestBody::Streaming(Box::new(body)))
}

/// `frames` chunks of `chunk` bytes, always ready, counting what was
/// taken. Finite, so a test that goes wrong fails by asserting rather than
/// by running for ever.
struct Repeat {
    left: usize,
    chunk: Bytes,
    pulled: Arc<AtomicUsize>,
}

impl http_body::Body for Repeat {
    type Data = Bytes;
    type Error = hclient_core::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
        if self.left == 0 {
            return Poll::Ready(None);
        }
        self.left -= 1;
        self.pulled.fetch_add(1, Ordering::SeqCst);
        let chunk = self.chunk.clone();
        Poll::Ready(Some(Ok(http_body::Frame::data(chunk))))
    }
}

fn repeat(frames: usize, chunk: usize) -> (RequestBody, Arc<AtomicUsize>) {
    let pulled = Arc::new(AtomicUsize::new(0));
    let body = Repeat {
        left: frames,
        chunk: Bytes::from(vec![b'x'; chunk]),
        pulled: Arc::clone(&pulled),
    };
    (RequestBody::Streaming(Box::new(body)), pulled)
}

/// One frame off a response body, whatever kind it is.
///
/// `Response::chunk` deliberately skips trailers and `collect` drops them
/// (see `hclient::Response::chunk`'s doc comment, and the test below that
/// measures it), so a gRPC caller reads its body exactly like this: through
/// `into_parts` and `http_body::Body` directly.
async fn next_frame<B>(body: &mut B) -> Option<Result<http_body::Frame<Bytes>, B::Error>>
where
    B: http_body::Body<Data = Bytes> + Unpin,
{
    std::future::poll_fn(|cx| Pin::new(&mut *body).poll_frame(cx)).await
}

/// Reads a whole response body, keeping the data and the trailers apart —
/// which is the split that matters, since `grpc-status` lives in the
/// second.
async fn read_to_end<B>(body: &mut B) -> (Vec<u8>, Vec<usize>, Option<http::HeaderMap>)
where
    B: http_body::Body<Data = Bytes> + Unpin,
    B::Error: std::fmt::Debug,
{
    let mut data = Vec::new();
    let mut frames = Vec::new();
    let mut trailers = None;
    while let Some(frame) = next_frame(body).await {
        let frame = frame.expect("the response body must not fail");
        match frame.into_data() {
            Ok(d) => {
                frames.push(d.len());
                data.extend_from_slice(&d);
            }
            Err(other) => {
                if let Ok(t) = other.into_trailers() {
                    trailers = Some(t);
                }
            }
        }
    }
    (data, frames, trailers)
}

// ── the transport ───────────────────────────────────────────────────────

/// A `TlsConnect` that encrypts nothing, hands the stream straight through
/// — so the bytes on the wire really are HTTP/2 — and reports `h2` as
/// negotiated. See this file's module doc.
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

    async fn connect<S>(
        &self,
        io: S,
        _req: TlsRequest<'_>,
    ) -> Result<(S, TlsInfo), hclient_core::Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin,
    {
        Ok((
            io,
            TlsInfo {
                alpn: Some(b"h2".to_vec()),
                ..Default::default()
            },
        ))
    }
}

fn transport() -> Native<Tokio, FakeTls, SystemDns<Tokio>> {
    Native::new(Tokio, FakeTls::new(), SystemDns::new(Tokio))
}

fn client() -> Client {
    Client::builder(transport()).build().unwrap()
}

/// The same client with `Native::multiplexed()` — one h2 connection shared
/// between concurrent calls, which is gRPC's own transport model.
///
/// Off by default, so this is a **second** client rather than a change to
/// the one above: the three tests below each sit beside the assertion they
/// invert, and both readings are true — of a client that did not ask and
/// of one that did. `tests/http2_multiplex.rs` has the opt-in's own shape
/// and its prices.
fn multiplexed_client() -> Client {
    Client::builder(transport().multiplexed()).build().unwrap()
}

/// A unary call as the spec's Call-Definition describes one, minus the
/// pseudo-headers h2 derives from the URI itself.
fn call(server: &Fixture, path: &str, body: RequestBody) -> http::Request<RequestBody> {
    http::Request::builder()
        .method(http::Method::POST)
        .uri(server.url(path))
        .header("te", "trailers")
        .header("content-type", "application/grpc+proto")
        .header("grpc-timeout", "30S")
        .header("grpc-accept-encoding", "identity,gzip")
        .header("user-agent", "grpc-rust-hclient/0.1.0")
        .body(body)
        .unwrap()
}

// ── 1. Request-Headers ──────────────────────────────────────────────────

/// **`te: trailers` reaches the wire, and the headers HTTP/2 forbids do
/// not.**
///
/// `http2::strip_connection_headers` leaves `TE` alone with RFC 9113
/// §8.2.2's single-value permission written beside it — a decision in a
/// comment. This is the behaviour: the server decoded `te: trailers` off a
/// real HEADERS frame, in the same request whose `connection` and
/// `transfer-encoding` it did not decode, because they were removed before
/// the frame was built.
///
/// The rest of the Call-Definition is asserted in the same breath, because
/// each of them is a way for this to be right for the wrong reason: a
/// client that sent `te` and mangled `:path` would have failed the call
/// anyway.
#[tokio::test(flavor = "multi_thread")]
async fn the_call_definition_reaches_the_wire_te_trailers_included() {
    let server = spawn_server();
    let client = client();

    let mut req = call(
        &server,
        "/g.S/Unary",
        RequestBody::Full(Bytes::from(framed(b"ping"))),
    );
    // Two headers RFC 9113 §8.2.2 forbids on an HTTP/2 message, set by a
    // caller who could not know which protocol ALPN would pick. They must
    // not turn the request into a protocol error, and they must not reach
    // the server.
    req.headers_mut().insert(
        http::header::CONNECTION,
        http::HeaderValue::from_static("keep-alive"),
    );
    req.headers_mut().insert(
        http::header::TRANSFER_ENCODING,
        http::HeaderValue::from_static("chunked"),
    );

    let resp = tokio::time::timeout(BOUND, client.execute(req))
        .await
        .expect("must not hang")
        .expect("the call must succeed");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.version(), http::Version::HTTP_2);

    assert!(
        server.wait_for_seen(1, BOUND).await,
        "the server must record the call"
    );
    let seen = server.seen();
    assert_eq!(seen.len(), 1, "one call, one stream");
    let s = &seen[0];

    assert_eq!(
        s.header("te"),
        Some("trailers"),
        "the spec's TE header, decoded by a real HPACK decoder rather than \
         read off a comment in strip_connection_headers"
    );
    assert!(
        !s.has("connection") && !s.has("transfer-encoding"),
        "RFC 9113 §8.2.2: these two are removed, and TE is not — the whole \
         point of the exemption is that it is one header and not a rule \
         about connection-specific fields in general; saw {:?}",
        s.headers
    );
    assert_eq!(s.method, "POST", "Method → \":method POST\"");
    assert_eq!(
        s.scheme.as_deref(),
        Some("https"),
        "Scheme → \":scheme https\""
    );
    assert_eq!(
        s.path, "/g.S/Unary",
        "Path → \":path\" \"/\" Service-Name \"/\" method name, case-sensitive"
    );
    assert_eq!(
        s.authority.as_deref(),
        Some(server.addr.to_string().as_str()),
        "Authority → \":authority\", built from the absolute URI"
    );
    assert_eq!(s.header("content-type"), Some("application/grpc+proto"));
    assert_eq!(
        s.header("grpc-timeout"),
        Some("30S"),
        "the deadline is a header the caller sets and this client passes on \
         untouched — it is not a Timeouts field and must not become one"
    );
    assert_eq!(s.header("grpc-accept-encoding"), Some("identity,gzip"));
    assert_eq!(s.header("user-agent"), Some("grpc-rust-hclient/0.1.0"));
    assert!(
        !s.has("content-length"),
        "nothing here adds one, which is what a client-streaming call needs"
    );
    assert!(s.complete, "EOS: the request stream ended with END_STREAM");
}

/// **Custom-Metadata survives in all four positions, repeated names
/// included.**
///
/// The spec's Custom-Metadata is *"an arbitrary set of key-value pairs
/// defined by the application layer"*, carried as ordinary header fields
/// on the request, the response head and the trailers. Two of its rules
/// are the client's to keep and neither is about gRPC:
///
/// - **Repeated names keep both values.** *"Custom-Metadata header order is
///   not guaranteed to be preserved except for values with duplicate header
///   names"*, and a runtime *"must split Binary-Headers on ',' before
///   decoding"* — both of which are nonsense if the pairs were collapsed on
///   the way through. A client that kept one value of two would corrupt
///   metadata silently.
/// - **`-bin` values pass byte for byte.** Base64 is *"padded and
///   un-padded"* on the wire, so one of each goes out here; the encoding
///   itself is the caller's business and the point is that neither is
///   touched.
///
/// The names use `.` and `_` because the spec's own Header-Name production
/// admits them, and the response's `grpc-message` is percent-encoded for
/// the same reason it is in the Trailers-Only row: decoding is not this
/// client's job and neither is spotting that it was not done.
#[tokio::test(flavor = "multi_thread")]
async fn custom_metadata_survives_in_the_head_the_response_and_the_trailers() {
    let server = spawn_server();
    let client = client();

    let mut req = call(&server, "/g.S/Metadata", RequestBody::Empty);
    let h = req.headers_mut();
    // Two values under one name, and a padded / un-padded base64 pair.
    h.append("x-md-bin", http::HeaderValue::from_static("AAECAw=="));
    h.append("x-md-bin", http::HeaderValue::from_static("BAUGBw"));
    h.append(
        "my.service_v1-key",
        http::HeaderValue::from_static("an ascii value"),
    );

    let resp = tokio::time::timeout(BOUND, client.execute(req))
        .await
        .expect("must not hang")
        .expect("the call must succeed");
    assert_eq!(resp.status(), 200);

    let out: Vec<&[u8]> = resp
        .headers()
        .get_all("x-resp-md-bin")
        .iter()
        .map(|v| v.as_bytes())
        .collect();
    assert_eq!(
        out,
        vec![b"AAEC".as_slice(), b"AwQF".as_slice()],
        "both response-header values, in the order the server sent them"
    );

    let (_, mut body) = resp.into_parts();
    let (data, _, trailers) = tokio::time::timeout(BOUND, read_to_end(&mut body))
        .await
        .expect("must not hang");
    assert_eq!(data, framed(b"pong"));
    let trailers = trailers.expect("Trailers carry the status and the metadata with it");
    assert_eq!(
        trailers.get("grpc-status").map(|v| v.as_bytes()),
        Some(b"2".as_slice())
    );
    assert_eq!(
        trailers.get("grpc-message").map(|v| v.as_bytes()),
        Some(b"a%20message".as_slice()),
        "percent-encoded and untouched"
    );
    assert_eq!(
        trailers
            .get("grpc-status-details-bin")
            .map(|v| v.as_bytes()),
        Some(b"CAIS".as_slice()),
        "Status-Details is allowed only when Status is not OK, which is why \
         this route answers 2 rather than 0"
    );
    let tm: Vec<&[u8]> = trailers
        .get_all("x-trailer-md")
        .iter()
        .map(|v| v.as_bytes())
        .collect();
    assert_eq!(tm, vec![b"one".as_slice(), b"two".as_slice()]);

    assert!(server.wait_for_seen(1, BOUND).await);
    let s = &server.seen()[0];
    let sent: Vec<&str> = s
        .headers
        .iter()
        .filter(|(n, _)| n == "x-md-bin")
        .map(|(_, v)| v.as_str())
        .collect();
    assert_eq!(
        sent,
        vec!["AAECAw==", "BAUGBw"],
        "two values under one name, padded and un-padded, neither joined \
         nor dropped"
    );
    assert_eq!(s.header("my.service_v1-key"), Some("an ascii value"));
}

// ── 2. Trailers-Only ────────────────────────────────────────────────────

/// **Trailers-Only → HTTP-Status Content-Type Trailers**, in one HEADERS
/// frame with END_STREAM and no DATA at all.
///
/// The spec permits it *"for calls that produce an immediate error"*, and
/// it is the response shape a gRPC client meets most often when something
/// is wrong. What it asks of an HTTP client is that a body which never
/// existed ends cleanly rather than pending, erroring, or waiting for
/// trailers that will not come as a separate frame — and that the fields
/// in that one HEADERS block are readable as ordinary response headers,
/// because on the wire that is what they are.
///
/// Read through the whole `Client`, so `Deadline` and `Decompressed` are
/// both in the path: an empty body is exactly the shape a decoder could
/// turn into a spurious truncation error.
#[tokio::test(flavor = "multi_thread")]
async fn a_trailers_only_response_is_a_complete_response_with_no_body() {
    let server = spawn_server();
    let client = client();

    let resp = tokio::time::timeout(
        BOUND,
        client.execute(call(&server, "/g.S/TrailersOnly", RequestBody::Empty)),
    )
    .await
    .expect("must not hang")
    .expect("a Trailers-Only response is a response, not a failure");

    assert_eq!(resp.status(), 200, "HTTP-Status → \":status 200\"");
    assert_eq!(resp.version(), http::Version::HTTP_2);
    assert_eq!(
        resp.headers().get("grpc-status").map(|v| v.as_bytes()),
        Some(b"5".as_slice()),
        "Trailers-Only puts Status in the SAME header block as the status \
         line, so it arrives as a response header and not as a trailer"
    );
    assert_eq!(
        resp.headers().get("grpc-message").map(|v| v.as_bytes()),
        Some(b"it%20was%20not%20found".as_slice()),
        "percent-encoded, and passed through byte for byte — decoding it is \
         the caller's job and this client must not touch it"
    );

    let (_, body) = resp.into_parts();
    let mut body = body;
    assert!(
        body.is_end_stream(),
        "the HEADERS frame carried END_STREAM, so the body is over before \
         it is polled"
    );
    let (data, frames, trailers) = tokio::time::timeout(BOUND, read_to_end(&mut body))
        .await
        .expect("an empty body must not hang");
    assert!(
        data.is_empty() && frames.is_empty(),
        "no DATA frames at all"
    );
    assert!(
        trailers.is_none(),
        "and no trailers frame either — that is what makes this shape \
         Trailers-ONLY rather than a response with empty data"
    );
}

// ── 3. Response-Headers *Length-Prefixed-Message Trailers ───────────────

/// **The ordinary response shape, and the two things it asks of a client:
/// trailers reach the caller, and DATA frame boundaries are handed over as
/// they arrived.**
///
/// The server splits one four-byte message across two DATA frames, cutting
/// it in the middle of its own five-byte prefix. The spec's §"Data Frames"
/// says implementations *"should make no assumptions about their
/// alignment"* — which for the client under test means it must neither
/// coalesce them into one frame nor lose the split, because a caller
/// reassembling messages is entitled to see the stream as it arrived.
///
/// The second half of the test is the reason a gRPC caller cannot use
/// `Response::collect()`: it is documented to skip trailers, and here that
/// documentation is measured. `grpc-status` is *mandatory* in Trailers even
/// when it is OK, so a caller that used the convenience path would lose the
/// status of every successful call.
#[tokio::test(flavor = "multi_thread")]
async fn response_trailers_reach_the_caller_and_the_frame_split_survives() {
    let server = spawn_server();
    let client = client();

    let resp = tokio::time::timeout(
        BOUND,
        client.execute(call(
            &server,
            "/g.S/Unary",
            RequestBody::Full(Bytes::from(framed(b"ping"))),
        )),
    )
    .await
    .expect("must not hang")
    .expect("the call must succeed");
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get("content-type").map(|v| v.as_bytes()),
        Some(b"application/grpc+proto".as_slice())
    );

    let (_, mut body) = resp.into_parts();
    let (data, frames, trailers) = tokio::time::timeout(BOUND, read_to_end(&mut body))
        .await
        .expect("must not hang");

    assert_eq!(data, framed(b"pong"), "the message arrived whole");
    assert_eq!(
        frames,
        vec![2, 7],
        "and it arrived as the two DATA frames the server sent, split in \
         the middle of its own length prefix — neither coalesced nor lost"
    );
    let trailers = trailers.expect("Trailers are not optional: Status must be sent even when OK");
    assert_eq!(
        trailers.get("grpc-status").map(|v| v.as_bytes()),
        Some(b"0".as_slice())
    );

    // The convenience path, measured rather than assumed: `chunk()` skips
    // trailer frames and `collect()` is built on it, so `Collected` has no
    // way to carry a `grpc-status`. This is a documented limitation of the
    // ergonomic API, not a transport defect — the frames above came out of
    // the very same body type.
    let again = tokio::time::timeout(
        BOUND,
        client
            .post(&server.url("/g.S/Unary"))
            .header("te", "trailers")
            .header("content-type", "application/grpc+proto")
            .body(RequestBody::Full(Bytes::from(framed(b"ping"))))
            .send(),
    )
    .await
    .expect("must not hang")
    .expect("the call must succeed");
    let collected = again.collect().await.expect("collect must succeed");
    assert_eq!(
        collected.bytes().as_ref(),
        framed(b"pong").as_slice(),
        "collect() gets the data"
    );
    assert!(
        collected.headers().get("grpc-status").is_none(),
        "and not the trailers: a gRPC caller reads the body through \
         into_parts(), which is what Response::chunk's doc comment says"
    );
}

// ── 4. Bidirectional streaming ──────────────────────────────────────────

/// **Messages both ways on one stream, with each request message caused by
/// the previous response message.**
///
/// Round *i + 1*'s bytes do not exist until round *i*'s echo has been
/// read — they are not in the channel — so a transport that wrote the
/// request before reading the response could not finish this at any speed,
/// and one that stopped driving the upload once the head arrived would
/// stall at round 2. Sixteen rounds rather than two, because a
/// wake-up that is lost only sometimes is the failure this shape exists to
/// catch.
///
/// It also pins the thing that makes bidi possible at all here: **the
/// upload is driven from the response body's `poll_frame`**, since nothing
/// is spawned. Reading the response is what sends the next message, which
/// is exactly what a bidirectional caller does anyway.
#[tokio::test(flavor = "multi_thread")]
async fn a_bidirectional_stream_carries_sixteen_rounds_both_ways() {
    let server = spawn_server();
    let client = client();
    let (tx, body) = feed();

    let resp = tokio::time::timeout(BOUND, client.execute(call(&server, "/g.S/Echo", body)))
        .await
        .expect("must not hang")
        .expect("the call must succeed");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.version(), http::Version::HTTP_2);
    let (_, mut resp_body) = resp.into_parts();

    let mut inbox: Vec<u8> = Vec::new();
    for i in 0..16u8 {
        let message = framed(&[b'a' + i; 3]);
        tx.send(Bytes::from(message.clone()))
            .expect("the request stream is still open");

        // Read until this round's echo is complete. Every byte of it has to
        // come back before the next message is even constructed.
        let want = inbox.len() + message.len();
        while inbox.len() < want {
            let frame = tokio::time::timeout(BOUND, next_frame(&mut resp_body))
                .await
                .unwrap_or_else(|_| panic!("round {i} did not come back — the stream is one-way"))
                .expect("the response body ended early")
                .expect("the response body failed");
            if let Ok(d) = frame.into_data() {
                inbox.extend_from_slice(&d);
            }
        }
        assert_eq!(
            &inbox[want - message.len()..],
            message.as_slice(),
            "round {i} came back as something else"
        );
    }

    // Half-close: the request stream ends, the response stream has not.
    drop(tx);
    let (rest, _, trailers) = tokio::time::timeout(BOUND, read_to_end(&mut resp_body))
        .await
        .expect("the trailers must arrive once the request stream ends");
    assert!(rest.is_empty(), "nothing was left over after the last echo");
    assert_eq!(
        trailers
            .expect("Trailers close the call")
            .get("grpc-status")
            .map(|v| v.as_bytes()),
        Some(b"0".as_slice())
    );

    assert!(server.wait_for_seen(1, BOUND).await);
    let seen = server.seen();
    assert_eq!(seen.len(), 1, "sixteen rounds, one stream");
    assert!(seen[0].complete, "the request stream ended cleanly");
    assert_eq!(
        seen[0].frames,
        {
            let mut f = vec![framed(&[b'a'; 3]).len(); 16];
            f.push(0);
            f
        },
        "one DATA frame per message — and then an EMPTY one, which is the \
         spec's own MUST: \"in scenarios where the Request stream needs to \
         be closed but no data remains to be sent implementations MUST send \
         an empty DATA frame with this flag set\". `http2::Pump` sends it \
         unconditionally when the caller's body ends, so a half-close never \
         has to ride on a message frame"
    );
    assert_eq!(server.accepted(), 1, "and one connection");
}

// ── 5. Flow control ─────────────────────────────────────────────────────

/// **A response larger than the flow-control window arrives whole, and its
/// trailers arrive after it.**
///
/// Eight 64 KiB messages is eight times RFC 9113's default 65 535-byte
/// window, so a client that never released receive capacity would stop
/// after the first window's worth and never see the `grpc-status` behind
/// it. `tests/http2.rs` already pins the release for a plain body; what is
/// added here is the trailers on the far side of it, which is where a gRPC
/// call's outcome lives.
#[tokio::test(flavor = "multi_thread")]
async fn a_response_past_the_window_arrives_whole_with_its_trailers() {
    let server = spawn_server();
    let client = client();

    let resp = tokio::time::timeout(
        BOUND,
        client.execute(call(&server, "/g.S/Flood", RequestBody::Empty)),
    )
    .await
    .expect("must not hang")
    .expect("the call must succeed");

    let (_, mut body) = resp.into_parts();
    let (data, _, trailers) = tokio::time::timeout(BOUND, read_to_end(&mut body))
        .await
        .expect("must not hang");

    let expected: usize = (0..8)
        .map(|i| framed(&vec![b'a' + i; 64 * 1024]).len())
        .sum();
    assert_eq!(
        data.len(),
        expected,
        "{} bytes over a {WINDOW}-byte window",
        expected
    );
    assert_eq!(
        trailers
            .expect("the trailers are behind eight windows of data")
            .get("grpc-status")
            .map(|v| v.as_bytes()),
        Some(b"0".as_slice())
    );
}

/// **A request larger than the window is back-pressured, not swallowed.**
///
/// `Capabilities::streaming_request_body` is `true`, and on the HTTP/1 path
/// `tests/transport.rs` proves it by reading `transfer-encoding: chunked`
/// off the wire. On HTTP/2 the equivalent claim is about *capacity*: h2's
/// `SendStream::send_data` will accept data with no window available and
/// buffer it, in h2's own words, without bound — which would turn the
/// declared streaming into a promise to read a producer as fast as it can
/// produce and hold the result in memory. `http2::Pump` reserves capacity
/// instead, and this is what that is worth.
///
/// **Causal, not timed.** `/g.S/Sink` sends the response head *before* it
/// reads anything and then reads nothing at all until this test opens the
/// gate. So at the moment the head is in the caller's hands, the server has
/// provably released no capacity, and everything the client pulled from the
/// caller's body it pulled against one 65 535-byte window: two 32 KiB
/// chunks fit, a third does not.
#[tokio::test(flavor = "multi_thread")]
async fn a_request_past_the_window_is_backpressured_rather_than_buffered() {
    let server = spawn_server();
    let client = client();
    const CHUNK: usize = 32 * 1024;
    const FRAMES: usize = 16;
    let (body, pulled) = repeat(FRAMES, CHUNK);

    let resp = tokio::time::timeout(BOUND, client.execute(call(&server, "/g.S/Sink", body)))
        .await
        .expect("must not hang")
        .expect("the head arrives before the body is read — that is the fixture");

    let at_head = pulled.load(Ordering::SeqCst);
    assert!(
        at_head <= WINDOW / CHUNK + 1,
        "the server had released no capacity, so at most one window's worth \
         plus the chunk being written can have been taken from the caller's \
         body — {at_head} of {FRAMES} chunks were, which is the unbounded \
         buffering `Pump` reserves capacity to avoid"
    );

    server.open_the_gate();
    let (_, mut resp_body) = resp.into_parts();
    let (_, _, trailers) = tokio::time::timeout(BOUND, read_to_end(&mut resp_body))
        .await
        .expect("must not hang");
    assert!(trailers.is_some(), "the call completed");

    assert!(server.wait_for_seen(1, BOUND).await);
    let seen = server.seen();
    assert_eq!(
        seen[0].body.len(),
        CHUNK * FRAMES,
        "and every byte still got there once the window opened"
    );
    assert!(seen[0].complete);
    assert_eq!(
        pulled.load(Ordering::SeqCst),
        FRAMES,
        "the whole body was pulled in the end"
    );
}

// ── 6. Cancellation ─────────────────────────────────────────────────────

/// **A cancelled call ends at the server, and the next one is unaffected.**
///
/// gRPC's §"Errors" describes `RST_STREAM` as the frame a runtime uses to
/// tell its peer a stream is over, and a gRPC client cancelling a call
/// sends one. **This client does not: it closes the connection**, and the
/// server sees the socket go rather than a frame. That is a consequence of
/// `pool.rs`'s exclusive check-out — an h2 connection here carries one
/// stream at a time, so the connection *is* the call, and dropping the
/// exchange drops the `Connection` future with it before any queued
/// `RST_STREAM` can be written. `http2::Pump`'s `Drop` queues one anyway,
/// against the day that stops being true; its own doc comment says it is
/// unobservable today, and this test is where "unobservable" is measured
/// instead of assumed.
///
/// The half of W1's rule that gRPC actually depends on — *cancelling one
/// stream must not tear down the others* — holds here vacuously and for a
/// reason worth restating: there are no others. The second request below
/// gets its own connection and its own answer.
///
/// **Under `multiplexed()` both halves change, and the sibling below is
/// where** (v0.4): the server sees `Ending::Reset(CANCEL)` because the
/// driver outlives the stream, and W1's rule stops being vacuous because
/// there *is* a neighbour on that connection. This test is the default's,
/// and the default did not move.
#[tokio::test(flavor = "multi_thread")]
async fn cancelling_a_call_ends_it_at_the_server_and_leaves_the_next_one_alone() {
    let server = spawn_server();
    let client = client();

    let resp = tokio::time::timeout(
        BOUND,
        client.execute(call(&server, "/g.S/Cancel", RequestBody::Empty)),
    )
    .await
    .expect("must not hang")
    .expect("the head arrives, then nothing does");
    assert_eq!(resp.status(), 200);

    // The caller walks away with the call in progress.
    drop(resp);

    assert!(
        server.wait_for_endings(1, BOUND).await,
        "the server must learn the call is over"
    );
    assert_eq!(
        server.endings(),
        vec![Ending::ConnectionGone],
        "measured, and recorded as a limitation rather than claimed as \
         gRPC-shaped: a `RST_STREAM(CANCEL)` is what a gRPC client sends, \
         and what arrives here is the connection closing under the stream. \
         For a transport that carries one stream per connection the server \
         learns the same fact at the same instant; for one that multiplexed \
         it would not, which is why `Pump`'s Drop already queues the reset."
    );

    let again = tokio::time::timeout(
        BOUND,
        client.execute(call(
            &server,
            "/g.S/Unary",
            RequestBody::Full(Bytes::from(framed(b"ping"))),
        )),
    )
    .await
    .expect("must not hang")
    .expect("a cancelled call must not poison the client");
    assert_eq!(again.status(), 200);
    let (_, mut body) = again.into_parts();
    let (data, _, trailers) = tokio::time::timeout(BOUND, read_to_end(&mut body))
        .await
        .expect("must not hang");
    assert_eq!(data, framed(b"pong"));
    assert!(trailers.is_some());
    assert_eq!(
        server.accepted(),
        2,
        "the second call is on a connection of its own — the cancelled one \
         was not returned to the pool"
    );
}

// ── 7. Client's own stages ──────────────────────────────────────────────

/// **The redirect stage, the cookie jar and response decompression leave an
/// `application/grpc` exchange alone.**
///
/// Three separate claims, each measured from the server's side:
///
/// - **No cookie header**, because no jar was configured. A jar is opt-in
///   (`ClientBuilder::cookie_jar`, behind the `cookies` feature) and a gRPC
///   caller simply does not ask for one.
/// - **`Accept-Encoding` is either absent or something this build can
///   actually reverse.** With the `gzip`/`brotli` features on, `Client`
///   advertises what it can decode — and the assertion is not that it
///   stays silent but that it never advertises a coding it cannot reverse,
///   because *that* is what would corrupt a response. A gRPC server does
///   not compress at the HTTP layer (message compression is `grpc-encoding`
///   inside the frames, which this client never touches), so the header is
///   inert here; the next test shows the caller can remove it anyway.
/// - **One request reached the server**, so the redirect stage added no
///   hop: a `200` is `RedirectAction::Stop` before any counting.
///
/// And the response comes back byte for byte with its trailers, which is
/// the only outcome that matters if any of the three had interfered.
#[tokio::test(flavor = "multi_thread")]
async fn the_clients_own_stages_leave_a_grpc_exchange_alone() {
    let server = spawn_server();
    let client = client();

    let resp = tokio::time::timeout(
        BOUND,
        client.execute(call(
            &server,
            "/g.S/Unary",
            RequestBody::Full(Bytes::from(framed(b"ping"))),
        )),
    )
    .await
    .expect("must not hang")
    .expect("the call must succeed");

    let (_, mut body) = resp.into_parts();
    let (data, _, trailers) = tokio::time::timeout(BOUND, read_to_end(&mut body))
        .await
        .expect("must not hang");
    assert_eq!(
        data,
        framed(b"pong"),
        "the message is not touched by any stage"
    );
    assert!(trailers.is_some(), "and neither are the trailers");

    assert!(server.wait_for_seen(1, BOUND).await);
    let seen = server.seen();
    assert_eq!(seen.len(), 1, "no redirect stage hop was added");
    let s = &seen[0];
    assert!(
        !s.has("cookie"),
        "no jar was asked for, so no jar attached one"
    );
    match s.header("accept-encoding") {
        None => {}
        Some(v) => {
            // `hclient`'s `Decoders::PREFERENCE`, which is not visible
            // from here — this crate depends on `hclient` only as a dev-
            // dependency and the type is `pub(crate)` anyway. What the
            // list is doing is asserting that the header names codings
            // and nothing else; that each named one has a decoder in this
            // build is `decompress.rs`'s own test, where `Decoders` is in
            // scope. It was `gzip || br` until `deflate` and `zstd`
            // landed, and this run is what caught it.
            const KNOWN: [&str; 4] = ["zstd", "br", "gzip", "deflate"];
            for coding in v.split(',').map(str::trim) {
                assert!(
                    KNOWN.contains(&coding),
                    "the client must never advertise a coding it cannot \
                     reverse; saw {coding:?} in {v:?}"
                );
            }
        }
    }
}

/// **A caller who negotiates for themselves keeps their header**, which is
/// the opt-out for the row above.
///
/// `decompress::negotiate` returns without touching a request that already
/// carries an `Accept-Encoding` — *"the caller did their own negotiating"*
/// — so a gRPC caller who wants HTTP-layer compression off says
/// `identity` and gets exactly that on the wire, in every build of this
/// crate, whichever decoding features are compiled in.
#[tokio::test(flavor = "multi_thread")]
async fn a_caller_who_sets_accept_encoding_keeps_it_exactly() {
    let server = spawn_server();
    let client = client();

    let mut req = call(&server, "/g.S/Unary", RequestBody::Empty);
    req.headers_mut().insert(
        http::header::ACCEPT_ENCODING,
        http::HeaderValue::from_static("identity"),
    );

    let resp = tokio::time::timeout(BOUND, client.execute(req))
        .await
        .expect("must not hang")
        .expect("the call must succeed");
    assert_eq!(resp.status(), 200);

    assert!(server.wait_for_seen(1, BOUND).await);
    assert_eq!(
        server.seen()[0].header("accept-encoding"),
        Some("identity"),
        "exactly what the caller wrote, with nothing appended"
    );
}

// ── 8. Deadlines ────────────────────────────────────────────────────────

/// **A stream that says nothing for a long time is not cut, by default —
/// and is cut when the caller asks for it.**
///
/// Silence is a gRPC stream's normal state: a server-streaming call may
/// have minutes between messages, and `grpc-timeout` — the deadline that
/// governs it — is a header the *server* enforces. `Timeouts` is this
/// client's own, and the row worth measuring is that none of its fields is
/// set unless a caller sets one.
///
/// An A/B against the same server, because either half alone is
/// meaningless: the first arm shows the stream survives a gap, the second
/// shows the gap was really there and a bound placed over it really fires.
/// This is the one place in the file where a duration decides an outcome,
/// and it decides it in both directions.
#[tokio::test(flavor = "multi_thread")]
async fn an_idle_stream_survives_by_default_and_is_cut_only_when_asked() {
    let server = spawn_server();

    // A. The default client: no bound of any kind.
    let resp = tokio::time::timeout(
        BOUND,
        client().execute(call(&server, "/g.S/Idle", RequestBody::Empty)),
    )
    .await
    .expect("must not hang")
    .expect("the head arrives at once");
    let (_, mut body) = resp.into_parts();
    let (data, _, trailers) = tokio::time::timeout(BOUND, read_to_end(&mut body))
        .await
        .expect("must not hang");
    assert_eq!(
        data,
        framed(b"late"),
        "a {IDLE_GAP:?} silence in the middle of a stream is not a failure"
    );
    assert!(trailers.is_some());

    // B. The same server, with a `between_bytes` bound under the gap.
    let bounded = Client::builder(transport())
        .timeouts(Timeouts {
            resolve: None,
            between_bytes: Some(IDLE_GAP / 4),
            ..Default::default()
        })
        .build()
        .unwrap();
    let resp = tokio::time::timeout(
        BOUND,
        bounded.execute(call(&server, "/g.S/Idle", RequestBody::Empty)),
    )
    .await
    .expect("must not hang")
    .expect("the head still arrives at once — the bound is on the gap after it");
    let (_, mut body) = resp.into_parts();
    let err = tokio::time::timeout(BOUND, async {
        loop {
            match next_frame(&mut body).await {
                Some(Ok(_)) => continue,
                Some(Err(e)) => return Some(e),
                None => return None,
            }
        }
    })
    .await
    .expect("must not hang")
    .expect("the bounded arm must fail rather than deliver");
    assert_eq!(
        err.kind(),
        &hclient_core::ErrorKind::Timeout(hclient_core::Phase::BetweenBytes),
        "the same gap the unbounded arm rode out, cut by the knob the \
         caller reached for: {err}"
    );
}

// ── 9. Connection management ────────────────────────────────────────────

/// **Two calls with trailers reuse one connection.**
///
/// The check-in happens on exactly one path in `H2Body::poll_frame` — the
/// arm where `poll_trailers` answers `None` — and a response *with*
/// trailers reaches that arm one poll later than a response without,
/// because the trailers frame is handed to the caller first. A gRPC call
/// always has trailers, so if that extra step lost the connection, every
/// RPC would pay a handshake. It does not: the server accepted one socket
/// for both calls.
#[tokio::test(flavor = "multi_thread")]
async fn two_calls_with_trailers_share_one_connection() {
    let server = spawn_server();
    let client = client();

    for _ in 0..2 {
        let resp = tokio::time::timeout(
            BOUND,
            client.execute(call(
                &server,
                "/g.S/Unary",
                RequestBody::Full(Bytes::from(framed(b"ping"))),
            )),
        )
        .await
        .expect("must not hang")
        .expect("the call must succeed");
        let (_, mut body) = resp.into_parts();
        let (data, _, trailers) = tokio::time::timeout(BOUND, read_to_end(&mut body))
            .await
            .expect("must not hang");
        assert_eq!(data, framed(b"pong"));
        assert!(trailers.is_some(), "every gRPC call ends in trailers");
    }

    assert!(server.wait_for_seen(2, BOUND).await);
    assert_eq!(
        server.accepted(),
        1,
        "the trailers frame is delivered before the body ends, and the \
         check-in is on the other side of that — a connection lost here \
         would cost a handshake per RPC"
    );
}

/// **Two calls in flight at once take two connections, not two streams.**
///
/// This is the gap between this client and a gRPC one, measured rather
/// than argued: gRPC's whole transport model is many concurrent calls
/// multiplexed over one HTTP/2 connection, and an h2 connection here is
/// **checked out of the pool exclusively** (`pool.rs`'s module doc, and
/// `http2.rs`'s "One stream per connection"). The reason is `Spawn`:
/// without a background task the only thing driving a connection is the
/// in-flight request futures, so a caller that stopped polling one stream
/// would stall its neighbours.
///
/// The barrier is what makes the count meaningful. `/g.S/Pair` answers
/// nobody until two requests have arrived, so the two calls provably
/// overlapped at the server; a client that had serialised them would hang
/// and fail on the ceiling rather than quietly report `1`.
///
/// **The gap is closed, on request** (v0.4): `Native::multiplexed()` makes
/// this `1`, and the sibling below asserts it on this same fixture. What
/// this test still measures is the **default**, which did not change —
/// a spawner is opt-in for the reason `pool.rs`'s module doc gives, so a
/// client that has not asked pays a connection per concurrent call exactly
/// as it did.
#[tokio::test(flavor = "multi_thread")]
async fn two_concurrent_calls_take_two_connections_rather_than_two_streams() {
    let server = spawn_server();
    let client = client();

    let one = client.execute(call(&server, "/g.S/Pair", RequestBody::Empty));
    let two = client.execute(call(&server, "/g.S/Pair", RequestBody::Empty));
    let (a, b) = tokio::time::timeout(BOUND, futures_util::future::join(one, two))
        .await
        .expect("both calls must be in flight at once — the server answers neither alone");
    assert_eq!(a.expect("first call").status(), 200);
    assert_eq!(b.expect("second call").status(), 200);

    assert!(server.wait_for_seen(2, BOUND).await);
    assert_eq!(
        server.accepted(),
        2,
        "one connection per concurrent call: the pool hands an h2 \
         connection out exclusively, so this client does not multiplex. \
         A recorded limitation with `Spawn` behind it, not a defect — but \
         a gRPC caller with many concurrent RPCs pays a connection for each"
    );
}

/// **A server's `PING` on a pooled connection is answered when the
/// connection is next used, and not before.**
///
/// The spec's §"PING Frame" says the peer *"must respond to"* a PING and
/// that *"an expired client initiated PING will cause all calls to be
/// closed"* — from which a gRPC deployment's keepalive is built. This
/// client answers, but only from inside a request: **nothing polls a
/// pooled connection**, which `pool.rs`'s module doc and
/// `http2::is_reusable`'s already say in as many words, and an h2 `PONG`
/// is written from inside `Connection::poll`.
///
/// An A/B rather than a bare negative, and causal in the arm that
/// matters: the gate is opened only after the first call's body has been
/// read to its end, so the PING provably goes out on a connection the
/// client has finished with; the quiet window then measures a client that
/// is doing nothing at all, and the second call — one connection, not two
/// — is what makes the pong arrive.
///
/// **The consequence for a gRPC caller, stated because it is not a
/// defect.** A server enforcing a keepalive deadline against an idle
/// pooled connection will drop it, and this client will discover that at
/// the next checkout (`is_reusable` polls once, sees the close, opens a
/// fresh connection) — a wasted socket, never a failed call, because a
/// connection with no call on it has no call to fail. There is no
/// keepalive knob on `Native`; the WebSocket seam has one for the
/// opposite reason, that an open WebSocket has no request future behind
/// it at all.
///
/// **`Native::multiplexed()` inverts arm A** (v0.4) — a driven connection
/// answers while it is idle, and the sibling below is the same fixture and
/// the same `QUIET` window with the assertion the other way round. Nothing
/// was written for it: it falls out of the connection having a poller.
#[tokio::test(flavor = "multi_thread")]
async fn a_ping_to_a_pooled_connection_waits_for_the_next_call() {
    let server = spawn_server();
    let client = client();

    let resp = tokio::time::timeout(
        BOUND,
        client.execute(call(&server, "/g.S/Ping", RequestBody::Empty)),
    )
    .await
    .expect("must not hang")
    .expect("the call must succeed");
    let (_, mut body) = resp.into_parts();
    let (_, _, trailers) = tokio::time::timeout(BOUND, read_to_end(&mut body))
        .await
        .expect("must not hang");
    assert!(
        trailers.is_some(),
        "the call is over, so the connection is idle"
    );

    // A. The connection is pooled and nobody is polling it.
    server.open_the_gate();
    assert!(
        !server.wait_for_pongs(1, QUIET).await,
        "a PONG within {QUIET:?} would mean something is driving a pooled \
         connection, which nothing in this transport does — there is no \
         spawn anywhere on this path"
    );

    // B. The same ping, answered, because a request is what polls the
    // connection.
    let again = tokio::time::timeout(
        BOUND,
        client.execute(call(
            &server,
            "/g.S/Unary",
            RequestBody::Full(Bytes::from(framed(b"ping"))),
        )),
    )
    .await
    .expect("must not hang")
    .expect("the call must succeed");
    let (_, mut body) = again.into_parts();
    let _ = tokio::time::timeout(BOUND, read_to_end(&mut body))
        .await
        .expect("must not hang");

    assert!(
        server.wait_for_pongs(1, BOUND).await,
        "the second call polls the connection, which is what writes the PONG"
    );
    assert_eq!(server.pongs(), vec![Pinged::Answered]);
    assert_eq!(
        server.accepted(),
        1,
        "and it is the SAME connection: an unanswered PING did not cost it"
    );
}

/// How long arm A of the ping test watches a connection nobody is
/// driving. Generous against the answer's own cost, which is one poll of
/// `Connection` inside `is_reusable` — microseconds once a call arrives.
const QUIET: Duration = Duration::from_millis(750);

/// **The same two calls with `multiplexed()`: one connection, two
/// streams.** L1 closed, and the name of the test above is what inverts.
///
/// The barrier is what makes it causal in the direction that matters:
/// `/g.S/Pair` answers nobody until both have arrived, so a client that
/// had one connection and used it serially would deadlock and fail on the
/// ceiling rather than report `1` for the wrong reason.
#[tokio::test(flavor = "multi_thread")]
async fn multiplexing_turns_those_two_connections_into_two_streams() {
    let server = spawn_server();
    let client = multiplexed_client();

    let one = client.execute(call(&server, "/g.S/Pair", RequestBody::Empty));
    let two = client.execute(call(&server, "/g.S/Pair", RequestBody::Empty));
    let (a, b) = tokio::time::timeout(BOUND, futures_util::future::join(one, two))
        .await
        .expect("both calls must be in flight at once — the server answers neither alone");
    assert_eq!(a.expect("first call").status(), 200);
    assert_eq!(b.expect("second call").status(), 200);

    assert!(server.wait_for_seen(2, BOUND).await);
    assert_eq!(
        server.accepted(),
        1,
        "two concurrent calls on one connection is gRPC's transport model, \
         and `Native::multiplexed()` is what asks for it"
    );
}

/// **With `multiplexed()`, a cancelled call is a `RST_STREAM(CANCEL)` and
/// its neighbour on the same connection survives.** L2 closed, and by
/// nothing written for it.
///
/// `http2::Pump`'s `Drop` has queued this frame since v0.2 W3 and its own
/// doc comment says why it was unobservable: the connection was owned by
/// the same future as the stream and went with it. A driver outlives the
/// stream, so the frame reaches the wire — and h2's own `maybe_cancel`
/// does the same for a request that had no body to pump.
///
/// **The neighbour is the point, and it is in flight rather than
/// afterwards.** W1's rule — cancelling one stream must not tear down the
/// others sharing its connection — held vacuously while there were no
/// others; here there is one, waiting out `/g.S/Idle`'s silence when the
/// cancellation happens, and it has to finish with its trailers on the
/// same connection.
#[tokio::test(flavor = "multi_thread")]
async fn a_multiplexed_cancellation_resets_the_stream_and_leaves_its_neighbour_alone() {
    let server = spawn_server();
    let client = multiplexed_client();

    // The doomed call: its head arrives and then nothing does.
    let doomed = tokio::time::timeout(
        BOUND,
        client.execute(call(&server, "/g.S/Cancel", RequestBody::Empty)),
    )
    .await
    .expect("must not hang")
    .expect("the head arrives, then nothing does");
    assert_eq!(doomed.status(), 200);

    // The neighbour, on the same connection and still in flight: the
    // server says nothing for `IDLE_GAP` before answering it.
    let survivor = client.execute(call(&server, "/g.S/Idle", RequestBody::Empty));
    let survivor = std::pin::pin!(survivor);

    // The caller walks away with the first call in progress.
    drop(doomed);

    let survivor = tokio::time::timeout(BOUND, survivor)
        .await
        .expect("must not hang")
        .expect("a cancelled stream must not take its neighbour with it");
    assert_eq!(survivor.status(), 200);
    let (_, mut body) = survivor.into_parts();
    let (_, _, trailers) = tokio::time::timeout(BOUND, read_to_end(&mut body))
        .await
        .expect("must not hang");
    assert!(
        trailers.is_some(),
        "the survivor's stream ran to its trailers"
    );

    assert!(
        server.wait_for_endings(1, BOUND).await,
        "the server must learn the cancelled call is over"
    );
    assert_eq!(
        server.endings(),
        vec![Ending::Reset(h2::Reason::CANCEL.to_string())],
        "the frame a gRPC client sends to cancel a call, and the variant \
         this fixture's `Ending` enum has had since the yardstick with \
         nothing to produce it. Written as `Reason::CANCEL` rather than as \
         the string h2 prints for it (\"stream no longer needed\"), so \
         the assertion names the code on the wire"
    );
    assert_eq!(
        server.accepted(),
        1,
        "one connection throughout — which is what makes the survivor's \
         stream a neighbour of the cancelled one rather than a stranger"
    );
}

/// **With `multiplexed()`, a server's `PING` is answered while the
/// connection is idle.** L3 closed, and by nothing written for it either.
///
/// The A/B is the test above: there, `QUIET` passes with no `PONG` because
/// nothing polls a pooled connection; here the driver does, so the same
/// ping on the same fixture comes back inside the same window and without
/// a second call being made.
///
/// Causal in the arm that matters: the gate is opened only after the
/// call's body has been read to its end, so the ping provably goes out on
/// a connection the client has finished with.
#[tokio::test(flavor = "multi_thread")]
async fn a_ping_to_a_shared_connection_is_answered_while_it_is_idle() {
    let server = spawn_server();
    let client = multiplexed_client();

    let resp = tokio::time::timeout(
        BOUND,
        client.execute(call(&server, "/g.S/Ping", RequestBody::Empty)),
    )
    .await
    .expect("must not hang")
    .expect("the call must succeed");
    let (_, mut body) = resp.into_parts();
    let (_, _, trailers) = tokio::time::timeout(BOUND, read_to_end(&mut body))
        .await
        .expect("must not hang");
    assert!(
        trailers.is_some(),
        "the call is over, so the connection is idle"
    );

    server.open_the_gate();
    assert!(
        server.wait_for_pongs(1, QUIET).await,
        "a driven connection answers a PING while nobody is making a \
         request on it — which is what an idle pooled connection cannot do"
    );
    assert_eq!(server.pongs(), vec![Pinged::Answered]);
    assert_eq!(
        server.accepted(),
        1,
        "and no second call and no second connection were needed to do it"
    );
}

/// **A `GOAWAY` is not raced: the next call opens a new connection and
/// succeeds.**
///
/// The spec's §"GOAWAY Frame" puts the duty on the client — *"clients
/// should consider any stream initiated after the last successfully
/// accepted stream as UNAVAILABLE and retry the call elsewhere"*. Here
/// "elsewhere" is a new connection, and the client never has to consider
/// anything unavailable because it never initiates that stream:
/// `http2::is_reusable` polls the pooled `Connection` once before offering
/// it, which is the only moment a `GOAWAY` on an otherwise idle socket can
/// be noticed, and `exchange` polls it once more while the request is
/// still ours — a `Failed::NotSent`, which is the verdict that permits the
/// retry.
///
/// Causal: the test waits for the server's connection task to have ENDED
/// before making the second call, so the second call provably faces a
/// pooled entry the peer has finished with, rather than racing the GOAWAY
/// across the wire.
#[tokio::test(flavor = "multi_thread")]
async fn a_goaway_costs_a_connection_and_not_the_next_call() {
    let server = spawn_server();
    let client = client();

    let resp = tokio::time::timeout(
        BOUND,
        client.execute(call(&server, "/g.S/Goaway", RequestBody::Empty)),
    )
    .await
    .expect("must not hang")
    .expect("the call the GOAWAY names must still be answered");
    let (_, mut body) = resp.into_parts();
    let (_, _, trailers) = tokio::time::timeout(BOUND, read_to_end(&mut body))
        .await
        .expect("must not hang");
    assert!(
        trailers.is_some(),
        "GOAWAY does not cut the stream it names"
    );

    assert!(
        server.wait_for_closed(1, BOUND).await,
        "the server's connection task must have finished — that is what \
         makes the next call face a dead pool entry rather than race one"
    );

    let again = tokio::time::timeout(
        BOUND,
        client.execute(call(
            &server,
            "/g.S/Unary",
            RequestBody::Full(Bytes::from(framed(b"ping"))),
        )),
    )
    .await
    .expect("must not hang")
    .expect("the call after a GOAWAY must go elsewhere, not fail");
    assert_eq!(again.status(), 200);
    let (_, mut body) = again.into_parts();
    let (data, _, trailers) = tokio::time::timeout(BOUND, read_to_end(&mut body))
        .await
        .expect("must not hang");
    assert_eq!(data, framed(b"pong"));
    assert!(trailers.is_some());
    assert_eq!(server.accepted(), 2, "elsewhere is a second connection");
}
