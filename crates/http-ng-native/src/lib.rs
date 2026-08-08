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
mod pool;

pub use connect::Conn;
pub use pool::PoolConfig;

use http_ng_core::unversioned::Transport;
use http_ng_core::{
    CancelSupport, Capabilities, Error, ErrorKind, Phase, RedirectSupport, RequestBody,
    ReuseSupport, TimeoutSupport, Timeouts, UpgradeSupport,
};
use http_ng_dns::Resolve;
use http_ng_rt::{TcpConnect, TcpOpts, Timer};
use http_ng_tls::TlsConnect;
use pool::{Pool, PoolKey, Protocol, Security};
use std::future::Future;
use std::task::Poll;
use std::time::Duration;

/// The IO a [`Native`] speaks HTTP/1 over: a plain socket from the runtime,
/// or that same socket wrapped by the TLS backend.
///
/// A public alias rather than an anonymous type in a signature, because it
/// is what `Native`'s `Transport::Body` is generic over, and a caller who
/// has to name that body should not have to spell this out themselves.
pub type NativeIo<R, T> =
    Conn<<R as TcpConnect>::Stream, <T as TlsConnect>::Stream<<R as TcpConnect>::Stream>>;

/// The http-ng transport over real TCP/TLS/HTTP1: wires together the
/// runtime `R` ([`http_ng_rt::TcpConnect`] + [`http_ng_rt::Timer`]), TLS `T`
/// ([`http_ng_tls::TlsConnect`]) and resolver `D` ([`http_ng_dns::Resolve`]).
///
/// Connections are reused (v0.2 W2): see [`PoolConfig`], [`Native::pool`]
/// and [`Native::without_pool`], and read [`crate::pool`]'s module doc for
/// what "reused" costs when there is no `Spawn` to drive an idle
/// connection. HTTP/1.1 only, no upgrade — see [`Native::new`] and
/// [`Capabilities`], which state these limits honestly rather than staying
/// silent about them. The request body is **not** buffered whole:
/// `RequestBody::Streaming` goes to the wire as a stream (see
/// [`Native::new`]'s doc comment on `streaming_request_body` — an earlier
/// version of this paragraph claimed the opposite and was wrong).
#[derive(Debug)]
pub struct Native<R, T, D>
where
    R: TcpConnect + Timer,
    T: TlsConnect,
{
    rt: R,
    tls: T,
    dns: D,
    opts: TcpOpts,
    caps: Capabilities,
    /// The instant this transport was built, and the origin every pool
    /// deadline is measured from.
    ///
    /// A `Timer` instant rather than `std::time::Instant`: `Timer` is the
    /// one seam through which time reaches this crate, and a wall clock
    /// read behind its back would disagree with a caller testing under
    /// `tokio::time::pause()`. Storing an origin and comparing `Duration`s
    /// against it — rather than storing an `R::Instant` per pooled entry —
    /// is what keeps the pool and [`h1::NativeBody`] free of a second type
    /// parameter for the runtime.
    epoch: R::Instant,
    pool: Pool<NativeIo<R, T>>,
}

/// `T: TlsConnect` and `R: TcpConnect + Timer` are on the struct as of
/// v0.2 W2, where before only the constructor carried `T: TlsConnect`.
/// The rule has not changed — the bound is still paid where the answer is
/// needed — what changed is where the answer is needed: `Native` now
/// *stores connections*, and the type of a connection is
/// `NativeIo<R, T>`, which cannot be named without them. Before the pool,
/// the only question needing `T: TlsConnect` was `new`'s "what should I
/// advertise", and the bound sat on `new` alone for exactly that reason.
impl<R: TcpConnect + Timer, T: TlsConnect, D> Native<R, T, D> {
    pub fn new(rt: R, tls: T, dns: D) -> Self {
        let pool = Pool::new(Some(PoolConfig::default()));
        let mut caps = Capabilities::none();
        // Honest: no upgrade — the
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
        // Asked of the pool, never written down twice. `reuse_of` reads the
        // same `Option<PoolConfig>` the pool actually behaves on, so the
        // capability cannot drift from the behaviour — which is the failure
        // this project has caught four times, most recently as a field
        // hardcoded to its strongest value instead of being asked for. Both
        // values are reachable (`Native::without_pool`) and both are
        // checked from outside the client, by a server counting the
        // connections it accepted: `tests/pool.rs`.
        caps.connection_reuse = reuse_of(&pool);
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
            // There is no response timer here — claiming these phases
            // would be a capability that lies about its own state. (The
            // pool arrived in v0.2 W2; these two did not arrive with it,
            // and `first_byte` in particular is not the pool's idle
            // timeout wearing a different name.)
            first_byte: false,
            between_bytes: false,
        };
        caps.upgrade = UpgradeSupport::None;
        Self {
            epoch: rt.now(),
            rt,
            tls,
            dns,
            opts: TcpOpts::default(),
            caps,
            pool,
        }
    }

    /// How this transport reuses connections — see [`PoolConfig`], and
    /// [`crate::pool`]'s module doc for what reuse can and cannot promise
    /// without a `Spawn` to drive an idle connection.
    ///
    /// Reuse is **on by default**, with [`PoolConfig::default`]; this
    /// method is for changing the numbers, and [`Native::without_pool`] is
    /// for turning it off.
    pub fn pool(mut self, config: PoolConfig) -> Self {
        self.pool = Pool::new(Some(config));
        self.caps.connection_reuse = reuse_of(&self.pool);
        self
    }

    /// One connection per request, closed when the response body ends —
    /// what this transport did before v0.2 W2.
    ///
    /// `Capabilities::connection_reuse` becomes `ReuseSupport::None` to
    /// match, because it is derived from the same value rather than set
    /// alongside it.
    pub fn without_pool(mut self) -> Self {
        self.pool = Pool::new(None);
        self.caps.connection_reuse = reuse_of(&self.pool);
        self
    }

    /// Socket parameters for EVERY TCP attempt this transport makes (see
    /// [`http_ng_rt::TcpOpts`]).
    pub fn tcp_opts(mut self, opts: TcpOpts) -> Self {
        self.opts = opts;
        self
    }
}

