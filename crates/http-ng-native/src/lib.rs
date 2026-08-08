//! Native transport for http-ng: TCP + TLS + HTTP/1.1 on top of hyper.
//!
//! This crate wires together the runtime ([`http_ng_rt`]), DNS ([`http_ng_dns`])
//! and TLS ([`http_ng_tls`]) on top of `hyper`. Task 10 laid down the request
//! body adapter ([`body`], `pub(crate)`); Task 11 added the connector
//! ([`connect`], also `pub(crate)`); Task 12 added the HTTP/1 driver
//! ([`h1`], `pub(crate)`); Task 13 assembles all of this into [`Native`] —
//! the crate's only public type, which implements `http_ng_core::
//! unversioned::Transport`.
//!
//! # `Native::execute` does not resolve DNS itself
//!
//! This task's draft (`task-13-brief.md`) resolved addresses by hand —
//! `.filter_map(|r| async { r.ok() })` over `Resolve::lookup_ipv4`/
//! `lookup_ipv6`, discarding ANY resolver error (including
//! `ErrorKind::Cancelled` — an ordinary background-pool shutdown, Task 7)
//! and synthesizing a single `ErrorKind::Resolve` if both streams turned
//! out empty. Review Task 7 already found exactly this defect in exactly
//! this spot: conflating "the resolver failed" with "the runtime is
//! shutting down" breaks a circuit breaker keyed on `Resolve` — it would
//! wrongly blacklist a live host during an ordinary shutdown.
//!
//! Task 11 solved this once, structurally, in `connect::drive`/
//! `ResolveErrors::distinguishing_error` (see `connect.rs`'s doc comment):
//! a `kind()` that differs from the synthetic `Resolve` is checked BEFORE
//! either failure branch, so discarding it becomes structurally
//! unreachable rather than merely handled for the one case that was found.
//! `execute` below therefore does not resolve or run Happy Eyeballs itself —
//! it calls `connect::connect`, the same entry point already covered by
//! `connect.rs`'s unit tests; `crates/http-ng-native/tests/transport.rs`'s
//! `resolver_cancelled_error_reaches_the_caller_through_execute_not_flattened`
//! checks that this property survives the whole `Client::execute` path, not
//! just `connect::drive` on its own.
#![forbid(unsafe_code)]

mod body;
mod connect;
mod h1;

use http_ng_core::unversioned::Transport;
use http_ng_core::{
    CancelSupport, Capabilities, Error, ErrorKind, Phase, RedirectSupport, RequestBody,
    TimeoutSupport, Timeouts, UpgradeSupport,
};
use http_ng_dns::Resolve;
use http_ng_rt::{TcpConnect, TcpOpts, Timer};
use http_ng_tls::TlsConnect;
use std::future::Future;
use std::task::Poll;
use std::time::Duration;

/// The http-ng transport over real TCP/TLS/HTTP1: wires together the
/// runtime `R` ([`http_ng_rt::TcpConnect`] + [`http_ng_rt::Timer`]), TLS `T`
/// ([`http_ng_tls::TlsConnect`]) and resolver `D` ([`http_ng_dns::Resolve`]).
///
/// v0.1: one connection per request (no pool), HTTP/1.1 only, no upgrade —
/// see [`Native::new`] and [`Capabilities`], which state these limits
/// honestly rather than staying silent about them. The request body is
/// **not** buffered whole: `RequestBody::Streaming` goes to the wire as a
/// stream (see [`Native::new`]'s doc comment on `streaming_request_body` —
/// an earlier version of this paragraph claimed the opposite and was
/// wrong).
#[derive(Debug)]
pub struct Native<R, T, D> {
    rt: R,
    tls: T,
    dns: D,
    opts: TcpOpts,
    caps: Capabilities,
}

