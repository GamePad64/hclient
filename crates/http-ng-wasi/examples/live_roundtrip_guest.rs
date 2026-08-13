//! Guest for `tests/live_roundtrip.rs` (Task 16) — the one place in the
//! project where `Transport::execute` is actually run against a real
//! `wasi:http` host, not just compiled.
//!
//! Not `fn main()`. An ordinary `fn main()` on `wasm32-wasip2` compiles
//! to a SYNCHRONOUS `wasi:cli/run@0.2.0` export — the one the rustc
//! target gives you out of the box. A synchronous (not async-lifted)
//! root task in the Component Model can't genuinely WAIT (`task.wait`)
//! on its subtasks: the first version of this guest was exactly
//! `fn main()` with its own `std::net` sockets inside the guest itself,
//! and as soon as `wasip3::http::client::send(..).await` reached a point
//! with nothing left to poll non-blockingly and needed a genuine wait,
//! wasmtime trapped: `cannot block a synchronous task before returning`.
//! `wasip3::cli::command::export!` exports the ASYNCHRONOUS
//! `wasi:cli/run@0.3.0`, which can wait on subtasks — exactly what a
//! real `WasiHttp::execute` call requires, since `wasi:http` 0.3 is an
//! asynchronous protocol. The mock server therefore doesn't live here
//! but natively on the `tests/live_roundtrip.rs` side, outside WASI
//! entirely — no mixing synchronous `wasi:sockets` calls with this
//! asynchronous task.
//!
//! # Why the response is chunked with trailers, not just `Content-Length`
//!
//! Empirically verified (see `wasip3::http_compat::IncomingBody::poll_frame`
//! /`is_end_stream`, `wasip3-0.7.0+wasi-0.3.0/src/http_compat/mod.rs:216-283`):
//! for a body without trailers, `i.is_end_stream()` becomes `true` in
//! EXACTLY the same `poll_frame` call that returns `Ready(None)` — that
//! is, exactly when our own `Body::poll_frame` itself transitions
//! `self.inner` to `Inner::Done`. In that case the hardcoded-`false`
//! mutation has no externally observable difference from the honest
//! implementation: both branches are unreachable on their own (checked
//! by hand both ways while preparing this test). But when there are
//! trailers, `IncomingBody` sets its internal `IncomingState::Done`
//! EARLIER — at the moment it's still returning
//! `Ready(Some(Ok(trailers_frame)))`, not `Ready(None)`. Our own
//! `Body::poll_frame`, on the `Ready(Some(Ok(f)))` branch, doesn't
//! change `self.inner`'s state — meaning at that moment it's still
//! `Inner::Incoming`, while `i.is_end_stream()` is already `true`.
//! That's the exact window checked below: the only place where
//! `Inner::Incoming(i) => i.is_end_stream()` is actually
//! distinguishable from `Inner::Incoming(_) => false`.
//!
//! `#![cfg(target_arch = "wasm32")]`: `wasip3::cli::command::export!`
//! generates a component-model export name
//! (`[async-lift]wasi:cli/run@0.3.0#run`) that the native linker rejects
//! outright — `cargo test --workspace` (no `--target`, i.e. every non-wasip2
//! CI job) still visits every `[[example]]` in the workspace to build it for
//! the host, so without this gate the mere existence of this file would
//! break `cargo test --workspace` on every platform. Gated out, it compiles
//! to an empty, harmless native `cdylib` there instead.
#![cfg(target_arch = "wasm32")]

use http_body::{Body as HttpBody, Frame};
use http_ng_core::RequestBody;
use http_ng_core::unversioned::Transport;
use std::future::poll_fn;
use std::pin::Pin;
use std::task::{Context, Poll};

wasip3::cli::command::export!(Guest);

struct Guest;

/// Must match the chunk data the mock server writes in
/// `tests/live_roundtrip.rs`.
const EXPECTED_BODY: &[u8] = b"hello from a real wasi:http host";

