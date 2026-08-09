//! `Native<R, T, D>: Transport` — this crate's integration tests against
//! `http_ng::Client`, not just against `crate::testing::*` directly: what
//! the tests below check (the error category, `Capabilities` honesty,
//! timeout enforcement at `build()`) are properties of the SEAM between
//! `Client::execute` and `Transport`, not of one function in this crate,
//! so they're checked through a real `Client`.
//!
//! # Why there's no `filter_map` resolution from the task brief here
//!
//! The task's draft (`task-13-brief.md`) collected addresses by hand —
//! `self.dns.lookup_ipv6(&host).filter_map(|r| async { r.ok()... })` —
//! which discards ANY resolver error (`ErrorKind::Cancelled` included)
//! and synthesizes a single `ErrorKind::Resolve` if both streams are
//! empty. Review Task 7 already caught this in exactly this spot:
//! `Cancelled` (an ordinary runtime shutdown) is indistinguishable from
//! "this name doesn't resolve," and a circuit breaker keyed on `Resolve`
//! would wrongly blacklist a live host during an ordinary shutdown.
//!
//! Task 11 solved this ONCE, structurally, in `connect::drive`/
//! `ResolveErrors::distinguishing_error` — it's checked BEFORE both
//! failure branches, so discarding an error code other than the
//! synthetic `Resolve` is structurally unreachable. `Native::execute`
//! (`src/lib.rs`) therefore doesn't resolve on its own: it calls
//! `connect::connect`, the same entry point `connect.rs`'s unit tests
//! already exercise — and
//! `resolver_cancelled_error_reaches_the_caller_through_execute_not_flattened`
//! below checks that this property survives the WHOLE path: from
//! `Resolve::lookup_ipv4`/`lookup_ipv6`, through `Native::execute`,
//! through `Client::execute` (which has its own step,
//! `.map_err(|e| self.transport.to_error(e))`), to the `kind()` the
//! caller sees.
mod net_fixtures;

use http_ng::Client;
use http_ng_core::ErrorKind;
use http_ng_core::unversioned::Transport;
use http_ng_dns::{Resolve, ResolvedAddr};
use http_ng_dns_system::SystemDns;
use http_ng_native::Native;
use http_ng_rt_tokio::Tokio;
use http_ng_tls_rustls::Rustls;
use std::future::Future;
use std::io::{Read, Write};
use std::time::Duration;

const BOUND: Duration = Duration::from_secs(30);

fn spawn_h1_server() -> std::net::SocketAddr {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let mut b = [0u8; 2048];
            let _ = s.read(&mut b);
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
        }
    });
    addr
}

#[tokio::test]
async fn end_to_end_over_plain_tcp() {
    let addr = spawn_h1_server();
    let t = Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio));
    let c = Client::builder(t).build().unwrap();
    let resp = tokio::time::timeout(BOUND, c.get(&format!("http://{addr}/")).send())
        .await
        .expect("must not hang")
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.collect().await.unwrap().text().unwrap(), "ok");
}

/// `end_to_end_over_plain_tcp` proves a response comes back, but the
/// fixture server above answers with the same fixed bytes no matter what it
/// receives — a mutation that dropped `Native`'s origin-form/`Host:`
/// rewrite entirely (leaving the absolute-form URI and no `Host:` header)
/// would still pass it, because nothing checks what was actually sent on
/// the wire. This test captures the raw request bytes and checks the
/// request-line and `Host:` header directly, so that rewrite has real
/// coverage.
#[tokio::test]
async fn request_line_is_origin_form_and_host_header_is_set() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        if let Ok((mut s, _)) = l.accept() {
            let mut buf = [0u8; 4096];
            let n = s.read(&mut buf).unwrap_or(0);
            let _ = tx.send(buf[..n].to_vec());
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
        }
    });
    let t = Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio));
    let c = Client::builder(t).build().unwrap();
    let resp = tokio::time::timeout(BOUND, c.get(&format!("http://{addr}/hello")).send())
        .await
        .expect("must not hang")
        .unwrap();
    assert_eq!(resp.status(), 200);

    let raw = rx
        .recv_timeout(BOUND)
        .expect("server must have seen a request");
    let text = String::from_utf8_lossy(&raw);
    let request_line = text.lines().next().unwrap_or_default();
    assert_eq!(
        request_line, "GET /hello HTTP/1.1",
        "request-line must be origin-form (path only), not absolute-form: {text:?}"
    );
    // hyper's h1 writer lowercases header names on the wire by default
    // (no `http1_preserve_header_case`), so the comparison below is
    // case-insensitive on the header name — the VALUE (the authority) is
    // what this test actually cares about.
    let host_line = text
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("host:"))
        .unwrap_or_else(|| panic!("no Host: header sent: {text:?}"));
    assert_eq!(
        host_line.trim().to_ascii_lowercase(),
        format!("host: {addr}"),
        "Host: header must name the authority actually connected to: {text:?}"
    );
}

