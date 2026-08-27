//! `Native<R, T, D>: Transport` — this crate's integration tests against
//! `hclient::Client`, not just against `crate::testing::*` directly: what
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
//! empty. In that shape `Cancelled` (an ordinary runtime shutdown) is
//! indistinguishable from
//! "this name doesn't resolve," and a circuit breaker keyed on `Resolve`
//! would wrongly blacklist a live host during an ordinary shutdown.
//!
//! It is solved ONCE, structurally, in `connect::drive`/
//! `ResolveErrors::distinguishing_error` — checked BEFORE both
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

use hclient::Client;
use hclient_core::ErrorKind;
use hclient_core::unversioned::Transport;
use hclient_dns::{Resolve, ResolvedAddr};
use hclient_dns_system::SystemDns;
use hclient_native::Native;
use hclient_rt_tokio::Tokio;
use hclient_tls_rustls::Rustls;
use std::error::Error as StdError;
use std::fmt::Display;
use std::future::Future;
use std::io::{Read, Write};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::Ordering;
use std::task::Context;
use std::task::Poll;
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

/// **What this test can and cannot prove.** It reads the same struct
/// literal `Native::new` wrote; it cannot tell a truthful capability from
/// a lying one, because both read back identically here. Two of these
/// assertions have been wrong in exactly that unfalsifiable way
/// (`streaming_request_body` claimed `false` while the body was genuinely
/// streamed; `timeouts.connect` claimed `true` while nothing in `execute`
/// ever read `Timeouts` at all) — a test that only reads fields agreed
/// with both mistakes, because it never asked whether the field matched
/// behaviour. The two claims that actually need a
/// behavioral witness have one, elsewhere in this file:
/// `streaming_request_body` → `streaming_request_body_is_actually_streamed_not_buffered`
/// (captures wire bytes, asserts `transfer-encoding: chunked`);
/// `timeouts.connect` → `declared_connect_timeout_is_actually_applied`
/// (a `TcpConnect` that never resolves, raced against a real timeout).
///
/// **A third was wrong the same way and is corrected here (v0.4 W1).**
/// `redirects` read `RedirectSupport::Configurable` — "we set the policy" —
/// for a crate that reads no `RedirectPolicy` and follows no `Location`,
/// and this assertion agreed with it for three verticals. It reads
/// `Transparent` now, and the variant that made the mistake expressible is
/// deleted.
///
/// This assertion is also the *only* thing in the workspace that can catch
/// this field changing, and that was measured rather than assumed: with
/// `Native` made to declare each other variant in turn and all 362 tests of
/// `-p hclient-native -p hclient --all-features` run, `None` fails this
/// test and nothing else, `Transparent` (before the fix) failed this test
/// and nothing else, and only `Internal` gets a second, behavioural witness
/// — `hclient::deadline::the_deadline_spans_redirect_hops_rather_than_restarting_on_each`,
/// which dies at `build()` with `UnsupportedCapability { what:
/// "redirect_policy" }` because `check_redirect_supported` refuses a
/// configured policy against an `Internal` backend. A behavioural witness
/// for `Transparent` specifically is not available and is not being
/// claimed: `Client`'s redirect stage follows a 3xx whatever this field
/// says, so no server-side observation can separate `Transparent` from
/// `None`. The honest guard for those two is that the variant set is small
/// enough for the read-back to be reviewable.
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
        caps.timeouts.first_byte,
        "enforced since v0.2 W4's middle bullet — see tests/timeouts.rs,          where a server sends a head and then nothing"
    );
    assert!(
        caps.timeouts.between_bytes,
        "enforced since v0.2 W4's middle bullet — see tests/timeouts.rs,          where a server stalls mid-body"
    );
    assert_eq!(caps.tls_config, hclient_core::TlsSupport::Full);
    assert_eq!(
        caps.redirects,
        hclient_core::RedirectSupport::Transparent,
        "this crate follows nothing: a 3xx comes back as an ordinary response \
         and Client's redirect stage owns the chain"
    );
    assert!(
        caps.request_trailers,
        "declared and enforced in one change: both protocols this \
         transport speaks put a request \
         body's trailers on the wire, and the `Trailer:` header HTTP/1.1 \
         additionally wants is RFC 9110 §6.6.2's requirement of a sender \
         rather than a limitation of ours — a request that omits it is \
         malformed, and gets `UndeclaredRequestTrailers` instead of the \
         silent drop it used to get. See tests/request_trailers.rs, which \
         reads the field off a raw socket on HTTP/1.1 and off an \
         `h2::server`'s decoded stream on HTTP/2"
    );
    assert!(caps.version_reported);
    assert!(
        caps.version_select,
        "declared and enforced in one change (v0.4 W2): `execute` reads a \
         `RequireVersion` demand, narrows the ALPN offer to what it admits, \
         filters pool buckets by it, and refuses with `VersionNotAvailable` \
         before the head — see tests/require_version.rs, which watches the \
         refusal from the server's side of the socket"
    );
}

