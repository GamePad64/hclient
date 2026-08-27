//! Full duplex on the HTTP/2 path, measured from the server.
//!
//! `http2::exchange` used to write the whole request body and then wait
//! for the response — the v0.4 design document's P6, *"`full_duplex:
//! false` is not a declaration, it is the code"*. It no longer is, and
//! this file is where that is checked rather than asserted.
//!
//! # The duplex test is causal, not timed
//!
//! `a_response_head_arrives_while_the_request_body_is_still_going_out`
//! measures no threshold. The caller's body produces its second chunk
//! **only after `send()` has returned a response head** — the chunk is not
//! in the channel until then — so a transport that finished the body
//! before reading the head could not complete the exchange at all, at any
//! speed. The server's own clock is the second witness: it records when it
//! sent the head and when the last request byte arrived, and the first is
//! before the second.
//!
//! `a_stalled_upload_does_not_stall_the_response_body` is causal in the
//! same way and needs no clock at all: HTTP/2's default flow-control
//! window is 65 535 bytes at both the stream and the connection level, and
//! `/hold` never reads a byte of the request, so it never enlarges either.
//! A request body sixteen times that size **cannot** be written, whatever
//! the scheduler does.
//!
//! Three timing-based assertions elsewhere in this workspace turned out to
//! be flakes in one week, so the only `Duration`s here are ceilings that
//! turn a hang into a named failure.
//!
//! # What is NOT here
//!
//! `Capabilities::full_duplex` is still `false`, and
//! `tests/http2.rs`'s `capabilities_report_the_floor_with_the_feature_on`
//! is still the test that says so. The floor is a *static* answer for a
//! transport that negotiates HTTP/1.1 whenever ALPN says so, in a build
//! whose `http2` feature may have been turned on by a crate that never
//! asked this one — see `http2.rs`'s module doc.
//!
//! # Why this file carries its own TLS stub
//!
//! ALPN is the only route to HTTP/2 here and `TlsConnect` is what reports
//! it; `tests/http2.rs`'s module doc argues the technique at length, and
//! `tests/stream_reset.rs` already carries the second copy. The hazard of
//! another one is that a mis-wired stub would silently test HTTP/1.1
//! instead, so every test below either asserts `Response::version()` —
//! which only the `h2` crate can set — or talks to a server that speaks
//! nothing else.
#![cfg(all(feature = "http2", not(target_family = "wasm")))]

use bytes::Bytes;
use hclient::Client;
use hclient_core::RequestBody;
use hclient_dns_system::SystemDns;
use hclient_native::Native;
use hclient_rt_tokio::Tokio;
use hclient_tls::{TlsConfigId, TlsConnect, TlsIdentity, TlsInfo, TlsRequest};
use std::error::Error as StdError;
use std::fmt::Display;
use std::future::poll_fn;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

/// Ceiling for anything that must not hang. Every failure mode in this
/// file is a hang if the client gets it wrong, so the bound is what turns
/// "wrong" into a named failure instead of a stuck run.
const BOUND: Duration = Duration::from_secs(30);

/// One HTTP/2 flow-control window, which is what `/hold` never enlarges.
const WINDOW: usize = 65_535;

/// What the server made of one request stream. Every claim about what
/// crossed the wire is read from here, never from the client's own account
/// of itself.
#[derive(Debug, Clone, Copy)]
struct Report {
    bytes: usize,
    /// The request stream ended cleanly, rather than being reset or
    /// cut off by the connection going away.
    complete: bool,
    /// When the server sent the response head, measured from its own
    /// start — `None` for a handler that answers only at the end.
    head_sent: Option<Duration>,
    /// When the last request byte arrived, on the same clock.
    last_byte: Option<Duration>,
}

struct Fixture {
    addr: SocketAddr,
    /// TCP connections accepted. Claims about pooling are this number.
    accepted: Arc<AtomicUsize>,
    reports: Arc<Mutex<Vec<Report>>>,
}

