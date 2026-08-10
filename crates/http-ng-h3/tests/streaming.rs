//! Streaming request bodies and full duplex, measured from the server.
//!
//! Two capabilities changed from `false` to `true` in this crate, and the
//! expensive one is `full_duplex`: over-claiming it costs a caller a
//! deadlock rather than a degradation, which is the whole argument behind
//! v0.2 W3's floor rule. So it is not declared on the strength of the code
//! reading right.
//!
//! # The duplex test is causal, not timed
//!
//! `a_response_head_arrives_while_the_request_body_is_still_going_out` does
//! not measure a threshold. The caller's body produces its second chunk
//! **only after `execute` has returned a response head** — the chunk is not
//! in the channel until then — so a transport that read the head after
//! finishing the body could not complete the exchange at all, at any speed.
//! The server's own clock is the second witness: it records when it sent
//! the head and when the last request byte arrived, and the first is before
//! the second.
//!
//! Everything else here is observed from the server too: how many bytes of
//! request body arrived, whether the request stream ended cleanly or was
//! reset, and how many connections it had to accept. The one client-side
//! number any test reads is how many frames were pulled out of the caller's
//! own body — that is the caller's object, not the transport's insides, and
//! it is the only place "the pump stopped" can be seen.
#![cfg(not(target_family = "wasm"))]

mod server;

use bytes::Bytes;
use http_body_util::BodyExt;
use http_ng_core::unversioned::Transport;
use http_ng_core::{AllowEarlyData, RequestBody};
use http_ng_dns::IpLiteralOnly;
use http_ng_h3::H3;
use http_ng_rt_tokio::TokioHandle;
use server::Behaviour;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

/// The transport under test, over the real runtime seam. Same shape as
/// `live.rs`'s, and duplicated rather than shared because `mod server` is
/// per-test-binary anyway.
fn h3(
    cert: &rustls::pki_types::CertificateDer<'static>,
) -> H3<TokioHandle, http_ng_tls_rustls::Rustls, IpLiteralOnly> {
    H3::new(
        TokioHandle::current().expect("inside #[tokio::test]"),
        server::client_tls(cert),
        IpLiteralOnly,
    )
    .expect("H3::new does no I/O")
}

fn post(addr: std::net::SocketAddr, path: &str, body: RequestBody) -> http::Request<RequestBody> {
    http::Request::builder()
        .method("POST")
        .uri(format!("https://{addr}{path}"))
        .body(body)
        .unwrap()
}

async fn text<B>(r: http::Response<B>) -> String
where
    B: http_body::Body<Data = Bytes>,
    B::Error: std::fmt::Debug,
{
    let bytes = r.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
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
    type Error = http_ng_core::Error;

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
        pulled: pulled.clone(),
    };
    (tx, RequestBody::Streaming(Box::new(body)), pulled)
}

/// `frames` chunks of the same bytes, always ready, counting what was
/// taken.
///
/// Never-ending would do as well for the "stop pumping" test, but a finite
/// body means a test that fails by asserting rather than by hanging.
struct Repeat {
    left: usize,
    chunk: Bytes,
    pulled: Arc<AtomicUsize>,
}

impl http_body::Body for Repeat {
    type Data = Bytes;
    type Error = http_ng_core::Error;

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
        pulled: pulled.clone(),
    };
    (RequestBody::Streaming(Box::new(body)), pulled)
}

