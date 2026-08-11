//! Native transport for http-ng: TCP + TLS + HTTP/1.1, and HTTP/2 behind
//! a feature.
//!
//! This crate wires together the runtime ([`http_ng_rt`]), DNS ([`http_ng_dns`])
//! and TLS ([`http_ng_tls`]) on top of `hyper`. Task 10 laid down the request
//! body adapter ([`body`], `pub(crate)`); Task 11 added the connector
//! ([`connect`], also `pub(crate)`); Task 12 added the HTTP/1 driver
//! ([`h1`], `pub(crate)`); Task 13 assembles all of this into [`Native`] —
//! the crate's only public type, which implements `http_ng_core::
//! unversioned::Transport`. v0.2 W2 added the connection pool
//! ([`pool`]); v0.2 W3 added the HTTP/2 driver ([`http2`], behind the
//! `http2` feature and **not** on hyper — see its module doc) and
//! [`established`], the one place that knows there is more than one
//! protocol.
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
mod discovery;
mod established;
mod h1;
#[cfg(feature = "http2")]
mod http2;
mod idle;
mod pool;
#[cfg(feature = "websocket")]
mod websocket;

pub use connect::Conn;
pub use discovery::SVCB_FAILURE_TTL;
pub use idle::{BetweenBytesElapsed, IdleTimeout};
pub use pool::{PoolConfig, Reaper};
#[cfg(feature = "websocket")]
pub use websocket::{NativeWebSocket, PongNotReceived, WebSocketKeepAlive};