impl Fixture {
    fn url(&self, path: &str) -> String {
        format!("https://{}{}", self.addr, path)
    }

    fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }

    fn reports(&self) -> Vec<Report> {
        self.reports.lock().unwrap().clone()
    }

    /// Waits for the server to have finished with `n` request streams, or
    /// gives up. Returning `false` rather than hanging is what makes a
    /// missing reset a failed assertion instead of a stuck suite.
    async fn wait_for_reports(&self, n: usize, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if self.reports().len() >= n {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }
}

fn spawn_server() -> Fixture {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let accepted = Arc::new(AtomicUsize::new(0));
    let reports: Arc<Mutex<Vec<Report>>> = Arc::new(Mutex::new(Vec::new()));

    let accepted_for_thread = Arc::clone(&accepted);
    let reports_for_thread = Arc::clone(&reports);
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
                let reports = Arc::clone(&reports_for_thread);
                tokio::spawn(async move {
                    let _ = serve(tcp, reports).await;
                });
            }
        });
    });

    Fixture {
        addr,
        accepted,
        reports,
    }
}

/// One connection's worth of HTTP/2, and nothing else — no HTTP/1
/// fallback, so a client that did not really speak h2 would get no answer
/// at all rather than a degraded one.
///
/// Handlers are spawned rather than awaited inline for the reason h2's own
/// server example gives: `Connection::accept` is what drives the
/// connection's IO, so a handler awaited inside the accept loop would
/// stall the very connection it needs flushed. Every path below depends on
/// that directly — each one sends something and then reads.
async fn serve(
    tcp: tokio::net::TcpStream,
    reports: Arc<Mutex<Vec<Report>>>,
) -> Result<(), h2::Error> {
    let mut conn = h2::server::handshake(tcp).await?;
    while let Some(accepted) = conn.accept().await {
        let (req, respond) = accepted?;
        let path = req.uri().path().to_owned();
        let (_parts, body) = req.into_parts();
        let reports = Arc::clone(&reports);
        tokio::spawn(async move {
            let _ = handle(path, respond, body, reports).await;
        });
    }
    Ok(())
}

fn ok() -> http::Response<()> {
    http::Response::builder().status(200).body(()).unwrap()
}