impl wasip3::exports::cli::run::Guest for Guest {
    async fn run() -> Result<(), ()> {
        let args = wasip3::cli::environment::get_arguments();
        let port: u16 = args
            .get(1)
            .unwrap_or_else(|| {
                eprintln!("usage: live_roundtrip_guest <port> [mode]");
                std::process::abort()
            })
            .parse()
            .unwrap_or_else(|e| {
                eprintln!("port must be numeric: {e}");
                std::process::abort()
            });
        let mode = args
            .get(2)
            .map(String::as_str)
            .unwrap_or("response-roundtrip");

        match mode {
            "response-roundtrip" => response_roundtrip(port).await,
            "request-trailers-undeclared" => request_trailers(port, TrailerCase::Undeclared).await,
            "request-trailers-declared" => request_trailers(port, TrailerCase::Declared).await,
            "request-trailers-wrong-name" => request_trailers(port, TrailerCase::WrongName).await,
            "request-trailers-empty-frame" => request_trailers(port, TrailerCase::EmptyFrame).await,
            "cancel-drop" => cancel_on_drop(port, CancelCase::Drop).await,
            "cancel-hold" => cancel_on_drop(port, CancelCase::Hold).await,
            "reuse-two-requests" => reuse_two_requests(port).await,
            "hooks-head" => hooks_head(port).await,
            "hooks-no-head" => hooks_no_head(port).await,
            "hooks-quiet" => hooks_quiet(port).await,
            other => {
                eprintln!("unknown mode: {other}");
                Err(())
            }
        }
    }
}

/// This guest's original scenario: a real response through
/// `WasiHttp::execute`, closing the gap in `Body::is_end_stream()` (see
/// the module doc comment).
async fn response_roundtrip(port: u16) -> Result<(), ()> {
    let uri: http::Uri = format!("http://127.0.0.1:{port}/probe")
        .parse()
        .expect("uri");
    let req = http::Request::builder()
        .method(http::Method::GET)
        .uri(uri)
        .body(RequestBody::Empty)
        .expect("request");

    let transport = http_ng_wasi::WasiHttp::new();
    let resp = transport.execute(req).await.map_err(|e| {
        eprintln!("execute failed: {e}");
    })?;
    if resp.status() != http::StatusCode::OK {
        eprintln!("unexpected status: {}", resp.status());
        return Err(());
    }

    let mut body = resp.into_body();
    let mut collected = Vec::new();
    let mut end_flagged_at_trailers = false;
    let mut saw_trailers = false;
    loop {
        match poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await {
            Some(Ok(f)) => {
                if f.is_trailers() {
                    // The task's key check, and key on timing: poll
                    // `is_end_stream()` RIGHT AFTER the trailers frame,
                    // BEFORE the next `poll_frame` — i.e. while
                    // `Body::inner` is still `Inner::Incoming`, not yet
                    // `Inner::Done` (that only happens on the next
                    // `Ready(None)`). This is exactly the window where
                    // the honest `Inner::Incoming(i) => i.is_end_stream()`
                    // branch is actually distinguishable from the
                    // hardcoded-`false` mutation — see the module doc
                    // comment for why that window doesn't exist at all
                    // without trailers.
                    saw_trailers = true;
                    end_flagged_at_trailers = body.is_end_stream();
                } else if let Ok(data) = f.into_data() {
                    collected.extend_from_slice(&data);
                }
            }
            Some(Err(e)) => {
                eprintln!("body error: {e}");
                return Err(());
            }
            None => break,
        }
    }

    if collected != EXPECTED_BODY {
        eprintln!("body mismatch: {:?}", String::from_utf8_lossy(&collected));
        return Err(());
    }
    if !saw_trailers {
        eprintln!("expected a trailers frame from the mock server, got none");
        return Err(());
    }
    if !end_flagged_at_trailers {
        eprintln!(
            "is_end_stream() must already report true right after the trailers frame \
             arrives, while Body::inner is still Inner::Incoming — this is the exact gap \
             Task 16 exists to close, see the doc-comment on Body::is_end_stream"
        );
        return Err(());
    }

    println!("ROUNDTRIP_OK");
    Ok(())
}