/// `capabilities_are_honest_about_v01_limits` above only samples the fields
/// the brief calls out. This one destructures the rest of the struct so a
/// field nobody asked `Native` to turn on can't silently end up
/// `true`/non-`None`.
///
/// **What this test cannot do, despite what an earlier name implied.** Unlike
/// `Capabilities::none_is_the_conservative_base` in `hclient-core` (which
/// lives inside that crate and can destructure with no `..` rest pattern, so
/// a brand-new field is a compile error naming it), this file is an
/// *external* crate — `Capabilities` is `#[non_exhaustive]`, so `..` is
/// mandatory here (E0638), and that `..` silently absorbs any field added
/// later. A reviewer built exactly this scenario (added a seventeenth field
/// to `Capabilities`): `hclient-core`'s own internal test failed to compile
/// as designed, and this test compiled and passed without noticing.
/// Renamed from `every_undeclared_capability_stays_at_the_conservative_default`
/// to stop promising that it does — it checks that the fields enumerated
/// below are at their conservative defaults *today*, nothing more. The
/// exhaustiveness guarantee (a new field becomes a compile error, not a
/// silent pass) exists exactly once, inside `hclient-core`'s own
/// `Capabilities::none_is_the_conservative_base` — `#[non_exhaustive]` makes
/// that guarantee structurally unavailable to any test outside the crate
/// that owns the type, this one included (**amendment-C6**, in
/// `docs/exceptions.md`). Any future capabilities-completeness check
/// belongs in `hclient-core`,
/// not in a transport crate like this one. Not the same rule as amendment-C3
/// (which is about where `Send`/`Sync` assertions live, so
/// `no-declared-send`'s `src`-only grep doesn't trip on its own test text) —
/// the two share a "belongs elsewhere" shape but are different amendments,
/// citing one for the other was a mistake caught and corrected once already.
#[tokio::test]
async fn undeclared_capability_fields_match_their_conservative_defaults_today() {
    let t = Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio));
    let hclient_core::Capabilities {
        streaming_request_body: _,
        full_duplex,
        // `request_trailers` left this list in v0.4 and is asserted
        // `true` in `capabilities_are_honest_about_v01_limits` above,
        // where the declared fields live. Its `false` here was never the
        // floor rule holding a line — it was a field nobody had
        // measured, and the measurement says otherwise:
        // HTTP/1.1 sends request trailers, HTTP/2 sends them, and the
        // one shape that lost data now raises.
        request_trailers: _,
        response_trailers,
        redirects: _,
        tls_config: _,
        client_certs,
        proxy,
        owns_cookie_jar,
        owns_cache,
        // `version_select` left this list and is asserted `true` in
        // `capabilities_are_honest_about_v01_limits` above, where the
        // declared fields live. It was about to be deleted for having no
        // reader until `RequireVersion` gave it one, so its move from
        // "undeclared, conservative" to "declared, enforced" is the whole
        // event and belongs where the other declarations are asserted.
        version_select: _,
        version_reported: _,
        timeouts: _,
        informational_1xx,
        forbidden_request_headers,
        ..
    } = *t.capabilities();
    assert!(!full_duplex);
    assert!(!response_trailers);
    assert!(!client_certs);
    assert!(!proxy);
    assert!(!owns_cookie_jar);
    assert!(!owns_cache);
    assert!(!informational_1xx);
    assert!(forbidden_request_headers.is_empty());
}

/// The category `Native` set has to survive all the way to the caller
/// through `Client::execute`'s whole path (see the module doc comment).
/// This test checks exactly that whole path, not `to_error`'s default —
/// that's checked in `hclient-core/tests/shape.rs` and on its own only
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
/// stand-in for a real background thread pool shutting down,
/// good enough to check that `Native::execute` doesn't wrap or flatten it
/// on the way to `Client`.
struct CancelledDns;

#[derive(Debug)]
struct FakeCancelled;
impl Display for FakeCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("resolver background pool went away")
    }
}
impl StdError for FakeCancelled {}