use http_ng_core::unversioned::Transport;
use http_ng_core::{
    CancelSupport, Capabilities, Error, ErrorKind, Phase, RedirectSupport, RequestBody,
    ReuseSupport, TimeoutSupport, Timeouts,
};
use http_ng_dns::Resolve;
use http_ng_rt::{Spawn, TcpConnect, TcpOpts, Timer};
use http_ng_tls::TlsConnect;
use pool::{CheckIn, Pool, PoolKey, Protocol, Security};
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
/// connection.
///
/// HTTP/1.1 always; **HTTP/2 with the `http2` feature**, on `https://`
/// origins whose TLS backend both negotiated `h2` and can say so — see
/// `TlsConnect::reports_alpn` and [`crate::http2`]'s module doc. There is
/// no h2c: cleartext stays HTTP/1.1. What was negotiated is readable after
/// the fact from `Response::version()`; it is deliberately not readable
/// from [`Capabilities`], which report the floor (see [`Native::new`]).
/// No upgrade on either protocol. The request body is **not** buffered whole:
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
    /// is what keeps the pool and [`established::NativeBody`] free of a second type
    /// parameter for the runtime.
    epoch: R::Instant,
    pool: Pool<NativeIo<R, T>>,
    /// The origins whose HTTPS record has already cost a failed
    /// connection — see [`crate::discovery`], and [`SVCB_FAILURE_TTL`] for
    /// how long that is remembered.
    ///
    /// On the transport rather than inside `connect::connect` for the same
    /// reason the pool is: it is a memory across requests, and one built
    /// per call would be no memory at all. Shared by `Arc`, so a
    /// transport's clones (and the response bodies that outlive a call)
    /// all see the same one.
    svcb_failures: discovery::NegativeCache,
    /// The liveness bound every WebSocket opened from this transport gets
    /// — `None`, and therefore none, unless
    /// [`Native::websocket_keep_alive`] was called.
    ///
    /// On the transport rather than on the seam or in the request's
    /// extensions: `WebSocketConnect` is implemented by `http-ng-fetch`
    /// too, and a browser has no `send(ping)` at all, so a knob on the
    /// trait would be a capability one backend could not honour. The
    /// reasoning in full is `docs/w4-upgrade-seam.md` §7 and
    /// [`crate::websocket`]'s module doc.
    #[cfg(feature = "websocket")]
    ws_keep_alive: Option<websocket::WebSocketKeepAlive>,
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
        // **The floor, and it is the same value with the `http2` feature on
        // as off** (v0.2 W3). `full_duplex`, `request_trailers` and
        // `response_trailers` are all things HTTP/2 can do and HTTP/1.1
        // cannot, and they all stay at `Capabilities::none()`'s `false`
        // here, because what this reports is the value that holds on the
        // WORST protocol this transport might negotiate.
        //
        // Not blanket conservatism — chosen per field by what over-claiming
        // costs. Over-claiming `streaming_request_body` (above) would cost
        // a buffered copy: recoverable, visible. Over-claiming
        // `full_duplex` costs a **deadlock**: a caller structured for
        // bidirectional streaming writes its request body while reading the
        // response, and on HTTP/1.1 the response does not arrive until the
        // request completes. A capability whose over-claim hangs the
        // program cannot be optimistic.
        //
        // And it has to be the floor rather than the best case for a second
        // reason that has nothing to do with this crate: Cargo unifies
        // features across the whole graph, so a *library* built on `http-ng`
        // can never know whether some other crate turned h2 on. The floor is
        // the only answer such a library can act on.
        //
        // **This half expired in v0.4 W2 and is corrected rather than
        // deleted.** It used to read "on this implementation it is not
        // merely a declaration: `http2::exchange` writes the whole request
        // body before it awaits the response". That stopped being true
        // when the h2 path became duplex — the loop lost exactly one
        // branch, `Poll::Pending => return Poll::Pending`, which *was* the
        // implementation this sentence described.
        //
        // The `false` stands anyway, and for the reason it always had:
        // this is the FLOOR, and the floor is HTTP/1.1, which cannot do
        // duplex at all. A library cannot know whether some other crate in
        // the graph turned `http2` on, so the static answer must hold on
        // the worst protocol that might be negotiated.
        // `tests/http2.rs`'s
        // `capabilities_report_the_floor_with_the_feature_on` pins it.
        //
        // What a caller who wants to know the protocol gets instead:
        // `Response::version()`, after the fact, which is the only honest
        // time — `version_reported` below is `true` and the negotiated
        // version really does come off the wire.
        // `Transparent`, and it is a correction rather than a change: this
        // crate has never followed a redirect. A 3xx comes back from
        // `h1::exchange`/`http2::exchange` as an ordinary response and
        // `Client`'s redirect stage does the chain — grep `src/` for
        // `Location` or a 3xx status and there is nothing to find.
        //
        // It said `Configurable` from v0.1 until v0.4 W1: "we set the
        // policy", for a crate that reads no policy and sets nothing.
        // `http-ng-fetch` shipped the same wrong value and corrected it to
        // `Internal` a vertical earlier, in an audit that never came here.
        // The variant is deleted now, so this line no longer has a way to
        // be wrong in that direction.
        //
        // Not `None`, which is the stronger claim that redirects are
        // impossible and is also what `Capabilities::none()` returns for a
        // backend that said nothing at all — the same distinction
        // `http-ng-h3` writes down beside its own `Transparent`.
        caps.redirects = RedirectSupport::Transparent;
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
        // **What `tls_config` does not say, and where the answer is.**
        // Since v0.3 W2 this transport reads HTTPS records, so it can hold
        // an `EchConfigList` for an origin that published one — and it
        // offers it only to a backend whose `TlsConnect::applies_ech()`
        // says it will use one, which today is none of them (see
        // `crate::connect`'s module doc for why filling the field
        // regardless would make every such origin unreachable). The cost
        // of that is a privacy fact rather than an implementation detail:
        // the connection is still made, and the server name the origin
        // asked to have encrypted still goes out in the clear.
        //
        // It is not a `Capabilities` field because the answer is the TLS
        // backend's and the caller already holds it — `tls.applies_ech()`
        // is readable at construction, before any request — where a field
        // here would be this crate copying someone else's answer into a
        // second place it could drift from. `tests/svcb.rs`'s
        // `the_name_a_record_asked_to_protect_goes_out_in_the_clear` is
        // the same fact, exhibited on the wire.
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
            // Both enforced as of v0.2 W4's middle bullet, and declared
            // in the same commit that enforced them — the rule that item
            // was written under. Neither is the pool's idle timeout
            // wearing a different name: that one bounds a connection
            // between two exchanges and lives on `PoolConfig`, these two
            // bound one exchange and travel with the request.
            //
            // `first_byte` wraps `established::exchange` — from the
            // request being handed to a connection until the response head
            // is in hand — and expires as `Timeout(Phase::FirstByte)`.
            // `between_bytes` wraps the response body and expires as
            // `Timeout(Phase::BetweenBytes)`; it races a real sleep rather
            // than reading a clock on each frame, which is the only shape
            // that can cut a body that has gone completely silent (see
            // `idle.rs`'s module doc for the measurement, and
            // `http_ng::Deadline`'s for the same fact from the other end).
            //
            // Both are checked from outside the client, by servers that
            // send a head and then nothing and that stop mid-body:
            // `tests/timeouts.rs`.
            first_byte: true,
            between_bytes: true,
        };
        Self {
            epoch: rt.now(),
            rt,
            tls,
            dns,
            opts: TcpOpts::default(),
            caps,
            pool,
            svcb_failures: discovery::NegativeCache::default(),
            // Off, which is the whole of the default and is checked
            // rather than asserted: `tests/websocket.rs`'s
            // `keep_alive_is_off_by_default_and_pings_only_when_it_is_configured`
            // watches a default socket send nothing at all from the
            // server's side of the wire.
            #[cfg(feature = "websocket")]
            ws_keep_alive: None,
        }
    }

    /// Prove the peer of an open WebSocket is still there, with RFC 6455
    /// §5.5.2 ping/pong — **off unless this is called.**
    ///
    /// Without it an open WebSocket has no bound of any kind: only the
    /// handshake reads `Timeouts::connect`, so a peer that vanishes
    /// without a `FIN` leaves a `Stream` that never yields and never
    /// errors. With it, a socket silent for
    /// [`every`](WebSocketKeepAlive::every) sends a `Ping`, and a peer
    /// that does not answer within
    /// [`within`](WebSocketKeepAlive::within) ends the `Stream` with an
    /// [`ErrorKind::Body`] whose source is [`PongNotReceived`] — an
    /// error, distinguishable from the peer having said goodbye, which
    /// arrives as `Message::Close`.
    ///
    /// It is **off by default** because a default that pings is a default
    /// that sends traffic nobody asked for, and on a metered radio that is
    /// not free. Two more things a caller should know before turning it
    /// on, both of them properties of this transport rather than of the
    /// seam:
    ///
    /// - **A caller that stops polling gets no keep-alive.** Nothing is
    ///   spawned here — this crate has no `Spawn` bound, deliberately — so
    ///   the ping is written from `poll_next` or not at all. Unlike
    ///   `http-ng-h3`, where a spawned driver keeps a pooled connection
    ///   alive for requests nobody has made yet, a WebSocket always has a
    ///   caller, and one that is not polling is not waiting for anything.
    /// - **A busy connection never pings.** `every` measures silence on
    ///   the wire, and any inbound frame restarts it.
    ///
    /// It applies to every WebSocket opened from this transport
    /// afterwards; [`NativeWebSocket::keep_alive`] reads back what a given
    /// socket got. There is no counterpart on `http-ng-fetch`, so asking a
    /// browser for this does not compile — `docs/w4-upgrade-seam.md` §7.
    #[cfg(feature = "websocket")]
    pub fn websocket_keep_alive(mut self, keep_alive: WebSocketKeepAlive) -> Self {
        self.ws_keep_alive = Some(keep_alive);
        self
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

    /// Connection reuse **with a background task that actually closes idle
    /// connections** when their deadline passes, instead of only refusing
    /// to hand them out.
    ///
    /// Sets the pool exactly as [`Native::pool`] does and additionally
    /// spawns a [`Reaper`] on `R`. Read [`crate::pool`]'s module doc first:
    /// without one, [`PoolConfig::idle_timeout`] is a filter applied at
    /// checkout, so a client that goes quiet holds its sockets until its
    /// next request or until `Drop`. Measured with the server watching its
    /// own end of the socket, under a 300 ms idle timeout: closed **299.7
    /// ms** after the response on the shipped `Tokio` and **300.6 ms** on
    /// the shipped `Smol`, where the same client with `pool` in place of
    /// this call still held the connection 1200 ms later
    /// (`tests/reaper.rs`).
    ///
    /// # Why this is not what [`Native::new`] does
    ///
    /// Because `Native` is generic over `R`, and **not every `R` has a
    /// `Spawn` impl at all** — `connect.rs`'s own `FakeRt` has none, and
    /// W7's embassy backend expects none. A reaper started by `new` would
    /// assume a capability the type parameter does not promise: a default
    /// stronger than the truth, which is the one thing this crate refuses
    /// everywhere else.
    ///
    /// This is *not* the reason that used to be recorded here and in two
    /// documents — "a pool driven by a spawned task does not compile on
    /// this seam, because `Spawn<F>` requires `F: Send + 'static` and this
    /// vertical's IO is not `Send`". That was measured and withdrawn:
    /// `Spawn<F>` declares no bounds whatsoever, and the pool is an
    /// `Arc<..<Mutex<..>>>`, so a reaper over it is `Send` whenever the
    /// connection is. See `pool.rs`'s module doc for how the mistake was
    /// made, which is the reusable part of it.
    ///
    /// # The bound is on this constructor, and that is the point
    ///
    /// `R: Spawn<Reaper<R, NativeIo<R, T>>>` is a compile error at the
    /// call site for a runtime that cannot spawn — not a reaper that is
    /// silently never started. `Spawn<F>` makes the future a type
    /// parameter of the *trait*, so the bound has to name it, which is why
    /// [`Reaper`] is a hand-written struct rather than an `async` block.
    ///
    /// # Three things it cannot promise
    ///
    /// - **A spawner nobody drives.** `Spawn::spawn` returns `()`: an
    ///   executor that is never run accepts the task, drops it, and has no
    ///   way to say so. A reaper is at most as good as the executor under
    ///   it.
    /// - **Start it last.** [`Native::pool`] and [`Native::without_pool`]
    ///   install a *new* pool; a reaper started before one of those calls
    ///   is left holding a weak reference to the old pool, and ends
    ///   (quietly, and having reaped nothing, because that pool is empty
    ///   and dropped). This method sets the configuration itself precisely
    ///   so that `pool()` need not be called alongside it.
    /// - **Reuse has to be on.** There is nothing to reap in a transport
    ///   that never keeps a connection, so `PoolConfig` is taken here
    ///   rather than left to be `None` — `without_pool()` afterwards turns
    ///   both off together.
    ///
    /// `R: Clone` because the task needs its own clock; both shipped
    /// runtimes are ZSTs and `TokioHandle` is an `Arc`-shaped handle.
    /// Nothing here touches the runtime before the task is first polled —
    /// the sleep is built on the executor's thread — so a `Tokio` whose
    /// ambient runtime is elsewhere fails in `spawn`, where it would
    /// anyway, rather than half-way through this call. See
    /// `http_ng_rt_tokio::TokioHandle` for the way round that.
    pub fn with_reaper(mut self, config: PoolConfig) -> Self
    where
        R: Clone + Spawn<Reaper<R, NativeIo<R, T>>>,
    {
        self.pool = Pool::new(Some(config));
        self.caps.connection_reuse = reuse_of(&self.pool);
        let reaper = Reaper::new(
            self.rt.clone(),
            self.pool.downgrade(),
            self.epoch,
            config.idle_timeout,
        );
        self.rt.spawn(reaper);
        self
    }

    /// Socket parameters for EVERY TCP attempt this transport makes (see
    /// [`http_ng_rt::TcpOpts`]) — **refused here, once, if the runtime
    /// cannot apply them.**
    ///
    /// `Result`, not `Self`, and that is the whole point of the method.
    /// W7 gave [`http_ng_rt::TcpConnect`] an `APPLIES` constant and
    /// [`TcpOpts::reject_unsupported`], so a runtime that cannot apply an
    /// option the caller set fails the connect rather than dropping it —
    /// honest, but it fails once per `connect`, on a request that had
    /// nothing to do with the mistake, and only if a request is ever made.
    /// The set of options and the runtime's answer are both known at
    /// construction, so the answer is given at construction: the same move
    /// `ClientBuilder::build()` makes for an unsupported capability, for
    /// the same reason — a configuration that can never work should not
    /// need traffic to say so.
    ///
    /// **The error names the options**, not merely their number: the
    /// source is a [`http_ng_rt::UnsupportedTcpOpts`], carried inside an
    /// [`std::io::Error`] exactly as `reject_unsupported` builds it, and
    /// [`UnsupportedTcpOpts::names`](http_ng_rt::UnsupportedTcpOpts::names)
    /// lists every offending field rather than the first — a caller who
    /// fixed the one option a message mentioned would otherwise meet a
    /// second, identical-looking failure.
    ///
    /// This does not replace the per-`connect` refusal, and must not: the
    /// `APPLIES` contract belongs to the runtime, `connect` is reachable
    /// without ever going through this method (`connect::connect` takes a
    /// `&TcpOpts`), and a check here would be a second place deciding a
    /// question the trait already decides. What it does is move the moment
    /// of the answer for the one caller that always goes through it.
    ///
    /// [`TcpOpts::default`] is all-off, so a transport that never calls
    /// this method never has anything to refuse, whatever the runtime.
    pub fn tcp_opts(mut self, opts: TcpOpts) -> Result<Self, Error> {
        opts.reject_unsupported(<R as TcpConnect>::APPLIES)
            .map_err(|e| Error::new(ErrorKind::Unsupported, e))?;
        self.opts = opts;
        Ok(self)
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
    R: TcpConnect + Timer + Clone,
    R::Stream: 'static,
    T: TlsConnect,
    T::Stream<R::Stream>: 'static,
{
    /// Everything about a pool key except the protocol — and, as a side
    /// effect the rest of `execute` relies on, the point at which an
    /// unsupported scheme or a missing host becomes a typed error.
    ///
    /// The same two checks `connect::connect` makes, in the same order, so
    /// a request that would have failed there fails here instead, with the
    /// identical error. That is not duplication for its own sake: the key
    /// needs the authority before `origin_form` removes it, and a request
    /// served from the pool never reaches `connect::connect` at all.
    ///
    /// The protocol is the one component that cannot be known here: on a
    /// fresh connection it is whatever ALPN selects, several round trips
    /// from now. So this returns the parts, and [`KeyParts::key`] finishes
    /// the key once there is an answer.
    fn key_parts(&self, uri: &http::Uri) -> Result<KeyParts, Error> {
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
        Ok(KeyParts {
            security,
            host: host.into(),
            port,
        })
    }

    /// Whether this transport may propose `h2` for `parts`, and therefore
    /// whether it may speak it.
    ///
    /// Two conditions, and the second one is the whole reason
    /// `TlsConnect::reports_alpn` exists. h2 is only reachable through TLS
    /// here — there is no prior-knowledge or upgrade path to h2c — and it
    /// is only *safe* to propose to a backend that will tell us what was
    /// selected. A backend that sends the ALPN list and cannot read the
    /// answer back (`http-ng-tls-native-tls` is exactly that) would leave
    /// us speaking HTTP/1 into a connection the server switched to HTTP/2:
    /// not a lost optimisation, a protocol error on every request, and one
    /// arriving through a feature the user may not have enabled — Cargo
    /// unifies features across the whole graph. So the offer is withheld
    /// unless the answer can be read.
    #[cfg(feature = "http2")]
    fn may_speak_h2(&self, parts: &KeyParts) -> bool {
        matches!(parts.security, Security::Tls(_)) && self.tls.reports_alpn()
    }

    /// Without the feature there is no h2 code to reach at all — the
    /// module is not compiled — so this is a constant and the ALPN list
    /// stays what it was before v0.2 W3.
    #[cfg(not(feature = "http2"))]
    fn may_speak_h2(&self, _parts: &KeyParts) -> bool {
        false
    }

    /// Which pool buckets may hold a connection for this request, in
    /// preference order.
    ///
    /// `&'static [Protocol]`, not a `Vec`: the answer is one of two
    /// compile-time constants, and a request should not allocate to
    /// discover which.
    fn pooled_candidates(&self, parts: &KeyParts) -> &'static [Protocol] {
        #[cfg(feature = "http2")]
        if self.may_speak_h2(parts) {
            return &[Protocol::H2, Protocol::Http11];
        }
        let _ = parts;
        &[Protocol::Http11]
    }

    /// The instruction to hand this request's connection back when its
    /// response body ends cleanly — or `None` when reuse is off, which is
    /// how an exchange is told to drop the sender and let the connection
    /// close.
    fn checkin_for(&self, key: &PoolKey, now: Duration) -> Option<CheckIn<NativeIo<R, T>>> {
        let cfg = self.pool.config()?;
        Some(CheckIn::new(
            self.pool.clone(),
            key.clone(),
            // Saturating: an idle timeout of `Duration::MAX` is a legal
            // thing for a caller to ask for and means "never too old", and
            // a panic on overflow would be a strange way to answer it.
            now.saturating_add(cfg.idle_timeout),
        ))
    }

    /// Runs one exchange under `Timeouts::first_byte`, or unchanged when
    /// no bound was set.
    ///
    /// # What the bound covers
    ///
    /// From the request being handed to a connection to the response
    /// **head** being in hand — writing the request included. `Timeouts`'
    /// own doc calls this phase "response-wait", and the head is the
    /// earliest moment this transport can observe: hyper reports a
    /// response when it has parsed the status line and the headers, and on
    /// a real socket those arrive with the first bytes.
    ///
    /// # Per attempt, not per `execute`, and that is not a loophole
    ///
    /// `execute` may make more than one attempt with the same request
    /// object, and each gets the whole bound. That is honest rather than
    /// generous: an attempt is retried **only** when hyper hands the
    /// request back untouched (`Failed::NotSent` — not a byte of it
    /// reached the wire), so no response was ever waited for on it. A
    /// budget shared across attempts would be counting a wait that did not
    /// happen. Contrast `Timeouts::connect`, which deliberately is one
    /// budget for the whole Happy Eyeballs race, because there every
    /// attempt really is spending the caller's time.
    ///
    /// # `Failed::Sent`, deliberately
    ///
    /// A timeout here means the request went out and nothing came back, so
    /// the retry above must not fire: resending would not be a retry but a
    /// second request, with whatever the server has already done to the
    /// first one still standing. `Failed::Sent` is how that is said, and
    /// it is the same verdict the enum carries everywhere else.
    ///
    /// Expiry drops the exchange future, which drops the connection, which
    /// closes the socket — `Capabilities::cancel_on_drop` is `Supported`
    /// on this transport and this is one of the places that relies on it.
    async fn within_first_byte<F, V>(
        &self,
        d: Option<Duration>,
        fut: F,
    ) -> Result<V, established::Failed>
    where
        F: Future<Output = Result<V, established::Failed>>,
    {
        let Some(d) = d else {
            return fut.await;
        };
        let mut fut = std::pin::pin!(fut);
        // Built only on this branch: `Tokio::sleep` panics outside a
        // runtime, and a request that asked for nothing must not need one.
        let mut sleep_fut = std::pin::pin!(self.rt.sleep(d));
        std::future::poll_fn(|cx| {
            // The exchange first, so a head that arrived in the same wake
            // as the deadline expiring is a response rather than a
            // timeout — the same ordering rule as `with_connect_timeout`
            // below and `http_ng::within`.
            if let Poll::Ready(r) = fut.as_mut().poll(cx) {
                return Poll::Ready(r);
            }
            if sleep_fut.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Err(established::Failed::Sent(Error::new(
                    ErrorKind::Timeout(Phase::FirstByte),
                    FirstByteTimedOut(d),
                ))));
            }
            Poll::Pending
        })
        .await
    }

    /// Puts the `between_bytes` bound round a response body.
    ///
    /// One place, called from both attempt paths, because a body that
    /// escaped it would be a silent hole in a declared capability — and
    /// `Transport::Body` names the wrapper, so the compiler is what stops
    /// a third path forgetting.
    fn bound_body(
        &self,
        resp: http::Response<established::NativeBody<NativeIo<R, T>>>,
        every: Option<Duration>,
    ) -> http::Response<IdleTimeout<established::NativeBody<NativeIo<R, T>>, R>> {
        resp.map(|b| IdleTimeout::new(b, self.rt.clone(), every))
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
    ) -> Option<established::Established<NativeIo<R, T>>> {
        loop {
            let mut est = self.pool.take(key, now)?;
            if established::is_reusable(&mut est).await {
                return Some(est);
            }
        }
    }
}