/// Two requests to the same origin, one after the other, through one
/// `WasiHttp` — the guest half of the connection-reuse observer.
///
/// Nothing here can see a socket, and that is the point: the whole claim
/// is the server's accept count on the native side (see
/// `tests/live_roundtrip.rs`). All this half owes is that there really
/// were two complete exchanges, in sequence, to one origin.
///
/// **Each body is drained to its end before the next request starts**, and
/// that is not tidiness. A response whose body is still open is a
/// connection no host can return to a pool, so a guest that stopped
/// reading would be measuring its own impatience rather than the host's
/// policy — and it would do it in the direction that makes the answer look
/// worse than the truth.
///
/// One transport for both requests, as a `Client` would have it. It makes
/// no difference to what is observed — `WasiHttp` holds only its
/// capability set, and the pool, if there is one, is the host's — but a
/// fixture that constructed a fresh transport per request would leave the
/// reader wondering whether that was the variable.
async fn reuse_two_requests(port: u16) -> Result<(), ()> {
    let transport = http_ng_wasi::WasiHttp::new();
    for path in ["/one", "/two"] {
        let uri: http::Uri = format!("http://127.0.0.1:{port}{path}")
            .parse()
            .expect("uri");
        let req = http::Request::builder()
            .method(http::Method::GET)
            .uri(uri)
            .body(RequestBody::Empty)
            .expect("request");

        let resp = transport.execute(req).await.map_err(|e| {
            eprintln!("execute {path} failed: {e}");
        })?;
        if resp.status() != http::StatusCode::OK {
            eprintln!("unexpected status for {path}: {}", resp.status());
            return Err(());
        }

        let mut body = resp.into_body();
        let mut collected = Vec::new();
        loop {
            match poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await {
                Some(Ok(f)) => {
                    if let Ok(data) = f.into_data() {
                        collected.extend_from_slice(&data);
                    }
                }
                Some(Err(e)) => {
                    eprintln!("body error for {path}: {e}");
                    return Err(());
                }
                None => break,
            }
        }
        if collected != REUSE_RESPONSE_BODY {
            eprintln!(
                "body mismatch for {path}: {:?}",
                String::from_utf8_lossy(&collected)
            );
            return Err(());
        }
    }

    println!("REUSE_TWO_REQUESTS_OK");
    Ok(())
}

/// Must match what the counting server in `tests/live_roundtrip.rs`
/// writes for the reuse scenario.
const REUSE_RESPONSE_BODY: &[u8] = b"ok";

/// A request body that emits one data frame, then one trailers frame
/// (empty or carrying the `x-checksum` field, depending), needed by
/// `request_trailers` below to actually exercise `convert::TrailerWatch`
/// inside `WasiHttp::execute`.
struct DataThenTrailers {
    data: Option<bytes::Bytes>,
    trailers: Option<http::HeaderMap>,
}
impl DataThenTrailers {
    fn with_checksum_trailer() -> Self {
        let mut trailers = http::HeaderMap::new();
        trailers.insert("x-checksum", "deadbeef".parse().unwrap());
        Self {
            data: Some(bytes::Bytes::from_static(b"payload")),
            trailers: Some(trailers),
        }
    }
    /// Review resolution, fix round 2 finding 3: an empty trailers frame
    /// loses nothing on the wire (there's nothing to lose) — the guard
    /// must not reject it, even without `Trailer:`.
    fn with_empty_trailer_frame() -> Self {
        Self {
            data: Some(bytes::Bytes::from_static(b"payload")),
            trailers: Some(http::HeaderMap::new()),
        }
    }
}
impl HttpBody for DataThenTrailers {
    type Data = bytes::Bytes;
    type Error = http_ng_core::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<bytes::Bytes>, http_ng_core::Error>>> {
        if let Some(d) = self.data.take() {
            return Poll::Ready(Some(Ok(Frame::data(d))));
        }
        if let Some(t) = self.trailers.take() {
            return Poll::Ready(Some(Ok(Frame::trailers(t))));
        }
        Poll::Ready(None)
    }
}