/// `T: TlsConnect` on the constructor only — not on the struct — so the
/// bound is paid where the answer is needed. `new` has to ask the TLS
/// implementation what to advertise; nothing else about `Native` requires
/// knowing.
impl<R, T: TlsConnect, D> Native<R, T, D> {
    pub fn new(rt: R, tls: T, dns: D) -> Self {
        let mut caps = Capabilities::none();
        // Honest about v0.1: no connection pool, no upgrade — the
        // remaining fields stay at the conservative baseline of
        // `Capabilities::none()` (see `tests/transport.rs`'s
        // `undeclared_capability_fields_match_their_conservative_defaults_today`).
        //
        // `streaming_request_body: true` — NOT the same as what was here
        // before the branch's final review (`false`). `body.rs`'s
        // `Inner::Streaming` hands `RequestBody::Streaming` to hyper as a
        // stream instead of buffering it into memory first (see `body.rs`'s
        // doc comment — it has always claimed this, unlike the earlier
        // version of THIS comment); measured on the wire:
        // `tests/transport.rs`'s
        // `streaming_request_body_is_actually_streamed_not_buffered` sees
        // `transfer-encoding: chunked` and separate frames for a
        // two-frame body. Claiming `false` while the code honestly
        // streams would be the same capability lie in reverse —
        // understated, but a caller who believes it will buffer a large
        // body into memory itself when it didn't have to.
        caps.streaming_request_body = true;
        caps.redirects = RedirectSupport::Configurable;
        // Structural, not a promise made in prose: `execute`'s future owns
        // everything the exchange runs on. The connected stream goes into
        // `h1::exchange`, which hands hyper's `Connection` future to
        // `NativeBody` — and until `execute` returns, all of it lives
        // inside this one future. Dropping it drops the `Connection`, which
        // drops the socket, which closes the TCP connection; there is no
        // spawn anywhere on this path (see `h1.rs`'s module doc comment)
        // and therefore nothing left running behind the drop. Measured
        // from the far end rather than argued: `tests/cancel.rs`'s
        // `dropping_the_execute_future_closes_the_connection_the_server_sees`
        // has the server observe its socket close.
        caps.cancel_on_drop = CancelSupport::Supported;
        // Asked, not assumed. This line used to be a hardcoded
        // `TlsSupport::Full` regardless of which `TlsConnect` was plugged
        // in, so `Native<R, NoTls, D>` would have advertised full TLS while
        // refusing every `https://` connect — a capability that lies, of
        // exactly the kind this project has caught in three other backends.
        // `TlsConnect::tls_support` defaults to `Full`, so every real
        // implementation reports what it did before; only a stub differs.
        caps.tls_config = tls.tls_support();
        caps.version_reported = true;
        caps.timeouts = TimeoutSupport {
            // Actually enforced — see `execute`'s race between
            // `connect::connect` and `rt.sleep(d)`, below, and
            // `tests/transport.rs`'s
            // `declared_connect_timeout_is_actually_applied`.
            //
            // Scope, spelled out (final review round 3: the capability
            // alone left this ambiguous — RFC 8305 staggers several TCP
            // attempts, and "connect" could plausibly mean a budget per
            // attempt or one budget for all of them, which are materially
            // different promises to a caller). This is the LATTER: one
            // deadline for `connect::connect` as a whole — DNS resolution,
            // every Happy Eyeballs attempt across every address offered,
            // and (for `https`) the TLS handshake — not a per-attempt
            // budget. `with_connect_timeout` below wraps the entire
            // `connect::connect(..)` future exactly once; there is no
            // per-attempt timer anywhere in this file or in `connect.rs`.
            // `tests/transport.rs`'s `connect_timeout_covers_the_whole_
            // race_not_a_single_attempt` pins this by counting how many
            // Happy Eyeballs attempts two different deadlines let through,
            // not merely that a timeout fires.
            connect: true,
            // v0.1 has neither a pool nor a response timer — claiming
            // these phases would be a capability that lies about its own
            // state.
            first_byte: false,
            between_bytes: false,
        };
        caps.upgrade = UpgradeSupport::None;
        Self {
            rt,
            tls,
            dns,
            opts: TcpOpts::default(),
            caps,
        }
    }

    /// Socket parameters for EVERY TCP attempt this transport makes (see
    /// [`http_ng_rt::TcpOpts`]).
    pub fn tcp_opts(mut self, opts: TcpOpts) -> Self {
        self.opts = opts;
        self
    }
}