/// **What this test can and cannot prove — final review, §3.1.** This test
/// reads the same struct literal `Native::new` wrote; it cannot tell a
/// truthful capability from a lying one, because both would read back
/// identically here. Two of these assertions used to be wrong in exactly
/// that unfalsifiable way (`streaming_request_body` claimed `false` while
/// the body was genuinely streamed; `timeouts.connect` claimed `true` while
/// nothing in `execute` ever read `Timeouts` at all) — a test that only
/// reads fields agreed with both mistakes, because it never asked whether
/// the field matched behavior. The two claims that actually need a
/// behavioral witness have one, elsewhere in this file:
/// `streaming_request_body` → `streaming_request_body_is_actually_streamed_not_buffered`
/// (captures wire bytes, asserts `transfer-encoding: chunked`);
/// `timeouts.connect` → `declared_connect_timeout_is_actually_applied`
/// (a `TcpConnect` that never resolves, raced against a real timeout).
#[tokio::test]
async fn capabilities_are_honest_about_v01_limits() {
    let t = Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio));
    let caps = t.capabilities();
    assert!(
        caps.streaming_request_body,
        "the body genuinely streams — see streaming_request_body_is_actually_streamed_not_buffered"
    );
    assert!(caps.timeouts.connect);
    assert!(
        !caps.timeouts.first_byte,
        "there is no response timer — can't be claimed"
    );
    assert!(
        !caps.timeouts.between_bytes,
        "there's no response timer — can't be claimed"
    );
    assert_eq!(caps.upgrade, http_ng_core::UpgradeSupport::None);
    assert_eq!(caps.tls_config, http_ng_core::TlsSupport::Full);
    assert_eq!(caps.redirects, http_ng_core::RedirectSupport::Configurable);
    assert!(caps.version_reported);
}

/// `capabilities_are_honest_about_v01_limits` above only samples the fields
/// the brief calls out. This one destructures the rest of the struct so a
/// field nobody asked `Native` to turn on can't silently end up
/// `true`/non-`None`.
///
/// **What this test cannot do, despite what an earlier name implied.** Unlike
/// `Capabilities::none_is_the_conservative_base` in `http-ng-core` (which
/// lives inside that crate and can destructure with no `..` rest pattern, so
/// a brand-new field is a compile error naming it), this file is an
/// *external* crate — `Capabilities` is `#[non_exhaustive]`, so `..` is
/// mandatory here (E0638), and that `..` silently absorbs any field added
/// later. A reviewer built exactly this scenario (added a seventeenth field
/// to `Capabilities`): `http-ng-core`'s own internal test failed to compile
/// as designed, and this test compiled and passed without noticing.
/// Renamed from `every_undeclared_capability_stays_at_the_conservative_default`
/// to stop promising that it does — it checks that the fields enumerated
/// below are at their conservative defaults *today*, nothing more. The
/// exhaustiveness guarantee (a new field becomes a compile error, not a
/// silent pass) exists exactly once, inside `http-ng-core`'s own
/// `Capabilities::none_is_the_conservative_base` — `#[non_exhaustive]` makes
/// that guarantee structurally unavailable to any test outside the crate
/// that owns the type, this one included (design-doc **amendment-C6** —
/// `docs/superpowers/specs/2026-08-05-http-ng-design.md`, `## Design
/// amendments`; recorded there with this exact evidence after this test
/// found it). Any future capabilities-completeness check belongs in `http-ng-core`,
/// not in a transport crate like this one. Not the same rule as amendment-C3
/// (which is about where `Send`/`Sync` assertions live, so
/// `no-declared-send`'s `src`-only grep doesn't trip on its own test text) —
/// the two share a "belongs elsewhere" shape but are different amendments,
/// citing one for the other was a mistake caught and corrected once already.
#[tokio::test]
async fn undeclared_capability_fields_match_their_conservative_defaults_today() {
    let t = Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio));
    let http_ng_core::Capabilities {
        streaming_request_body: _,
        full_duplex,
        request_trailers,
        response_trailers,
        redirects: _,
        tls_config: _,
        client_certs,
        proxy,
        owns_cookie_jar,
        owns_cache,
        version_select,
        version_reported: _,
        timeouts: _,
        informational_1xx,
        upgrade: _,
        forbidden_request_headers,
        ..
    } = *t.capabilities();
    assert!(!full_duplex);
    assert!(!request_trailers);
    assert!(!response_trailers);
    assert!(!client_certs);
    assert!(!proxy);
    assert!(!owns_cookie_jar);
    assert!(!owns_cache);
    assert!(!version_select);
    assert!(!informational_1xx);
    assert!(forbidden_request_headers.is_empty());
}