async fn handle(
    path: String,
    mut respond: h2::server::SendResponse<Bytes>,
    mut body: h2::RecvStream,
    reports: Arc<Mutex<Vec<Report>>>,
) -> Result<(), h2::Error> {
    let start = Instant::now();
    match path.as_str() {
        // Answers first, reads afterwards — the shape only a duplex client
        // can finish. The head goes out before a single request byte has
        // been read, and the response body is the byte count, so a client
        // that never sent the rest gets no answer at all.
        "/duplex" => {
            let mut send = respond.send_response(ok(), false)?;
            let head_sent = Some(start.elapsed());
            let mut report = drain(&mut body, start, None).await;
            report.head_sent = head_sent;
            reports.lock().unwrap().push(report);
            send.send_data(Bytes::from(format!("{} bytes", report.bytes)), true)?;
        }
        // Reads the whole request, then answers with what it counted.
        "/count" => {
            let report = drain(&mut body, start, None).await;
            reports.lock().unwrap().push(report);
            let mut send = respond.send_response(ok(), false)?;
            send.send_data(Bytes::from(format!("{} bytes", report.bytes)), true)?;
        }
        // The complete response, and then **not one byte of the request
        // read and the stream not dropped either**. Dropping the
        // `RecvStream` is what would schedule a `RST_STREAM(NO_ERROR)`
        // (`h2-0.4.15/src/proto/streams/streams.rs:1601`, `maybe_cancel`),
        // and that is precisely the message this path must not send: it
        // exists to leave the client's request stream stuck with no
        // flow-control window and no reset to end it.
        "/hold" => {
            let mut send = respond.send_response(ok(), false)?;
            send.send_data(Bytes::from_static(b"answered"), true)?;
            tokio::time::sleep(Duration::from_secs(60)).await;
            drop(body);
        }
        // The complete response is a full flow-control window of body and
        // then a `RST_STREAM(NO_ERROR)` — but not until the client has read
        // part of it. The reservation below can only be granted by a
        // `WINDOW_UPDATE`, which the client sends once it has actually
        // consumed some of the response, so "the reset arrives while the
        // client is driving its stuck upload from the response body" is a
        // fact about this handler rather than a race it usually wins.
        //
        // The request stream is neither read nor dropped, so the reset
        // above is the only one on this stream.
        "/answer-then-reset" => {
            let mut send = respond.send_response(ok(), false)?;
            send.send_data(Bytes::from(vec![b'y'; WINDOW]), false)?;
            send.reserve_capacity(1);
            let _ = poll_fn(|cx| send.poll_capacity(cx)).await;
            send.send_reset(h2::Reason::NO_ERROR);
            tokio::time::sleep(Duration::from_secs(60)).await;
            drop(body);
        }
        // The complete response, and then the request stream dropped
        // rather than read — which is what makes h2 schedule a
        // `RST_STREAM(NO_ERROR)`. The report goes in afterwards, so a test
        // can wait for the reset to have been scheduled rather than
        // guessing at it.
        "/answer-and-drop" => {
            let mut send = respond.send_response(ok(), false)?;
            send.send_data(Bytes::from_static(b"answered"), true)?;
            drop(send);
            drop(respond);
            drop(body);
            reports.lock().unwrap().push(Report {
                bytes: 0,
                complete: false,
                head_sent: Some(start.elapsed()),
                last_byte: None,
            });
        }
        // Reads at a frame every 40 ms and answers only at the end, so a
        // request cancelled part-way through is cancelled while the server
        // is still reading — and the server is the one that says how much
        // it got and whether the stream ended cleanly.
        "/slow" => {
            let report = drain(&mut body, start, Some(Duration::from_millis(40))).await;
            reports.lock().unwrap().push(report);
            let mut send = respond.send_response(ok(), false)?;
            send.send_data(Bytes::from(format!("{} bytes", report.bytes)), true)?;
        }
        _ => {
            let mut send = respond.send_response(ok(), false)?;
            send.send_data(Bytes::from_static(b"ok"), true)?;
        }
    }
    Ok(())
}

/// Reads a request body to its end, or to whatever ended it.
async fn drain(body: &mut h2::RecvStream, start: Instant, per_frame: Option<Duration>) -> Report {
    let mut report = Report {
        bytes: 0,
        complete: false,
        head_sent: None,
        last_byte: None,
    };
    loop {
        match body.data().await {
            Some(Ok(chunk)) => {
                report.bytes += chunk.len();
                report.last_byte = Some(start.elapsed());
                // Not optional: without releasing it the client stops after
                // one window, which is what `/hold` wants and no other path
                // does.
                let _ = body.flow_control().release_capacity(chunk.len());
                if let Some(d) = per_frame {
                    tokio::time::sleep(d).await;
                }
            }
            Some(Err(_)) => return report,
            None => {
                report.complete = true;
                return report;
            }
        }
    }
}

// ── the caller's bodies ─────────────────────────────────────────────────

/// A body the test feeds by hand, counting what was taken from it.
///
/// An unbounded channel is what makes the duplex test causal: a chunk that
/// has not been sent into it yet cannot be pulled, whatever the transport
/// does, so "the second chunk exists only after the head arrived" is a fact
/// about the fixture rather than a race the fixture usually wins.
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

fn feed() -> (
    tokio::sync::mpsc::UnboundedSender<Bytes>,
    RequestBody,
    Arc<AtomicUsize>,
) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let pulled = Arc::new(AtomicUsize::new(0));
    let body = Feed {
        rx,
        pulled: Arc::clone(&pulled),
    };
    (tx, RequestBody::Streaming(Box::new(body)), pulled)
}

/// `frames` chunks of the same bytes, always ready, counting what was
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