impl<R, T, D> Transport for Native<R, T, D>
where
    R: TcpConnect + Timer,
    R::Stream: 'static,
    T: TlsConnect,
    T::Stream<R::Stream>: 'static,
    D: Resolve,
{
    type Body = h1::NativeBody;
    type Error = Error;

    async fn execute(
        &self,
        req: http::Request<RequestBody>,
    ) -> Result<http::Response<Self::Body>, Error> {
        let (parts, body) = req.into_parts();

        // Branch final review, finding F1 (blocking): `Capabilities`
        // claimed `timeouts.connect = true`, but nothing in this file read
        // `Timeouts` from `parts.extensions` — the claimed timeout was a
        // silent no-op, exactly the class of defect (B1 of vertical 1's
        // final review) this channel exists to root out.
        // `Transport::execute`'s doc comment
        // (`http-ng-core/src/unversioned/transport.rs`) spells out the
        // correct reading literally: "presence is not intent",
        // `.get::<Timeouts>().copied().unwrap_or_default()`, then field by
        // field — don't branch on the extension's `is_some()` as a whole.
        let timeouts = parts
            .extensions
            .get::<Timeouts>()
            .copied()
            .unwrap_or_default();

        // See the module doc comment: `connect::connect` isn't reuse for
        // code-size's sake, it's the only path on which a resolver
        // `ErrorKind` that differs from the synthetic one (in particular,
        // `Cancelled`) structurally cannot be discarded. It also resolves
        // the scheme (`http`/`https`, anything else is a typed
        // `ErrorKind::Unsupported`) and optionally runs the TLS handshake
        // with ALPN `http/1.1` — the only protocol `h1::exchange` speaks.
        let connect_fut = connect::connect(
            &self.rt,
            &self.dns,
            &self.tls,
            &parts.uri,
            &self.opts,
            &[b"http/1.1"],
        );
        let (conn, _tls_info) = match timeouts.connect {
            Some(d) => with_connect_timeout(&self.rt, d, connect_fut).await?,
            None => connect_fut.await?,
        };

        let outgoing = body::OutgoingBody::from_request_body(body);
        let mut req = http::Request::from_parts(parts, outgoing);
        // hyper h1 requires origin-form and a `Host:` header — `connect::
        // connect` has already checked both host and scheme, so we don't
        // recheck them here.
        origin_form(&mut req);

        h1::exchange(conn, req).await
    }

    /// Identity: `Self::Error` is already `http_ng_core::Error`, and its
    /// category is set wherever the failure happened (`Resolve`/`Connect`/
    /// `Unsupported` in `connect::connect`, `Tls` in `TlsConnect::connect`,
    /// `Body`/`Connect` in `h1::exchange`). The hook's default would do
    /// exactly the same thing (it recognizes our `Error` and passes it
    /// through unchanged) — the line is behaviorally redundant and
    /// semantically needed: it names the intent where it's read, and it
    /// will survive the default changing later. See the doc comment on
    /// `Transport::to_error` in `http-ng-core`.
    fn to_error(&self, e: Self::Error) -> Error {
        e
    }

    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
}

/// The failure [`with_connect_timeout`] ends in when the timer wins the
/// race against `connect::connect`.
#[derive(Debug, thiserror::Error)]
#[error("connect timed out after {0:?}")]
struct ConnectTimedOut(Duration);

/// Races `fut` against `rt.sleep(d)`: `fut` if it finished first,
/// otherwise `Err(ErrorKind::Timeout(Phase::Connect))` — closes branch
/// final review finding F1.
///
/// `std::future::poll_fn`, polling both arms by hand, rather than
/// `futures_util::select!`/`select` — the same technique and the same
/// reasoning as `connect::drive` (see its doc comment, the section on why
/// the brief's `select_biased!` didn't fit): there are only two arms here
/// and each is needed exactly once, the macro gives this pair nothing that
/// a direct `poll` doesn't.
///
/// Scope (final review round 3): `fut` is expected to be the whole
/// `connect::connect(..)` call, passed in as one opaque future — this
/// function has no visibility into, and no way to time, any individual
/// attempt inside it. That's what makes the deadline an overall one rather
/// than a per-attempt one: there is exactly one `sleep(d)` racing exactly
/// one `fut`, called exactly once per `execute()`, not once per address
/// Happy Eyeballs tries.
async fn with_connect_timeout<R, F, T>(rt: &R, d: Duration, fut: F) -> Result<T, Error>
where
    R: Timer,
    F: Future<Output = Result<T, Error>>,
{
    let mut fut = std::pin::pin!(fut);
    let mut sleep_fut = std::pin::pin!(rt.sleep(d));
    std::future::poll_fn(|cx| {
        if let Poll::Ready(r) = fut.as_mut().poll(cx) {
            return Poll::Ready(r);
        }
        if sleep_fut.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(Error::new(
                ErrorKind::Timeout(Phase::Connect),
                ConnectTimedOut(d),
            )));
        }
        Poll::Pending
    })
    .await
}