/// The category `Native` set has to survive all the way to the caller
/// through `Client::execute`'s whole path (see the module doc comment).
/// This test checks exactly that whole path, not `to_error`'s default —
/// that's checked in `http-ng-core/tests/shape.rs` and on its own only
/// guarantees `Error` passes through unchanged.
///
/// The host is deliberately nonexistent (`.invalid`, RFC 2606 —
/// guaranteed to never resolve): this is the one failure `execute` can
/// produce with no network and no server, and the `wasi` counterpart of
/// this test is built the same way — it runs the real backend
/// classifier, not a manually constructed `Error`.
#[tokio::test]
async fn transport_error_kind_survives_the_client_instead_of_flattening_to_other() {
    let t = Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio));
    let c = Client::builder(t).build().unwrap();
    let err = tokio::time::timeout(BOUND, c.get("http://nonexistent.invalid/").send())
        .await
        .expect("must not hang")
        .unwrap_err();

    assert_eq!(
        *err.kind(),
        ErrorKind::Resolve,
        "the category must survive to the caller, not flatten into Other: {err}"
    );
    assert!(
        !err.to_string().starts_with("Other:"),
        "the category is printed once, and it's the real category: {err}"
    );
}

/// A resolver that always returns `ErrorKind::Cancelled` — a synthetic
/// stand-in for a real background thread pool shutting down (Task 7),
/// good enough to check that `Native::execute` doesn't wrap or flatten it
/// on the way to `Client`.
struct CancelledDns;

#[derive(Debug)]
struct FakeCancelled;
impl std::fmt::Display for FakeCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("resolver background pool went away")
    }
}
impl std::error::Error for FakeCancelled {}

impl Resolve for CancelledDns {
    fn lookup_ipv4(
        &self,
        _name: &str,
    ) -> impl futures_util::Stream<Item = Result<ResolvedAddr, http_ng_core::Error>> {
        futures_util::stream::once(async {
            Err(http_ng_core::Error::new(
                ErrorKind::Cancelled,
                FakeCancelled,
            ))
        })
    }
    fn lookup_ipv6(
        &self,
        _name: &str,
    ) -> impl futures_util::Stream<Item = Result<ResolvedAddr, http_ng_core::Error>> {
        futures_util::stream::once(async {
            Err(http_ng_core::Error::new(
                ErrorKind::Cancelled,
                FakeCancelled,
            ))
        })
    }
}

/// Constraint from this task's brief: "an `ErrorKind::Cancelled` from the
/// resolver must reach the caller as `Cancelled` through `Transport::execute`,
/// and a mutation that flattens it must go red." This is that test, run
/// through the real `Client::execute` seam (not just `connect::drive`'s own
/// unit tests in `src/connect.rs`, which cover the same property one layer
/// lower) — see the module doc for why `Native::execute` must not re-derive
/// resolution itself.
#[tokio::test]
async fn resolver_cancelled_error_reaches_the_caller_through_execute_not_flattened() {
    let t = Native::new(Tokio, Rustls::with_webpki_roots(), CancelledDns);
    let c = Client::builder(t).build().unwrap();
    let err = tokio::time::timeout(BOUND, c.get("http://example.invalid/").send())
        .await
        .expect("must not hang")
        .unwrap_err();
    assert_eq!(
        *err.kind(),
        ErrorKind::Cancelled,
        "the runtime is shutting down — this is not 'the name doesn't resolve': {err}"
    );
}

#[tokio::test]
async fn unsupported_timeout_is_rejected_at_build_time() {
    let t = Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio));
    let err = Client::builder(t)
        .timeouts(http_ng::Timeouts {
            between_bytes: Some(Duration::from_secs(1)),
            ..Default::default()
        })
        .build()
        .unwrap_err();
    assert_eq!(err.what, "between_bytes_timeout");
}

/// A TLS handshake that fails (the server accepted the TCP connection and
/// immediately dropped it, without sending a single byte of TLS) — checks
/// that the `Tls` category (`TlsConnect::connect`, Task 8/9) also survives
/// to the caller through `Native::execute`, not just `Resolve`/`Cancelled`
/// above.
#[tokio::test]
async fn tls_handshake_failure_reports_tls_kind_through_the_client() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        // Accepts and immediately drops it — not a single byte of TLS is sent.
        let _ = l.accept();
    });
    let t = Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio));
    let c = Client::builder(t).build().unwrap();
    let err = tokio::time::timeout(
        BOUND,
        c.get(&format!("https://{}:{}/", addr.ip(), addr.port()))
            .send(),
    )
    .await
    .expect("must not hang")
    .unwrap_err();
    assert_eq!(*err.kind(), ErrorKind::Tls, "{err}");
}

