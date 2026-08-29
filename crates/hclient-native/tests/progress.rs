//! Octets, counted on the wire and reported to a hook.
//!
//! # Every number here is checked against what the server saw
//!
//! A counter is trivially right about itself, so nothing below asserts a
//! total on its own: the request-body total is compared with the octets
//! the server actually read after the head, and the response-body total
//! with the length the server actually wrote. That is `tests/hooks.rs`'s
//! rule — *every claim about a connection is the server's, not the
//! hook's* — applied to a count rather than to a connect.
//!
//! # What each test is paired against
//!
//! The two facts that need a control are the ones an implementation could
//! get right by accident. *The total is cumulative* is paired with a
//! multi-chunk upload, where a delta and a running total differ; *the
//! denominator is absent when nobody stated one* is paired with a
//! `Content-Length` response, where it is present.
#![cfg(not(target_family = "wasm"))]

use hclient::Client;
use hclient_core::unversioned::{Direction, Event, Hooks};
use hclient_dns_system::SystemDns;
use hclient_native::Native;
use hclient_rt_tokio::Tokio;
use hclient_tls_rustls::Rustls;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const BOUND: Duration = Duration::from_secs(30);

// ── the recorder ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Seen {
    direction: Direction,
    transferred: u64,
    expected: Option<u64>,
}

#[derive(Clone, Default)]
struct Recorder {
    seen: Arc<Mutex<Vec<Line>>>,
}

impl Recorder {
    fn lines(&self) -> Vec<Line> {
        self.seen.lock().expect("recorder").clone()
    }
    fn take(&self) -> Vec<Seen> {
        self.lines()
            .into_iter()
            .filter_map(|l| match l {
                Line::Progress(s) => Some(s),
                Line::Head => None,
            })
            .collect()
    }
    fn one_way(&self, d: Direction) -> Vec<Seen> {
        self.take()
            .into_iter()
            .filter(|s| s.direction == d)
            .collect()
    }
    fn totals(&self, d: Direction) -> Vec<u64> {
        self.one_way(d).into_iter().map(|s| s.transferred).collect()
    }
    /// The last thing said about a direction, which is the whole answer —
    /// the number is cumulative, so nothing earlier adds to it.
    fn last(&self, d: Direction) -> Option<Seen> {
        self.one_way(d).last().copied()
    }
}

/// Progress and heads in **one** list, because the order between them is
/// what one of the tests below is entirely about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Line {
    Progress(Seen),
    Head,
}

impl Hooks for Recorder {
    fn on(&self, event: &Event<'_>) {
        let line = match event {
            Event::Progress(p) => Line::Progress(Seen {
                direction: p.direction,
                transferred: p.transferred,
                expected: p.expected,
            }),
            Event::Head(_) => Line::Head,
            _ => return,
        };
        self.seen.lock().expect("recorder").push(line);
    }
}

// ── the server ──────────────────────────────────────────────────────────

/// How the fixture answers, and what it records about what it was sent.
#[derive(Clone)]
struct Behaviour {
    /// The response body, sent with an accurate `Content-Length` unless
    /// [`Self::chunked`].
    body: Vec<u8>,
    /// Answer with `Transfer-Encoding: chunked` and no `Content-Length`,
    /// so the client has no stated length to report as a denominator.
    chunked: bool,
    /// How many body octets arrived after the request head.
    request_octets: Arc<AtomicUsize>,
}

impl Behaviour {
    fn answering(body: &str) -> Self {
        Self {
            body: body.as_bytes().to_vec(),
            chunked: false,
            request_octets: Arc::new(AtomicUsize::new(0)),
        }
    }
}

fn server(behaviour: Behaviour) -> SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    std::thread::spawn(move || {
        for sock in listener.incoming() {
            let Ok(sock) = sock else { continue };
            let behaviour = behaviour.clone();
            std::thread::spawn(move || serve(sock, behaviour));
        }
    });
    addr
}