/// Rewrites the request URI into origin-form (`hyper`'s h1 client requires
/// exactly that, not absolute-form) and sets `Host:` if the caller didn't
/// set it themselves.
///
/// By the time this is called, `connect::connect` has already succeeded —
/// meaning its own checks (`host()`, `wants_tls()`) already passed, so
/// `req.uri()` is guaranteed to carry a host and a supported (`http`/
/// `https`) scheme; this function doesn't recheck them.
fn origin_form(req: &mut http::Request<body::OutgoingBody>) {
    let uri = req.uri().clone();
    let https = uri.scheme_str() == Some("https");
    let default_port = if https { 443 } else { 80 };
    let port = uri.port_u16().unwrap_or(default_port);
    let host = uri.host().unwrap_or_default();

    if !req.headers().contains_key(http::header::HOST) {
        let authority = if port == default_port {
            host.to_owned()
        } else {
            format!("{host}:{port}")
        };
        // A host that made it to `connect::connect` (having passed DNS
        // resolution, and for `https` also having built a TLS SNI value)
        // is, in practice, always valid as a header value. If it somehow
        // isn't, the request goes out without `Host:`, and that's not a
        // silent loss: no server this crate talks to will accept an
        // HTTP/1.1 request without `Host:`, so the failure will be an
        // immediate, explicit protocol failure, not a silent no-op.
        if let Ok(v) = http::HeaderValue::from_str(&authority) {
            req.headers_mut().insert(http::header::HOST, v);
        }
    }
    let pq = uri
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/")
        .to_owned();
    if let Ok(u) = pq.parse::<http::Uri>() {
        *req.uri_mut() = u;
    }
}

/// For this crate's integration tests only: `pub`, not `pub(crate)`,
/// because `tests/*.rs` compile as a separate external crate and can't see
/// `pub(crate)` items like `connect::race_connect`/`h1::exchange`
/// directly. `#[doc(hidden)]` isn't part of the crate's public API, it's a
/// gap opened specifically for this task's integration tests (see
/// `tests/connect.rs`, `tests/dual_runtime.rs`, `tests/h1.rs`); Task 13
/// need not, and must not, rely on it.
#[doc(hidden)]
pub mod testing {
    /// Runs Happy Eyeballs over a ready-made address list, bypassing DNS —
    /// a wrapper around `connect::race_connect` with a default `HeConfig`
    /// and `TcpOpts`, exactly what a test that only controls the address
    /// list and port needs.
    pub async fn connect_for_test<R>(
        rt: &R,
        addrs: &[std::net::IpAddr],
        port: u16,
    ) -> Result<R::Stream, http_ng_core::Error>
    where
        R: http_ng_rt::TcpConnect + http_ng_rt::Timer,
    {
        let (v6, v4): (Vec<_>, Vec<_>) = addrs.iter().copied().partition(|a| a.is_ipv6());
        crate::connect::race_connect(
            rt,
            v6,
            v4,
            port,
            &http_ng_rt::TcpOpts::default(),
            http_ng_proto::happy_eyeballs::HeConfig::default(),
        )
        .await
    }

    pub use crate::body::OutgoingBody;
    pub use crate::h1::NativeBody;

    /// An empty request body — what any `h1::exchange` test with nothing
    /// to send needs (a bodyless GET).
    pub fn empty_body() -> crate::body::OutgoingBody {
        crate::body::OutgoingBody::from_request_body(http_ng_core::RequestBody::Empty)
    }