/// What a transport with this pool should advertise in
/// [`Capabilities::connection_reuse`].
///
/// A function rather than a literal at each of the three sites that set the
/// field, so that "the pool is configured" and "the capability says
/// connections are reused" are the same fact read twice, not two facts kept
/// in step by hand. The class of defect this is guarding against —
/// a capability describing an intention rather than the code — has been
/// caught four times in this workspace.
fn reuse_of<I>(pool: &Pool<I>) -> ReuseSupport
where
    I: hyper::rt::Read + hyper::rt::Write + Unpin,
{
    match pool.config() {
        Some(_) => ReuseSupport::Supported,
        None => ReuseSupport::None,
    }
}

/// The pool-facing half of the transport. Its bounds are the ones
/// `Transport` needs anyway (`'static` on the two stream types is what
/// hyper's handshake requires), kept in a separate block so `new` and the
/// builder methods above don't inherit them.
impl<R, T, D> Native<R, T, D>
where
    R: TcpConnect + Timer,
    R::Stream: 'static,
    T: TlsConnect,
    T::Stream<R::Stream>: 'static,
{
    /// Which pool bucket this request belongs to — and, as a side effect
    /// the rest of `execute` relies on, the point at which an unsupported
    /// scheme or a missing host becomes a typed error.
    ///
    /// The same two checks `connect::connect` makes, in the same order, so
    /// a request that would have failed there fails here instead, with the
    /// identical error. That is not duplication for its own sake: the key
    /// needs the authority before `origin_form` removes it, and a request
    /// served from the pool never reaches `connect::connect` at all.
    fn pool_key(&self, uri: &http::Uri) -> Result<PoolKey, Error> {
        let host = connect::host(uri)?;
        let use_tls = connect::wants_tls(uri)?;
        let port = connect::port(uri, use_tls);
        let security = if use_tls {
            // The identity of the trust configuration, asked of the TLS
            // backend rather than inferred from its type — see
            // `http_ng_tls::TlsConfigId`.
            Security::Tls(self.tls.config_id())
        } else {
            Security::Plaintext
        };
        Ok(PoolKey::new(security, host, port, Protocol::Http11))
    }

    /// The instruction to hand this request's connection back when its
    /// response body ends cleanly — or `None` when reuse is off, which is
    /// how `h1::exchange` is told to drop the sender and let hyper close.
    fn checkin_for(&self, key: &PoolKey, now: Duration) -> Option<h1::CheckIn<NativeIo<R, T>>> {
        let cfg = self.pool.config()?;
        Some(h1::CheckIn::new(
            self.pool.clone(),
            key.clone(),
            // Saturating: an idle timeout of `Duration::MAX` is a legal
            // thing for a caller to ask for and means "never too old", and
            // a panic on overflow would be a strange way to answer it.
            now.saturating_add(cfg.idle_timeout),
        ))
    }

    /// A pooled connection that is still worth a request, or `None`.
    ///
    /// Dead candidates are dropped as they are found — dropping closes the
    /// socket — and the loop moves on to the next one, so a burst that left
    /// several dead connections behind costs a walk through them rather
    /// than a failed request. It terminates because every iteration removes
    /// one entry from the pool and `take` returns `None` on an empty
    /// bucket.
    async fn checkout(
        &self,
        key: &PoolKey,
        now: Duration,
    ) -> Option<h1::Established<NativeIo<R, T>>> {
        loop {
            let mut est = self.pool.take(key, now)?;
            if h1::is_reusable(&mut est).await {
                return Some(est);
            }
        }
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
    type Body = h1::NativeBody<NativeIo<R, T>>;
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

        // Which connections may serve this request. Computed BEFORE
        // `origin_form` below rewrites the URI into origin-form and the
        // authority stops being there to read — and, in doing so, this is
        // now where the scheme and host are validated: `pool_key` runs the
        // same `connect::wants_tls`/`connect::host` checks
        // `connect::connect` runs, and fails with the same typed errors, so
        // everything downstream may still assume they passed.
        let key = self.pool_key(&parts.uri)?;
        let uri = parts.uri.clone();

        let outgoing = body::OutgoingBody::from_request_body(body);
        let mut req = http::Request::from_parts(parts, outgoing);
        // hyper h1 requires origin-form and a `Host:` header — `pool_key`
        // above has already checked both host and scheme, so we don't
        // recheck them here.
        origin_form(&mut req);

        // Elapsed on the runtime's own clock, from this transport's epoch —
        // see the `epoch` field. Read once and used for both ends of the
        // pool's bookkeeping (which entries are too old to hand out, and
        // when the connection this request uses would become too old), so
        // the two cannot disagree.
        let now = self.rt.elapsed_since(self.epoch);

        // 1. A connection somebody else already opened, if one is still
        //    alive. `checkout` polls each candidate before offering it, and
        //    that poll is the only thing standing between us and handing
        //    out a socket the server closed while it was idle — see
        //    `pool.rs`'s module doc.
        let req = match self.checkout(&key, now).await {
            None => req,
            Some(est) => {
                let checkin = self.checkin_for(&key, now);
                match h1::exchange(est, req, checkin).await {
                    Ok(resp) => return Ok(resp),
                    // The one retry, and the reason it exists: a pool turns
                    // "the server closed this connection while it was idle"
                    // from something that could not happen into something
                    // that happens to a request that did nothing wrong. The
                    // condition is hyper's, not a guess of ours — it hands
                    // the request back only when not a byte of it reached
                    // the wire (`h1::Failed`), so what is resent below is
                    // the original request object, its body untouched at
                    // its first byte. No clone, no rewind, and nothing to
                    // decide about idempotency: this is not a second
                    // request, it is the first one, which never left.
                    Err(h1::Failed::NotSent { request, .. }) => *request,
                    Err(other) => return Err(other.into_error()),
                }
            }
        };

        // 2. A fresh connection. Reached either because the pool had
        //    nothing live or because the pooled attempt handed the request
        //    straight back; a fresh connection is never retried, so there
        //    are at most two attempts and the second one cannot loop.
        //
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
            &uri,
            &self.opts,
            &[b"http/1.1"],
        );
        let (conn, tls_info) = match timeouts.connect {
            Some(d) => with_connect_timeout(&self.rt, d, connect_fut).await?,
            None => connect_fut.await?,
        };

        // The guard that keeps `PoolKey`'s protocol component honest: a
        // connection is only allowed into the pool under `Protocol::Http11`
        // if the protocol actually negotiated is one this transport speaks.
        // `None` is not a failure of the check — plaintext has no ALPN, and
        // `http-ng-tls-native-tls` cannot report one (its own module doc
        // says so) — and it cannot be a lie today either, because
        // `connect::connect` above offered exactly `http/1.1` and a server
        // that answered with anything else would have failed the handshake.
        // W3, which will offer `h2` as well, is where this stops being
        // vacuous and starts being the thing that prevents an h2 socket
        // from being handed to an h1 request.
        let speaks_h1 = tls_info
            .as_ref()
            .and_then(|i| i.alpn.as_deref())
            .is_none_or(|alpn| alpn == b"http/1.1");
        let checkin = if speaks_h1 {
            self.checkin_for(&key, now)
        } else {
            None
        };

        let est = h1::handshake(conn).await?;
        h1::exchange(est, req, checkin)
            .await
            .map_err(h1::Failed::into_error)
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

    /// One exchange over `io`, with no pool: the handshake and
    /// `h1::exchange` in one call, exactly as `Native::execute` does them
    /// for a fresh connection that is not to be reused.
    ///
    /// `None` for the check-in is not a simplification for tests' sake —
    /// it is the same value `Native::execute` passes when reuse is off,
    /// so what `tests/h1.rs` exercises is a real path rather than one that
    /// exists only for it.
    pub async fn exchange_for_test<I>(
        io: I,
        req: http::Request<crate::body::OutgoingBody>,
    ) -> Result<http::Response<crate::h1::NativeBody<I>>, http_ng_core::Error>
    where
        I: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
    {
        let est = crate::h1::handshake(io).await?;
        crate::h1::exchange(est, req, None)
            .await
            .map_err(crate::h1::Failed::into_error)
    }

    pub async fn collect<I>(
        b: crate::h1::NativeBody<I>,
    ) -> Result<bytes::Bytes, http_ng_core::Error>
    where
        I: hyper::rt::Read + hyper::rt::Write + Unpin,
    {
        use http_body_util::BodyExt;
        Ok(b.collect().await?.to_bytes())
    }
}