/// Reads one request — head, then exactly `Content-Length` body octets —
/// and answers. Deliberately not a general HTTP server: every request
/// this file sends declares a length, so counting to it is enough and
/// leaves nothing to guess about where the body ended.
fn serve(mut sock: std::net::TcpStream, behaviour: Behaviour) {
    sock.set_read_timeout(Some(BOUND)).expect("read timeout");
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
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
        let head = String::from_utf8_lossy(&buf[..head_end]).to_lowercase();
        let want: usize = head
            .lines()
            .find_map(|l| l.strip_prefix("content-length:"))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0);
        buf.drain(..head_end);
        while buf.len() < want {
            match sock.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
        behaviour.request_octets.fetch_add(want, Ordering::SeqCst);
        buf.drain(..want);

        let wrote = if behaviour.chunked {
            let mut out = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
            for piece in behaviour.body.chunks(4) {
                out.extend_from_slice(format!("{:x}\r\n", piece.len()).as_bytes());
                out.extend_from_slice(piece);
                out.extend_from_slice(b"\r\n");
            }
            out.extend_from_slice(b"0\r\n\r\n");
            sock.write_all(&out)
        } else {
            let mut out = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                behaviour.body.len()
            )
            .into_bytes();
            out.extend_from_slice(&behaviour.body);
            sock.write_all(&out)
        };
        if wrote.is_err() {
            return;
        }
    }
}

fn watched(rec: &Recorder) -> Native<Tokio, Rustls, SystemDns<Tokio>, Recorder> {
    Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio)).hooks(rec.clone())
}

// ── the tests ───────────────────────────────────────────────────────────

/// **A response body's octets are reported as they arrive, cumulatively,
/// and the count agrees with what the server wrote.**
#[tokio::test]
async fn a_response_body_is_counted_and_the_total_matches_what_the_server_wrote() {
    let body = "0123456789abcdefghij";
    let addr = server(Behaviour::answering(body));
    let rec = Recorder::default();
    let client = Client::builder(watched(&rec)).build().expect("build");

    let text = tokio::time::timeout(BOUND, client.get(format!("http://{addr}/")).send())
        .await
        .expect("must not hang")
        .expect("request")
        .collect()
        .await
        .expect("body")
        .text()
        .expect("text");
    assert_eq!(text, body, "premise: the whole body arrived");

    let seen = rec.last(Direction::Receiving).expect("a receiving total");
    assert_eq!(
        seen.transferred,
        body.len() as u64,
        "the count is the server's own byte count, not a guess",
    );
    assert_eq!(
        seen.expected,
        Some(body.len() as u64),
        "and `Content-Length` is the denominator",
    );
    let totals = rec.totals(Direction::Receiving);
    assert!(
        totals.windows(2).all(|w| w[0] < w[1]),
        "every event moved the total forward: {totals:?}",
    );
}

/// **A chunked response has no stated length, and the denominator is
/// absent rather than invented.**
///
/// The pair with the test above: same client, same body, one header
/// different. An implementation that answered `Some(0)` — or the running
/// total — would pass that one and fail this.
#[tokio::test]
async fn a_chunked_response_reports_no_denominator_at_all() {
    let body = "0123456789abcdefghij";
    let addr = server(Behaviour {
        chunked: true,
        ..Behaviour::answering(body)
    });
    let rec = Recorder::default();
    let client = Client::builder(watched(&rec)).build().expect("build");

    let text = tokio::time::timeout(BOUND, client.get(format!("http://{addr}/")).send())
        .await
        .expect("must not hang")
        .expect("request")
        .collect()
        .await
        .expect("body")
        .text()
        .expect("text");
    assert_eq!(text, body, "premise: the whole body arrived");

    let seen = rec.last(Direction::Receiving).expect("a receiving total");
    assert_eq!(seen.transferred, body.len() as u64);
    assert_eq!(
        seen.expected, None,
        "nobody stated a length, so there is no denominator to report",
    );
}