    /// `std::net::TcpStream` as `hyper::rt` IO for tests on a bare
    /// executor with no reactor at all — `works_on_a_bare_futures_
    /// executor_with_no_spawn` in `tests/h1.rs` deliberately pulls in
    /// neither `tokio` nor `smol`.
    ///
    /// # Why NOT a literally blocking socket, despite the name
    ///
    /// The first version of this helper honestly blocked inside
    /// `poll_read` (`std::io::Read::read` on a socket in blocking mode,
    /// `Poll::Ready` with no exceptions) — and hung EVERY exchange, not
    /// just edge cases: `hyper::proto::h1::dispatch::Dispatcher::
    /// poll_loop` (hyper 1.11.0) calls `let _ = self.poll_read(cx)?;`
    /// first on EVERY iteration, and only then `self.poll_write(cx)` — the
    /// read result is discarded via `let _ =`, which under non-blocking IO
    /// means "no read yet, that's fine, let's try writing this same
    /// iteration." But if `poll_read` itself blocks the thread until bytes
    /// show up, `poll_write` is never reached: the client waits for a
    /// response that will never come, while the server waits for a
    /// request that was never sent, because the client is stuck reading.
    /// Caught not by reading the code but by `works_on_a_bare_futures_
    /// executor_with_no_spawn` actually hanging (see the Task 12 report) —
    /// it never got as far as the request's first byte.
    ///
    /// So the socket has to be non-blocking, and `poll_read`/`poll_write`
    /// have to return `Pending` on `WouldBlock` instead of waiting. But
    /// there's also no reactor to wake us once the socket becomes ready —
    /// so `Pending` here comes paired with an immediate
    /// `cx.waker().wake_by_ref()`: this is a busy-spin
    /// (`futures_executor::block_on` just polls again right away, instead
    /// of a real wait for OS-signaled readiness), not a pretense that no
    /// reactor is needed. This is only fit for a test against a local
    /// socket that answers in a fraction of a millisecond — not a pattern
    /// to reuse anywhere outside this helper.
    pub fn blocking_io(s: std::net::TcpStream) -> BlockingIo {
        s.set_nonblocking(true)
            .expect("a freshly connected TcpStream should accept set_nonblocking");
        BlockingIo(s)
    }

    /// See [`blocking_io`].
    #[derive(Debug)]
    pub struct BlockingIo(std::net::TcpStream);

    /// `Pending` on `WouldBlock`, but with an immediate `wake_by_ref` — see
    /// [`blocking_io`]'s doc comment for why, without a reactor, this is
    /// the only way not to lose the wakeup.
    fn poll_would_block<T>(
        cx: &std::task::Context<'_>,
        r: std::io::Result<T>,
    ) -> std::task::Poll<std::io::Result<T>> {
        match r {
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                cx.waker().wake_by_ref();
                std::task::Poll::Pending
            }
            other => std::task::Poll::Ready(other),
        }
    }

    impl hyper::rt::Read for BlockingIo {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            mut buf: hyper::rt::ReadBufCursor<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            // No `unsafe`: read into a stack buffer, then copy via
            // `put_slice` — the same path as the safe example in
            // `hyper::rt::Read`'s doc comment.
            let mut scratch = [0u8; 8192];
            let want = buf.remaining().min(scratch.len());
            match poll_would_block(
                cx,
                std::io::Read::read(&mut self.get_mut().0, &mut scratch[..want]),
            ) {
                std::task::Poll::Ready(Ok(n)) => {
                    buf.put_slice(&scratch[..n]);
                    std::task::Poll::Ready(Ok(()))
                }
                std::task::Poll::Ready(Err(e)) => std::task::Poll::Ready(Err(e)),
                std::task::Poll::Pending => std::task::Poll::Pending,
            }
        }
    }

    impl hyper::rt::Write for BlockingIo {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            poll_would_block(cx, std::io::Write::write(&mut self.get_mut().0, buf))
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            // `TcpStream::flush` is a no-op (no userspace buffering),
            // never blocks and never returns `WouldBlock`.
            std::task::Poll::Ready(std::io::Write::flush(&mut self.get_mut().0))
        }
        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            // `NotConnected` is success, not failure. On macOS and the BSDs
            // `shutdown(2)` on a socket whose peer has already closed
            // returns `ENOTCONN` (errno 57); Windows answers `WSAENOTCONN`.
            // Linux does not, which is why this only ever appeared on real
            // CI: the first run on `macos-latest` failed both of the tests
            // that prove the seam works without spawn, with
            // `hyper::Error(Shutdown, Os { code: 57, kind: NotConnected })`,
            // while every local Linux run had been green.
            //
            // Swallowing it is correct rather than lenient: the call asked
            // for the socket to be shut down, and it is. Any other error
            // still propagates.
            let r = self.get_mut().0.shutdown(std::net::Shutdown::Both);
            std::task::Poll::Ready(match r {
                Err(e) if e.kind() == std::io::ErrorKind::NotConnected => Ok(()),
                other => other,
            })
        }
    }

    pub async fn exchange_for_test<I>(
        io: I,
        req: http::Request<crate::body::OutgoingBody>,
    ) -> Result<http::Response<crate::h1::NativeBody>, http_ng_core::Error>
    where
        I: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
    {
        crate::h1::exchange(io, req).await
    }

    pub async fn collect(b: crate::h1::NativeBody) -> Result<bytes::Bytes, http_ng_core::Error> {
        use http_body_util::BodyExt;
        Ok(b.collect().await?.to_bytes())
    }
}