enum TrailerCase {
    /// No `Trailer:`, the body emits `x-checksum` — must be an error.
    Undeclared,
    /// `Trailer: X-Checksum`, the body emits `x-checksum` — must
    /// succeed.
    Declared,
    /// Review resolution, fix round 2 finding 2: `Trailer: X-Other`, the
    /// body emits `x-checksum` — the header is present but names the
    /// WRONG field. Must be the same error as when the header is absent
    /// entirely: measured on a live host that the wire loses
    /// `x-checksum` exactly the same way in both cases.
    WrongName,
    /// Review resolution, fix round 2 finding 3: no `Trailer:`, the body
    /// emits an EMPTY trailers frame — nothing to lose, must succeed.
    EmptyFrame,
}

/// Review resolution, finding B-1, live run (refined by fix round 2
/// findings 2 and 3): a `Streaming` request body really does emit
/// trailers (`DataThenTrailers`) in one of four configurations.
/// `Undeclared` and `WrongName` must fail with a typed error
/// (`convert::undeclared_trailers`) — both lose `x-checksum` on the wire
/// the same way. `Declared` and `EmptyFrame` must succeed: the guard
/// must not false-positive on either a correctly declared name or a
/// frame that has nothing to lose.
async fn request_trailers(port: u16, case: TrailerCase) -> Result<(), ()> {
    let uri: http::Uri = format!("http://127.0.0.1:{port}/probe")
        .parse()
        .expect("uri");
    let mut builder = http::Request::builder().method(http::Method::POST).uri(uri);
    let body: Box<dyn HttpBody<Data = bytes::Bytes, Error = http_ng_core::Error> + Unpin + Send> =
        match case {
            TrailerCase::Undeclared => Box::new(DataThenTrailers::with_checksum_trailer()),
            TrailerCase::Declared => {
                builder = builder.header(http::header::TRAILER, "X-Checksum");
                Box::new(DataThenTrailers::with_checksum_trailer())
            }
            TrailerCase::WrongName => {
                builder = builder.header(http::header::TRAILER, "X-Other");
                Box::new(DataThenTrailers::with_checksum_trailer())
            }
            TrailerCase::EmptyFrame => Box::new(DataThenTrailers::with_empty_trailer_frame()),
        };
    let req = builder.body(RequestBody::Streaming(body)).expect("request");

    let transport = http_ng_wasi::WasiHttp::new();
    let result = transport.execute(req).await;

    let expect_ok = matches!(case, TrailerCase::Declared | TrailerCase::EmptyFrame);
    match (expect_ok, result) {
        (true, Ok(_)) => {
            println!("TRAILERS_ACCEPTED_OK");
            Ok(())
        }
        (true, Err(e)) => {
            eprintln!("expected success, got error: {e}");
            Err(())
        }
        (false, Err(e)) if e.kind() == &http_ng_core::ErrorKind::Body => {
            let msg = e.to_string();
            // Finding 2: the message must name the specific field, not
            // just "rejected".
            if !msg.contains("x-checksum") {
                eprintln!("error must name the specific field `x-checksum`: {msg}");
                return Err(());
            }
            println!("TRAILERS_REJECTED_OK");
            Ok(())
        }
        (false, Err(e)) => {
            eprintln!("expected ErrorKind::Body naming x-checksum, got: {e:?}");
            Err(())
        }
        (false, Ok(_)) => {
            eprintln!(
                "expected an error for undeclared/mismatched trailers, got success — this is \
                 exactly the silent data loss Task 16's B-1 exists to catch"
            );
            Err(())
        }
    }
}

/// How long the guest lets the exchange run before it acts on it.
///
/// The one thing this must be is *later than the request reaching the
/// server*, and that is not left to this number to guarantee: the mock
/// server on the other side reads the whole request head before it starts
/// watching the connection, and fails the run if the head never arrives.
/// So this is a delay, not an assumption.
const IN_FLIGHT_NS: u64 = 200_000_000;

/// How long the guest keeps going after acting, so that "the exchange was
/// cancelled" stays distinguishable from "the guest exited".
///
/// Without this the whole comparison would be worthless: a component that
/// returns from `run` has its sockets closed by the host regardless, so a
/// server would see the connection drop either way and the test would pass
/// against a backend that cancels nothing.
const STAY_ALIVE_NS: u64 = 1_500_000_000;