/// One chunk and then a failure the test can recognise again on the other
/// side of the transport.
///
/// A marker type rather than a message, because the claim is that *this*
/// error came back — and matching on a `Display` string would pass for a
/// transport that invented an error of its own with a similar wording.
struct FailsAfterOneChunk {
    sent: bool,
}

#[derive(Debug)]
struct TheCallersOwnBodyError;

impl Display for TheCallersOwnBodyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the caller's body gave up")
    }
}

impl StdError for TheCallersOwnBodyError {}

impl http_body::Body for FailsAfterOneChunk {
    type Data = Bytes;
    type Error = hclient_core::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
        if self.sent {
            return Poll::Ready(Some(Err(hclient_core::Error::new(
                hclient_core::ErrorKind::Body,
                TheCallersOwnBodyError,
            ))));
        }
        self.sent = true;
        Poll::Ready(Some(Ok(http_body::Frame::data(Bytes::from_static(
            b"a first chunk",
        )))))
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

    type Handshake<'a, S>
        = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(S, TlsInfo), hclient_core::Error>> + 'a>,
    >
    where
        Self: 'a,
        S: hyper::rt::Read + hyper::rt::Write + Unpin + 'a;

    fn connect<'a, S>(&'a self, io: S, _req: TlsRequest<'a>) -> Self::Handshake<'a, S>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin + 'a,
    {
        Box::pin(async move {
            Ok((
                io,
                TlsInfo {
                    alpn: Some(b"h2".to_vec()),
                    ..Default::default()
                },
            ))
        })
    }
}

fn client() -> Client {
    Client::builder(Native::new(Tokio, FakeTls::new(), SystemDns::new(Tokio)))
        .build()
        .unwrap()
}

