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
mod staged;
mod upgrade;

pub use body::UndeclaredRequestTrailers;
pub use connect::Conn;
pub use discovery::{Discovered, Prepared, SVCB_FAILURE_TTL};
// `Prefetch` is declared in this file, beside the exchange it refines.
pub use idle::{BetweenBytesElapsed, IdleTimeout};
pub use pool::{PoolConfig, Reaper};
pub use staged::{Refused, Staged, StagedConnect};
pub use upgrade::{EndedBeforeTheResponse, NotSwitchingProtocols, Upgrading};

use http_ng_core::unversioned::{
    CloseReason, Closed, ConnectTiming, Connected, ConnectionId, Event, Head, Hooks, NoHooks,
    Reused, Transport,
};
use http_ng_core::{
    CancelSupport, Capabilities, Error, ErrorKind, Phase, RedirectSupport, RequestBody,
    ReuseSupport, TimeoutSupport, Timeouts, check_version,
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

/// What [`Native`]'s `Transport::Body` is, named once.
///
/// A public alias for [`NativeIo`]'s reason — a caller who has to name
/// the body should not have to spell out three nested generics — and
/// because with the hook parameter added there are now two places that
/// have to spell it identically, which is one more than is safe.
///
/// The order is load bearing: the `between_bytes` bound is the
/// **innermost** wrapper, next to the socket, so it measures the gap
/// between reads on the wire rather than the gap between whatever a
/// wrapper above chose to pass on.
pub type NativeBody<R, T, H> = IdleTimeout<established::NativeBody<NativeIo<R, T>, H>, R>;

/// A stopwatch that does not exist when nobody is watching.
///
/// Every clock read whose only purpose is an event goes through this, and
/// `H::WATCHING` is a `const`, so on `NoHooks` the `then` is a
/// compile-time `false` and there is nothing left for the optimiser to
/// remove — not a branch, not a call. `crates/http-ng-native/tests/
/// hooks_cost.rs` counts the reads from outside, on a runtime whose clock
/// reports how often it was asked.
pub(crate) fn mark<H: Hooks, R: Timer>(rt: &R) -> Option<R::Instant> {
    H::WATCHING.then(|| rt.now())
}

/// The other half of [`mark`]: the interval since one, or `ZERO` when
/// there was no mark to measure from.
///
/// The `ZERO` is never reported — a build that produced `None` above has
/// no hook to hand it to — which is why this is not an `Option<Duration>`
/// that every call site would then have to unwrap into a lie.
///
/// It takes no `H`: the gate is [`mark`]'s, once, and a second `H` here
/// would be a second place that has to agree with it.
pub(crate) fn since<R: Timer>(rt: &R, at: Option<R::Instant>) -> Duration {
    match at {
        Some(t) => rt.elapsed_since(t),
        None => Duration::ZERO,
    }
}

/// The id for a connection about to be made — or [`ConnectionId::UNWATCHED`]
/// when nobody will read it, so that a no-hook build does not touch the
/// process-wide counter once per connection.
pub(crate) fn connection_id<H: Hooks>() -> ConnectionId {
    if H::WATCHING {
        ConnectionId::next()
    } else {
        ConnectionId::UNWATCHED
    }
}

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
///
/// # `H`, the observability hook (v0.4 W2)
///
/// `NoHooks` by default, which is a zero-sized type whose
/// `Hooks::WATCHING` is `false` — so `Native<R, T, D>` still names the
/// transport it always named, and a build that asks for nothing reads no
/// clock and takes no connection id. [`Native::hooks`] is how a caller
/// asks; what comes back is a different type, because the hook is a type
/// parameter rather than a `Box<dyn Hooks>`, which is the whole of the
/// zero-cost claim.
///
/// `H` is deliberately last, after the three seams: it is the only one of
/// the four that is not a seam a backend author has to fill in.
#[derive(Debug)]
pub struct Native<R, T, D, H = NoHooks>
where
    R: TcpConnect + Timer,
    T: TlsConnect,
{
    rt: R,
    tls: T,
    dns: D,
    /// Where the events go. `NoHooks` is a ZST, so this field costs a
    /// build that wants nothing exactly nothing.
    hooks: H,
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
}

/// `T: TlsConnect` and `R: TcpConnect + Timer` are on the struct as of
/// v0.2 W2, where before only the constructor carried `T: TlsConnect`.
/// The rule has not changed — the bound is still paid where the answer is
/// needed — what changed is where the answer is needed: `Native` now
/// *stores connections*, and the type of a connection is
/// `NativeIo<R, T>`, which cannot be named without them. Before the pool,
/// the only question needing `T: TlsConnect` was `new`'s "what should I
/// advertise", and the bound sat on `new` alone for exactly that reason.
impl<R: TcpConnect + Timer, T: TlsConnect, D> Native<R, T, D, NoHooks> {
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
        // as off** (v0.2 W3). `full_duplex` and `response_trailers` are
        // things HTTP/2 can do and HTTP/1.1 cannot, and they stay at
        // `Capabilities::none()`'s `false` here, because what this
        // reports is the value that holds on the WORST protocol this
        // transport might negotiate.
        //
        // **`request_trailers` used to be the third name in that sentence
        // and it was measured wrong** (v0.4, `docs/v04-design.md` Appendix
        // C). HTTP/1.1 sends request trailers perfectly well —
        // `tests/request_trailers.rs` reads `0\r\ngrpc-status: 0\r\n\r\n`
        // off a raw socket from a plaintext `http://` exchange — so the
        // floor was never the reason for that `false`; nobody had looked.
        // It is `true` below, and the line that follows this one is the
        // whole of the change the declaration owed: what HTTP/1.1 wants
        // in addition is the `Trailer:` header RFC 9110 §6.6.2 asks of
        // any sender, and a request that omits it is malformed rather
        // than unsupported. It now gets
        // `body::UndeclaredRequestTrailers` instead of a `200` with its
        // data gone.
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
        //
        // **`true`, on both protocols, and it does not breach the floor
        // rule above — it satisfies it** (v0.4, Appendix C). A capability
        // says what the transport does with a well-formed request, and on
        // the worst protocol this transport might negotiate a request
        // that declares its trailer fields gets them delivered:
        // `tests/request_trailers.rs`'s
        // `sends_request_trailers_on_http1_when_the_caller_declares_them`
        // reads them off the wire, and
        // `sends_request_trailers_on_http2_without_any_declaration`
        // reads them off an `h2::server`'s decoded stream. What
        // over-claiming this would have cost is worth naming beside
        // `full_duplex`'s deadlock, because it is the reason this one may
        // be raised where that one may not: a caller who believes it and
        // omits `Trailer:` now gets a typed error naming the field, which
        // is a message rather than a hang or a loss.
        //
        // The asymmetry with `http-ng-h3`, which keeps `false`, is a real
        // difference between two transports rather than a drift between
        // two declarations: that crate sends no request trailers at all
        // and refuses with `RequestTrailersNotSent`.
        caps.request_trailers = true;
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
        // **`true` unconditionally, feature or no feature**, and that is
        // not the floor rule being broken — it is the floor rule being
        // read correctly. The floor governs what a caller may *assume* a
        // request will get; this field says what happens when a caller
        // *asks*, and the answer is the same either way: `execute` reads
        // `RequireVersion`, narrows the ALPN offer to protocols the demand
        // admits, filters pool buckets by it, and refuses with
        // `VersionNotAvailable` before the head if the connection still
        // does not match.
        //
        // Without the `http2` feature this transport only ever speaks
        // HTTP/1.1, and it still honours demands: `RequireVersion(HTTP_11)`
        // proceeds, `RequireVersion(HTTP_2)` is refused before anything is
        // written. A `false` here would make `Client` reject both at the
        // `UnsupportedCapability` gate, including the one this build
        // satisfies trivially — which is exactly the under-claim
        // `Capabilities::version_select`'s doc warns about.
        //
        // This is also the answer to "does the demand widen the floor": it
        // does not, and cannot. `full_duplex` stays `false` and
        // `tests/http2.rs`'s `capabilities_report_the_floor_with_the_
        // feature_on` still holds. The demand is how a caller converts
        // "the floor says no" into "this connection says yes", per
        // request, and a request that does not ask is unaffected by every
        // line of it.
        caps.version_select = true;
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
            hooks: NoHooks,
            // **Nagle off, and asked for here rather than defaulted one
            // layer down.**
            //
            // Measured, twice. `docs/v04-w1-acceptance.md` §7.3 found a
            // cold TLS 1.3 exchange on loopback costing **41.9 ms**, all
            // of it in the head and 10 µs of it in the body;
            // `tests/nagle_cost.rs` re-measures it with the observer on
            // the server's side of the wire, and the shape is
            // unambiguous. The ClientHello arrives at +0.17 ms, the
            // change-cipher-spec record at +0.64 ms, and then **the
            // client's Finished and its whole request arrive together at
            // +41.6 ms**, coalesced into one segment. That is Nagle (RFC
            // 896) meeting the peer's delayed-ACK timer: the second write
            // is held until the first is acknowledged, and Linux's
            // delayed ACK is 40 ms. With `nodelay` the same two writes
            // leave as two segments at +0.76 and +0.81 ms and the whole
            // exchange takes 0.9 ms.
            //
            // **The opinion is HTTP's, so it lives in the HTTP client.**
            // `TcpOpts::default()` stays all-off, because `http-ng-rt` is
            // a socket seam and knows nothing about who is writing:
            // request/response over TLS is exactly the write-write-read
            // pattern Nagle punishes, and a protocol that streams one way
            // is exactly the one it helps. A default down there would
            // impose this crate's protocol on every other caller of the
            // seam.
            //
            // **And it is asked for only where the runtime says it
            // applies it**, which is the whole difference between this and
            // `TcpOpts { nodelay: true }` written unconditionally.
            // `TcpOpts::reject_unsupported` makes a set option a *refusal*
            // on a runtime that cannot apply it — deliberately, since
            // dropping a caller's option silently is worse — so asking
            // unconditionally would turn every connect on a backend whose
            // `TcpConnect::APPLIES` is the trait's default `NONE` into an
            // `Unsupported` error, for an option that caller never
            // mentioned. That default exists precisely to protect a
            // backend that forgot the line; a performance fix that broke
            // it would be aimed at the people it was written for.
            //
            // The shape is `TlsConnect::applies_ech`'s, one seam over and
            // for the same reason: a connector filling a field from
            // something the backend declared it cannot use makes every
            // such origin unreachable. Here silence costs a slow
            // connection rather than a refused one, and that is the
            // direction a claim made by silence must fail in.
            //
            // A caller who *wants* the refusal still gets it, by asking:
            // `tcp_opts(TcpOpts { nodelay: true, .. })` on a `NONE`
            // backend fails at construction, naming `nodelay`.
            // `tests/tcp_opts.rs` pins both halves.
            opts: TcpOpts {
                nodelay: <R as TcpConnect>::APPLIES.nodelay,
                ..TcpOpts::default()
            },
            caps,
            pool,
            svcb_failures: discovery::NegativeCache::default(),
        }
    }
}