/// A pool key with its protocol not yet decided — see
/// [`Native::key_parts`].
struct KeyParts {
    security: Security,
    host: Box<str>,
    port: u16,
}

impl KeyParts {
    fn key(&self, protocol: Protocol) -> PoolKey {
        PoolKey::new(self.security, &self.host, self.port, protocol)
    }
}

/// Which protocol a connection speaks, given what the TLS backend reported
/// as negotiated.
///
/// `None` means **neither of the two this transport speaks**, and the
/// caller's answer to that has not changed since v0.2 W2: fall back to
/// HTTP/1.1 on this one connection and keep it out of the pool, so that a
/// socket running some protocol nobody here understands can never be
/// handed to a later request.
///
/// A missing ALPN is `Http11`, not `None`, and that is not a guess: it is
/// either a plaintext connection (no ALPN exists) or a backend that does
/// not report one — and in the latter case [`Native::may_speak_h2`] made
/// sure `h2` was never offered, so HTTP/1.1 is the only thing the server
/// could have selected.
///
/// **`offered_h2` is the reason this takes a second argument, and it is
/// not belt-and-braces.** A peer cannot select a protocol that was never
/// proposed, so on a correct TLS stack the check is redundant — but the
/// value comes from a `TlsConnect` implementation, which is a trait
/// anyone may implement, and "what was negotiated" arriving as `h2` when
/// `h2` was not on the wire would otherwise make this transport speak a
/// protocol the server is not listening for. Answering only with
/// protocols we proposed costs one `bool` and removes the whole class.
fn negotiated_protocol(alpn: Option<&[u8]>, offered_h2: bool) -> Option<Protocol> {
    let Some(alpn) = alpn else {
        return Some(Protocol::Http11);
    };
    if alpn == b"http/1.1".as_slice() {
        return Some(Protocol::Http11);
    }
    #[cfg(feature = "http2")]
    if offered_h2 && alpn == b"h2".as_slice() {
        return Some(Protocol::H2);
    }
    let _ = offered_h2;
    None
}