/// **A request body is counted, and the total is the server's count of
/// what it read.**
#[tokio::test]
async fn a_request_body_is_counted_and_the_total_matches_what_the_server_read() {
    let behaviour = Behaviour::answering("ok");
    let read_by_server = behaviour.request_octets.clone();
    let addr = server(behaviour);
    let rec = Recorder::default();
    let client = Client::builder(watched(&rec)).build().expect("build");

    let payload = "x".repeat(1024);
    let resp = tokio::time::timeout(
        BOUND,
        client
            .post(format!("http://{addr}/"))
            .body(hclient_core::RequestBody::Full(bytes::Bytes::from(
                payload.clone(),
            )))
            .send(),
    )
    .await
    .expect("must not hang")
    .expect("request");
    resp.collect().await.expect("body");

    assert_eq!(
        read_by_server.load(Ordering::SeqCst),
        payload.len(),
        "premise: the server read the whole request body",
    );
    let seen = rec.last(Direction::Sending).expect("a sending total");
    assert_eq!(seen.transferred, payload.len() as u64);
    assert_eq!(
        seen.expected,
        Some(payload.len() as u64),
        "a buffered body knows its own length before a byte of it moves",
    );
}

/// **A request that carries no body reports nothing in that direction.**
///
/// No traffic, no event — the rule that lets a hook read the absence of
/// `Sending` events as *this request had nothing to send* rather than as
/// *this backend does not report uploads*.
#[tokio::test]
async fn a_request_with_no_body_reports_no_sending_progress() {
    let addr = server(Behaviour::answering("ok"));
    let rec = Recorder::default();
    let client = Client::builder(watched(&rec)).build().expect("build");

    let resp = tokio::time::timeout(BOUND, client.get(format!("http://{addr}/")).send())
        .await
        .expect("must not hang")
        .expect("request");
    resp.collect().await.expect("body");

    assert!(
        rec.one_way(Direction::Sending).is_empty(),
        "a GET with no body moved no octets out: {:?}",
        rec.one_way(Direction::Sending),
    );
    assert!(
        !rec.one_way(Direction::Receiving).is_empty(),
        "premise: the other direction did report, so silence above is about the body",
    );
}

/// **An empty response body reports nothing either**, which is the same
/// rule pointed the other way and the control for the test above.
#[tokio::test]
async fn an_empty_response_body_reports_no_receiving_progress() {
    let addr = server(Behaviour::answering(""));
    let rec = Recorder::default();
    let client = Client::builder(watched(&rec)).build().expect("build");

    let resp = tokio::time::timeout(BOUND, client.get(format!("http://{addr}/")).send())
        .await
        .expect("must not hang")
        .expect("request");
    assert_eq!(resp.status(), 200);
    resp.collect().await.expect("body");

    assert!(
        rec.one_way(Direction::Receiving).is_empty(),
        "a zero-length body moved no octets: {:?}",
        rec.one_way(Direction::Receiving),
    );
}

/// **A build with no hook counts nothing and reports nothing**, which is
/// what `Hooks::WATCHING` promises and what keeps the whole feature out of
/// a default build.
///
/// Asserted through behaviour rather than by reading the const: the
/// request still works, which is the half a broken gate would break.
#[tokio::test]
async fn an_unwatched_transport_still_serves_the_request() {
    let addr = server(Behaviour::answering("ok"));
    let client = Client::builder(Native::new(
        Tokio,
        Rustls::with_webpki_roots(),
        SystemDns::new(Tokio),
    ))
    .build()
    .expect("build");

    let text = tokio::time::timeout(
        BOUND,
        client
            .post(format!("http://{addr}/"))
            .body(hclient_core::RequestBody::Full(bytes::Bytes::from_static(
                b"payload",
            )))
            .send(),
    )
    .await
    .expect("must not hang")
    .expect("request")
    .collect()
    .await
    .expect("body")
    .text()
    .expect("text");
    assert_eq!(text, "ok");
}