// ── duplex ──────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn a_response_head_arrives_while_the_request_body_is_still_going_out() {
    // **The acceptance**, and the exchange below cannot complete without
    // duplex — no threshold, no timing luck.
    //
    // The caller's body holds one chunk. The second is put into its channel
    // only AFTER `send()` has returned a response head, so a transport that
    // wrote the whole request body before reading the head would be waiting
    // for a chunk that is waiting for it. The `timeout` turns that into a
    // failed assertion instead of a hung suite.
    const A: usize = 4096;
    const B: usize = 8192;

    let server = spawn_server();
    let client = client();
    let (tx, body, _) = feed();
    tx.send(Bytes::from(vec![b'a'; A])).unwrap();

    let resp = tokio::time::timeout(BOUND, client.post(&server.url("/duplex")).body(body).send())
        .await
        .expect(
            "the head is on the wire and the second chunk is not: a transport that \
         waits for the body before reading the head deadlocks here, which is \
         exactly what P6 said the code did",
        )
        .expect("request must succeed");
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.version(),
        http::Version::HTTP_2,
        "set by the h2 crate while decoding a real HEADERS frame — the \
         witness that this went out as HTTP/2 at all"
    );

    // Only now does the rest of the request body exist.
    tx.send(Bytes::from(vec![b'b'; B])).unwrap();
    drop(tx);

    // Collecting the response is what drives the remaining upload: the pump
    // moved into the body, and nothing else polls it.
    let text = tokio::time::timeout(BOUND, resp.collect())
        .await
        .expect("the rest of the upload rides on the response body being read")
        .expect("the body must arrive")
        .text()
        .unwrap();
    assert_eq!(
        text,
        format!("{} bytes", A + B),
        "the count is the server's: both chunks crossed"
    );

    assert!(server.wait_for_reports(1, BOUND).await);
    let report = server.reports()[0];
    assert_eq!(report.bytes, A + B);
    assert!(report.complete, "the stream ended cleanly: {report:?}");

    // The server's own clock as the second witness: it sent the head
    // before the last request byte reached it.
    let (head, last) = (
        report.head_sent.expect("/duplex sends one"),
        report.last_byte.expect("a body arrived"),
    );
    println!("server: head sent at {head:?}, last request byte at {last:?}");
    assert!(
        head < last,
        "the server answered before it had the whole request, which is what \
         the client was able to act on: head {head:?}, last byte {last:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stalled_upload_does_not_stall_the_response_body() {
    // The other half of duplex, and nothing else here reaches it.
    //
    // `H2Body::poll_frame` drives the request pump before it reads the
    // response, and it must **not** hand back the pump's `Pending` as its
    // own: the two halves of a stream are independent, so a write that
    // cannot proceed has to leave the read alone. On `hclient-h3` the
    // mutation that returns `Poll::Pending` whenever the pump is unfinished
    // left all 43 tests green, which is why this one exists here from the
    // start rather than after the fact.
    //
    // `/hold` sends the whole response and then holds the request stream
    // open without reading a byte of it, so no `RST_STREAM` comes back and
    // no flow-control window is ever granted: the pump is stuck for the
    // rest of the test, by construction rather than by timing.
    const FRAMES: usize = 256;
    const CHUNK: usize = 16 * 1024;

    let server = spawn_server();
    let client = client();
    let (body, pulled) = repeat(FRAMES, CHUNK);

    let resp = tokio::time::timeout(BOUND, client.post(&server.url("/hold")).body(body).send())
        .await
        .expect("the head is sent before anything is read")
        .expect("request must succeed");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.version(), http::Version::HTTP_2);

    let text = tokio::time::timeout(BOUND, resp.collect())
        .await
        .expect(
            "the response body is on the wire and the request body cannot \
             move: a body that returned the pump's Pending as its own hangs \
             here",
        )
        .expect("the body must arrive")
        .text()
        .unwrap();
    assert_eq!(text, "answered");

    // And the upload really was stuck, or the paragraph above describes a
    // situation this test never entered. One window is 65 535 bytes and the
    // body is 4 MiB.
    let n = pulled.load(Ordering::SeqCst);
    println!("frames pulled while the peer read nothing: {n} of {FRAMES}");
    assert!(
        n * CHUNK <= WINDOW + CHUNK,
        "the peer granted no window at all, so at most one window plus the \
         chunk in hand can have been pulled; got {n} of {FRAMES}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_connection_whose_upload_never_finished_is_not_pooled() {
    // The check-in duty duplex created. A response can now end while the
    // request body is still going out — `/hold` is exactly that — and the
    // stream it leaves behind was never finished. A connection whose last
    // stream ended that way is not the evidence a check-in is made of, so
    // `H2Body::end` drops the pump and the reuse together.
    //
    // The server counts the connections, which is the only place the
    // decision is visible from outside.
    const FRAMES: usize = 256;
    const CHUNK: usize = 16 * 1024;

    let server = spawn_server();
    let client = client();
    let (body, _) = repeat(FRAMES, CHUNK);

    let resp = tokio::time::timeout(BOUND, client.post(&server.url("/hold")).body(body).send())
        .await
        .expect("must not hang")
        .expect("request must succeed");
    let text = tokio::time::timeout(BOUND, resp.collect())
        .await
        .expect("must not hang")
        .expect("the body must arrive")
        .text()
        .unwrap();
    assert_eq!(text, "answered");

    let second = tokio::time::timeout(BOUND, client.get(&server.url("/other")).send())
        .await
        .expect("must not hang")
        .expect("the next request must still work");
    assert_eq!(second.version(), http::Version::HTTP_2);
    assert_eq!(second.collect().await.unwrap().text().unwrap(), "ok");

    assert_eq!(
        server.accepted(),
        2,
        "the first connection carried a request that was never finished, so \
         it must not have been handed back to the pool — `two_requests_travel\
         _over_one_http2_connection` in tests/http2.rs is the control that \
         says reuse otherwise works"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reset_while_the_body_drives_the_pump_does_not_discard_the_response() {
    // RFC 9113 §8.1 — *"Clients MUST NOT discard responses as a result of
    // receiving such a `RST_STREAM`"* — met where only duplex can put it:
    // **in the middle of a pump that `H2Body` is driving**, rather than in
    // the middle of `exchange`.
    //
    // This test exists because duplex took a kill away. Before it,
    // `stream_reset.rs`'s `a_stalled_streaming_body_…` was what
    // distinguished asking `SendStream::poll_reset` at the top of the pump
    // loop from tolerating a `Ready(None)` at `poll_capacity`: with the
    // question asked anywhere else, a pump parked on the caller's body
    // never noticed the reset and `exchange` hung. Now `exchange` does not
    // hang — it stops waiting for the pump and takes the head — so that
    // test passes either way and the discrimination moved here.
    //
    // Without the gate the pump's next write fails
    // (`StreamClosedWhileSendingTheRequestBody`, or an
    // `InactiveStreamId` from one of the three sites no public h2 API can
    // tell from an API misuse of ours), `drive_pump` hands that back, and
    // the response body — every byte of which is already in hand — fails.
    // A response whose body cannot be read is a response discarded,
    // whatever the head says.
    const FRAMES: usize = 256;
    const CHUNK: usize = 16 * 1024;

    let server = spawn_server();
    let client = client();
    let (body, pulled) = repeat(FRAMES, CHUNK);

    let resp = tokio::time::timeout(
        BOUND,
        client
            .post(&server.url("/answer-then-reset"))
            .body(body)
            .send(),
    )
    .await
    .expect("must not hang")
    .expect("the head is sent long before the reset");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.version(), http::Version::HTTP_2);

    let collected = tokio::time::timeout(BOUND, resp.collect())
        .await
        .expect("must not hang")
        .expect(
            "the reset acts on the request stream and says the response was              complete; a client that turned it into a write error would fail              the response body it had already received in full",
        )
        ;
    let bytes = collected.bytes();
    assert_eq!(
        bytes.len(),
        WINDOW,
        "and every byte the server sent before the reset is handed over"
    );

    // The upload really was still in flight when the reset landed, or the
    // paragraph above describes a situation this test never entered.
    let n = pulled.load(Ordering::SeqCst);
    println!("frames pulled from the caller's body: {n} of {FRAMES}");
    assert!(
        n > 0 && n < FRAMES,
        "the pump must have been mid-body: {n} of {FRAMES}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_body_that_ends_just_after_the_peer_stopped_reading_is_not_an_error() {
    // The write site the tolerance's *placement* turns on, reached where
    // only duplex can reach it.
    //
    // Three of the pump's four writes fail with `UserError::
    // InactiveStreamId` on a reset stream, which no public h2 API can tell
    // from an API misuse of ours — which is why the question is asked once,
    // of `SendStream::poll_reset`, at the top of the loop. A tolerance
    // placed at `poll_capacity` instead covers a *large* body, because a
    // reset stream has no capacity and every large body goes through that
    // site; it does not cover a body that simply **ends** while the stream
    // is reset, and `send_data(Bytes::new(), true)` is then the write that
    // fails.
    //
    // The moment is constructed rather than timed. `/answer-and-drop`
    // answers in full and drops the request stream, then reports; the test
    // waits for that report before closing the caller's body, so the
    // reset has been scheduled before there is anything left to write. The
    // client decodes it in the same `poll_frame` — `Connection::poll` runs
    // before the pump, which is what that order is for.
    let server = spawn_server();
    let client = client();
    let (tx, body, _) = feed();
    tx.send(Bytes::from_static(b"a first chunk")).unwrap();

    let resp = tokio::time::timeout(
        BOUND,
        client
            .post(&server.url("/answer-and-drop"))
            .body(body)
            .send(),
    )
    .await
    .expect("must not hang")
    .expect("the server answered without reading; that is the whole exchange");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.version(), http::Version::HTTP_2);

    assert!(
        server.wait_for_reports(1, BOUND).await,
        "the server must have dropped the request stream, which is what \
         schedules the RST_STREAM"
    );
    // Only now does the caller's body end, and ending it is the write that
    // meets the reset.
    drop(tx);

    let text = tokio::time::timeout(BOUND, resp.collect())
        .await
        .expect("must not hang")
        .expect(
            "the request stream is reset and the caller's body has just \
             ended: end-of-stream cannot be written, and that is not a \
             failure of a response the server already sent in full",
        )
        .text()
        .unwrap();
    assert_eq!(text, "answered");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_body_that_fails_fails_the_request_with_the_callers_own_error() {
    // **A pump error is a verdict, and a reset is not a tolerance.**
    //
    // The two are one branch apart in `exchange`: `Pump::poll` answers
    // `Ready(Ok(PeerStoppedReading))` for a peer that stopped reading, and
    // `Ready(Err(e))` for everything else. Widening the second into the
    // first — "stop pumping and let `resp_fut` answer" — is the mutation
    // `http2.rs`'s doc comment used to claim was killed by
    // `stream_reset.rs`'s `a_connection_that_dies_mid_request_is_still_an_
    // error`. Measured, it was not: that mutation left all 14 tests green
    // on the code as it stood BEFORE duplex, and all 22 after. The reason
    // is that the connection is polled first, so a dead connection is
    // reported by `Connection::poll` and the pump's verdict is never
    // consulted.
    //
    // This is the case where it is consulted, and it is a caller's error
    // rather than a network one: a request body that fails part-way. The
    // widened version resets the stream, defers to `resp_fut` and hands
    // back h2's reset error — a failure either way, so `expect_err` alone
    // would not notice. What it loses is *whose* error it is, so that is
    // what is asserted.
    let server = spawn_server();
    let client = client();

    let err = tokio::time::timeout(
        BOUND,
        client
            .post(&server.url("/count"))
            .body(RequestBody::Streaming(Box::new(FailsAfterOneChunk {
                sent: false,
            })))
            .send(),
    )
    .await
    .expect("must not hang")
    .expect_err("a request whose body failed is not a request that succeeded");

    assert_eq!(
        *err.kind(),
        hclient_core::ErrorKind::Body,
        "the caller's own kind, not the `Connect` a reset would produce: {err}"
    );
    assert!(
        StdError::source(&err)
            .and_then(|s| s.downcast_ref::<TheCallersOwnBodyError>())
            .is_some(),
        "and the caller's own error, carried out whole rather than replaced \
         by the transport's account of what it did about it: {err:?}"
    );
}

// ── cancellation ────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn dropping_a_streaming_request_mid_upload_stops_pulling_and_leaves_the_pool_usable() {
    // Cancellation, which streaming makes ordinary: an upload is exactly
    // the request a caller changes its mind about.
    //
    // Mid-upload is a construction rather than a window: `/slow` answers
    // only at the end of the body and reads a frame every 40 ms, so a
    // 4 MiB body cannot have finished, and the `execute` future is
    // certainly still pending when it is dropped.
    const FRAMES: usize = 256;
    const CHUNK: usize = 16 * 1024;

    let server = spawn_server();
    let client = client();
    let (body, pulled) = repeat(FRAMES, CHUNK);

    // `Box::pin`, not `tokio::pin!`: the latter gives a `Pin<&mut F>`, and
    // dropping THAT drops a borrow rather than the future.
    let mut doomed = Box::pin(client.post(&server.url("/slow")).body(body).send());
    tokio::select! {
        _ = &mut doomed => panic!("/slow answers only at the end of the body"),
        _ = tokio::time::sleep(Duration::from_millis(200)) => {}
    }
    let pulled_at_drop = pulled.load(Ordering::SeqCst);
    assert!(
        pulled_at_drop > 0,
        "the upload had not started, so nothing was cancelled mid-way"
    );
    assert!(
        pulled_at_drop < FRAMES,
        "the upload had already finished, so there was nothing to cancel"
    );
    drop(doomed);

    // 1. The server saw the request stream end WITHOUT being finished, and
    //    it had already received part of it.
    assert!(
        server.wait_for_reports(1, BOUND).await,
        "the cancellation must reach the server, not leave it reading for ever"
    );
    let report = server.reports()[0];
    assert!(
        !report.complete,
        "a cancelled upload must not look like a request that ended: {report:?}"
    );
    assert!(report.bytes > 0, "and part of it had arrived: {report:?}");

    // 2. Nothing is still pulling from the caller's body. Nothing here is
    //    spawned, so dropping the future drops the pump — this is the guard
    //    against a later change that spawns it and leaves an upload running
    //    behind a caller that walked away.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        pulled.load(Ordering::SeqCst),
        pulled_at_drop,
        "the pump outlived the request future"
    );

    // 3. The client is still usable against the same origin. On this
    //    transport a connection is checked out exclusively, so the
    //    cancelled request had no neighbour on its connection to damage —
    //    that is `pool.rs`'s guarantee rather than `http2.rs`'s, and the
    //    claim here is only the weaker one that nothing was left poisoned
    //    behind it.
    let after = tokio::time::timeout(BOUND, client.get(&server.url("/after")).send())
        .await
        .expect("must not hang")
        .expect("a cancelled upload must not break the next request");
    assert_eq!(after.version(), http::Version::HTTP_2);
    assert_eq!(after.collect().await.unwrap().text().unwrap(), "ok");
}