// ── what a streamed body does on the wire ───────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn a_streamed_request_body_arrives_whole() {
    // The base claim behind `streaming_request_body: true`, and the count
    // is the SERVER's: it read the request body to its end and answered
    // with how many bytes that was, so a client that wrote the first frame
    // and stopped cannot produce this number.
    const FRAMES: usize = 8;
    const CHUNK: usize = 32 * 1024;

    let s = server::start(Behaviour::CountBody);
    let t = h3(&s.cert_der);
    let (body, pulled) = repeat(FRAMES, CHUNK);

    let r = t
        .execute(post(s.addr, "/upload", body))
        .await
        .expect("the server answers once it has read the body");
    assert_eq!(r.status(), 200);
    assert_eq!(text(r).await, format!("{} bytes", FRAMES * CHUNK));

    let report = *s.bodies().first().expect("the server read a body");
    assert_eq!(report.bytes, FRAMES * CHUNK);
    assert!(
        report.complete,
        "the request stream must end cleanly, not be reset: {report:?}"
    );
    assert!(
        report.frames > 1,
        "a streamed body reaches the server as several DATA frames, not one \
         buffered blob: {report:?}"
    );
    assert_eq!(pulled.load(Ordering::SeqCst), FRAMES);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_response_head_arrives_while_the_request_body_is_still_going_out() {
    // `full_duplex`, earned rather than declared, and the exchange below
    // cannot complete without it — no threshold, no timing luck.
    //
    // The client's body holds one chunk. The second is put into its channel
    // only AFTER `execute` has returned a response head, so a transport
    // that wrote the whole request body before reading the head would be
    // waiting for a chunk that is waiting for it. The `timeout` turns that
    // into a failed assertion instead of a hung suite.
    const A: usize = 4096;
    const B: usize = 8192;

    let s = server::start(Behaviour::HeadThenRead);
    let t = h3(&s.cert_der);
    let (tx, body, _) = feed();
    tx.send(Bytes::from(vec![b'a'; A])).unwrap();

    let r = tokio::time::timeout(
        Duration::from_secs(5),
        t.execute(post(s.addr, "/duplex", body)),
    )
    .await
    .expect(
        "the head is on the wire and the second chunk is not: a transport that \
         waits for the body before reading the head deadlocks here, which is \
         exactly what `full_duplex: false` used to mean",
    )
    .expect("h3 request");
    assert_eq!(r.status(), 200);

    // Only now does the rest of the request body exist.
    tx.send(Bytes::from(vec![b'b'; B])).unwrap();
    drop(tx);

    // Collecting the response is what drives the remaining upload: the pump
    // moved into the body, and nothing else polls it.
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), text(r))
            .await
            .expect("the rest of the upload rides on the response body being read"),
        format!("{} bytes", A + B)
    );

    assert!(
        s.wait_for_bodies(1, Duration::from_secs(5)).await,
        "the server must have finished reading"
    );
    let report = *s.bodies().first().unwrap();
    assert_eq!(report.bytes, A + B, "both chunks arrived: {report:?}");
    assert!(report.complete, "and the stream ended cleanly: {report:?}");

    // The server's own clock, as the second witness: it sent the head
    // before the last request byte reached it.
    let (head, last) = (
        report.head_sent.expect("HeadThenRead sends one"),
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
async fn a_streaming_body_stops_when_the_server_stops_reading_and_the_response_still_arrives() {
    // RFC 9114 §4.1 — *"Clients MUST NOT discard complete responses as a
    // result of having their request terminated abruptly"* — across many
    // frames rather than one. `tests/stop_sending.rs` makes the same claim
    // for a single 4 MiB `Full` body, where the whole write is one
    // `send_data`; with a streaming body the `STOP_SENDING` lands in the
    // middle of a loop, on whichever frame it happens to reach.
    //
    // The second half is new and belongs only to streaming: the pump must
    // STOP, not merely tolerate. A tolerance that skipped the failed frame
    // and pulled the next one would keep draining the caller's body into a
    // stream nobody is reading, for as long as the producer produces.
    //
    // 64 MiB of body against quinn's 1.25 MiB default stream receive
    // window: the client cannot get more than a window's worth onto the
    // stream before the peer grants more, and this peer never will — it
    // answered and dropped its half. So a client that stops pulls a few
    // dozen frames, and one that does not pulls four thousand.
    const FRAMES: usize = 4096;
    const CHUNK: usize = 16 * 1024;
    const STOPPED_WELL_BEFORE: usize = 1024;

    let s = server::start(Behaviour::Echo);
    let t = h3(&s.cert_der);
    let (body, pulled) = repeat(FRAMES, CHUNK);

    let r = t
        .execute(post(s.addr, "/ignored", body))
        .await
        .expect("the response was never affected: STOP_SENDING acts on one direction");
    assert_eq!(r.status(), 200);
    assert_eq!(
        text(r).await,
        "hello over h3",
        "and the body too, not just the head"
    );
    assert_eq!(s.requests(), 1);

    let n = pulled.load(Ordering::SeqCst);
    println!("frames pulled from the caller's body: {n} of {FRAMES}");
    assert!(
        n < STOPPED_WELL_BEFORE,
        "the peer stopped reading at around one flow-control window, so the \
         pump must have stopped too; it pulled {n} of {FRAMES} frames"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn dropping_a_streaming_request_mid_upload_resets_it_and_leaves_the_connection() {
    // Cancellation, which streaming makes ordinary: an upload is exactly
    // the request a caller changes its mind about. Three claims, and the
    // first two are the server's.
    //
    // The server reads the body and answers only at its end, so the
    // `execute` future is still pending when it is dropped — mid-upload by
    // construction rather than by timing.
    let s = server::start(Behaviour::CountBody);
    let t = h3(&s.cert_der);
    let (body, pulled) = repeat(4096, 16 * 1024);

    // `Box::pin`, not `tokio::pin!`: the latter gives a `Pin<&mut F>`, and
    // dropping THAT drops a borrow rather than the future. Same trap
    // `live.rs`'s cancellation test records.
    let mut doomed = Box::pin(t.execute(post(s.addr, "/abandoned", body)));
    tokio::select! {
        _ = &mut doomed => panic!("the server answers only at the end of the body"),
        _ = tokio::time::sleep(Duration::from_millis(150)) => {}
    }
    let pulled_at_drop = pulled.load(Ordering::SeqCst);
    assert!(
        pulled_at_drop > 0,
        "the upload had not started, so nothing was cancelled mid-way"
    );
    drop(doomed);

    // 1. The server saw the request stream end WITHOUT being finished.
    assert!(
        s.wait_for_bodies(1, Duration::from_secs(5)).await,
        "the reset must reach the server, not leave it reading for ever"
    );
    let report = *s.bodies().first().unwrap();
    assert!(
        !report.complete,
        "a cancelled upload must be reset, so the server learns that what it \
         has is not the whole request: {report:?}"
    );
    assert!(
        report.bytes > 0,
        "and it had received part of it: {report:?}"
    );

    // 2. Nothing is still pulling from the caller's body. Nothing here is
    // spawned, so dropping the future drops the pump — this is the guard
    // against a later change that spawns it and leaves an upload running
    // behind a caller that walked away.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        pulled.load(Ordering::SeqCst),
        pulled_at_drop,
        "the pump outlived the request future"
    );

    // 3. The connection is untouched, which on this transport has a subject:
    // requests share it.
    let r = t
        .execute(post(s.addr, "/after", RequestBody::Empty))
        .await
        .expect("cancelling one stream must not tear down the connection");
    assert_eq!(r.status(), 200);
    assert_eq!(text(r).await, "0 bytes");
    assert_eq!(s.accepted(), 1, "and it was the same connection throughout");
}

#[tokio::test(flavor = "multi_thread")]
async fn dropping_a_buffered_upload_mid_write_leaves_the_connection_too() {
    // The same claim as the test above, for a `RequestBody::Full` — because
    // the defect it guards against is **older than streaming**. Dropping
    // `execute` in the middle of a large buffered body already left quinn
    // to `finish()` a stream with a truncated DATA frame on it, which RFC
    // 9114 §7.1 makes a connection error; nothing reached it because the
    // suite's one cancellation test used an empty body. The guard lives in
    // the send half's `Drop`, so one fix covers both, and this is the half
    // that says so.
    //
    // Mid-write is a fact here rather than a race: the server pauses 40 ms
    // between DATA frames, so the peer's flow-control window (quinn's
    // default is 1.25 MiB per stream) fills and stays full, and a 4 MiB
    // body cannot have been written by the time the drop happens.
    const BODY: usize = 4 * 1024 * 1024;

    let s = server::start(Behaviour::ReadSlowly(Duration::from_millis(40)));
    let t = h3(&s.cert_der);
    let body = RequestBody::Full(Bytes::from(vec![b'z'; BODY]));

    let mut doomed = Box::pin(t.execute(post(s.addr, "/abandoned", body)));
    tokio::select! {
        _ = &mut doomed => panic!("a 4 MiB body cannot be read by a 40ms-per-frame reader in 150ms"),
        _ = tokio::time::sleep(Duration::from_millis(150)) => {}
    }
    drop(doomed);

    assert!(
        s.wait_for_bodies(1, Duration::from_secs(5)).await,
        "the reset must reach the server"
    );
    let report = *s.bodies().first().unwrap();
    assert!(!report.complete, "reset, not finished: {report:?}");
    assert!(report.bytes < BODY, "it was mid-write: {report:?}");

    let r = t
        .execute(post(s.addr, "/after", RequestBody::Empty))
        .await
        .expect("a truncated frame on a cleanly-closed stream would have killed this");
    assert_eq!(text(r).await, "0 bytes");
    assert_eq!(s.accepted(), 1, "still one connection");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rewindable_body_whose_factory_streams_is_actually_sent() {
    // A body this crate used to drop on the floor. `one_attempt` matched
    // `if let RequestBody::Full(b) = f()`, so a factory returning anything
    // else sent NOTHING — no bytes, no error, a `200` for a request whose
    // body vanished. `RequestBody`'s own doc says the factory may return
    // any variant, `Streaming` included.
    //
    // The count is the server's, so this cannot pass on an empty request.
    const FRAMES: usize = 4;
    const CHUNK: usize = 8192;

    let s = server::start(Behaviour::CountBody);
    let t = h3(&s.cert_der);
    let body = RequestBody::rewindable(|| repeat(FRAMES, CHUNK).0);

    let r = t
        .execute(post(s.addr, "/rewindable", body))
        .await
        .expect("h3 request");
    assert_eq!(text(r).await, format!("{} bytes", FRAMES * CHUNK));
    assert_eq!(s.bodies().first().unwrap().bytes, FRAMES * CHUNK);
}

// ── what streaming does NOT unlock ──────────────────────────────────────

/// One data frame and then trailers — the shape `Capabilities::
/// request_trailers` is about, and one a caller can only build now that
/// bodies stream.
struct DataThenTrailers {
    data: Option<Bytes>,
    trailers: Option<http::HeaderMap>,
}

impl http_body::Body for DataThenTrailers {
    type Data = Bytes;
    type Error = http_ng_core::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
        if let Some(d) = self.data.take() {
            return Poll::Ready(Some(Ok(http_body::Frame::data(d))));
        }
        Poll::Ready(
            self.trailers
                .take()
                .map(|t| Ok(http_body::Frame::trailers(t))),
        )
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_body_that_yields_trailers_is_refused_by_name() {
    // `Capabilities::request_trailers` stays `false`, and streaming is what
    // gives that declaration something to refuse: before it, no caller
    // could hand this transport a trailers frame at all. A silent drop
    // would be the failure mode the capability model exists to prevent —
    // the caller sent trailers, the server never saw them, and nothing
    // said so.
    let s = server::start(Behaviour::CountBody);
    let t = h3(&s.cert_der);
    let mut trailers = http::HeaderMap::new();
    trailers.insert("x-checksum", http::HeaderValue::from_static("deadbeef"));
    let body = RequestBody::Streaming(Box::new(DataThenTrailers {
        data: Some(Bytes::from_static(b"payload")),
        trailers: Some(trailers),
    }));

    let err = t
        .execute(post(s.addr, "/trailers", body))
        .await
        .expect_err("declared false, so refused");
    assert!(err.is_unsupported(), "{err}");
    assert!(
        err.to_string().contains("request_trailers"),
        "the refusal names the capability rather than failing vaguely: {err}"
    );

    // And the refusal costs the connection nothing, which is the half worth
    // pinning: requests share one here.
    let after = t
        .execute(post(s.addr, "/after", RequestBody::Empty))
        .await
        .expect("a refused request must not poison a shared connection");
    assert_eq!(text(after).await, "0 bytes");
    assert_eq!(s.accepted(), 1);

    // What the server saw of the refused request is deliberately NOT
    // asserted, and the reason is a race that is genuine rather than
    // fixable. `quinn::SendStream::reset` discards whatever is still in the
    // send buffer, and `send_request(..).await` only guarantees that the
    // HEADERS frame was ACCEPTED by quinn, not that it left the host — so
    // the reset can overtake the request entirely and the server resolves
    // nothing at all. Measured: it went both ways across three runs of this
    // file. A test that asserted either outcome would be asserting the
    // scheduler.
}

#[tokio::test(flavor = "multi_thread")]
async fn a_marked_streaming_request_is_not_admitted_to_early_data() {
    // The correctness condition under `AllowEarlyData`, from outside.
    //
    // A rejected 0-RTT request is REPLAYED by this transport, and a
    // single-pass body has nothing to replay — so `admits_early_data`
    // refuses a `RetryKind::Impossible` body however the caller marked it.
    // That is not visible in the response, which is a `200` either way. It
    // is visible in the server's accept count: `early_data` is part of the
    // pool key, so a request admitted to early data cannot be served by the
    // connection an unmarked one is using. `live.rs`'s
    // `early_data_is_offered_only_to_a_request_the_caller_marked` is the
    // other half of the pair — same mark, replayable body, TWO connections.
    let s = server::start(Behaviour::CountBody);
    let t = h3(&s.cert_der);

    let warm = t
        .execute(post(s.addr, "/warm", RequestBody::Empty))
        .await
        .unwrap();
    let _ = text(warm).await;
    assert_eq!(s.accepted(), 1);

    let (body, _) = repeat(2, 1024);
    let mut marked = post(s.addr, "/marked", body);
    marked.extensions_mut().insert(AllowEarlyData);
    let r = t.execute(marked).await.expect("it is sent, just not early");
    assert_eq!(text(r).await, "2048 bytes");

    assert_eq!(
        s.accepted(),
        1,
        "a streaming body cannot be replayed, so it is not admitted to early \
         data — and a request that was not admitted keeps the unmarked pool \
         key and the unmarked connection with it"
    );
    assert_eq!(s.requests(), 2);
}

// ── shape ───────────────────────────────────────────────────────────────

/// Amendment C2's rule, and C3's placement: a `dyn` on the `Client ->
/// Transport` path cuts the auto-traits off everything above it, so the
/// check is a compile-time one rather than a review.
///
/// `H3Body` gained a `Pin<Box<dyn Future<..>>>` when the request pump moved
/// into it. Without `+ Send` on that box the body would silently stop being
/// `Send`, and a caller who spawns a request would stop compiling with an
/// error pointing anywhere but here.
#[test]
fn an_h3_body_is_still_send() {
    fn assert_send(_: impl Send) {}
    fn take(b: http_ng_h3::H3Body) {
        assert_send(b);
    }
    let _ = take;
}