/// The handshake for the protocol ALPN selected.
#[cfg(feature = "http2")]
async fn handshake_for<I>(
    conn: I,
    protocol: Option<Protocol>,
) -> Result<established::Established<I>, Error>
where
    I: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
{
    if is_h2(protocol) {
        Ok(established::Established::H2(Box::new(
            http2::handshake(conn).await?,
        )))
    } else {
        Ok(established::Established::H1(h1::handshake(conn).await?))
    }
}

/// Without the feature, [`is_h2`] is a constant `false` and the HTTP/2
/// branch is not merely unreached — the module it would call into is not
/// compiled at all. Two definitions rather than one with a dead branch,
/// so that a build without the feature contains no h2 code path to
/// audit.
#[cfg(not(feature = "http2"))]
async fn handshake_for<I>(
    conn: I,
    protocol: Option<Protocol>,
) -> Result<established::Established<I>, Error>
where
    I: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
{
    debug_assert!(!is_h2(protocol));
    Ok(established::Established::H1(h1::handshake(conn).await?))
}

/// Reads as `protocol == Some(Protocol::H2)`, written as a function
/// because `Protocol::H2` does not exist without the feature and a
/// `matches!` at each of the three call sites would need its own `#[cfg]`.
fn is_h2(protocol: Option<Protocol>) -> bool {
    #[cfg(feature = "http2")]
    {
        matches!(protocol, Some(Protocol::H2))
    }
    #[cfg(not(feature = "http2"))]
    {
        let _ = protocol;
        false
    }
}