// ── the second h3 defect shape ──────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn a_rewindable_body_whose_factory_streams_is_actually_sent() {
    // The shape that on `hclient-h3` sent **nothing** — no bytes, no
    // error, a `200` for a request whose body had vanished — because
    // `one_attempt` matched `if let RequestBody::Full(b) = f()`.
    // `RequestBody`'s own doc says the factory may return any variant,
    // `Streaming` included.
    //
    // This path unpacks a `Rewindable` recursively in
    // `body::Inner::from_request_body`, one conversion shared with HTTP/1,
    // so the defect is structurally absent here rather than fixed. That is
    // a claim worth a test either way: the count is the SERVER's, so this
    // cannot pass on an empty request.
    const FRAMES: usize = 4;
    const CHUNK: usize = 8192;

    let server = spawn_server();
    let client = client();
    let body = RequestBody::rewindable(|| repeat(FRAMES, CHUNK).0);

    let resp = tokio::time::timeout(BOUND, client.post(&server.url("/count")).body(body).send())
        .await
        .expect("must not hang")
        .expect("request must succeed");
    assert_eq!(resp.version(), http::Version::HTTP_2);
    assert_eq!(
        resp.collect().await.unwrap().text().unwrap(),
        format!("{} bytes", FRAMES * CHUNK)
    );

    assert!(server.wait_for_reports(1, BOUND).await);
    let report = server.reports()[0];
    assert_eq!(report.bytes, FRAMES * CHUNK);
    assert!(report.complete, "and it ended cleanly: {report:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_streamed_request_body_arrives_whole() {
    // The base claim under the pump, and the count is the SERVER's: it read
    // the request body to its end and answered with how many bytes that
    // was, so a client that wrote the first frame and stopped cannot
    // produce this number. `/count` answers only afterwards, so this one
    // exercises the pump inside `exchange`; the duplex test above exercises
    // the same loop after it has moved into `H2Body`.
    const FRAMES: usize = 8;
    const CHUNK: usize = 32 * 1024;

    let server = spawn_server();
    let client = client();
    let (body, pulled) = repeat(FRAMES, CHUNK);

    let resp = tokio::time::timeout(BOUND, client.post(&server.url("/count")).body(body).send())
        .await
        .expect("must not hang")
        .expect("request must succeed");
    assert_eq!(
        resp.collect().await.unwrap().text().unwrap(),
        format!("{} bytes", FRAMES * CHUNK)
    );
    assert_eq!(pulled.load(Ordering::SeqCst), FRAMES);

    assert!(server.wait_for_reports(1, BOUND).await);
    assert!(server.reports()[0].complete);
}