// --- The remaining two of the brief's five `ErrorKind` fidelity properties ---
//
// The tests above cover `Resolve`, `Cancelled` and `Tls` surviving
// `Client::execute`. `Connect` and `Body` were the other two the brief
// named, and fix round 1's review found neither had a test of its own at
// this composed layer — both pass on unmodified code (no bug), but nothing
// held that property in place: wrapping `h1::exchange`'s error in a fresh
// `Error::new(ErrorKind::Connect, e)` in `src/lib.rs` turns the `Body` test
// below red while every other test in this file stays green (verified
// directly, see this task's report). The two tests below close that gap.

/// `ErrorKind::Connect` (`connect::connect`'s own `AllAttemptsFailed`,
/// `ErrorKind::Connect` — Task 11) surviving `Client::execute`, the same
/// property `Resolve`/`Cancelled`/`Tls` already have tests for above.
/// `net_fixtures::closed_port` (not a hand-rolled bind-then-drop) reuses the
/// helper this crate already has for "a port that genuinely refuses" — see
/// its doc comment for why a hand-rolled version is a trap worth avoiding a
/// second time, and why a supposedly-unroutable *address* would not do here
/// (this container's `tun0` makes those connect successfully; a closed
/// *local* port still gets a real `ECONNREFUSED` from the kernel regardless).
#[tokio::test]
async fn connect_refused_kind_survives_the_client() {
    let addr = net_fixtures::closed_port();
    let t = Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio));
    let c = Client::builder(t).build().unwrap();
    let err = tokio::time::timeout(BOUND, c.get(&format!("http://{addr}/")).send())
        .await
        .expect("must not hang")
        .unwrap_err();
    assert_eq!(*err.kind(), ErrorKind::Connect, "{err}");
}

/// A one-shot `RequestBody::Streaming` whose only frame is an error —
/// exercises the same "outgoing body fails mid-stream" shape `h1.rs`'s own
/// unit test already proves one layer down
/// (`exchange_recovers_error_kind_through_hyper_error_not_flattening_it`,
/// which calls `h1::exchange` directly), but through the full
/// `Native::execute` + `Client::execute` composition instead.
struct OneShotErrBody(Option<http_ng_core::Error>);
impl http_body::Body for OneShotErrBody {
    type Data = bytes::Bytes;
    type Error = http_ng_core::Error;
    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<bytes::Bytes>, http_ng_core::Error>>> {
        std::task::Poll::Ready(self.0.take().map(Err))
    }
    fn is_end_stream(&self) -> bool {
        self.0.is_none()
    }
}

/// `ErrorKind::Body` (the outgoing request body itself failing, not the
/// response) surviving `Client::execute` — see the module-level comment
/// above for the mutation that proves this test is load-bearing, not
/// decorative.
#[tokio::test]
async fn streaming_request_body_error_kind_survives_the_client() {
    let addr = spawn_h1_server();
    let t = Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio));
    let c = Client::builder(t).build().unwrap();
    let body = http_ng_core::RequestBody::Streaming(Box::new(OneShotErrBody(Some(
        http_ng_core::Error::new(ErrorKind::Body, std::io::Error::other("stream broke")),
    ))));
    let err = tokio::time::timeout(BOUND, c.post(&format!("http://{addr}/")).body(body).send())
        .await
        .expect("must not hang")
        .unwrap_err();
    assert_eq!(*err.kind(), ErrorKind::Body, "{err}");
}

// --- Final review, F1: does the declared connect timeout actually fire? ---