/// Everything a `Native` can be configured with, whatever its hook —
/// separated from [`Native::new`] above only because `new` is the one
/// method that names a *particular* `H` ([`NoHooks`]), and putting it in
/// this block would make `Native::<_, _, _, MyHook>::new` a thing a
/// caller could write and get a hookless transport from.
impl<R: TcpConnect + Timer, T: TlsConnect, D, H> Native<R, T, D, H> {
    /// Send this transport's events to `hooks` — see
    /// [`http_ng_core::unversioned::Hooks`] for what it hears and what it
    /// costs, and [`Event`] for the vocabulary.
    ///
    /// **It returns a different type**, and that is the zero-cost
    /// mechanism rather than an inconvenience: the hook is a type
    /// parameter, so the `NoHooks` build monomorphises to code with no
    /// clock reads in it at all, where a `Box<dyn Hooks>` field would
    /// leave every no-hook build carrying a null check on the request
    /// path. A caller who wants to choose at run time can hold an `H`
    /// that decides for itself — an `Arc<dyn Fn..>` inside their own type
    /// — and pay for the choice they actually made.
    ///
    /// The hook may be `!Send`: nothing on this path declares it, so an
    /// `Rc` inside a hook makes this transport `!Send` and leaves it
    /// working (P13; `crates/http-ng-core/tests/shape.rs`).
    pub fn hooks<H2>(self, hooks: H2) -> Native<R, T, D, H2> {
        Native {
            rt: self.rt,
            tls: self.tls,
            dns: self.dns,
            hooks,
            opts: self.opts,
            caps: self.caps,
            epoch: self.epoch,
            pool: self.pool,
            svcb_failures: self.svcb_failures,
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
    /// **This replaces the whole set, including the `nodelay` this
    /// transport asked for itself.** [`Native::new`] sets `nodelay` to
    /// whatever the runtime's [`TcpConnect::APPLIES`] says it can apply —
    /// Nagle's algorithm costs the head of a TLS exchange 41 ms, measured
    /// there — and a caller passing `TcpOpts { keepalive: Some(..),
    /// ..Default::default() }` here turns it back off along with
    /// everything else, because `TcpOpts::default()` is all-off. That is
    /// deliberate rather than a trap left open: these are *the* socket
    /// parameters for every attempt this transport makes, and a method
    /// that silently kept one field of its own would be a worse surprise
    /// than one that takes the caller at their word. `..transport
    /// .tcp_opts_now()` does not exist for the same reason a getter for a
    /// setting nobody set does not: the value to start from is
    /// `TcpOpts { nodelay: true, .. }`, which is what
    /// `tcp_opts_replace_the_whole_set_including_the_nodelay_new_asked_for`
    /// says on the record.
    ///
    /// [`TcpOpts::default`] is all-off, so a runtime that applies nothing
    /// still takes it — which is what makes `Native::new`'s conditional
    /// `nodelay` free of the refusal this method exists to give.
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
impl<R, T, D, H> Native<R, T, D, H>
where
    R: TcpConnect + Timer + Clone,
    R::Stream: 'static,
    T: TlsConnect,
    T::Stream<R::Stream>: 'static,
    // `H: Unpin` is not a taste: the response body holds the hook, and
    // `H1Body::poll_frame` reaches its fields through a safe projection
    // (`Pin<&mut Self>` -> `&mut Self`), which this workspace's
    // `forbid(unsafe_code)` leaves as the only one available. Every hook
    // worth writing is `Unpin` already — `Rc`, `Arc`, an atomic, a
    // closure that captures them.
    H: Hooks + Clone + Unpin,
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
        resp: http::Response<established::NativeBody<NativeIo<R, T>, H>>,
        every: Option<Duration>,
    ) -> http::Response<NativeBody<R, T, H>> {
        resp.map(|b| IdleTimeout::new(b, self.rt.clone(), every))
    }
    /// The `Head` event, from the one place both paths reach it.
    ///
    /// A method rather than two copies of four lines, because the two
    /// call sites are the pooled attempt and the fresh one and a caller
    /// counting heads must not be able to tell them apart — a `Head` that
    /// fired on one path only would look exactly like a request that had
    /// never answered.
    fn report_head(
        &self,
        resp: &http::Response<established::NativeBody<NativeIo<R, T>, H>>,
        id: ConnectionId,
        uri: &http::Uri,
        began: Option<R::Instant>,
    ) {
        self.hooks.on(Event::Head(Head {
            id,
            uri,
            status: resp.status(),
            // `Some`, because this transport read it: off the status line
            // on HTTP/1, off ALPN with the `http2` feature. That is the
            // same claim `capabilities()` makes with `version_reported:
            // true`, and `Head::version`'s doc says the two must agree.
            version: Some(resp.version()),
            elapsed: since::<R>(&self.rt, began),
        }));
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
            // Reported here rather than at the drop below, and outside
            // the pool's mutex rather than inside it — the two rules the
            // hooks seam is built on (`http_ng_core::unversioned::hooks`,
            // module doc). `Stale` is the honest reason: the peer closed
            // it while it sat idle, which is why this loop is walking
            // past it. A connection the pool itself drops for age never
            // reaches this function, and that hole is written down in
            // `docs/v03-acceptance.md` with the reason it exists.
            self.hooks.on(Event::Closed(Closed {
                id: est.id(),
                reason: CloseReason::Stale,
            }));
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
    id: ConnectionId,
) -> Result<established::Established<I>, Error>
where
    I: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
{
    if is_h2(protocol) {
        Ok(established::Established::H2(Box::new(
            http2::handshake(conn, id).await?,
        )))
    } else {
        Ok(established::Established::H1(h1::handshake(conn, id).await?))
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
    id: ConnectionId,
) -> Result<established::Established<I>, Error>
where
    I: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
{
    debug_assert!(!is_h2(protocol));
    Ok(established::Established::H1(h1::handshake(conn, id).await?))
}

/// The HTTP version this transport will actually speak on a connection
/// [`negotiated_protocol`] answered for.
///
/// Derived from [`is_h2`] rather than from a `match` of its own, and that
/// is the whole point: `is_h2` is the predicate [`handshake_for`] branches
/// on, so this cannot answer `HTTP_2` for a connection that got an HTTP/1
/// handshake, or the reverse, however either is later changed.
///
/// **`None` is `HTTP_11`, and that is not a fallback of convenience.**
/// `negotiated_protocol` returns `None` for an ALPN value this transport
/// does not speak, and the standing answer to that (v0.2 W2, unchanged) is
/// to speak HTTP/1.1 on that one connection and keep it out of the pool.
/// So HTTP/1.1 is what a [`RequireVersion`] demand is compared against
/// there — the truth about what the next bytes will be — rather than a
/// separate "unknown" that would have to invent a third answer.
fn spoken_version(protocol: Option<Protocol>) -> http::Version {
    if is_h2(protocol) {
        http::Version::HTTP_2
    } else {
        http::Version::HTTP_11
    }
}

/// Whether a connection speaking `protocol` may serve this request.
///
/// Used to *filter* pool candidates and to *narrow* the ALPN offer, where
/// [`check_version`] is used to *refuse*. The two must agree, so both read
/// the same demand through the same [`spoken_version`] mapping: a
/// candidate this returns `false` for is precisely one `check_version`
/// would have refused, which is why skipping it is a routing decision and
/// not a silent downgrade.
fn protocol_admissible(extensions: &http::Extensions, protocol: Option<Protocol>) -> bool {
    check_version(extensions, spoken_version(protocol)).is_ok()
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

/// A transport whose own name resolution can be done ahead of the
/// exchange, and handed straight back to it.
///
/// # Why this is a trait of this crate's and not a method on `Transport`
///
/// `Transport` is the seam **every** backend fills in, and this is a
/// question exactly one kind of backend can be asked: a `fetch`-shaped
/// transport has no DNS of its own to save, and a `wasi:http` one has no
/// connector at all. Putting it on the seam would make every other backend
/// answer for a thing it does not have — the mistake
/// `Capabilities::upgrade` was deleted for.
///
/// It is a trait rather than two inherent methods for a duller reason,
/// worth writing down because it is not obvious from the outside: a caller
/// generic over `Native<R, T, D>` reaches it through a `where` bound, and
/// an inherent method would make that caller repeat every structural bound
/// [`Native`]'s exchange impl declares — and then still not be able to
/// name the response body, because `<Native<..> as Transport>::Body`
/// behind a `where` clause is an opaque projection that does not normalise
/// to a concrete type. As a supertrait-bounded trait method the return
/// type *is* that projection, which is what `http-ng-select` already
/// writes.
pub trait Prefetch: Transport {
    /// Do now the HTTPS-record lookup this transport would otherwise do
    /// inside [`Transport::execute`], and hand the request back with the
    /// answer attached.
    /// Do now the name resolution this transport would otherwise do inside
    /// [`Transport::execute`], and hand the request back with the answer
    /// attached.
    ///
    /// # Who this is for
    ///
    /// A caller that is going to ask the same question anyway. RFC 9460's
    /// `alpn` says which protocols an origin speaks, and a caller owning a
    /// second protocol stack (`http-ng-select`) has to read it *before* it
    /// can decide which stack a request belongs to — after which this
    /// transport, left to itself, would ask for the very same record a
    /// second time. Measured, that was two type-65 queries for one request
    /// (`crates/http-ng-select/tests/dns_cost.rs`).
    ///
    /// # What it does not become
    ///
    /// **Not a way to tell a transport something.** The answer is fetched
    /// *here*, by the transport, with its own resolver and its own memory,
    /// for the authority of the request handed in — so there is no version
    /// of this call in which a caller supplies the record. See [`Prepared`]
    /// for why that is the whole point, and `docs/v04-w1-acceptance.md` §3
    /// for the shape that was rejected.
    ///
    /// **Not a cache.** What comes back is good for the one request it
    /// travels with, and nothing here remembers it — for
    /// `http-ng-native`'s reason: an HTTPS record carries no TTL, and
    /// inventing a lifetime for someone else's answer is how a resolver's
    /// cache and ours drift apart.
    ///
    /// # What it costs, and when it costs nothing
    ///
    /// Exactly what discovery costs inside `execute`, which for a URI
    /// discovery does not apply to (`http://`, a port other than 443, an
    /// origin held off by a failure this transport remembers) is **no
    /// query at all** — [`Discovered::NotConsulted`] comes back and
    /// nothing was asked.
    ///
    /// One difference in *timing* is worth knowing. Inside `execute` the
    /// record is fetched only when a connection is opened, and beside the
    /// address lookups; here it is fetched before either, so a request
    /// that would have been served from a pooled connection still pays for
    /// the query. That is the caller's trade, and for the caller this
    /// exists for it is not a new cost: it is the query that caller was
    /// about to make itself.
    ///
    /// Written as `-> impl Future` rather than `async fn` for
    /// `Transport::execute`'s reason: no `Send` bound is added anywhere in
    /// this workspace's seams, and this one is on the same footing.
    fn prepare(&self, req: http::Request<RequestBody>) -> impl Future<Output = Prepared>;

    /// [`Transport::execute`], for a request whose record has already been
    /// fetched by [`Self::prepare`].
    ///
    /// Identical in every other respect — same pool, same timeouts, same
    /// errors. What it does not do is ask DNS for a record a moment after
    /// somebody else asked for the same one.
    ///
    /// A [`Prepared::new`] — nothing looked up — is exactly
    /// [`Transport::execute`], which is what a caller that prepares some
    /// requests and not others hands over for the rest.
    fn execute_prepared(
        &self,
        prepared: Prepared,
    ) -> impl Future<Output = Result<http::Response<Self::Body>, Self::Error>>;
}

/// The exchange itself.
///
/// The bounds are the [`Transport`] impl's below, repeated because an
/// inherent method cannot inherit them.
impl<R, T, D, H> Native<R, T, D, H>
where
    R: TcpConnect + Timer + Clone,
    R::Stream: 'static,
    T: TlsConnect,
    T::Stream<R::Stream>: 'static,
    H: Hooks + Clone + Unpin,
{
    /// The whole of an exchange, from a request that may or may not have
    /// been prepared.
    ///
    /// One body for both entry points rather than two that must be kept
    /// in step: [`Transport::execute`] is this with [`Prepared::new`] —
    /// nothing looked up — and [`Prefetch::execute_prepared`] is this with
    /// whatever [`Prefetch::prepare`] found.
    async fn run(&self, prepared: Prepared) -> Result<http::Response<NativeBody<R, T, H>>, Error>
    where
        D: Resolve,
    {
        let Prepared {
            req,
            found: prefetched,
        } = prepared;
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

        // When this transport received the request, for `Head::elapsed`
        // — and `None` when nobody is watching, so a build with no hook
        // does not read the clock a second time here. It is deliberately
        // NOT derived from `now` above: that is an elapsed time from this
        // transport's epoch, and subtracting two of those would be the
        // same measurement done in a way that a later change to `epoch`
        // could silently break.
        let began = mark::<H, R>(&self.rt);

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
            // A `RequireVersion` demand filters the buckets before it
            // refuses anything: a pooled HTTP/1.1 connection under a
            // demand for HTTP/2 is not a failure, it is the wrong
            // connection, and a fresh one may still negotiate h2. Skipping
            // here is what makes the refusal below rare rather than the
            // normal outcome for any origin that has ever been spoken to.
            //
            // The head is not written on a skipped candidate because
            // `established::exchange` is never reached for it — the check
            // is the loop's first statement, above every borrow of the
            // pool.
            if !protocol_admissible(req.extensions(), Some(protocol)) {
                continue;
            }
            let key = parts_of_key.key(protocol);
            let Some(est) = self.checkout(&key, now).await else {
                continue;
            };
            // Emitted from the branch that took a connection out of the
            // pool, so the event cannot be right for the wrong reason:
            // there is no flag anywhere saying "this one was reused",
            // only the code path on which it was. The id is the
            // connection's own, assigned when it was made, so a caller
            // can find the `Connected` this refers back to.
            let id = est.id();
            self.hooks.on(Event::Reused(Reused {
                id,
                uri: &uri,
                version: spoken_version(Some(protocol)),
            }));
            let checkin = self.checkin_for(&key, now);
            let attempt = established::exchange(est, req, checkin, &uri, self.hooks.clone());
            match self.within_first_byte(timeouts.first_byte, attempt).await {
                Ok(resp) => {
                    self.report_head(&resp, id, &uri, began);
                    return Ok(self.bound_body(resp, timeouts.between_bytes));
                }
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
        // The second half of honouring a demand, and the half that makes
        // it useful rather than merely fatal: **a demand narrows what is
        // offered**, so a caller who requires HTTP/1.1 gets a connection
        // that negotiates HTTP/1.1 instead of one that negotiates h2 and
        // is then refused. Without this conjunct the h1 direction of the
        // demand would be unsatisfiable against any h2-capable server —
        // the client would propose `h2`, the server would take it, and the
        // request the caller said needed HTTP/1.1 would fail on a
        // connection the client itself chose to make wrong.
        //
        // It cannot over-offer: `may_speak_h2` still governs, so this only
        // ever removes `h2` from the list, never adds it.
        let offered_h2 = self.may_speak_h2(&parts_of_key)
            && check_version(req.extensions(), http::Version::HTTP_2).is_ok();
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
        let connect_fut = connect::connect::<R, D, T, H>(
            &self.rt,
            &self.dns,
            &self.tls,
            &uri,
            &self.opts,
            alpn,
            &self.svcb_failures,
            now,
            // Whatever `prepare` found, or `NotConsulted` for a request
            // that came through `Transport::execute`. This is the only
            // way a record reaches the connector from outside this
            // function, and it was fetched for this request's own
            // authority — see `Prepared`.
            prefetched,
        );
        let (conn, tls_info, attempted) =
            with_connect_timeout(&self.rt, timeouts.connect, connect_fut).await?;
        // The whole connect, measured from the same mark `Head::elapsed`
        // uses, so the two are comparable — which is the pair that
        // answers "was it the connection or was it the server".
        let connect_took = since::<R>(&self.rt, began);

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

        // Assigned before the connection is used and carried into the
        // handshake, so the pool holds it and every later event about
        // this socket — `Reused`, `Closed` — names the same number.
        let id = connection_id::<H>();
        // `Some` exactly when `H::WATCHING` — see `connect::Attempted`,
        // which is boxed for the stack it would otherwise cost every
        // request that wanted none of this.
        // Emitted here, at the honest instant: the connection exists and
        // nothing has been spoken on it. In particular this is BEFORE
        // the `RequireVersion` refusal below, which drops a connection
        // that really was established — reporting it afterwards would
        // leave that case invisible, and reporting it as closed as well
        // would need a fourth `CloseReason` for a drop, which the seam
        // deliberately does not have.
        //
        // `spoken_version(protocol)` rather than the ALPN string: it is
        // the same function `handshake_for` branches on, so this cannot
        // claim HTTP/2 for a connection that got an HTTP/1 handshake.
        if let Some(attempted) = attempted {
            self.hooks.on(Event::Connected(Connected {
                id,
                uri: &uri,
                remote: attempted.remote,
                version: spoken_version(protocol),
                timing: ConnectTiming {
                    dns: attempted.dns,
                    tcp: attempted.tcp,
                    tls: attempted.tls,
                    total: connect_took,
                },
            }));
        }

        // **The refusal, and its position is the guarantee.** This is the
        // first instant the negotiated protocol is known, and it is before
        // `handshake_for` — which for h2 writes the connection preface and
        // for h1 writes nothing, and before `established::exchange`, which
        // writes the head. So a `RequireVersion` demand this connection
        // cannot meet costs the server a TCP connection and a TLS
        // handshake and **not one byte of HTTP**.
        //
        // The narrowing above is why this is reached at all rather than
        // being dead: the offer already excludes a protocol the demand
        // forbids, so what remains is the case the client does not
        // control — a server that selects something else, or a TLS backend
        // that reports one. That is precisely the case a caller cannot
        // discover any other way, which is the whole argument for the
        // demand existing (`docs/v04-design.md`, Appendix A).
        //
        // Checked before `checkin_for`, so a connection about to be
        // refused is never given a check-in token either. It is dropped
        // here, unpooled, which is right: nothing was spoken on it.
        check_version(req.extensions(), spoken_version(protocol))?;

        let checkin = match protocol {
            Some(p) => self.checkin_for(&parts_of_key.key(p), now),
            None => None,
        };

        let est = handshake_for(conn, protocol, id).await?;
        let attempt = established::exchange(est, req, checkin, &uri, self.hooks.clone());
        let resp = self
            .within_first_byte(timeouts.first_byte, attempt)
            .await
            .map_err(established::Failed::into_error)?;
        self.report_head(&resp, id, &uri, began);
        Ok(self.bound_body(resp, timeouts.between_bytes))
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
impl<R, T, D, H> Transport for Native<R, T, D, H>
where
    R: TcpConnect + Timer + Clone,
    R::Stream: 'static,
    T: TlsConnect,
    T::Stream<R::Stream>: 'static,
    D: Resolve,
    // `H: Clone` for the same reason `R: Clone` is here: the response
    // body outlives `execute` and reports the connection's end from
    // `poll_frame`, so it needs a hook of its own rather than a borrow of
    // this transport's. `H: Unpin` is argued where the sibling impl
    // above declares it.
    H: Hooks + Clone + Unpin,
{
    /// The pooled body, with the `between_bytes` bound wrapped round it.
    ///
    /// The order matters and is the mirror of `http_ng::ClientBody`'s: the
    /// idle bound is the **innermost** wrapper, next to the socket, so it
    /// measures the gap between reads on the wire. Outside it the client
    /// may add its own `Deadline` and decompression, neither of which can
    /// hide a silent peer from this one.
    type Body = NativeBody<R, T, H>;
    type Error = Error;

    /// [`Native::run`] with nothing looked up — which is what every
    /// request through this seam is, because the seam has no way to carry
    /// an answer and [deliberately does not gain one](Prefetch::prepare).
    /// The record is fetched inside, when and if a connection is opened.
    async fn execute(
        &self,
        req: http::Request<RequestBody>,
    ) -> Result<http::Response<Self::Body>, Error> {
        self.run(Prepared::new(req)).await
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

/// The one implementation, and the contract is on the trait.
///
/// There are deliberately **no inherent methods of the same names**: an
/// inherent method wins method resolution over a trait one, so a caller
/// with the trait in scope would silently get the other function and, with
/// it, a concrete response body where the projection was wanted. Two
/// spellings of one thing is how that mistake gets made.
impl<R, T, D, H> Prefetch for Native<R, T, D, H>
where
    R: TcpConnect + Timer + Clone,
    R::Stream: 'static,
    T: TlsConnect,
    T::Stream<R::Stream>: 'static,
    D: Resolve,
    H: Hooks + Clone + Unpin,
{
    async fn prepare(&self, req: http::Request<RequestBody>) -> Prepared {
        // The same reading of the same clock the negative cache's window
        // is measured on — see the `epoch` field. `run` takes its own for
        // the pool's bookkeeping; these are two questions, and a request
        // that never reaches `run` must not need the second.
        let now = self.rt.elapsed_since(self.epoch);
        let uri = req.uri();
        let found = match (connect::host(uri), connect::wants_tls(uri)) {
            (Ok(host), Ok(use_tls)) => {
                let port = connect::port(uri, use_tls);
                // The connector's own function, not a copy of its rule:
                // where discovery applies is decided in one place, so this
                // cannot drift into asking where `execute` would not, or
                // into staying silent where it would.
                connect::discovered_endpoint(
                    &self.dns,
                    host,
                    use_tls,
                    port,
                    &self.svcb_failures,
                    now,
                )
                .await
            }
            // A URI with no host, or a scheme this transport refuses.
            // Nothing is looked up and nothing is reported: the error is
            // this request's to meet where it always met it, inside the
            // exchange, with the same type and the same message. Failing
            // here would move a typed error into a method a caller need
            // never have called.
            _ => discovery::Prefetched::NotConsulted,
        };
        Prepared { req, found }
    }

    async fn execute_prepared(
        &self,
        prepared: Prepared,
    ) -> Result<http::Response<Self::Body>, Self::Error> {
        self.run(prepared).await
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
///
/// # `Option<Duration>`, and one `await` at the call site
///
/// `None` is "no bound", handled here rather than by a second arm at the
/// call site — [`Native::within_first_byte`] has taken `Option` for the
/// same reason since v0.2 W4, and this half had simply not caught up.
/// It is not tidying: a `match` with `fut.await` in one arm and
/// `with_connect_timeout(.., fut).await` in the other is **two** await
/// points holding the same future, and a debug build lays out both — the
/// `connect::connect` future is about half of `Native::execute`'s, so
/// counting it twice cost ten kilobytes of stack on every request.
/// Measured, in the round that added the observability hooks: 22952 →
/// 12464 bytes for `execute`'s future, on a workspace test that had been
/// passing at 99% of a 2 MiB test thread.
async fn with_connect_timeout<R, F, T>(rt: &R, d: Option<Duration>, fut: F) -> Result<T, Error>
where
    R: Timer,
    F: Future<Output = Result<T, Error>>,
{
    let Some(d) = d else {
        return fut.await;
    };
    let mut fut = std::pin::pin!(fut);
    // Built only on this branch, exactly as in `within_first_byte`:
    // `Tokio::sleep` panics outside a runtime, and a request that asked
    // for no bound must not need one.
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
        use http_ng_core::unversioned::{ConnectionId, NoHooks};
        let est = crate::h1::handshake(io, ConnectionId::UNWATCHED).await?;
        crate::h1::exchange(est, req, None, NoHooks, ConnectionId::UNWATCHED)
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