/// `R: Clone` is new as of the `first_byte`/`between_bytes` work, and it
/// is the one bound this impl gained for it. `between_bytes` is enforced
/// by a sleep held **inside the response body**, which outlives `execute`
/// and therefore cannot borrow this transport's clock — it needs one of
/// its own. Every runtime in this workspace was already `Clone` (both
/// shipped ones are ZSTs, `TokioHandle` is a handle, `Embassy` is two
/// pointers and says in its own doc that `Native` wants to clone it), so
/// this pins down a fact rather than adding a restriction.
impl<R, T, D> Transport for Native<R, T, D>
where
    R: TcpConnect + Timer + Clone,
    R::Stream: 'static,
    T: TlsConnect,
    T::Stream<R::Stream>: 'static,
    D: Resolve,
{
    /// The pooled body, with the `between_bytes` bound wrapped round it.
    ///
    /// The order matters and is the mirror of `http_ng::ClientBody`'s: the
    /// idle bound is the **innermost** wrapper, next to the socket, so it
    /// measures the gap between reads on the wire. Outside it the client
    /// may add its own `Deadline` and decompression, neither of which can
    /// hide a silent peer from this one.
    type Body = IdleTimeout<established::NativeBody<NativeIo<R, T>>, R>;
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
        // now where the scheme and host are validated: `key_parts` runs the
        // same `connect::wants_tls`/`connect::host` checks
        // `connect::connect` runs, and fails with the same typed errors, so
        // everything downstream may still assume they passed.
        let parts_of_key = self.key_parts(&parts.uri)?;
        let uri = parts.uri.clone();

        let outgoing = body::OutgoingBody::from_request_body(body);
        // Left exactly as it arrived — absolute URI, and no `Host:` of
        // ours. That is the one shape both protocols can be derived from,
        // and deriving is `established::exchange`'s job, from the
        // connection it is about to speak on rather than from anything
        // decided here. `uri` above is the copy that puts it back when an
        // attempt hands the request straight back.
        let mut req = http::Request::from_parts(parts, outgoing);

        // Elapsed on the runtime's own clock, from this transport's epoch —
        // see the `epoch` field. Read once and used for both ends of the
        // pool's bookkeeping (which entries are too old to hand out, and
        // when the connection this request uses would become too old), so
        // the two cannot disagree.
        let now = self.rt.elapsed_since(self.epoch);

        // 1. Connections somebody else already opened, if any are still
        //    alive. `checkout` polls each candidate before offering it, and
        //    that poll is the only thing standing between us and handing
        //    out a socket the server closed while it was idle — see
        //    `pool.rs`'s module doc.
        //
        //    Two buckets rather than one, and at most one of them is ever
        //    populated: an origin either negotiated h2 with this
        //    transport's TLS configuration or it did not, and that does
        //    not oscillate. Looking in both is two hash lookups and no
        //    assumption; assuming would be free and wrong on the day a
        //    server changes its mind.
        for &protocol in self.pooled_candidates(&parts_of_key) {
            let key = parts_of_key.key(protocol);
            let Some(est) = self.checkout(&key, now).await else {
                continue;
            };
            let checkin = self.checkin_for(&key, now);
            let attempt = established::exchange(est, req, checkin, &uri);
            match self.within_first_byte(timeouts.first_byte, attempt).await {
                Ok(resp) => return Ok(self.bound_body(resp, timeouts.between_bytes)),
                // The one retry, and the reason it exists: a pool turns
                // "the server closed this connection while it was idle"
                // from something that could not happen into something
                // that happens to a request that did nothing wrong. The
                // condition is hyper's, not a guess of ours — it hands
                // the request back only when not a byte of it reached
                // the wire (`established::Failed`), so what is resent
                // below is the original request object, its body
                // untouched at its first byte. No clone, no rewind, and
                // nothing to decide about idempotency: this is not a
                // second request, it is the first one, which never left.
                Err(established::Failed::NotSent { request, .. }) => req = *request,
                Err(other) => return Err(other.into_error()),
            }
        }

        // 2. A fresh connection. Reached either because the pool had
        //    nothing live or because a pooled attempt handed the request
        //    straight back; a fresh connection is never retried, so the
        //    number of attempts is bounded by the number of protocols
        //    plus one and nothing here can loop.
        //
        // See the module doc comment: `connect::connect` isn't reuse for
        // code-size's sake, it's the only path on which a resolver
        // `ErrorKind` that differs from the synthetic one (in particular,
        // `Cancelled`) structurally cannot be discarded. It also resolves
        // the scheme (`http`/`https`, anything else is a typed
        // `ErrorKind::Unsupported`) and runs the TLS handshake, proposing
        // the protocols below.
        let offered_h2 = self.may_speak_h2(&parts_of_key);
        let alpn: &[&[u8]] = if offered_h2 {
            // Order is the preference: RFC 7301 leaves the choice to the
            // server, but every implementation reads the client's list as
            // ranked, and h2 is what we want when it is on offer.
            &[b"h2", b"http/1.1"]
        } else {
            &[b"http/1.1"]
        };
        // `now` is the same reading of the runtime's clock the pool's
        // bookkeeping above uses, passed on rather than taken again: the
        // negative cache's window and a connection's idle deadline are
        // measured from one epoch on one clock, and two reads a
        // microsecond apart would be two facts where there is one.
        let connect_fut = connect::connect(
            &self.rt,
            &self.dns,
            &self.tls,
            &uri,
            &self.opts,
            alpn,
            &self.svcb_failures,
            now,
        );
        let (conn, tls_info) = match timeouts.connect {
            Some(d) => with_connect_timeout(&self.rt, d, connect_fut).await?,
            None => connect_fut.await?,
        };

        // The guard that keeps `PoolKey`'s protocol component honest: a
        // connection only enters the pool if what was actually negotiated
        // is a protocol this transport speaks, and then only under that
        // protocol's own key. Before v0.2 W3 this could not fire, because
        // `http/1.1` was the only thing ever proposed; now that `h2` is
        // proposed too it is what stops an h2 socket from being handed to
        // a request that would speak HTTP/1 on it.
        let protocol = negotiated_protocol(
            tls_info.as_ref().and_then(|i| i.alpn.as_deref()),
            offered_h2,
        );
        let checkin = match protocol {
            Some(p) => self.checkin_for(&parts_of_key.key(p), now),
            None => None,
        };

        let est = handshake_for(conn, protocol).await?;
        let attempt = established::exchange(est, req, checkin, &uri);
        self.within_first_byte(timeouts.first_byte, attempt)
            .await
            .map(|resp| self.bound_body(resp, timeouts.between_bytes))
            .map_err(established::Failed::into_error)
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

/// The failure [`Native::within_first_byte`] ends in when the timer wins
/// the race against the exchange.
///
/// A named type rather than a string, for the same reason
/// [`ConnectTimedOut`] is one: a caller must be able to tell the phases
/// apart with `Error::source().downcast_ref()`, and to read the bound
/// that was actually in force.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("no response head within the first_byte timeout of {0:?}")]
pub struct FirstByteTimedOut(pub Duration);

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
    pub use crate::established::NativeBody;

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
    ) -> Result<http::Response<crate::established::NativeBody<I>>, http_ng_core::Error>
    where
        I: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
    {
        let est = crate::h1::handshake(io).await?;
        crate::h1::exchange(est, req, None)
            .await
            .map(|r| r.map(crate::established::NativeBody::h1))
            .map_err(crate::established::Failed::into_error)
    }

    pub async fn collect<I>(
        b: crate::established::NativeBody<I>,
    ) -> Result<bytes::Bytes, http_ng_core::Error>
    where
        I: hyper::rt::Read + hyper::rt::Write + Unpin,
    {
        use http_body_util::BodyExt;
        Ok(b.collect().await?.to_bytes())
    }
}