impl Resolve for CancelledDns {
    fn lookup_ipv4(
        &self,
        _name: &str,
    ) -> impl futures_util::Stream<Item = Result<ResolvedAddr, hclient_core::Error>> {
        futures_util::stream::once(async {
            Err(hclient_core::Error::new(
                ErrorKind::Cancelled,
                FakeCancelled,
            ))
        })
    }
    fn lookup_ipv6(
        &self,
        _name: &str,
    ) -> impl futures_util::Stream<Item = Result<ResolvedAddr, hclient_core::Error>> {
        futures_util::stream::once(async {
            Err(hclient_core::Error::new(
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

/// This test used to assert the opposite — that `between_bytes` was
/// refused at `build()` — and it was right for as long as this transport
/// honestly declared `TimeoutSupport::between_bytes = false`. Both halves
/// moved together (the rule v0.2 W4's middle bullet was written under), so
/// the assertion moved with them: all three phases are now settable here,
/// and each one is enforced against a server in `tests/timeouts.rs`.
///
/// The refusal itself has not gone anywhere and is not this crate's to
/// check: `check_supported` lives in `hclient`, and `hclient/src/config.rs`
/// has a case per phase against capabilities that declare them `false`,
/// including that the error names the RIGHT phase. What would be lost is a
/// test asserting a refusal that this transport can no longer produce.
#[tokio::test]
async fn every_timeout_phase_is_accepted_at_build_time_now_that_each_is_enforced() {
    let t = Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio));
    Client::builder(t)
        .timeouts(hclient::Timeouts {
            resolve: None,
            connect: Some(Duration::from_secs(1)),
            first_byte: Some(Duration::from_secs(1)),
            between_bytes: Some(Duration::from_secs(1)),
        })
        .build()
        .expect("all three phases are declared and enforced");
}

/// A TLS handshake that fails (the server accepted the TCP connection and
/// immediately dropped it, without sending a single byte of TLS) — checks
/// that the `Tls` category (`TlsConnect::connect`) also survives
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
// named, and neither had a test of its own at this composed layer — both
// pass on unmodified code (no bug), but nothing held that property in
// place: wrapping `h1::exchange`'s error in a fresh
// `Error::new(ErrorKind::Connect, e)` in `src/lib.rs` turns the `Body` test
// below red while every other test in this file stays green (verified
// directly, see this task's report). The two tests below close that gap.

/// `ErrorKind::Connect` (`connect::connect`'s own `AllAttemptsFailed`,
/// `ErrorKind::Connect`) surviving `Client::execute`, the same
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
struct OneShotErrBody(Option<hclient_core::Error>);
impl http_body::Body for OneShotErrBody {
    type Data = bytes::Bytes;
    type Error = hclient_core::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<bytes::Bytes>, hclient_core::Error>>> {
        Poll::Ready(self.0.take().map(Err))
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
    let body = hclient_core::RequestBody::Streaming(Box::new(OneShotErrBody(Some(
        hclient_core::Error::new(ErrorKind::Body, std::io::Error::other("stream broke")),
    ))));
    let err = tokio::time::timeout(BOUND, c.post(&format!("http://{addr}/")).body(body).send())
        .await
        .expect("must not hang")
        .unwrap_err();
    assert_eq!(*err.kind(), ErrorKind::Body, "{err}");
}

// --- Does the declared connect timeout actually fire? ---

/// Polls `fut` in a bare loop — no parking, no reactor, and (unlike the
/// `std::thread::spawn` + `std::process::exit` watchdog this replaces)
/// no second thread — until it resolves or `bound` elapses. Returns `None`
/// on timeout, letting the caller fail through an ordinary `panic!`/
/// `assert!` instead.
///
/// **Why this replaces a watchdog thread.** Measured directly, not
/// assumed: a `std::process::exit`
/// called from a spawned thread, even immediately after an `eprintln!`,
/// prints nothing but `error: test failed` under a plain `cargo test` —
/// libtest's output capture swallows the message and, because `exit`
/// terminates the process before libtest's own harness can report a named
/// failure, the test name is gone too. Only `--nocapture` reveals either.
/// That is the failure mode the bounded waits elsewhere in this crate
/// exist against — "a stalled run gives no test name and no diagnosis" —
/// in a different shape, guarding the one capability whose silent no-op
/// blocked this
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
    let mut cx = Context::from_waker(waker);
    loop {
        if let Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
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
///
/// `Clone` because `Native`'s `Transport` impl asks it of every runtime:
/// the response body carries a clock of its own to enforce
/// `between_bytes`, and a clock that cannot be cloned cannot be carried.
#[derive(Clone)]
struct NeverConnects;
struct NeverStream;
impl hyper::rt::Read for NeverStream {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _b: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<std::io::Result<()>> {
        Poll::Pending
    }
}
impl hyper::rt::Write for NeverStream {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        b: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Poll::Ready(Ok(b.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
impl hclient_rt::TcpConnect for NeverConnects {
    type Stream = NeverStream;
    type Connecting<'a>
        = std::pin::Pin<
        Box<dyn std::future::Future<Output = std::io::Result<NeverStream>> + Send + 'a>,
    >
    where
        Self: 'a;

    fn connect<'a>(
        &'a self,
        _addr: std::net::SocketAddr,
        _opts: &hclient_rt::TcpOpts,
    ) -> Self::Connecting<'a> {
        Box::pin(async move { std::future::pending().await })
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
    done: Arc<std::sync::atomic::AtomicBool>,
}

impl ThreadSleep {
    fn new(d: Duration) -> Self {
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done2 = done.clone();
        std::thread::spawn(move || {
            std::thread::sleep(d);
            done2.store(true, Ordering::SeqCst);
        });
        Self { done }
    }
}

impl std::future::Future for ThreadSleep {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.done.load(Ordering::SeqCst) {
            Poll::Ready(())
        } else {
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

impl hclient_rt::Timer for NeverConnects {
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
    ) -> impl futures_util::Stream<Item = Result<ResolvedAddr, hclient_core::Error>> {
        futures_util::stream::iter([Ok(ResolvedAddr {
            addr: std::net::IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, 7)),
            ttl: None,
        })])
    }
    fn lookup_ipv6(
        &self,
        _name: &str,
    ) -> impl futures_util::Stream<Item = Result<ResolvedAddr, hclient_core::Error>> {
        futures_util::stream::empty()
    }
}

struct NoOpTls;
impl hclient_tls::TlsIdentity for NoOpTls {
    fn config_id(&self) -> hclient_tls::TlsConfigId {
        static ID: OnceLock<hclient_tls::TlsConfigId> = OnceLock::new();
        *ID.get_or_init(hclient_tls::TlsConfigId::new_unique)
    }
}

/// `NoOpTls`'s twin, differing in the one answer under test — the shape
/// `hclient-h3`'s `StubTls` uses one crate over, and the reason the
/// assertion below is a pair rather than a single `true`.
struct CertTls;
impl hclient_tls::TlsIdentity for CertTls {
    fn config_id(&self) -> hclient_tls::TlsConfigId {
        static ID: OnceLock<hclient_tls::TlsConfigId> = OnceLock::new();
        *ID.get_or_init(hclient_tls::TlsConfigId::new_unique)
    }
    fn presents_client_certs(&self) -> bool {
        true
    }
}

impl hclient_tls::TlsConnect for CertTls {
    type Stream<S>
        = S
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin;

    async fn connect<S>(
        &self,
        _: S,
        _: hclient_tls::TlsRequest<'_>,
    ) -> Result<(Self::Stream<S>, hclient_tls::TlsInfo), hclient_core::Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin,
    {
        unreachable!("this stub never connects")
    }
}

/// **`client_certs` is the TLS backend's answer, not this transport's.**
///
/// It was `Capabilities::default()`'s `false` for every `T`, which understated
/// two backends that can present one: `hclient-tls-native-tls` through its
/// `identity()` setter, and `hclient-tls-rustls` through a `from_config`
/// whose config was built with `with_client_auth_cert`. The `true` arm is
/// the one no constant here could have produced.
#[test]
fn client_certs_is_read_from_the_tls_backend_not_from_a_constant() {
    use hclient_core::unversioned::Transport;
    let rt = hclient_rt_tokio::Tokio;
    let plain = Native::new(rt, NoOpTls, hclient_dns::IpLiteralOnly);
    let certs = Native::new(rt, CertTls, hclient_dns::IpLiteralOnly);
    assert!(!plain.capabilities().client_certs);
    assert!(certs.capabilities().client_certs);
}

impl hclient_tls::TlsConnect for NoOpTls {
    type Stream<S>
        = S
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin;
    /// One stub, one configuration, one identity — drawn once rather than
    /// per call, which is what `TlsConnect::config_id` requires.
    async fn connect<S>(
        &self,
        io: S,
        _req: hclient_tls::TlsRequest<'_>,
    ) -> Result<(S, hclient_tls::TlsInfo), hclient_core::Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin,
    {
        Ok((io, hclient_tls::TlsInfo::default()))
    }
}

/// `Native` declaring `timeouts.connect = true` while nothing in
/// `execute` reads `Timeouts` means `check_timeouts_supported` lets a
/// connect timeout through `build()` because the capability says so, and
/// then it does nothing, forever. Driven through `poll_bounded` rather
/// than a watchdog thread — see that function's doc comment: a
/// thread-based version's failure message and even its test name are
/// invisible under a plain `cargo test`.
#[test]
fn declared_connect_timeout_is_actually_applied() {
    let t = Native::new(NeverConnects, NoOpTls, OneUnroutableAddr);
    let c = Client::builder(t)
        .timeouts(hclient::Timeouts {
            resolve: None,
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
             silent no-op"
        ),
        Some(Ok(_)) => panic!("connect never completes, so this must be a timeout, not a success"),
        Some(Err(e)) => e,
    };
    assert_eq!(
        *err.kind(),
        ErrorKind::Timeout(hclient_core::Phase::Connect),
        "{err}"
    );
}

// --- What does the connect deadline cover, and is its declared duration
// actually the one used? ---

/// `TcpConnect` that never completes any attempt (same shape as
/// `NeverConnects`), but records the wall-clock time each attempt started —
/// the observable `connect_timeout_covers_the_whole_race_not_a_single_
/// attempt` needs to tell "the deadline stopped the race early" apart from
/// "Happy Eyeballs simply ran out of addresses on its own."
#[derive(Clone, Default)]
struct LoggingNeverConnects {
    attempts: Arc<Mutex<Vec<std::time::Instant>>>,
}
impl hclient_rt::TcpConnect for LoggingNeverConnects {
    type Stream = NeverStream;
    type Connecting<'a>
        = std::pin::Pin<
        Box<dyn std::future::Future<Output = std::io::Result<NeverStream>> + Send + 'a>,
    >
    where
        Self: 'a;

    fn connect<'a>(
        &'a self,
        _addr: std::net::SocketAddr,
        _opts: &hclient_rt::TcpOpts,
    ) -> Self::Connecting<'a> {
        Box::pin(async move {
            self.attempts
                .lock()
                .unwrap()
                .push(std::time::Instant::now());
            std::future::pending().await
        })
    }
}
impl hclient_rt::Timer for LoggingNeverConnects {
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
    ) -> impl futures_util::Stream<Item = Result<ResolvedAddr, hclient_core::Error>> {
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
    ) -> impl futures_util::Stream<Item = Result<ResolvedAddr, hclient_core::Error>> {
        futures_util::stream::empty()
    }
}

/// `declared_connect_timeout_is_actually_applied` above proves a deadline
/// exists and fires; it does not prove
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
            .timeouts(hclient::Timeouts {
                resolve: None,
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
        ErrorKind::Timeout(hclient_core::Phase::Connect),
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
        ErrorKind::Timeout(hclient_core::Phase::Connect),
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

// --- Is the request body actually streamed? ---

struct TwoFrames(u8);
impl http_body::Body for TwoFrames {
    type Data = bytes::Bytes;
    type Error = hclient_core::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<bytes::Bytes>, hclient_core::Error>>> {
        self.0 += 1;
        Poll::Ready(match self.0 {
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

/// `Native::new` once declared `streaming_request_body = false` while
/// `body.rs`'s own doc comment said the opposite about the same code. This
/// is the tiebreaker: what actually goes out on the wire. `transfer-encoding:
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
    let body = hclient_core::RequestBody::Streaming(Box::new(TwoFrames(0)));
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

// --- Does h1.rs's own response-body classification survive to the ---
// --- caller, not just Response::chunk()'s passthrough? ---

/// Stopping `Response::chunk()` from relabeling an already-classified body
/// error is only half of it. The mutation that flips `h1.rs`'s two
/// `from_hyper_error(e, ErrorKind::Body)` call sites in
/// `NativeBody::poll_frame` to `ErrorKind::Other` kills zero tests
/// otherwise: `chunk()` passing through whatever `h1.rs` decided does not
/// force `h1.rs` itself to decide on a
/// genuine, unclassified transport failure — every existing body-error test
/// injects an already-classified `hclient_core::Error` via
/// `RequestBody::Streaming`, which `from_hyper_error` recovers from
/// `hyper::Error::source()` without ever reaching its `fallback` argument.
///
/// This test reaches the fallback for real: a server that announces a
/// `Content-Length` far larger than what it actually sends, then closes the
/// connection outright. hyper's own h1 decoder — not anything this crate
/// injects — detects the truncation and returns a `hyper::Error` whose
/// `source()` is NOT an `hclient_core::Error` (nothing in this call chain
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