/// Polls `fut` in a bare loop — no parking, no reactor, and (unlike the
/// `std::thread::spawn` + `std::process::exit` watchdog this replaces)
/// no second thread — until it resolves or `bound` elapses. Returns `None`
/// on timeout, letting the caller fail through an ordinary `panic!`/
/// `assert!` instead.
///
/// **Why this replaces a watchdog thread, final review round 3.** Measured
/// directly (throwaway experiment, not assumed): a `std::process::exit`
/// called from a spawned thread, even immediately after an `eprintln!`,
/// prints nothing but `error: test failed` under a plain `cargo test` —
/// libtest's output capture swallows the message and, because `exit`
/// terminates the process before libtest's own harness can report a named
/// failure, the test name is gone too. Only `--nocapture` reveals either.
/// That is exactly the failure mode Task 3 and Task 9's bounded-wait work
/// spent effort eliminating elsewhere in this vertical ("a stalled run
/// gives no test name and no diagnosis") — reproduced here in a different
/// shape, guarding the one capability whose silent no-op blocked this
/// branch.
///
/// A watchdog **thread** is unnecessary here in particular: the future this
/// helper drives (`Client::execute`'s `send()`, backed by `NeverConnects`'s
/// `TcpConnect::connect`, which resolves via `std::future::pending()`) never
/// blocks the calling thread — polling it is instant and returns `Pending`
/// immediately, every time. What made the old thread-based watchdog seem
/// necessary was routing through `futures_executor::block_on`, which PARKS
/// the calling thread on a waker that a `pending()`-backed future will never
/// invoke — an external thread was the only thing that could ever unpark
/// it. Polling directly, without parking, sidesteps that: the same thread
/// that's running the `#[test]` fn just asks again, on a short interval,
/// until `bound` says to stop asking. Same trick `Native::testing::
/// BlockingIo`'s doc comment already names ("busy-spin, not park, because
/// nothing here can wake a parked thread") — applied to a future instead of
/// raw socket I/O.
///
/// Scoped to this file rather than published from `Native::testing`: no
/// other test in this crate currently needs a bounded driver for a plain,
/// non-tokio `#[test]` (`connect.rs`/`h1.rs`/`dual_runtime.rs`'s own
/// watchdog helpers guard different properties, reviewed and accepted in
/// earlier rounds, and carry the same `process::exit` shape unconverted —
/// noted as a followup, not fixed here, since converting them isn't what
/// this review found; blocking-severity findings get fixed on sight,
/// pre-existing lower-severity siblings don't get silently swept in). The
/// pattern is what's meant to be reused, not this specific function.
fn poll_bounded<F: Future>(fut: F, bound: Duration) -> Option<F::Output> {
    let mut fut = std::pin::pin!(fut);
    let start = std::time::Instant::now();
    let waker = std::task::Waker::noop();
    let mut cx = std::task::Context::from_waker(waker);
    loop {
        if let std::task::Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
            return Some(v);
        }
        if start.elapsed() >= bound {
            return None;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// `TcpConnect` whose `connect` future never resolves — the timer is the
/// only thing that can end this race.
struct NeverConnects;
struct NeverStream;
impl hyper::rt::Read for NeverStream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        _b: hyper::rt::ReadBufCursor<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Pending
    }
}
impl hyper::rt::Write for NeverStream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        b: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::task::Poll::Ready(Ok(b.len()))
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}
impl http_ng_rt::TcpConnect for NeverConnects {
    type Stream = NeverStream;
    async fn connect(
        &self,
        _addr: std::net::SocketAddr,
        _opts: &http_ng_rt::TcpOpts,
    ) -> std::io::Result<NeverStream> {
        std::future::pending().await
    }
}
/// A real clock (not a virtual one, unlike `connect.rs`'s `FakeRt`): this
/// probe needs to observe that a REAL 50 ms deadline actually elapses in
/// wall-clock time relative to a real `std::thread::sleep`-based watchdog,
/// not just that the scheduler's arithmetic is internally consistent.
/// A real wall-clock sleep with no reactor, as a **named** future.
///
/// Both clocks in this file need one and both used to write the same
/// `async fn` body; `Timer::Sleep` needs a name, and needing a name is
/// what merged the two copies. The construction is unchanged: a dedicated
/// thread plus a polled flag, the same shape `connect.rs`'s own
/// `bounded_block_on` watchdog and `Native::testing::BlockingIo` use for
/// "no reactor, but still real wall-clock time".
struct ThreadSleep {
    done: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl ThreadSleep {
    fn new(d: Duration) -> Self {
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done2 = done.clone();
        std::thread::spawn(move || {
            std::thread::sleep(d);
            done2.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        Self { done }
    }
}

impl std::future::Future for ThreadSleep {
    type Output = ();
    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<()> {
        if self.done.load(std::sync::atomic::Ordering::SeqCst) {
            std::task::Poll::Ready(())
        } else {
            cx.waker().wake_by_ref();
            std::task::Poll::Pending
        }
    }
}

impl http_ng_rt::Timer for NeverConnects {
    type Instant = std::time::Instant;
    type Sleep = ThreadSleep;
    fn sleep(&self, d: Duration) -> ThreadSleep {
        ThreadSleep::new(d)
    }
    fn now(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
    fn elapsed_since(&self, earlier: std::time::Instant) -> Duration {
        earlier.elapsed()
    }
}

struct OneUnroutableAddr;
impl Resolve for OneUnroutableAddr {
    fn lookup_ipv4(
        &self,
        _name: &str,
    ) -> impl futures_util::Stream<Item = Result<ResolvedAddr, http_ng_core::Error>> {
        futures_util::stream::iter([Ok(ResolvedAddr {
            addr: std::net::IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, 7)),
            ttl: None,
        })])
    }
    fn lookup_ipv6(
        &self,
        _name: &str,
    ) -> impl futures_util::Stream<Item = Result<ResolvedAddr, http_ng_core::Error>> {
        futures_util::stream::empty()
    }
}

struct NoOpTls;
impl http_ng_tls::TlsConnect for NoOpTls {
    type Stream<S>
        = S
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin;
    /// One stub, one configuration, one identity — drawn once rather than
    /// per call, which is what `TlsConnect::config_id` requires.
    fn config_id(&self) -> http_ng_tls::TlsConfigId {
        static ID: std::sync::OnceLock<http_ng_tls::TlsConfigId> = std::sync::OnceLock::new();
        *ID.get_or_init(http_ng_tls::TlsConfigId::new_unique)
    }
    async fn connect<S>(
        &self,
        io: S,
        _req: http_ng_tls::TlsRequest<'_>,
    ) -> Result<(S, http_ng_tls::TlsInfo), http_ng_core::Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin,
    {
        Ok((io, http_ng_tls::TlsInfo::default()))
    }
}

/// Final review, F1 (blocking): `Native` declared `timeouts.connect = true`
/// while nothing in `execute` ever read `Timeouts` — `check_timeouts_supported`
/// let a connect timeout through `build()` because the capability said so,
/// and then it did nothing, forever. This is the review's own probe (its
/// `StuckRt`/`OneAddr`/`NeverStream` renamed to match this file's naming) —
/// not a rewrite of what it tests, since the point is to verify the
/// review's own finding, not a different one. Driven through `poll_bounded`
/// rather than a watchdog thread — see that function's doc comment (final
/// review round 3: the thread-based version's failure message and even its
/// test name were invisible under a plain `cargo test`).
#[test]
fn declared_connect_timeout_is_actually_applied() {
    let t = Native::new(NeverConnects, NoOpTls, OneUnroutableAddr);
    let c = Client::builder(t)
        .timeouts(http_ng::Timeouts {
            connect: Some(Duration::from_millis(50)),
            ..Default::default()
        })
        .build()
        .expect("Native declares timeouts.connect = true, so build() must accept this");

    let result = poll_bounded(
        c.get("http://example.invalid/").send(),
        Duration::from_secs(5),
    );
    let err = match result {
        None => panic!(
            "a 50 ms connect timeout never fired after 5 s — the declared capability is a \
             silent no-op (final review, F1)"
        ),
        Some(Ok(_)) => panic!("connect never completes, so this must be a timeout, not a success"),
        Some(Err(e)) => e,
    };
    assert_eq!(
        *err.kind(),
        ErrorKind::Timeout(http_ng_core::Phase::Connect),
        "{err}"
    );
}

// --- Final review round 3, item 2: what does the connect deadline cover,
// and is its declared duration actually the one used? ---

/// `TcpConnect` that never completes any attempt (same shape as
/// `NeverConnects`), but records the wall-clock time each attempt started —
/// the observable `connect_timeout_covers_the_whole_race_not_a_single_
/// attempt` needs to tell "the deadline stopped the race early" apart from
/// "Happy Eyeballs simply ran out of addresses on its own."
#[derive(Clone, Default)]
struct LoggingNeverConnects {
    attempts: std::sync::Arc<std::sync::Mutex<Vec<std::time::Instant>>>,
}
impl http_ng_rt::TcpConnect for LoggingNeverConnects {
    type Stream = NeverStream;
    async fn connect(
        &self,
        _addr: std::net::SocketAddr,
        _opts: &http_ng_rt::TcpOpts,
    ) -> std::io::Result<NeverStream> {
        self.attempts
            .lock()
            .unwrap()
            .push(std::time::Instant::now());
        std::future::pending().await
    }
}
impl http_ng_rt::Timer for LoggingNeverConnects {
    type Instant = std::time::Instant;
    type Sleep = ThreadSleep;
    fn sleep(&self, d: Duration) -> ThreadSleep {
        ThreadSleep::new(d)
    }
    fn now(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
    fn elapsed_since(&self, earlier: std::time::Instant) -> Duration {
        earlier.elapsed()
    }
}

/// Five addresses, all unroutable — enough that Happy Eyeballs alone
/// (`HeConfig::default()`'s `attempt_delay`, 250 ms, staggering each new
/// attempt) would still be launching attempts long after either deadline
/// below has expired, if nothing cut it off first.
struct FiveUnroutableAddrs;
impl Resolve for FiveUnroutableAddrs {
    fn lookup_ipv4(
        &self,
        _name: &str,
    ) -> impl futures_util::Stream<Item = Result<ResolvedAddr, http_ng_core::Error>> {
        futures_util::stream::iter((1..=5u8).map(|n| {
            Ok(ResolvedAddr {
                addr: std::net::IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, n)),
                ttl: None,
            })
        }))
    }
    fn lookup_ipv6(
        &self,
        _name: &str,
    ) -> impl futures_util::Stream<Item = Result<ResolvedAddr, http_ng_core::Error>> {
        futures_util::stream::empty()
    }
}

/// Final review round 3, item 2: `declared_connect_timeout_is_actually_
/// applied` above proves a deadline exists and fires; it does not prove
/// WHAT it covers or that its DURATION is the one the caller asked for —
/// with only one address and one deadline, a hardcoded duration or a
/// deadline re-armed per attempt would both still produce a `Timeout`
/// error, and that test would stay green either way.
///
/// This test tells those apart. `connect::connect` always builds
/// `HeConfig::default()` internally (`Native` has no way to configure it),
/// so its 250 ms `attempt_delay` is a fact about today's code, not a choice
/// made here: with five never-connecting addresses, attempts start at
/// roughly t=0, 250 ms, 500 ms, 750 ms, 1000 ms. Two declared deadlines,
/// far apart, then discriminate the three candidate behaviours without
/// pinning an exact attempt count:
///
/// - **A deadline applied per attempt** cuts off each `pending()` connect
///   on its own and moves to the next address, so all five are tried.
/// - **No bound on the race at all** likewise reaches all five.
///   Both cases are caught by requiring FEWER than five for either
///   deadline.
/// - **A hardcoded duration, ignoring what the caller asked for** would
///   produce the same count for both deadlines. Caught by requiring the
///   long one to see STRICTLY MORE attempts than the short one.
///
/// The exact counts are deliberately not asserted. They were once — `== 1`
/// for a 150 ms deadline and `== 2` for 400 ms — and it failed on
/// `macos-latest` with 2 where it demanded 1. Nothing was wrong with
/// `connect`: `sleep` in this file is a real `thread::sleep` on a helper
/// thread (there is no reactor here), and a loaded shared runner can start
/// the DEADLINE's thread late enough that the t≈250 ms attempt begins
/// first. A 100 ms margin is not a margin on that hardware. The two
/// deadlines here are 100 ms and 620 ms — one attempt against three — so
/// scheduler slop would have to exceed half a second to collapse the
/// strict inequality.
#[test]
fn connect_timeout_covers_the_whole_race_not_a_single_attempt() {
    let run = |d: Duration| -> (ErrorKind, usize) {
        let rt = LoggingNeverConnects::default();
        let t = Native::new(rt.clone(), NoOpTls, FiveUnroutableAddrs);
        let c = Client::builder(t)
            .timeouts(http_ng::Timeouts {
                connect: Some(d),
                ..Default::default()
            })
            .build()
            .unwrap();
        let result = poll_bounded(
            c.get("http://example.invalid/").send(),
            Duration::from_secs(10),
        );
        let err = result
            .expect("must not hang")
            .expect_err("none of the five addresses ever connects");
        (err.kind().clone(), rt.attempts.lock().unwrap().len())
    };

    // 100 ms: expected to admit the t≈0 attempt and nothing else.
    let short = Duration::from_millis(100);
    let (kind, short_attempts) = run(short);
    assert_eq!(
        kind,
        ErrorKind::Timeout(http_ng_core::Phase::Connect),
        "{kind:?}"
    );
    assert!(
        (1..5).contains(&short_attempts),
        "a {short:?} deadline must cut the race off before all five addresses are tried; a \
         count of 5 would mean the deadline bounds a single attempt rather than connect() as \
         a whole, or bounds nothing at all. Got {short_attempts}"
    );

    // 620 ms: expected to admit t≈0, 250 ms and 500 ms.
    let long = Duration::from_millis(620);
    let (kind, long_attempts) = run(long);
    assert_eq!(
        kind,
        ErrorKind::Timeout(http_ng_core::Phase::Connect),
        "{kind:?}"
    );
    assert!(
        (1..5).contains(&long_attempts),
        "a {long:?} deadline must cut the race off before all five addresses are tried; a \
         count of 5 would mean the deadline bounds a single attempt rather than connect() as \
         a whole, or bounds nothing at all. Got {long_attempts}"
    );
    assert!(
        long_attempts > short_attempts,
        "a {long:?} deadline must let strictly more attempts start than a {short:?} one — \
         equal counts mean the deadline ignored the requested duration in favour of a fixed \
         value, which the two-deadline shape of this test exists to catch. Got \
         {long_attempts} against {short_attempts}"
    );
}

// --- Final review, F3: is the request body actually streamed? ---

struct TwoFrames(u8);
impl http_body::Body for TwoFrames {
    type Data = bytes::Bytes;
    type Error = http_ng_core::Error;
    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<bytes::Bytes>, http_ng_core::Error>>> {
        self.0 += 1;
        std::task::Poll::Ready(match self.0 {
            1 => Some(Ok(http_body::Frame::data(bytes::Bytes::from_static(
                b"AAAA",
            )))),
            2 => Some(Ok(http_body::Frame::data(bytes::Bytes::from_static(
                b"BBBB",
            )))),
            _ => None,
        })
    }
    fn is_end_stream(&self) -> bool {
        self.0 >= 2
    }
}

/// Final review, F3 (major): `Native::new` declared `streaming_request_body
/// = false` and the crate doc comment claimed the request body was
/// "buffered whole" — `body.rs`'s own doc comment, written for a
/// different task, said the opposite about the same code, and this is the
/// tiebreaker: what actually goes out on the wire. `transfer-encoding:
/// chunked` plus two separate frames is only possible if `Native` streams
/// the body instead of buffering it first — a buffered body would go out as
/// one `Content-Length`-framed write.
#[tokio::test]
async fn streaming_request_body_is_actually_streamed_not_buffered() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        if let Ok((mut s, _)) = l.accept() {
            let mut b = [0u8; 4096];
            let n = s.read(&mut b).unwrap_or(0);
            let _ = tx.send(b[..n].to_vec());
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
        }
    });

    let t = Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio));
    let c = Client::builder(t).build().unwrap();
    let body = http_ng_core::RequestBody::Streaming(Box::new(TwoFrames(0)));
    let _ = tokio::time::timeout(BOUND, c.post(&format!("http://{addr}/")).body(body).send())
        .await
        .expect("must not hang");

    let raw = rx
        .recv_timeout(BOUND)
        .expect("server must have seen a request");
    let text = String::from_utf8_lossy(&raw).into_owned();
    assert!(
        text.to_lowercase().contains("transfer-encoding: chunked"),
        "expected chunked framing if the body is genuinely streamed, got:\n{text}"
    );
    assert!(
        text.contains("4\r\nAAAA\r\n4\r\nBBBB\r\n0\r\n"),
        "expected two separate chunk frames (proves streaming, not one \
         collect()-then-write), got:\n{text}"
    );
}