/// **Two composed hooks both see every progress event**, through the real
/// transport rather than by calling `on` by hand.
///
/// `tests/hooks_compose.rs` in `hclient-core` pins the ordering and the
/// `WATCHING` join with no socket; this is the same composition installed
/// where a caller would install it, which is what says `Native::hooks`
/// accepts an `And` at all.
#[tokio::test]
async fn a_composed_hook_delivers_progress_to_both_halves() {
    use hclient_core::unversioned::HooksExt as _;

    let addr = server(Behaviour::answering("0123456789"));
    let one = Recorder::default();
    let two = Recorder::default();
    let client = Client::builder(
        Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio))
            .hooks(one.clone().and(two.clone())),
    )
    .build()
    .expect("build");

    tokio::time::timeout(BOUND, client.get(format!("http://{addr}/")).send())
        .await
        .expect("must not hang")
        .expect("request")
        .collect()
        .await
        .expect("body");

    assert_eq!(
        one.last(Direction::Receiving).map(|s| s.transferred),
        Some(10),
    );
    assert_eq!(
        two.last(Direction::Receiving).map(|s| s.transferred),
        Some(10),
        "the second half of the composition saw the same events",
    );
}

/// **An upload is reported while it is happening, not once it is over.**
///
/// This is the whole reason there is a reporter around the exchange future
/// at all, and it is the one claim the totals above cannot make. On
/// HTTP/1 the request body is fully written before the response head
/// arrives, so a client that only reported what its *response* body could
/// see would hand a caller one `Sending` event at the end — a progress bar
/// that reads 0% for the length of the upload and then 100%. The
/// discrimination is therefore an **ordering** one: at least one `Sending`
/// event must precede the `Head`.
///
/// The body is streamed in eight chunks rather than buffered, so hyper
/// pulls it frame by frame and the exchange future is polled between them
/// — which is where the reporter looks. Nothing here is timed: the
/// assertion is on the order two events were recorded in, so a slow
/// machine changes nothing.
#[tokio::test]
async fn an_upload_is_reported_before_the_head_rather_than_only_after_it() {
    let behaviour = Behaviour::answering("ok");
    let read_by_server = behaviour.request_octets.clone();
    let addr = server(behaviour);
    let rec = Recorder::default();
    let client = Client::builder(watched(&rec)).build().expect("build");

    const CHUNKS: usize = 8;
    const CHUNK: usize = 8 * 1024;
    let len = (CHUNKS * CHUNK).to_string();
    let resp = tokio::time::timeout(
        BOUND,
        client
            .post(format!("http://{addr}/"))
            // `Content-Length` by hand, because a streaming body states no
            // length and this fixture reads exactly what the head declares.
            .header("content-length", &len)
            .body(hclient_core::RequestBody::Streaming(Box::new(Chunks::new(
                CHUNKS, CHUNK,
            ))))
            .send(),
    )
    .await
    .expect("must not hang")
    .expect("request");
    resp.collect().await.expect("body");

    assert_eq!(
        read_by_server.load(Ordering::SeqCst),
        CHUNKS * CHUNK,
        "premise: the server read the whole streamed body",
    );

    let lines = rec.lines();
    let head_at = lines
        .iter()
        .position(|l| matches!(l, Line::Head))
        .expect("premise: a head was reported");
    let sending_before_head = lines[..head_at]
        .iter()
        .filter(|l| matches!(l, Line::Progress(s) if s.direction == Direction::Sending))
        .count();
    assert!(
        sending_before_head > 0,
        "the upload must be reported while it is running, not only once the \
         response arrives: {lines:?}",
    );
    assert_eq!(
        rec.last(Direction::Sending).map(|s| s.transferred),
        Some((CHUNKS * CHUNK) as u64),
        "and the last word is still the whole body",
    );
    assert_eq!(
        rec.last(Direction::Sending).and_then(|s| s.expected),
        None,
        "a streaming body states no exact length, so there is no denominator \
         — the header the caller wrote is theirs, not the body's",
    );
}

/// A request body of `n` equal chunks, streamed rather than buffered, so
/// the transport pulls it one frame at a time.
struct Chunks {
    left: usize,
    each: usize,
}

impl Chunks {
    fn new(count: usize, each: usize) -> Self {
        Self { left: count, each }
    }
}

impl http_body::Body for Chunks {
    type Data = bytes::Bytes;
    type Error = hclient_core::Error;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<bytes::Bytes>, Self::Error>>> {
        if self.left == 0 {
            return std::task::Poll::Ready(None);
        }
        self.left -= 1;
        std::task::Poll::Ready(Some(Ok(http_body::Frame::data(bytes::Bytes::from(vec![
            b'x';
            self.each
        ])))))
    }
}