/// The two halves of the drop-cancellation contract for `wasi:http`, as
/// seen from the guest. Which one ran is not what the test asserts on —
/// the mock server's own view of the connection is (see
/// `tests/live_roundtrip.rs`).
enum CancelCase {
    /// Drop the `execute` future while the exchange is in flight, then
    /// stay alive. The server must see its connection close.
    Drop,
    /// Keep the same future alive AND polled for longer than the server
    /// watches. The server must see its connection intact — otherwise the
    /// `Drop` case would be measuring the passage of time.
    Hold,
}

/// v0.2 W1: dropping the future `Transport::execute` returns must stop the
/// exchange, and for this backend that is not something the guest does
/// itself — it owns no socket. `wasip3::http::client::send` is an
/// `[async-lower]` import whose generated future cancels its subtask on
/// drop (`wit-bindgen`'s `WaitableOperation::drop` -> `[subtask-cancel]`),
/// and this is where that stops being a claim about someone else's source
/// code: what the mock server sees on its own socket is the whole result.
async fn cancel_on_drop(port: u16, case: CancelCase) -> Result<(), ()> {
    use futures::future::{Either, select};

    let uri: http::Uri = format!("http://127.0.0.1:{port}/probe")
        .parse()
        .expect("uri");
    let req = http::Request::builder()
        .method(http::Method::GET)
        .uri(uri)
        .body(RequestBody::Empty)
        .expect("request");

    let transport = http_ng_wasi::WasiHttp::new();
    // `Box::pin` for `select`'s `Unpin` bound, and so that the drop below
    // is a drop of the future itself rather than of a borrow of it.
    let exec = Box::pin(transport.execute(req));
    let hold_ns = match case {
        CancelCase::Drop => IN_FLIGHT_NS,
        CancelCase::Hold => IN_FLIGHT_NS + STAY_ALIVE_NS,
    };
    // `select` keeps polling `exec` throughout — in the `Hold` case that
    // is the point, and in the `Drop` case it is what puts the request on
    // the wire in the first place.
    let exec = match select(
        exec,
        Box::pin(wasip3::clocks::monotonic_clock::wait_for(hold_ns)),
    )
    .await
    {
        Either::Left((result, _)) => {
            eprintln!(
                "execute finished on its own (ok={}) — the mock server is supposed to stay \
                 silent, so there was never an in-flight exchange to cancel",
                result.is_ok()
            );
            return Err(());
        }
        Either::Right((_, exec)) => exec,
    };

    match case {
        CancelCase::Drop => {
            drop(exec);
            // Outlive the drop, so the server's verdict is about the
            // cancellation and not about this component going away.
            wasip3::clocks::monotonic_clock::wait_for(STAY_ALIVE_NS).await;
            println!("CANCEL_DROPPED_OK");
        }
        CancelCase::Hold => {
            // Held and polled for the whole window; only now let it go.
            drop(exec);
            println!("CANCEL_HELD_OK");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------
// v0.4 W2: the observability hook, from inside a real guest.
//
// A hook is only worth having if it fires against a real host, so these
// modes run `WasiHttp::execute` for real and print what the hook heard.
// The verdict is the harness's: the guest reports, it does not assert
// what the harness is there to check — the same division of labour the
// cancellation modes above use, and the reason `observe_client_end` lives
// on the native side.
// ---------------------------------------------------------------------

/// Every event the hook heard, flattened into a line the harness can read.
///
/// The lines live behind an `Rc<RefCell<..>>` — genuinely `!Send`, not a
/// gesture — which is P13 exercised on this backend too: `Hooks` declares
/// no `Send`, so a single-threaded guest can watch. If a `Send` bound ever
/// appeared on that path, this file would stop compiling.
#[derive(Clone, Default)]
struct Recorder(std::rc::Rc<std::cell::RefCell<Vec<String>>>);

impl http_ng_core::unversioned::Hooks for Recorder {
    fn on(&self, event: http_ng_core::unversioned::Event<'_>) {
        use http_ng_core::unversioned::Event;
        let line = match event {
            // Named individually rather than through a catch-all, so that
            // a backend that started emitting one of them shows up in the
            // harness's output as the word it is, instead of as a count
            // that went up.
            Event::Connected(c) => format!("EVENT connected id={}", c.id),
            Event::Reused(r) => format!("EVENT reused id={}", r.id),
            Event::Closed(c) => format!("EVENT closed id={}", c.id),
            Event::Head(h) => format!(
                "EVENT head id={} uri={} status={} version={:?} elapsed_ns={}",
                h.id,
                h.uri,
                h.status.as_u16(),
                h.version,
                h.elapsed.as_nanos()
            ),
        };
        self.0.borrow_mut().push(line);
    }
}

impl Recorder {
    fn report(&self) {
        for line in self.0.borrow().iter() {
            println!("{line}");
        }
        println!("EVENTS={}", self.0.borrow().len());
    }
}

/// A successful exchange, watched: the harness asserts that what comes
/// back is one `Head` and nothing else.
async fn hooks_head(port: u16) -> Result<(), ()> {
    let rec = Recorder::default();
    let transport = http_ng_wasi::WasiHttp::new().hooks(rec.clone());

    let uri: http::Uri = format!("http://127.0.0.1:{port}/hooked?probe=1")
        .parse()
        .expect("uri");
    let req = http::Request::builder()
        .method(http::Method::GET)
        .uri(uri)
        .body(RequestBody::Empty)
        .expect("request");

    let resp = transport.execute(req).await.map_err(|e| {
        eprintln!("execute failed: {e}");
    })?;
    // The head is what the event is about, so it is reported before the
    // body is touched: an event that only arrived once the caller drained
    // the response would be a different promise, and the harness could
    // not tell the two apart if this drained first.
    rec.report();

    let mut body = resp.into_body();
    while let Some(frame) = poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await {
        if let Err(e) = frame {
            eprintln!("body error: {e}");
            return Err(());
        }
    }
    // And again after the body, so the harness can check that draining it
    // adds nothing: this backend has no body-level event, and a caller
    // counting heads must not get a second one.
    println!("AFTER_BODY");
    rec.report();

    println!("HOOKS_HEAD_OK");
    Ok(())
}

/// The counterpart, and the reason the mode above is not vacuous: an
/// exchange that never produced a head reports nothing at all.
///
/// The port is one the harness bound and released, so the connect is
/// refused rather than hanging — the guest owns no socket and cannot
/// arrange that for itself.
async fn hooks_no_head(port: u16) -> Result<(), ()> {
    let rec = Recorder::default();
    let transport = http_ng_wasi::WasiHttp::new().hooks(rec.clone());

    let uri: http::Uri = format!("http://127.0.0.1:{port}/nobody-home")
        .parse()
        .expect("uri");
    let req = http::Request::builder()
        .method(http::Method::GET)
        .uri(uri)
        .body(RequestBody::Empty)
        .expect("request");

    match transport.execute(req).await {
        Ok(_) => {
            eprintln!("the port was supposed to be dead, but something answered");
            return Err(());
        }
        Err(e) => println!("EXECUTE_FAILED {:?}", e.kind()),
    }
    rec.report();

    println!("HOOKS_NO_HEAD_OK");
    Ok(())
}

/// The default transport, unchanged by the type parameter that was added
/// beside it: `WasiHttp::new()` is still `WasiHttp<NoHooks>` and still
/// works. Nothing can be observed from inside a hookless build — that is
/// the point of it — so this mode exists to catch the ordinary
/// regression, not to measure the cost.
async fn hooks_quiet(port: u16) -> Result<(), ()> {
    let transport = http_ng_wasi::WasiHttp::new();
    let uri: http::Uri = format!("http://127.0.0.1:{port}/quiet")
        .parse()
        .expect("uri");
    let req = http::Request::builder()
        .method(http::Method::GET)
        .uri(uri)
        .body(RequestBody::Empty)
        .expect("request");

    let resp = transport.execute(req).await.map_err(|e| {
        eprintln!("execute failed: {e}");
    })?;
    // 203, matching `tests/hooks.rs`'s mock server — see `RESPONSE_STATUS`
    // there for why that fixture does not answer 200.
    if resp.status() != http::StatusCode::NON_AUTHORITATIVE_INFORMATION {
        eprintln!("unexpected status: {}", resp.status());
        return Err(());
    }
    println!("HOOKS_QUIET_OK");
    Ok(())
}