// --- Final review round 3, item 1: does h1.rs's own response-body ---
// --- classification survive to the caller, not just Response::chunk()'s ---
// --- passthrough? ---

/// Final review round 2 found `Response::chunk()` relabeling an
/// already-classified body error (fixed in round 2, F2). Round 3's re-review
/// reproduced the review's ORIGINAL mutation (`h1.rs`'s two
/// `from_hyper_error(e, ErrorKind::Body)` call sites in `NativeBody::
/// poll_frame`, flipped to `ErrorKind::Other`) directly and found it still
/// killed zero tests: round 2's fix made `chunk()` pass through whatever
/// `h1.rs` decided, but nothing forced `h1.rs` itself to decide on a
/// genuine, unclassified transport failure — every existing body-error test
/// injects an already-classified `http_ng_core::Error` via
/// `RequestBody::Streaming`, which `from_hyper_error` recovers from
/// `hyper::Error::source()` without ever reaching its `fallback` argument.
///
/// This test reaches the fallback for real: a server that announces a
/// `Content-Length` far larger than what it actually sends, then closes the
/// connection outright. hyper's own h1 decoder — not anything this crate
/// injects — detects the truncation and returns a `hyper::Error` whose
/// `source()` is NOT an `http_ng_core::Error` (nothing in this call chain
/// ever put one there), so `from_hyper_error`'s `None => Error::new(fallback,
/// e)` branch is what classifies it. That's the exact branch the review's
/// mutation targets.
#[tokio::test]
async fn truncated_response_body_reports_body_kind_from_the_h1_layer() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        if let Ok((mut s, _)) = l.accept() {
            let mut b = [0u8; 2048];
            let _ = s.read(&mut b);
            // Promise 1000 bytes, send 5, then drop the socket — no clean
            // shutdown, no `Content-Length` satisfied.
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\nshort");
        }
        // `s` (and the listener, after this one connection) drops here,
        // closing the socket without a TLS-style clean close or a
        // `Connection: close`-negotiated shutdown.
    });

    let t = Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio));
    let c = Client::builder(t).build().unwrap();
    let mut resp = tokio::time::timeout(BOUND, c.get(&format!("http://{addr}/")).send())
        .await
        .expect("must not hang")
        .unwrap();

    let mut err = None;
    loop {
        match tokio::time::timeout(BOUND, resp.chunk())
            .await
            .expect("must not hang")
        {
            Some(Ok(_)) => continue,
            Some(Err(e)) => {
                err = Some(e);
                break;
            }
            None => break,
        }
    }
    let err = err.expect(
        "a response body truncated far short of its declared Content-Length must surface as \
         an error, not a clean end of stream",
    );
    assert_eq!(*err.kind(), ErrorKind::Body, "{err}");
}
