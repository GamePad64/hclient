//! Native transport for hclient: TCP + TLS + HTTP/1.1, and HTTP/2 behind
//! a feature.
//!
//! This crate wires together the runtime ([`hclient_rt`]), DNS
//! ([`hclient_dns`]) and TLS ([`hclient_tls`]) on top of `hyper`.
//! [`Native`] is the only public type; the modules under it are
//! `pub(crate)` — `body` (the request-body adapter), `connect` (resolution
//! and Happy Eyeballs), `h1` and `http2` (the protocol drivers, the second
//! behind the `http2` feature and **not** on hyper — see its module doc),
//! `pool`, and `established`, the one place that knows there is more than
//! one protocol.
//!
//! # `Native::execute` does not resolve DNS itself
//!
//! It calls `connect::connect`, and the reason is a defect that is easy to
//! write twice. Resolving by hand — `.filter_map(|r| async { r.ok() })`
//! over `Resolve::lookup_ipv4`/`lookup_ipv6`, then synthesizing an
//! `ErrorKind::Resolve` if both streams came out empty — discards every
//! resolver error, `ErrorKind::Cancelled` among them. Conflating *the
//! resolver failed* with *the runtime is shutting down* breaks a circuit
//! breaker keyed on `Resolve`: it blacklists a live host during an
//! ordinary shutdown.
//!
//! `connect::drive`/`ResolveErrors::distinguishing_error` closes that
//! structurally — a `kind()` differing from the synthetic `Resolve` is
//! checked BEFORE either failure branch — so there is one implementation
//! rather than one per caller. `transport.rs`'s
//! `resolver_cancelled_error_reaches_the_caller_through_execute_not_flattened`
//! checks the property survives the whole `Client::execute` path.
#![forbid(unsafe_code)]

mod body;
mod connect;
mod discovery;
mod error;
/// RFC 9114's ALPN identifier, and the one string that decides whether an
/// origin's HTTPS record or `Alt-Svc` advertisement is about HTTP/3.
#[cfg(feature = "http3")]
pub(crate) const ALPN_H3: &[u8] = b"h3";

#[cfg(feature = "http3")]
pub mod altsvc;
#[cfg(feature = "http3")]
pub mod caps;
mod established;
#[cfg(feature = "http3")]
mod failures;
mod http1;
#[cfg(feature = "http3")]
mod http3;
/// Bind a QUIC endpoint on this workspace's own runtime seam — quinn
/// driven by whichever `hclient_rt` implementation the caller already has.
///
/// A build that wants bare QUIC and no HTTP opinion takes this and nothing
/// else from the crate; it was `hclient-quinn`'s whole public surface.
#[cfg(feature = "http3")]
pub use crate::http3::runtime::endpoint;
/// The QUIC connect deadline's error, renamed on the way out: this crate's
/// own TCP one is private and carries the same name, and two types with
/// one name in one crate is how a reader ends up reading the wrong doc.
#[cfg(feature = "http3")]
pub use http3::ConnectTimedOut as H3ConnectTimedOut;
/// The HTTP/3 stack this transport's QUIC arm is built from.
///
/// It was `hclient-h3`, a crate of its own, on a reason that measurement
/// disproved: `H3`'s declaration carries no where-clause, so a feature
/// here makes the module and the constructor unconditional rather than the
/// bounds, and the arm is stored erased so nothing reaches
/// `impl Transport for Native`. What a neighbour switching `http3` on
/// costs a build that never asked is dead code in the graph, not a broken
/// one — and the crate's only consumer was this one.
///
/// A build that wants HTTP/3 **alone**, with no TCP stack beside it, still
/// has `Client::builder(H3::new(..))`: the type is a full `Transport` and
/// nothing about it changed in the move.
#[cfg(feature = "http3")]
pub use http3::{
    DEFAULT_KEEP_ALIVE, H3, H3Body, H3Runtime, QuinnTask, RequestTrailersNotSent,
    UnknownRequestBodyFrame,
};
/// The QUIC stack's staged pair, renamed on the way out because this crate
/// has two.
///
/// They are separate traits rather than one, and the merge did not change
/// that: a trait is declared by the crate — now the module — that
/// implements it, and **the two do not agree on what `connect` takes**.
/// [`StagedConnect::connect`] takes a [`Prepared`], the request *with* the
/// HTTPS record fetched for it; [`H3StagedConnect::connect`] takes the
/// request alone, because the QUIC arm has no record lookup of its own.
///
/// Nothing needs polymorphism between them: the routing owns both
/// concretely.
#[cfg(feature = "http3")]
pub use http3::{Refused as H3Refused, Staged as H3Staged, StagedConnect as H3StagedConnect};
#[cfg(feature = "http3")]
mod race;
#[cfg(feature = "http3")]
mod route;

/// How long a failed HTTP/3 connect suppresses the QUIC arm for one
/// origin, and the memory that records it.
#[cfg(feature = "http3")]
pub use failures::{H3_FAILURE_TTL, H3Failures};
/// The default head start the hedge gives the QUIC arm.
#[cfg(feature = "http3")]
pub use race::DEFAULT_HEAD_START;
#[cfg(feature = "http2")]
mod http2;
mod idle;
mod pool;
pub mod proxy;
mod staged;
mod upgrade;

pub use connect::Conn;
pub use discovery::{Discovered, Prepared, SVCB_FAILURE_TTL};
pub use error::{
    BetweenBytesElapsed, EndedBeforeTheResponse, FirstByteTimedOut, Http2NotCompiledIn,
    MaxBufSizeTooSmall, NoVersionsLeft, NotSwitchingProtocols, PlaintextNeedsHttp1,
    ProxyAndUnixSocket, ProxySpokeFirst, ResolveTimedOut, UndeclaredRequestTrailers,
};
pub(crate) use error::{ConnectTimedOut, UnknownClientIdentity};
/// The future [`Native::multiplexed`] spawns — public because that
/// constructor's `Spawn` bound has to name it, and for no other reason.
#[cfg(feature = "http2")]
pub use http2::{H2Driver, H2KeepAlive, H2Opts, PingNotAnswered};
// `Prefetch` is declared in this file, beside the exchange it refines.
pub use http1::H1Opts;
pub use idle::IdleTimeout;
pub use pool::{PoolConfig, Reaper};
pub use proxy::{Approach, Handshake, NoProxy, Proxy, ProxyScheme};
#[cfg(feature = "proxy")]
pub use proxy::{
    HttpConnect, ProxyRefused, Socks4, Socks4HandshakeError, Socks4Refused, Socks5,
    Socks5HandshakeError, Socks5Refused,
};
pub use staged::{Refused, Staged, StagedConnect};
pub use upgrade::Upgrading;

use hclient_core::unversioned::{
    CloseReason, Closed, ConnectTiming, Connected, ConnectionId, Event, Head, Hooks, NoHooks,
    RequestId, Reused, Transport,
};
use hclient_core::{
    CancelSupport, Capabilities, ClientIdentity, Error, ErrorKind, Phase, RedirectSupport,
    RequestBody, ReuseSupport, TimeoutSupport, Timeouts, check_version,
};
use hclient_dns::Resolve;
use hclient_rt::{Spawn, TcpConnect, TcpOpts, Timer};
use hclient_tls::TlsConnect;
use pool::{CheckIn, Pool, PoolKey, Protocol, Security};
use std::fmt::Debug;
use std::future::Future;
use std::future::poll_fn;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

/// How a shared HTTP/2 connection's driver gets onto the runtime: the
/// spawner, captured as a plain function pointer.
///
/// **A type alias for a reason beyond the lint.** What it says is that the
/// only thing [`Native`] keeps of `R: Spawn<..>` is a pointer — no bound,
/// no trait object, nothing that reaches any other signature — which is
/// the whole mechanism behind [`Native::multiplexed`] and is easy to lose
/// sight of when the type is spelled out inline.
#[cfg(feature = "http2")]
type SpawnH2<R, T, H> = fn(&R, http2::H2Driver<NativeIo<R, T>, H, R>);

/// Monomorphised where `H: Clone + Send + Sync + 'static` is known, called
/// where it is not.
type Watch1xx<H> = fn(
    &H,
    &mut http::Request<body::OutgoingBody>,
    ConnectionId,
    RequestId,
    Option<Arc<body::ContinueGate>>,
);

/// The same callback with nothing to report to.
///
/// Separate from [`install_1xx`] rather than a branch inside it because
/// the two differ in their **bounds**: reporting to a hook needs
/// `H: Send + Sync + 'static`, and opening a gate needs nothing of `H` at
/// all. Folding them together would make `Native::expect_continue` demand
/// a bound it has no use for.
fn install_gate_only(req: &mut http::Request<body::OutgoingBody>, gate: Arc<body::ContinueGate>) {
    hyper::ext::on_informational(req, move |resp| {
        if resp.status() == http::StatusCode::CONTINUE {
            gate.open();
        }
    });
}

/// The body of [`Native::watching_1xx`]'s pointer, monomorphised where the
/// `Send + Sync + 'static` is known and called where it is not.
///
/// The hook is **cloned into** the callback rather than borrowed, because
/// hyper stores it in an `Arc<dyn .. + Send + Sync>` that outlives this
/// call. `H: Clone` is already what `Native` asks of a hook everywhere
/// else — `self.hooks.clone()` is on the request path today.
fn install_1xx<H>(
    hooks: &H,
    req: &mut http::Request<body::OutgoingBody>,
    id: ConnectionId,
    request: RequestId,
    gate: Option<Arc<body::ContinueGate>>,
) where
    // The marker sits on the `where` line rather than in the angle
    // brackets because `cargo fmt` splits a long signature and carries a
    // trailing comment off the line the bound is on — which silently
    // unmarked this once already, and the invariant caught it.
    H: Hooks + Clone + Send + Sync + 'static, // send-bound-exception: amendment-C2
{
    let hooks = hooks.clone();
    hyper::ext::on_informational(req, move |resp| {
        // **One closure, two readers**, because `on_informational` stores
        // ONE callback in the request's extensions and a second call
        // replaces the first. The gate and the hook cannot each install
        // their own.
        if let (http::StatusCode::CONTINUE, Some(g)) = (resp.status(), &gate) {
            g.open();
        }
        hooks.on(&Event::Informational(
            hclient_core::unversioned::Informational::new(id, resp.status(), resp.headers())
                .request(request),
        ));
    });
}

/// The IO a [`Native`] speaks HTTP/1 over: a plain socket from the runtime,
/// or that same socket wrapped by the TLS backend.
///
/// A public alias rather than an anonymous type in a signature, because it
/// is what `Native`'s `Transport::Body` is generic over, and a caller who
/// has to name that body should not have to spell this out themselves.
pub type NativeIo<R, T> =
    Conn<<R as TcpConnect>::Stream, <T as TlsConnect>::Stream<<R as TcpConnect>::Stream>>;

/// What [`Native::bound_body`] needs to count a response body.
///
/// A struct rather than three parameters because two of the three are
/// `Option`-shaped and a call site passing them positionally reads as a
/// puzzle — and because [`Counted::already`] is then a name for the one
/// caller that does not count, rather than three `None`s a reader has to
/// interpret.
pub(crate) struct Counted<'a> {
    id: ConnectionId,
    request: RequestId,
    /// `None` is *something below already counted this body*, which is the
    /// QUIC arm — see `hclient_core::unversioned::Counting::new`.
    uri: Option<&'a http::Uri>,
    sent: Option<std::sync::Arc<hclient_core::unversioned::Meter>>,
}

impl<'a> Counted<'a> {
    pub(crate) fn new(
        id: ConnectionId,
        request: RequestId,
        uri: &'a http::Uri,
        sent: Option<std::sync::Arc<hclient_core::unversioned::Meter>>,
    ) -> Self {
        Self {
            id,
            request,
            uri: Some(uri),
            sent,
        }
    }

    /// The QUIC arm's answer: `hclient_native::H3` wrapped this body
    /// before handing it up, and counting it twice would report every
    /// octet twice.
    #[cfg(feature = "http3")]
    pub(crate) fn already(id: ConnectionId) -> Self {
        Self {
            id,
            // Nothing is counted here and nothing is reported, so there is
            // no event for an id to name: the QUIC arm already put its own
            // on the `Progress` events for this body.
            request: RequestId::UNIDENTIFIED,
            uri: None,
            sent: None,
        }
    }
}

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
pub type NativeBody<R, T, H> = IdleTimeout<
    hclient_core::unversioned::Counting<established::NativeBody<NativeIo<R, T>, H>, H>,
    R,
>;

/// This crate's clock, handed to `hclient_core`'s gate.
///
/// The gate — *read a clock only where a hook is watching* — is
/// `hclient_core::unversioned::mark`, and lives there because four crates
/// were each writing it and one of them got it wrong in a way a mutation
/// survived. What stays here is the only part that is this crate's: which
/// clock.
pub(crate) fn mark<H: Hooks, R: Timer>(rt: &R) -> Option<R::Instant> {
    hclient_core::unversioned::mark::<H, _>(|| rt.now())
}

/// The elapsed half of the pair above.
pub(crate) fn since<R: Timer>(rt: &R, at: Option<R::Instant>) -> Duration {
    hclient_core::unversioned::since(at, |t| rt.elapsed_since(t))
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

/// The hclient transport over real TCP/TLS/HTTP1: wires together the
/// runtime `R` ([`hclient_rt::TcpConnect`] + [`hclient_rt::Timer`]), TLS `T`
/// ([`hclient_tls::TlsConnect`]) and resolver `D` ([`hclient_dns::Resolve`]).
///
/// Connections are reused: see [`PoolConfig`], [`Native::pool`]
/// and [`Native::without_pool`], and read `crate::pool`'s module doc for
/// what "reused" costs when there is no `Spawn` to drive an idle
/// connection.
///
/// HTTP/1.1 always; **HTTP/2 with the `http2` feature**, on `https://`
/// origins whose TLS backend both negotiated `h2` and can say so — see
/// `TlsConnect::reports_alpn` and `crate::http2`'s module doc. There is
/// no h2c: cleartext stays HTTP/1.1. What was negotiated is readable after
/// the fact from `Response::version()`; it is deliberately not readable
/// from [`Capabilities`], which report the floor (see [`Native::new`]).
/// No upgrade on either protocol. The request body is **not** buffered whole:
/// `RequestBody::Streaming` goes to the wire as a stream (see
/// [`Native::new`]'s doc comment on `streaming_request_body`).
///
/// # `H`, the observability hook
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
///
/// # `execute`'s future **is** `Send`, and what that rests on
///
/// The transport is `Send + Sync` and so is its body — both asserted in
/// `tests/shape.rs`. So is the future, on the shipped stacks, so a caller
/// **can** `tokio::spawn` a request; `tests/send_future.rs` pins it twice,
/// once as a type and once by actually spawning one.
///
/// **Two different properties, and they are proved differently.** At a
/// concrete stack — `Native<Tokio, Rustls, SystemDns<Tokio>>` — `Send` is
/// *inferred*, and nothing has to be declared: a `Resolve` handing back a
/// `!Send` stream still works and still yields a `!Send` future. That is
/// what `tests/send_future.rs` pins. Generic code cannot infer, so
/// `hclient_core::unversioned::SendTransport` is *declared* for this
/// transport, under a where-clause naming what its runtime, TLS backend
/// and resolver promise — which is what `hclient::Client`'s own request
/// future being `Send` rests on.
///
/// **What used to remove the first was one box.** `connect.rs`'s `Answers`
/// held the resolver's stream as `Pin<Box<dyn Stream<..> + 'a>>`. The
/// shipped resolvers all *produce* `Send` streams, and the `dyn` threw the
/// fact away on the way past: a trait object that declares no auto traits
/// is not neutral about them, it **removes** them from everything behind
/// it. It is `Pin<Box<S>>` now — same allocation, same absence of
/// `unsafe`, the type simply not discarded.
///
/// **What blocked the second was that nothing could be named**, and the
/// repair was not to declare `Send` in the seams. `Resolve`,
/// `TcpConnect`, `TlsConnect` and `Blocking` carry **associated futures**,
/// so a consumer can name them and each implementor still answers for
/// itself. Naming is not requiring, and this workspace has three proofs of
/// it in-tree: `hclient-rt-embassy` boxes its `Connecting` plain because
/// `embassy_net::Stack` is `&RefCell<Inner>`; `hclient-dns-doh` boxes its
/// streams plain because it resolves through a generic `C: Transport`; and
/// `connect.rs`'s own `FakeRuntime` does the same because it keeps an
/// `Rc`. All three are still what their seams say they are.
///
/// The shape that was measured and rejected is the other one: a fixed
/// `BoxStream` in `Resolve` *requires* `Send` and cost `hclient-dns-doh`
/// its impl outright. `scripts/no-send-or-sync-in-the-core-surface.sh`'s
/// own sentence is about exactly that — *declaring the bound in the seam
/// forces it on backends that cannot satisfy it* — and neither removing a
/// `dyn` nor naming a future does.
///
/// **The `http3` arm is `Send` too**, which took the same treatment one
/// level down: `StagedConnect` carries associated futures, so
/// `http3::arm`'s blanket impl can name them and its three boxes declare
/// `Send`. `H3` was always clean underneath — `H3::resolve` boxes the
/// concrete stream, and `UdpBind::bind` and
/// `QuicTlsConnect::quic_client_config` are synchronous.
///
/// **The positive half is a fence again**, and the sentence it replaces is
/// worth keeping: it read that a fence could not assert this, because a
/// doctest cannot be gated on `not(feature = "http3")` and the claim was
/// false with that feature on. The claim is true in every configuration
/// now, so the reason has no subject and the fence is back.
///
/// ```no_run
/// # use hclient_core::unversioned::Transport;
/// # use hclient_core::RequestBody;
/// # use hclient_native::Native;
/// # use hclient_dns::IpLiteralOnly;
/// # use hclient_tls::NoTls;
/// # use hclient_rt_tokio::Tokio;
/// fn assert_send<T: Send>(_: T) {}
/// let t = Native::new(Tokio, NoTls, IpLiteralOnly);
/// assert_send(t.execute(http::Request::new(RequestBody::Empty)));
/// ```
///
/// And the refusal, which differs in one seam. A hook holding an `Rc` is
/// the documented `!Send` allowance on that seam, and it travels into the
/// future — in every configuration, so this one can be a fence:
///
/// ```compile_fail
/// # use hclient_core::unversioned::Transport;
/// # use hclient_core::RequestBody;
/// # use hclient_core::unversioned::{Event, Hooks};
/// # use hclient_native::Native;
/// # use hclient_dns::IpLiteralOnly;
/// # use hclient_tls::NoTls;
/// # use hclient_rt_tokio::Tokio;
/// # use std::rc::Rc;
/// #[derive(Clone)]
/// struct Counting(Rc<std::cell::Cell<u32>>);
/// impl Hooks for Counting {
///     const WATCHING: bool = true;
///     fn on_event(&self, _: Event<'_>) {
///         self.0.set(self.0.get() + 1);
///     }
/// }
/// fn assert_send<T: Send>(_: T) {}
/// let t = Native::new(Tokio, NoTls, IpLiteralOnly).hooks(Counting(Rc::default()));
/// assert_send(t.execute(http::Request::new(RequestBody::Empty)));
/// ```
/// Which HTTP versions a [`Native`] may speak.
///
/// Private, and the setters are [`Native::http1`] and [`Native::http2`]:
/// a public struct here would be a fourth way to reach a state the two
/// setters already refuse between them.
#[derive(Debug, Clone, Copy)]
struct Versions {
    h1: bool,
    h2: bool,
    /// Whether the QUIC arm may serve a request. Meaningful only once
    /// [`Native::http3`] has installed one — a `true` with no arm has
    /// nothing to route to, which is why the constructor sets it rather
    /// than a setter of its own.
    ///
    /// Behind the feature with the arm it describes: a policy field for a
    /// protocol whose code is not compiled has no reader, and this crate
    /// treats a field nothing reads as a defect rather than as headroom.
    #[cfg(feature = "http3")]
    h3: bool,
}

impl Default for Versions {
    /// `h1` always, `h2` wherever the code was compiled in.
    ///
    /// The h2 default follows the feature because that is what the
    /// transport did before this setting existed: with `http2` on, h2 is
    /// offered wherever ALPN can carry it. A default of `false` would make
    /// the feature a no-op until a second call, which is a silent
    /// downgrade for every build that has it on today.
    fn default() -> Self {
        Self {
            h1: true,
            h2: cfg!(feature = "http2"),
            #[cfg(feature = "http3")]
            h3: false,
        }
    }
}

#[derive(Debug)]
pub struct Native<R, T, D, H = NoHooks, P = crate::proxy::NoProxy>
where
    R: TcpConnect + Timer,
    T: TlsConnect,
{
    /// Installs hyper's `1xx` callback on an outgoing HTTP/1 request, or
    /// `None` where nobody asked to watch them.
    ///
    /// A `fn` pointer for the reason `share_h2` is one, and against a
    /// harder constraint. `hyper::ext::on_informational` demands
    /// `F: Fn(..) + Send + Sync + 'static` and stores it as
    /// `Arc<dyn .. + Send + Sync>` — **the third time hyper's `Send`
    /// requirement has shaped this crate**, after the sealed
    /// `Http2ClientConnExec` that ruled out `hyper/http2` in v0.2 and the
    /// `Rewind<Box<dyn Io + Send>>` inside `hyper::upgrade::Upgraded` that
    /// ruled it out for the WebSocket work. Here it collides with a
    /// documented property: a hook may hold an `Rc`
    /// (`hclient-core/tests/shape.rs`, P13).
    ///
    /// So the bound lives on [`Native::watching_1xx`] and this field
    /// demands nothing of `H` — no signature a hook with an `Rc` in it
    /// meets gains a bound, and a build that never calls that constructor
    /// is unchanged.
    watch_1xx: Option<Watch1xx<H>>,
    /// How long a request carrying `Expect: 100-continue` withholds its
    /// body — see [`Native::expect_continue`]. `None` sends it at once,
    /// which is what every build did before this existed and is legal.
    expect_continue: Option<Duration>,
    /// Which HTTP versions this transport may speak, as a runtime setting
    /// rather than a compile-time one.
    ///
    /// The `http2` feature decides whether the h2 code exists at all; this
    /// decides whether it is used. The two are separate because a caller
    /// who wants the code present and the protocol off has no other way to
    /// say so, and one who writes `http2(true)` in a build that never
    /// compiled it should get a refusal naming the feature rather than
    /// silence.
    versions: Versions,
    /// The QUIC arm, erased.
    ///
    /// See [`crate::http3::arm::Arm`] for why the box is `Send + Sync`.
    #[cfg(feature = "http3")]
    h3: Option<Arc<crate::http3::arm::Arm>>,
    /// What origins have advertised about their own HTTP/3, and for how
    /// long — RFC 7838's `Alt-Svc`, with the lifetime the origin gave.
    ///
    /// The slow tier of discovery. It is only consulted where the fast one
    /// has no answer, so an origin that publishes an HTTPS record never
    /// touches it and the fast path takes no lock.
    #[cfg(feature = "http3")]
    alt_svc: altsvc::AltSvcCache,
    /// Origins whose HTTP/3 connect has already failed, and when.
    ///
    /// The negative half, and a different fact from
    /// [`discovery::NegativeCache`] one field up: that one remembers a TCP
    /// connect through a discovered endpoint, this one remembers that QUIC
    /// did not come up at all.
    #[cfg(feature = "http3")]
    h3_failures: failures::H3Failures,
    /// How long the QUIC arm runs alone before a TCP connect is started
    /// beside it, or `None` for no hedge at all.
    ///
    /// `None` by default: a transport that opened a UDP socket and a TCP
    /// one for the same request would be deciding, on every caller's
    /// behalf, what to spend on a network that blocks UDP/443.
    #[cfg(feature = "http3")]
    hedge: Option<Duration>,
    /// How the origin is reached, when it is not reached directly.
    ///
    /// `P` is defaulted to [`NoProxy`](crate::proxy::NoProxy), which is an
    /// **empty enum** — so a transport nobody configured a proxy on holds
    /// an `Option` that cannot be `Some`, by construction rather than by
    /// discipline, and the field costs it one word. It is a type parameter
    /// rather than a `Box<dyn ..>` because erasing the protocol erases the
    /// IO with it, and that needs a `Send` this crate does not declare.
    /// The proxies this transport may use, in the order the caller wrote
    /// them; the first that serves a request wins, and an empty list is
    /// direct. A `Vec` rather than an `Option` since v0.4: a caller with
    /// separate `http` and `https` proxies is the ordinary corporate
    /// setup, and one `Option` cannot hold two.
    proxies: Vec<crate::proxy::Proxy<P>>,
    rt: R,
    tls: T,
    dns: D,
    /// Where the events go. `NoHooks` is a ZST, so this field costs a
    /// build that wants nothing exactly nothing.
    hooks: H,
    opts: TcpOpts,
    /// What goes in this client's HTTP/2 `SETTINGS` frame where it does
    /// not want `h2`'s default. Constant within a `Native` and therefore
    /// within its pool, which is why it is not in `PoolKey` — the
    /// argument the TLS identity and the proxy already carry there.
    /// Send every request over this Unix-domain socket instead of
    /// resolving and dialling the origin — see [`Native::unix_socket`].
    unix_socket: Option<Arc<std::path::Path>>,
    /// What this client accepts in an HTTP/1 response head — see
    /// [`crate::H1Opts`]. Not `#[cfg]`-ed like `h2_opts` below, because
    /// the HTTP/1 path is the one every build has.
    h1_opts: crate::http1::H1Opts,
    #[cfg(feature = "http2")]
    h2_opts: crate::http2::H2Opts,
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
    /// How to spawn an HTTP/2 connection driver, or `None` for a transport
    /// that has not been asked to share connections — which is every
    /// transport this crate builds unless [`Native::multiplexed`] was
    /// called.
    ///
    /// **A function pointer, and that is the whole mechanism.**
    /// [`hclient_rt::Spawn`] declares no bounds at all, so
    /// `<R as Spawn<F>>::spawn` is an ordinary function item and coerces
    /// to `fn(&R, F)`; a field of that type is well-formed whatever `R`
    /// is, so **no signature a `Spawn`-less runtime meets gains a bound**.
    /// The bound lives on `multiplexed()` alone, which is what lets
    /// `Transport::execute` — a trait method, whose impl cannot carry an
    /// extra bound — reach a spawner without demanding one.
    ///
    /// `None` is today's transport exactly, down to the code path: the
    /// shared arm is not entered, the pool is `take`n rather than
    /// borrowed, and no task is created.
    #[cfg(feature = "http2")]
    share_h2: Option<SpawnH2<R, T, H>>,
    /// `None` unless [`Native::h2_keep_alive`] was called, and only ever
    /// read on the `multiplexed()` path — an unshared connection has no
    /// driver to hold the clock.
    #[cfg(feature = "http2")]
    h2_keep_alive: Option<http2::H2KeepAlive>,
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
        let mut caps = Capabilities::default();
        // Asked of the TLS backend rather than left at
        // `Capabilities::default()`'s `false`, which understated: both
        // backends in this workspace can present one —
        // `hclient-tls-native-tls` through its `identity()` setter, and
        // `hclient-tls-rustls` through a `from_config` whose
        // `rustls::ClientConfig` was built with `with_client_auth_cert`.
        caps.client_certs = tls.presents_client_certs();
        // Honest: no upgrade — the
        // remaining fields stay at the conservative baseline of
        // `Capabilities::default()` (see `tests/transport.rs`'s
        // `undeclared_capability_fields_match_their_conservative_defaults_today`).
        //
        // `streaming_request_body: true`. `body.rs`'s `Inner::Streaming`
        // hands `RequestBody::Streaming` to hyper as a stream instead of
        // buffering it into memory first; measured on the wire:
        // `tests/transport.rs`'s
        // `streaming_request_body_is_actually_streamed_not_buffered` sees
        // `transfer-encoding: chunked` and separate frames for a
        // two-frame body. Claiming `false` while the code honestly
        // streams would be the same capability lie in reverse —
        // understated, but a caller who believes it will buffer a large
        // body into memory itself when it didn't have to.
        caps.streaming_request_body = true;
        // **The floor, and it is the same value with the `http2` feature on
        // as off**. `full_duplex` and `response_trailers` are
        // things HTTP/2 can do and HTTP/1.1 cannot, and they stay at
        // `Capabilities::default()`'s `false` here, because what this
        // reports is the value that holds on the WORST protocol this
        // transport might negotiate.
        //
        // **`request_trailers` is deliberately not on that list.**
        // HTTP/1.1 sends request trailers perfectly well —
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
        // features across the whole graph, so a *library* built on `hclient`
        // can never know whether some other crate turned h2 on. The floor is
        // the only answer such a library can act on.
        //
        // **The h2 path is genuinely duplex**, so this is not "the
        // implementation cannot do it": one branch,
        // `Poll::Pending => return Poll::Pending`, is what would make it
        // so, and that branch is gone.
        //
        // The `false` stands anyway, for the reason it always had:
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
        // The asymmetry with `hclient-h3`, which keeps `false`, is a real
        // difference between two transports rather than a drift between
        // two declarations: that crate sends no request trailers at all
        // and refuses with `RequestTrailersNotSent`.
        caps.request_trailers = true;
        // `Transparent`, and it is a correction rather than a change: this
        // crate has never followed a redirect. A 3xx comes back from
        // `http1::exchange`/`http2::exchange` as an ordinary response and
        // `Client`'s redirect stage does the chain — grep `src/` for
        // `Location` or a 3xx status and there is nothing to find.
        //
        // It said `Configurable` from v0.1 until v0.4 W1: "we set the
        // policy", for a crate that reads no policy and sets nothing.
        // `hclient-fetch` shipped the same wrong value and corrected it to
        // `Internal` a vertical earlier, in an audit that never came here.
        // The variant is deleted now, so this line no longer has a way to
        // be wrong in that direction.
        //
        // Not `None`, which is the stronger claim that redirects are
        // impossible and is also what `Capabilities::default()` returns for a
        // backend that said nothing at all — the same distinction
        // `hclient-h3` writes down beside its own `Transparent`.
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
        // `http1::exchange`, which hands hyper's `Connection` future to
        // `NativeBody` — and until `execute` returns, all of it lives
        // inside this one future. Dropping it drops the `Connection`, which
        // drops the socket, which closes the TCP connection; there is no
        // spawn anywhere on this path (see `h1.rs`'s module doc comment)
        // and therefore nothing left running behind the drop. Measured
        // from the far end rather than argued: `tests/cancel.rs`'s
        // `dropping_the_execute_future_closes_the_connection_the_server_sees`
        // has the server observe its socket close.
        caps.cancel_on_drop = CancelSupport::Supported;
        // Asked, not assumed. A hardcoded `TlsSupport::Full` regardless
        // of which `TlsConnect` was plugged in would make
        // `Native<R, NoTls, D>` advertise full TLS while refusing every
        // `https://` connect — a capability that lies, of exactly the kind
        // this project has caught in three other backends.
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
            // Enforced by `connect::first_address_within`, and its scope is
            // narrower than the field name suggests on purpose: it bounds
            // the wait for the **first address from either family**, not a
            // phase, because Happy Eyeballs has no instant at which
            // resolution finished. `Timeouts::resolve`'s own doc has the
            // argument; `tests/timeouts.rs` has the server that never
            // answers a query.
            resolve: true,
            // Actually enforced — see `execute`'s race between
            // `connect::connect` and `rt.sleep(d)`, below, and
            // `tests/transport.rs`'s
            // `declared_connect_timeout_is_actually_applied`.
            //
            // Scope, spelled out, because the capability alone leaves it
            // ambiguous: RFC 8305 staggers several TCP attempts, so
            // "connect" could plausibly mean a budget per attempt or one
            // budget for all of them, which are materially different
            // promises to a caller. This is the LATTER: one
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
            // `hclient::body::Deadline`'s for the same fact from the other end).
            //
            // Both are checked from outside the client, by servers that
            // send a head and then nothing and that stop mid-body:
            // `tests/timeouts.rs`.
            first_byte: true,
            between_bytes: true,
        };
        Self {
            watch_1xx: None,
            expect_continue: None,
            versions: Versions::default(),
            #[cfg(feature = "http3")]
            h3: None,
            #[cfg(feature = "http3")]
            alt_svc: altsvc::AltSvcCache::default(),
            #[cfg(feature = "http3")]
            h3_failures: failures::H3Failures::default(),
            #[cfg(feature = "http3")]
            hedge: None,
            unix_socket: None,
            h1_opts: crate::http1::H1Opts::default(),
            #[cfg(feature = "http2")]
            h2_opts: crate::http2::H2Opts::default(),
            // `P` is `NoProxy` here, an empty enum, so this is the only
            // value this field can hold on a transport built by `new` —
            // `.proxy(..)` is what changes the type.
            proxies: Vec::new(),
            epoch: rt.now(),
            rt,
            tls,
            dns,
            hooks: NoHooks,
            // **Nagle off, and asked for here rather than defaulted one
            // layer down.**
            //
            // Measured, twice. A cold TLS 1.3 exchange on loopback costs
            // **41.9 ms**, all of it in the head and 10 µs of it in the
            // body;
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
            // `TcpOpts::default()` stays all-off, because `hclient-rt` is
            // a socket seam and knows nothing about who is writing:
            // request/response over TLS is exactly the write-write-read
            // pattern Nagle punishes, and a protocol that streams one way
            // is exactly the one it helps. A default down there would
            // impose this crate's protocol on every other caller of the
            // seam.
            //
            // **And it is asked for only where the runtime says it
            // applies it**, which is the whole difference between this and
            // `TcpOpts::default().nodelay(true)` written unconditionally.
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
            // `tcp_opts(TcpOpts::default().nodelay(true))` on a `NONE`
            // backend fails at connect, naming `nodelay`.
            // `tests/tcp_opts.rs` pins both halves.
            opts: TcpOpts::default().nodelay(<R as TcpConnect>::APPLIES.nodelay),
            caps,
            pool,
            svcb_failures: discovery::NegativeCache::default(),
            // Not shared. `Native::multiplexed` is the only thing that
            // ever sets this, and it carries the `Spawn` bound that makes
            // it possible.
            #[cfg(feature = "http2")]
            share_h2: None,
            #[cfg(feature = "http2")]
            h2_keep_alive: None,
        }
    }
}

/// Everything a `Native` can be configured with, whatever its hook —
/// separated from [`Native::new`] above only because `new` is the one
/// method that names a *particular* `H` ([`NoHooks`]), and putting it in
/// this block would make `Native::<_, _, _, MyHook>::new` a thing a
/// caller could write and get a hookless transport from.
impl<R: TcpConnect + Timer, T: TlsConnect, D, H, P> Native<R, T, D, H, P> {
    /// Send this transport's events to `hooks` — see
    /// [`hclient_core::unversioned::Hooks`] for what it hears and what it
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
    /// working (P13; `crates/hclient-core/tests/shape.rs`).
    ///
    /// # It turns [`Native::multiplexed`] back off, and the type is why
    ///
    /// The spawner `multiplexed()` captures is a
    /// `fn(&R, H2Driver<_, H>)` — it **names the hook**, because the
    /// driver carries it — so a method whose whole purpose is to change
    /// `H` cannot carry that pointer across. Write `.hooks(..)` first and
    /// `.multiplexed()` last; the other order compiles and shares no
    /// connections, which is the same cost, stated the same way, as
    /// [`Native::tcp_opts`] replacing the whole option set. Pinned by
    /// `tests/http2_multiplex.rs`'s pair of orders, so the rule is
    /// measured rather than only written here.
    /// Reach every origin through `proxy` instead of directly.
    ///
    /// Changes `P`, the way [`Native::hooks`] changes `H`, because the
    /// protocol is a type: see `proxy`'s module doc for why it is not a
    /// `Box<dyn ..>`. There is no per-origin routing and no `NO_PROXY`
    /// here — one proxy for everything this transport sends, which is
    /// stated rather than implied because the absence is the surprising
    /// half.
    ///
    /// What this changes beyond which socket is opened, each of which is
    /// somewhere else in this crate agreeing:
    ///
    /// - **The resolver is not consulted for the origin.** The proxy
    ///   resolves it — an HTTP proxy from the `CONNECT` target, SOCKS5
    ///   from `ATYP=0x03 DOMAINNAME`. Happy Eyeballs still runs, over the
    ///   *proxy's* addresses.
    /// - **No HTTPS/SVCB discovery for the origin**, which answers
    ///   [`Discovered::NotConsulted`](crate::Discovered): address hints
    ///   for an address nobody will dial, and a record port we would not
    ///   honour, are worse than no answer.
    /// - **The pool key names the proxy**, so a tunnel is never reused
    ///   through a different one.
    /// - **A `407` is a connect error, never a response.** It is the
    ///   proxy's answer to us, not the origin's to the caller.
    ///
    /// # Panics
    ///
    /// If a Unix socket is already configured. The mirror of
    /// [`unix_socket`](Self::unix_socket)'s refusal, and a panic rather
    /// than a `Result` because this method already changes `P` and cannot
    /// return `Result<Native<.., P2>, Error>` without making every caller
    /// who never touches Unix sockets write a `?`. The two orders are not
    /// symmetrical and the asymmetry is stated rather than hidden: put
    /// `.proxy(..)` first and `unix_socket` refuses politely.
    pub fn proxy<P2>(self, proxy: crate::proxy::Proxy<P2>) -> Native<R, T, D, H, P2> {
        self.with_proxies(vec![proxy])
    }

    /// A whole list at once — [`proxy`](Self::proxy)'s plural, and the
    /// one place the `P`-changing struct literal is written.
    ///
    /// **Including an empty list**, which is why it takes a `Vec` rather
    /// than a first-plus-rest: a machine with no proxy still has to come
    /// out of [`system_proxies_from`](Self::system_proxies_from) with the
    /// `P` its return type promises, and there is no `Proxy` to pass
    /// through the singular method to get there. An empty list proxies
    /// nothing and claims nothing — `capabilities().proxy` reads the
    /// list, not the fact that this was called.
    ///
    /// It is also the strict system path in one line, for a caller who
    /// wants the refusal rather than the degradation
    /// [`Client::new`](https://docs.rs/hclient) takes:
    ///
    /// ```no_run
    /// # #[cfg(feature = "system-proxy")]
    /// # fn f<R: hclient_rt::TcpConnect + hclient_rt::Timer, T: hclient_tls::TlsConnect, D>(
    /// #     t: hclient_native::Native<R, T, D>,
    /// # ) -> Result<(), Box<dyn std::error::Error>> {
    /// use hclient_native::proxy::system::{SystemProxies, http_proxies};
    ///
    /// let t = t.with_proxies(http_proxies(&SystemProxies::detect())?);
    /// # let _ = t; Ok(()) }
    /// ```
    ///
    /// # Panics
    ///
    /// If a Unix socket is configured and the list is non-empty, for
    /// [`proxy`](Self::proxy)'s reason and by way of it. An **empty** list
    /// does not panic: it configures no proxy, so the two settings that
    /// cannot coexist are not both present.
    pub fn with_proxies<P2>(self, proxies: Vec<crate::proxy::Proxy<P2>>) -> Native<R, T, D, H, P2> {
        assert!(
            self.unix_socket.is_none() || proxies.is_empty(),
            "a proxy and a Unix socket both answer `where does this connection go`; \
             configure at most one"
        );
        let mut caps = self.caps;
        // Read from the thing that knows, `client_certs`' lesson one field
        // over: this transport applies a proxy configuration of its own,
        // which is exactly what the field says. Mutated rather than
        // written as a literal because `Capabilities` is
        // `#[non_exhaustive]` — a field added later must arrive here as
        // whatever it already was, not as a compile error somebody
        // silences by copying its neighbour.
        // Read off the list rather than written as `true`: the empty
        // list is reachable now (a machine with no proxy configured),
        // and a transport reporting `proxy` while proxying nothing would
        // be a capability that lies — the defect `Native::hooks` was
        // found to have one field over.
        caps.proxy = !proxies.is_empty();
        Native {
            // Carried: the installer's type names `H`, which this method
            // does not change.
            watch_1xx: self.watch_1xx,
            expect_continue: self.expect_continue,
            versions: self.versions,
            #[cfg(feature = "http3")]
            h3: self.h3.clone(),
            #[cfg(feature = "http3")]
            alt_svc: self.alt_svc.clone(),
            #[cfg(feature = "http3")]
            h3_failures: self.h3_failures.clone(),
            #[cfg(feature = "http3")]
            hedge: self.hedge,
            unix_socket: self.unix_socket.clone(),
            h1_opts: self.h1_opts,
            #[cfg(feature = "http2")]
            h2_opts: self.h2_opts,
            proxies,
            rt: self.rt,
            tls: self.tls,
            dns: self.dns,
            hooks: self.hooks,
            opts: self.opts,
            caps,
            epoch: self.epoch,
            pool: self.pool,
            svcb_failures: self.svcb_failures,
            #[cfg(feature = "http2")]
            share_h2: self.share_h2,
            #[cfg(feature = "http2")]
            h2_keep_alive: self.h2_keep_alive,
        }
    }

    /// Reach every origin through the proxies **the machine itself is
    /// configured with**.
    ///
    /// ```no_run
    /// # use hclient_native::Native;
    /// # use hclient_rt::{TcpConnect, Timer};
    /// # use hclient_tls::TlsConnect;
    /// # fn f<R: TcpConnect + Timer, T: TlsConnect, D>(t: Native<R, T, D>)
    /// # -> Result<(), Box<dyn std::error::Error>> {
    /// let t = t.system_proxy()?;
    /// # Ok(()) }
    /// ```
    ///
    /// `HTTP_PROXY` and `HTTPS_PROXY` where the environment names them,
    /// and otherwise the platform's own settings — the registry through
    /// `WinHttpGetIEProxyConfigForCurrentUser` on Windows, the dynamic
    /// store on macOS. `NO_PROXY`, `ProxyOverride` and macOS's exceptions
    /// list all become this proxy's own
    /// [`bypass`](crate::proxy::Proxy::bypass) list, and the `<local>`
    /// rule becomes [`bypass_local`](crate::proxy::Proxy::bypass_local).
    ///
    /// A machine with nothing configured is not an error: the list is
    /// empty, nothing is proxied, and `capabilities().proxy` stays
    /// `false` — which is the same answer this transport gives when
    /// nobody called this at all, because it is the same fact.
    ///
    /// # Why it is a call rather than a default
    ///
    /// Reading the environment is policy — *which* variables, and whether
    /// a library may look at all — and this workspace's rule is that
    /// policy belongs to whoever builds the transport. This method is
    /// that person saying yes. `Native::new` reads nothing, and a build
    /// that does not name the `system-proxy` feature does not link a
    /// reader at all.
    ///
    /// # Errors
    ///
    /// It **refuses rather than narrowing**, and the refusal names what
    /// it could not honour: a SOCKS proxy when this call installs HTTP
    /// ones (`Native` holds one proxy protocol — see
    /// [`SystemProxyRefused`](crate::proxy::system::SystemProxyRefused)), or a
    /// bypass pattern in a shape this client's matcher cannot state
    /// exactly, such as a subnet.
    ///
    /// Both alternatives are worse in the same way: quietly dropping a
    /// proxy sends traffic direct that the machine's owner routed through
    /// one, and quietly dropping a bypass sends traffic through a proxy
    /// they excluded. Neither is visible from the call site, and both
    /// change where the bytes go. A caller who wants to decide for
    /// themselves reads
    /// [`SystemProxies`](crate::proxy::system::SystemProxies) and builds
    /// the proxies by hand — this method is the convenience, not the only
    /// road.
    ///
    /// # Panics
    ///
    /// If a Unix socket is already configured, for
    /// [`proxy`](Self::proxy)'s reason and by way of it.
    #[cfg(feature = "system-proxy")]
    pub fn system_proxy(
        self,
    ) -> Result<Native<R, T, D, H, crate::proxy::HttpConnect>, hclient_core::Error> {
        self.system_proxies_from(&crate::proxy::system::SystemProxies::detect())
    }

    /// [`system_proxies_from`](Self::system_proxies_from) for a caller
    /// that **must not fail**, handing back what it could not install.
    ///
    /// `Client::new` is that caller: it reads the machine's settings so
    /// that a client is a good citizen by default, and it did not *ask*
    /// for them the way somebody calling [`system_proxy`](Self::system_proxy)
    /// did. A refusal there would be a client that will not construct on
    /// a network with WPAD, or on a machine whose owner also configured a
    /// SOCKS proxy — a worse answer than proxying what we can.
    ///
    /// What each degradation costs is argued where it happens, in
    /// `hclient_proxy::system::http_proxies_lossy`. Nothing is silent at
    /// this level: the second half of the pair is the report.
    #[cfg(feature = "system-proxy")]
    pub fn system_proxies_from_lossy(
        self,
        sys: &crate::proxy::system::SystemProxies,
    ) -> (
        Native<R, T, D, H, crate::proxy::HttpConnect>,
        Vec<crate::proxy::system::SystemProxyRefused>,
    ) {
        let (proxies, dropped) = crate::proxy::system::http_proxies_lossy(sys);
        (self.with_proxies(proxies), dropped)
    }

    /// [`system_proxy`](Self::system_proxy) over settings already read.
    ///
    /// Separate so that every rule in the translation is testable without
    /// a machine that has such settings — which is every machine this
    /// workspace is developed on — and so that a caller who reads
    /// [`SystemProxies`](crate::proxy::system::SystemProxies) once can
    /// hand it to several transports rather than asking the OS again for
    /// each.
    #[cfg(feature = "system-proxy")]
    pub fn system_proxies_from(
        self,
        sys: &crate::proxy::system::SystemProxies,
    ) -> Result<Native<R, T, D, H, crate::proxy::HttpConnect>, hclient_core::Error> {
        let proxies = crate::proxy::system::http_proxies(sys)
            .map_err(|e| hclient_core::Error::new(hclient_core::ErrorKind::Unsupported, e))?;
        Ok(self.with_proxies(proxies))
    }

    /// The proxy this transport would use for one request, or `None` for
    /// direct — `Proxy::choose` over the list this transport is holding.
    ///
    /// `pub(crate)` and reached from `testing::chosen_proxy`: routing is
    /// not a caller's question, but it is the only question a test of the
    /// routing has.
    #[cfg(feature = "proxy")]
    pub(crate) fn chosen_proxy(
        &self,
        use_tls: bool,
        host: &str,
        port: u16,
    ) -> Option<&crate::proxy::Proxy<P>> {
        crate::proxy::Proxy::choose(&self.proxies, use_tls, host, port)
    }

    /// A second proxy, and a third — **the first that serves a request
    /// wins**.
    ///
    /// The case this exists for is the ordinary corporate one, an
    /// `HTTP_PROXY` and an `HTTPS_PROXY` at different hosts:
    ///
    /// ```no_run
    /// # use hclient_native::{Native, Proxy, ProxyScheme, HttpConnect};
    /// # use hclient_rt::{TcpConnect, Timer};
    /// # use hclient_tls::TlsConnect;
    /// # fn f<R: TcpConnect + Timer, T: TlsConnect, D>(t: Native<R, T, D>)
    /// # -> Native<R, T, D, hclient_core::unversioned::NoHooks, HttpConnect> {
    /// t.proxy(Proxy::new(HttpConnect::new(), "secure-proxy.corp", 8443)
    ///         .only_for(ProxyScheme::Https))
    ///  .and_proxy(Proxy::new(HttpConnect::new(), "proxy.corp", 8080))
    /// # }
    /// ```
    ///
    /// **It does not change `P`**, unlike [`proxy`](Self::proxy), and that
    /// is the limit rather than an oversight: `Native` has one proxy
    /// protocol, so every proxy on one transport speaks the same one. A
    /// caller wanting SOCKS5 for `https` and an HTTP proxy for `http`
    /// cannot say so here — lifting that would mean erasing `P`, and
    /// erasing `P` erases the IO with it, which is the objection
    /// `crate::proxy`'s module doc records against `Box<dyn
    /// ProxyProtocol>`.
    ///
    /// Uncallable before `proxy` for free rather than by a check: without
    /// it `P` is [`NoProxy`], an empty enum, so there is
    /// no `Proxy<NoProxy>` to pass.
    #[must_use]
    pub fn and_proxy(mut self, proxy: crate::proxy::Proxy<P>) -> Self {
        self.proxies.push(proxy);
        self
    }

    /// Report `1xx` responses (`100 Continue`, `103 Early Hints`) to this
    /// transport's hooks, and say so in
    /// [`Capabilities::informational_1xx`].
    ///
    /// # Why this is an opt-in rather than always on
    ///
    /// Not cost: hyper's own callback fires only when a `1xx` arrives.
    /// **The bound.** `hyper::ext::on_informational` takes
    /// `F: Fn(..) + Send + Sync + 'static`, so a hook that reaches it must
    /// be `Send + Sync + 'static` too — and this crate documents the
    /// opposite as supported: a hook may hold an `Rc`
    /// (`hclient-core/tests/shape.rs`, P13), which is the whole reason the
    /// seam declares no auto traits. Putting the bound here rather than on
    /// `H` is [`Native::multiplexed`]'s shape: the field is a `fn`
    /// pointer, so **no signature a single-threaded hook meets changes**,
    /// and a runtime that never calls this constructor is untouched. A
    /// hook holding an `Rc` gets `E0277` on the line where it asked.
    ///
    /// # What the two protocols cost, which is not the same thing
    ///
    /// HTTP/1 goes through hyper's callback, hence the bound. HTTP/2 needs
    /// no bound at all — `h2::client::ResponseFuture::poll_informational`
    /// is a poll, driven by the same future that awaits the response — and
    /// it is switched on by the same call anyway. The capability reports
    /// the **floor**, the rule v0.2 W3 set for `full_duplex`: one switch
    /// for both, because a `true` that held on h2 alone would be a claim
    /// an HTTP/1 connection could not keep.
    ///
    /// # Ordering
    ///
    /// `.hooks(..)` must come **before** this, for the reason it must come
    /// before [`Native::multiplexed`]: the pointer's type names `H`. The
    /// other order compiles and watches nothing.
    /// Withhold a request body carrying `Expect: 100-continue` until the
    /// server answers `100`, or until `after` has passed.
    ///
    /// # What it buys, and why it is not the default
    ///
    /// A body that will be rejected costs the same to upload as one that
    /// will be accepted, and the two things that reject one before reading
    /// it are a proxy answering `407` and an origin answering `401` or
    /// `413`. This asks first.
    ///
    /// **A default that waited would be a default that hangs.** A server
    /// that ignores `Expect` — legal for HTTP/1.0, and true of some
    /// proxies — sends no `100`, so every such upload would be held for
    /// the whole of `after` on a request nobody asked to change. Without
    /// this call the header goes out and the body follows immediately,
    /// which is what this crate has always done and is what RFC 9110
    /// §10.1.1 permits: a client must only not wait *indefinitely*.
    ///
    /// # Why not a `Timeouts` field
    ///
    /// `Timeouts::first_byte` bounds a wait that ends in **failure**;
    /// this bounds one that ends in **proceeding anyway**. Same clock,
    /// opposite outcome, so folding them would make one of the two
    /// silently wrong.
    ///
    /// # HTTP/1 only
    ///
    /// The gate is a request body hyper pulls; on HTTP/2 the body is a
    /// `SendStream` this crate drives itself and there is nothing to
    /// withhold in the same sense.
    pub fn expect_continue(mut self, after: Duration) -> Self {
        self.expect_continue = Some(after);
        self
    }

    pub fn watching_1xx(mut self) -> Self
    where
        H: Hooks + Clone + Send + Sync + 'static, // send-bound-exception: amendment-C2
    {
        self.watch_1xx = Some(install_1xx::<H>);
        self.caps.informational_1xx = true;
        self
    }

    pub fn hooks<H2>(self, hooks: H2) -> Native<R, T, D, H2, P> {
        // The capability must go with the pointer. Dropping one and
        // carrying the other left this transport saying it reports `1xx`
        // while nothing did — a capability lying, which is worse than the
        // silent downgrade it accompanies, because a caller can act on a
        // capability. Found by the pair of orders in
        // `tests/informational.rs`, not by reading.
        let mut caps = self.caps;
        caps.informational_1xx = false;
        Native {
            // Dropped for the same reason as `share_h2` below and by the
            // same rule: the installer's type names `H`, and this method's
            // whole purpose is to change it. `.hooks(..)` first, then
            // `.watching_1xx()`. The other order compiles and watches
            // nothing; the compiler does not catch it and this comment
            // must not say it does.
            watch_1xx: None,
            expect_continue: self.expect_continue,
            versions: self.versions,
            #[cfg(feature = "http3")]
            h3: self.h3.clone(),
            #[cfg(feature = "http3")]
            alt_svc: self.alt_svc.clone(),
            #[cfg(feature = "http3")]
            h3_failures: self.h3_failures.clone(),
            #[cfg(feature = "http3")]
            hedge: self.hedge,
            unix_socket: self.unix_socket.clone(),
            h1_opts: self.h1_opts,
            #[cfg(feature = "http2")]
            h2_opts: self.h2_opts,
            // Carried, unlike the two above: the proxy is named by
            // neither `H` nor the driver.
            proxies: self.proxies,
            rt: self.rt,
            tls: self.tls,
            dns: self.dns,
            hooks,
            opts: self.opts,
            caps,
            epoch: self.epoch,
            pool: self.pool,
            svcb_failures: self.svcb_failures,
            // Dropped rather than carried, and the type is why: the
            // spawner names `H`, and this method's whole purpose is to
            // change `H`. A caller who wants both writes `.hooks(..)`
            // first — the order that makes the driver carry the hook it
            // is supposed to report through. The other order **compiles**
            // and silently shares nothing; the compiler does not catch it
            // and this comment must not say it does. What catches it is
            // `tests/http2_multiplex.rs`'s pair of orders, and the doc
            // above says the cost out loud.
            #[cfg(feature = "http2")]
            share_h2: None,
            // Carried, unlike `share_h2` above: the setting names no type
            // parameter, so changing `H` cannot invalidate it.
            #[cfg(feature = "http2")]
            h2_keep_alive: self.h2_keep_alive,
        }
    }

    /// How this transport reuses connections — see [`PoolConfig`], and
    /// `crate::pool`'s module doc for what reuse can and cannot promise
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
    /// spawns a [`Reaper`] on `R`. Read `crate::pool`'s module doc first:
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
    /// It is *not* "a pool driven by a spawned task does not compile on
    /// this seam, because `Spawn<F>` requires `F: Send + 'static` and this
    /// crate's IO is not `Send`". Measured: `Spawn<F>` declares no bounds
    /// whatsoever, and the pool is an `Arc<..<Mutex<..>>>`, so a reaper
    /// over it is `Send` whenever the connection is. See `pool.rs`'s module
    /// doc for how that mistake is made, which is the reusable part of it.
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
    /// `hclient_rt_tokio::TokioHandle` for the way round that.
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

    /// **Share one HTTP/2 connection between concurrent requests**, by
    /// spawning its driver on `R` (v0.4).
    ///
    /// Without this, an h2 connection is checked out of the pool
    /// exclusively and two concurrent requests to one origin cost two
    /// connections and two handshakes — measured at 8× the sockets and 8×
    /// the TLS handshakes for a concurrency of 8, and the largest
    /// remaining gap between this client and a gRPC one. With it, they
    /// cost one.
    ///
    /// ```
    /// # use hclient_native::Native;
    /// # use hclient_rt_tokio::Tokio;
    /// let transport = Native::new(Tokio, hclient_tls::NoTls, hclient_dns::IpLiteralOnly)
    ///     .multiplexed();
    /// ```
    ///
    /// # Why this is not what [`Native::new`] does
    ///
    /// [`Native::with_reaper`]'s reason exactly, and it is the rule this
    /// crate is built on: `Native` is generic over `R`, and **not every
    /// `R` has a `Spawn` impl at all** — `connect.rs`'s own `FakeRt` has
    /// none, W7's embassy backend expects none, and
    /// `hclient/tests/two_runtimes.rs` runs this transport on a bare
    /// `futures_executor::block_on`. A default that spawned would be a
    /// default stronger than the truth.
    ///
    /// The bound is therefore on this method and on nothing else. What
    /// makes that possible is that [`hclient_rt::Spawn`] declares **no
    /// bounds**, so `<R as Spawn<F>>::spawn` coerces to `fn(&R, F)` and
    /// can be stored in a field that demands nothing of `R` — see
    /// [`Native`]'s `share_h2`. `Transport::execute` is a trait method and
    /// cannot carry an extra bound; it does not need one.
    ///
    /// # Three prices, none of them hidden
    ///
    /// **1. A spawner nobody drives hangs requests, where a reaper's would
    /// only leak sockets.** `Spawn::spawn` returns `()`: an executor that
    /// is never run accepts the driver, drops it, and has no way to say
    /// so. `pool.rs`'s module doc already names this as the thing no bound
    /// can catch — and here it is worse than there. Measured: a driver
    /// **dropped** fails its requests with a broken pipe, a driver **held and never polled**
    /// leaves them with no verdict at all. `Timeouts::first_byte` is the
    /// only bound that cuts it and it is not a default. **A shared
    /// connection is at most as good as the executor under it.**
    ///
    /// **2. Beyond the peer's `MAX_CONCURRENT_STREAMS`, requests queue.**
    /// h2 accepts them and opens the streams as capacity frees up:
    /// measured at a server limit of 2, six concurrent calls finished at
    /// 203/203/405/405/607/607 ms on one connection, where today the fifth
    /// and sixth would open sockets of their own and finish in ~200 ms. No
    /// second connection is opened at any concurrency, and that is a
    /// decision rather than an omission — see "When a shared connection is
    /// full" below.
    ///
    /// **3. A hook holding an `Rc` cannot multiplex.** The driver carries
    /// `H` so that a shared connection's `Closed` has an emitter at all,
    /// and `Tokio`'s `Spawn` impl wants `Send + 'static`; the seam's
    /// `!Send` allowance therefore meets `Spawn`'s bound here. It is a
    /// **compile error at this call**, naming the missing bound, and it
    /// costs such a build nothing but the multiplexing: the transport it
    /// had goes on working. `hclient-h3` met the same collision from the
    /// other side and could not close it — `CloseReason::Ended` has no
    /// emitter there — because its bound is on the transport rather than
    /// on an opt-in.
    ///
    /// # Reuse has to be on
    ///
    /// There is nowhere to share a connection without a pool, so this has
    /// no effect on a transport built with [`Native::without_pool`] —
    /// whichever order the two are written in, because the shared path is
    /// entered only where a pool exists. That is
    /// [`Native::with_reaper`]'s third bullet, one seam over. `.hooks(..)`
    /// is the other ordering rule and is a stronger one: it turns this
    /// back off, and [`Native::hooks`] says why.
    ///
    /// # When a shared connection is full
    ///
    /// **Nothing: it queues, and no second connection is opened.** The
    /// alternative needs a number nobody here can choose honestly.
    /// `SendRequest::poll_ready` is a **liveness** check and not a
    /// capacity one — it answers from a connection error, the next stream
    /// id and this clone's own pending stream, never from the peer's
    /// `MAX_CONCURRENT_STREAMS` — so a second-connection policy would have
    /// to count live streams in our own code, and the threshold depends on
    /// the peer's limit (which h2 will not report to us) and on the
    /// handshake cost, which is a network property and not a loopback one.
    /// A number measured on loopback would be a number about this
    /// machine. `tests/http2_multiplex.rs` measures what queueing costs so
    /// that the decision is a reading rather than a hope.
    ///
    /// # The two refusals, each beside the control that differs in one token
    ///
    /// A `compile_fail` doctest passes for **any** compile error, including
    /// a typo, so the pairing below is the discipline: each refusal sits
    /// next to a version of itself that differs in one thing and compiles.
    /// The messages were also read once, by building the same two calls as
    /// an ordinary test — `` `the trait bound `NoSpawn:
    /// Spawn<H2Driver<Conn<TokioIo, NoStream>, NoHooks>>` is not
    /// satisfied` `` and `` `Rc<Cell<usize>>` cannot be sent between
    /// threads safely `` — both
    /// pointing at the `multiplexed()` call and at this method's bound.
    /// (`compile_fail,E0277` would say so in the fence, but rustdoc's
    /// error-code annotation is unstable and is **not** enforced on
    /// stable — measured: the same block passes annotated `E0432`.)
    ///
    /// **A runtime with no `Spawn` impl at all** builds this transport and
    /// runs requests on it — that is the property
    /// `hclient/tests/two_runtimes.rs` and `tests/h1.rs`'s
    /// `works_on_a_bare_futures_executor_with_no_spawn` exist to hold — and
    /// is refused **here**, at the line where the caller asked for
    /// something it cannot do:
    ///
    /// ```compile_fail
    /// use hclient_native::Native;
    /// use hclient_rt::{TcpConnect, TcpOpts, TcpOptsSupport, Timer};
    /// use hclient_rt_tokio::Tokio;
    /// use std::{future::Future, net::SocketAddr, time::Duration};
    ///
    /// #[derive(Clone)]
    /// struct NoSpawn;
    /// impl Timer for NoSpawn {
    ///     type Instant = <Tokio as Timer>::Instant;
    ///     type Sleep = <Tokio as Timer>::Sleep;
    ///     fn sleep(&self, d: Duration) -> Self::Sleep { Tokio.sleep(d) }
    ///     fn now(&self) -> Self::Instant { Timer::now(&Tokio) }
    ///     fn elapsed_since(&self, e: Self::Instant) -> Duration { Tokio.elapsed_since(e) }
    /// }
    /// impl TcpConnect for NoSpawn {
    ///     type Stream = <Tokio as TcpConnect>::Stream;
    ///     const APPLIES: TcpOptsSupport = <Tokio as TcpConnect>::APPLIES;
    ///     type Connecting<'a> = <Tokio as TcpConnect>::Connecting<'a>;
    ///     type ConnectingUnix<'a> = <Tokio as TcpConnect>::ConnectingUnix<'a>;
    ///     fn connect<'a>(&'a self, a: SocketAddr, o: &TcpOpts) -> Self::Connecting<'a> {
    ///         Tokio.connect(a, o)
    ///     }
    ///     fn connect_unix<'a>(&'a self, p: &std::path::Path) -> Self::ConnectingUnix<'a> {
    ///         Tokio.connect_unix(p)
    ///     }
    /// }
    ///
    /// // Builds, and would run: no bound anywhere on this line.
    /// let plain = Native::new(NoSpawn, hclient_tls::NoTls, hclient_dns::IpLiteralOnly);
    /// // Does not build: `NoSpawn: Spawn<H2Driver<..>>` is not satisfied.
    /// let shared = plain.multiplexed();
    /// ```
    ///
    /// The control is the same file with a `Spawn` impl added and nothing
    /// else changed:
    ///
    /// ```
    /// use hclient_native::Native;
    /// use hclient_rt::{Spawn, TcpConnect, TcpOpts, TcpOptsSupport, Timer};
    /// use hclient_rt_tokio::Tokio;
    /// use std::{future::Future, net::SocketAddr, time::Duration};
    ///
    /// #[derive(Clone)]
    /// struct CanSpawn;
    /// impl Timer for CanSpawn {
    ///     type Instant = <Tokio as Timer>::Instant;
    ///     type Sleep = <Tokio as Timer>::Sleep;
    ///     fn sleep(&self, d: Duration) -> Self::Sleep { Tokio.sleep(d) }
    ///     fn now(&self) -> Self::Instant { Timer::now(&Tokio) }
    ///     fn elapsed_since(&self, e: Self::Instant) -> Duration { Tokio.elapsed_since(e) }
    /// }
    /// impl TcpConnect for CanSpawn {
    ///     type Stream = <Tokio as TcpConnect>::Stream;
    ///     const APPLIES: TcpOptsSupport = <Tokio as TcpConnect>::APPLIES;
    ///     type Connecting<'a> = <Tokio as TcpConnect>::Connecting<'a>;
    ///     type ConnectingUnix<'a> = <Tokio as TcpConnect>::ConnectingUnix<'a>;
    ///     fn connect<'a>(&'a self, a: SocketAddr, o: &TcpOpts) -> Self::Connecting<'a> {
    ///         Tokio.connect(a, o)
    ///     }
    ///     fn connect_unix<'a>(&'a self, p: &std::path::Path) -> Self::ConnectingUnix<'a> {
    ///         Tokio.connect_unix(p)
    ///     }
    /// }
    /// impl<F: Future<Output = ()> + Send + 'static> Spawn<F> for CanSpawn {
    ///     fn spawn(&self, f: F) { Tokio.spawn(f) }
    /// }
    ///
    /// let shared = Native::new(CanSpawn, hclient_tls::NoTls, hclient_dns::IpLiteralOnly)
    ///     .multiplexed();
    /// ```
    ///
    /// **A hook holding an `Rc`** is the third price, and it is refused in
    /// the same place. The `!Send` allowance on the hook seam is real —
    /// `hclient-fetch`'s hook is an `Rc<RefCell<..>>` — and the driver
    /// carries the hook, so a spawner that wants `Send` meets it here:
    ///
    /// ```compile_fail
    /// use hclient_core::unversioned::{Event, Hooks};
    /// use hclient_native::Native;
    /// use hclient_rt_tokio::Tokio;
    /// use std::{cell::Cell, rc::Rc};
    ///
    /// #[derive(Clone)]
    /// struct Counting(Rc<Cell<usize>>);
    /// impl Hooks for Counting {
    ///     fn on(&self, _e: &Event<'_>) { self.0.set(self.0.get() + 1) }
    /// }
    ///
    /// // Builds, and works: this transport reports events and is `!Send`.
    /// let watched = Native::new(Tokio, hclient_tls::NoTls, hclient_dns::IpLiteralOnly)
    ///     .hooks(Counting(Rc::new(Cell::new(0))));
    /// // Does not build: `Rc<Cell<usize>>` cannot be sent between threads.
    /// let shared = watched.multiplexed();
    /// ```
    ///
    /// The control is the same hook behind an `Arc`:
    ///
    /// ```
    /// use hclient_core::unversioned::{Event, Hooks};
    /// use hclient_native::Native;
    /// use hclient_rt_tokio::Tokio;
    /// use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
    ///
    /// #[derive(Clone)]
    /// struct Counting(Arc<AtomicUsize>);
    /// impl Hooks for Counting {
    ///     fn on(&self, _e: &Event<'_>) { self.0.fetch_add(1, Ordering::Relaxed); }
    /// }
    ///
    /// let shared = Native::new(Tokio, hclient_tls::NoTls, hclient_dns::IpLiteralOnly)
    ///     .hooks(Counting(Arc::new(AtomicUsize::new(0))))
    ///     .multiplexed();
    /// ```
    ///
    /// # What it does not change
    ///
    /// `Capabilities` are untouched, and that is checked rather than
    /// asserted: `ReuseSupport::Supported` means *"a second request to an
    /// origin need not pay for a TCP and TLS handshake again"*, which
    /// stays exactly true, and `CancelSupport::Supported` is a duty owed
    /// on a dropped future, which goes on being owed — what changes is
    /// that the peer now sees a `RST_STREAM(CANCEL)` where it used to see
    /// the socket close. `full_duplex` and `response_trailers` still
    /// Elapsed time on this transport's own clock, measured from its
    /// construction.
    ///
    /// `hclient_rt::Timer` rather than `std::time::Instant::now()`, for
    /// the reason every other clock reading here is: `Timer` is the one
    /// seam through which time reaches a transport, so a caller testing
    /// under `tokio::time::pause()` sees what this transport sees.
    #[cfg(feature = "http3")]
    fn now(&self) -> Duration {
        self.rt.elapsed_since(self.epoch)
    }

    /// The caller has seen a network configuration change: forget every
    /// `Alt-Svc` advertisement that did not carry `persist=1`.
    ///
    /// **This exists because the transport cannot see the event itself,
    /// and saying so at the type is the honest alternative to pretending
    /// the cache is safe.** RFC 7838 §2.2 asks a client to clear
    /// non-persistent alternatives on a network change *"when information
    /// about network state is available"* — and to a `Transport` it is
    /// not: nothing in `hclient-rt` reports an interface coming up, a
    /// default route changing or a VPN connecting. An application usually
    /// can see it, so the duty is handed to the caller in the one shape
    /// that lets them discharge it.
    ///
    /// Until it is called, every entry behaves as though it carried
    /// `persist=1`. That is the unsafe direction — a laptop that moved
    /// networks is advertising an alt-authority that was reachable
    /// somewhere else — and it is bounded by two things rather than
    /// argued away: nothing is written to disk, and the cache is a field
    /// of this transport, so dropping the client does the whole job.
    ///
    /// **Both memories are cleared, and not by the same rule** — see
    /// [`H3Failures`]. The advertisement cache keeps `persist=1` entries,
    /// because that flag is the origin's own claim that what it advertised
    /// is a property of the origin rather than of the path. The failure
    /// memory keeps nothing: *"UDP/443 did not get through"* is a fact
    /// about the network alone, no peer ever asked us to carry it, and it
    /// is exactly the entry a network change makes certainly wrong.
    #[cfg(feature = "http3")]
    pub fn network_changed(&self) {
        self.alt_svc.network_changed();
        self.h3_failures.network_changed();
    }

    /// Hedge every request this transport sends to QUIC with a TCP connect
    /// started `head_start` later — the race, and it is off until this is
    /// called.
    ///
    /// **It is a hedge and not a chooser**.
    /// The record and the advertisement decide which stack an origin
    /// speaks; this decides nothing, and only stops a request waiting out
    /// quinn's 30 s `max_idle_timeout` at an origin whose UDP does not get
    /// through. A request the two tiers sent to TCP is not raced, and
    /// neither is a `RequireVersion(HTTP_3)` demand — a TCP connection
    /// opened for a request that can never be sent over TCP is a
    /// connection opened for nothing.
    ///
    /// # What the head start is, and what to pass
    ///
    /// [`DEFAULT_HEAD_START`] where there is no reason to pass anything
    /// else: 250 ms, RFC 8305 §5's Connection Attempt Delay, which is
    /// already this workspace's answer to the same question one layer
    /// down. the hedge has what the staged connect changed about that
    /// number's justification — and about its floor, which is now
    /// [`Duration::ZERO`]: with no head start both stacks connect at once,
    /// **and the losing arm still sends nothing**, which is the property
    /// that made this worth building.
    ///
    /// Bigger is not free and smaller is not unsafe. A head start below
    /// one QUIC handshake costs a TCP connect and TLS handshake the
    /// request will probably not use — checked into the pool warm rather
    /// than thrown away. A head start above it is paid, in full, by the
    /// first request to an origin whose HTTP/3 cannot be reached, and by
    /// no other one for [`H3_FAILURE_TTL`] afterwards, because a QUIC arm
    /// that loses the race teaches [`H3Failures`].
    ///
    /// # And it is spent inside `Timeouts::connect`, not beside it
    ///
    /// The QUIC arm carries the caller's whole `connect` bound and the
    /// hedge carries what is left after the head start, so the pair costs
    /// one bound rather than two. A caller whose bound has no room for two
    /// connects in it — `head_start` at or above `Timeouts::connect` —
    /// gets the sequential fallback instead, unchanged, rather than a
    /// refusal or a doubled bound.
    #[must_use]
    #[cfg(feature = "http3")]
    pub fn hedging(mut self, head_start: Duration) -> Self {
        self.hedge = Some(head_start);
        self
    }

    /// Give this transport a QUIC arm, and let it serve HTTP/3.
    ///
    /// # Every bound in this crate's QUIC story is on this one signature
    ///
    /// `H3` needs `R: UdpBind + UdpAdoptStd + Spawn<QuinnTask> + Send +
    /// Sync + 'static` and `T: QuicTlsConnect`. None of that reaches
    /// [`Native`]'s own declaration or its `impl Transport`, because the
    /// arm is stored erased — so a runtime that has no UDP and a TLS
    /// backend that has no QUIC are unaffected by the `http3` feature
    /// being switched on somewhere else in the graph. They simply never
    /// write this call. `Native::new(Embassy, NoTls, IpLiteralOnly)`
    /// compiles with the feature on, which is asserted by the workspace
    /// building `--all-features --all-targets`.
    ///
    /// The shape is [`Native::multiplexed`]'s: a bound that belongs to one
    /// opt-in lives on the opt-in.
    ///
    /// # It takes an `H3`, it does not build one
    ///
    /// Building one here would mean choosing its resolver and its TLS
    /// backend, which are this transport's `D` and `T` — and `H3::new` is
    /// fallible in a way `Native::new` is not. A caller who wants both
    /// stacks over one configuration passes the same values twice, which
    /// is what `Rustls: Clone` is for.
    #[cfg(feature = "http3")]
    pub fn http3(mut self, quic: crate::http3::H3<R, T, D>) -> Result<Self, Box<caps::Disagreement>>
    where
        crate::http3::H3<R, T, D>: crate::http3::StagedConnect<Error = Error> + Debug,
        <crate::http3::H3<R, T, D> as hclient_core::unversioned::Transport>::Body:
            http_body::Body<Data = bytes::Bytes, Error = Error> + Send + 'static, // send-bound-exception: amendment-C12
        <crate::http3::H3<R, T, D> as crate::http3::StagedConnect>::Staged: Send + 'static, // send-bound-exception: amendment-C15
        for<'a> <crate::http3::H3<R, T, D> as crate::http3::StagedConnect>::Connecting<'a>: Send, // send-bound-exception: amendment-C15
        for<'a> <crate::http3::H3<R, T, D> as crate::http3::StagedConnect>::Exchanging<'a>: Send, // send-bound-exception: amendment-C15
        crate::http3::H3<R, T, D>: Sync, // send-bound-exception: amendment-C15
        R: Send + Sync + 'static,        // send-bound-exception: amendment-C12
        T: Send + Sync + 'static,        // send-bound-exception: amendment-C12
        D: Send + Sync + 'static,        // send-bound-exception: amendment-C12
    {
        // **The stored value must be true whichever path serves the
        // request**, and `capabilities()` hands back a reference computed
        // once — so the two are reduced here, at the only moment both are
        // in hand. Five fields take the weaker claim, `early_data` the
        // stronger one (the variant means *can*), and where the two are
        // different claims rather than a stronger and a weaker one there
        // is no true value and this refuses, naming the field.
        //
        // `Native::without_pool()` against a QUIC arm is the one refusal
        // reachable from what this workspace ships, and it is an ordinary
        // mistake rather than a contrived one.
        self.caps = caps::combine(&self.caps, quic.capabilities()).map_err(Box::new)?;
        self.h3 = Some(Arc::new(quic));
        self.versions.h3 = true;
        Ok(self)
    }

    /// Whether this transport may speak HTTP/1.1. On by default.
    ///
    /// **Turning it off is a guarantee, not a preference**, and that is
    /// what makes it worth having. Over TLS the ALPN offer becomes `h2`
    /// alone, so a server that cannot speak HTTP/2 finds no overlap and
    /// the handshake fails — there is no path by which a connection this
    /// transport made carries HTTP/1.1.
    ///
    /// # What it moves that nothing else can
    ///
    /// [`Capabilities::full_duplex`] and [`Capabilities::response_trailers`]
    /// report the **floor** — the value that holds on the worst protocol
    /// this transport might negotiate — which is why they stay `false`
    /// even with the `http2` feature on. With HTTP/1.1 forbidden the floor
    /// is HTTP/2's, and both become honestly `true`.
    ///
    /// `RequireVersion` cannot do this: it is per request and arrives
    /// after `capabilities()` has been answered, and `capabilities()`
    /// returns a `&Capabilities` stored at construction.
    ///
    /// # `http://` is refused, and it has to be
    ///
    /// Plaintext carries no ALPN, so the only way to reach HTTP/2 there is
    /// prior knowledge (RFC 9113 §3.4), which this transport does not do.
    /// A plaintext request on a transport that has forbidden HTTP/1.1 is
    /// therefore a typed [`ErrorKind::Unsupported`] at connect rather than
    /// a quiet fall back to the protocol the caller just ruled out — which
    /// would also make the raised floor above a lie.
    ///
    /// # Errors
    ///
    /// [`NoVersionsLeft`] if HTTP/2 is already off, or is not compiled in:
    /// the two setters refuse the empty state between them, so it is
    /// unreachable rather than checked later.
    pub fn http1(mut self, on: bool) -> Result<Self, Error> {
        if !on && !(self.versions.h2 && cfg!(feature = "http2")) {
            return Err(Error::new(ErrorKind::Unsupported, NoVersionsLeft));
        }
        self.versions.h1 = on;
        self.caps = Self::capabilities_for(&self.caps, self.versions);
        Ok(self)
    }

    /// Whether this transport may speak HTTP/2. Defaults to whether the
    /// `http2` feature compiled it in.
    ///
    /// Off, the ALPN offer loses `h2` and the pool stops holding h2
    /// buckets — `may_speak_h2` is the one predicate both read, so they
    /// cannot drift into a client that offers a protocol it will not pool
    /// or pools one it will not offer.
    ///
    /// The default follows the feature rather than being `false`, because
    /// that is what the transport did before this setting existed: a build
    /// with `http2` on offered h2 wherever ALPN could carry it, and a
    /// default of `false` would make the feature a silent no-op until a
    /// second call.
    ///
    /// # Errors
    ///
    /// [`Http2NotCompiledIn`] for `http2(true)` in a build without the
    /// feature — a named refusal, because the fix is a cargo feature and
    /// not something a caller can infer from a request that quietly went
    /// out over HTTP/1.1. [`NoVersionsLeft`] for `http2(false)` when
    /// HTTP/1.1 is already off.
    pub fn http2(mut self, on: bool) -> Result<Self, Error> {
        if on && !cfg!(feature = "http2") {
            return Err(Error::new(ErrorKind::Unsupported, Http2NotCompiledIn));
        }
        if !on && !self.versions.h1 {
            return Err(Error::new(ErrorKind::Unsupported, NoVersionsLeft));
        }
        self.versions.h2 = on;
        self.caps = Self::capabilities_for(&self.caps, self.versions);
        Ok(self)
    }

    /// The two capability fields whose value depends on which versions are
    /// allowed, recomputed wherever that changes.
    ///
    /// A function rather than two assignments at each setter, so the rule
    /// — *the floor is HTTP/1.1's unless HTTP/1.1 is forbidden* — is
    /// written once and cannot drift between the two call sites.
    fn capabilities_for(base: &Capabilities, versions: Versions) -> Capabilities {
        let mut caps = base.clone();
        let h2_only = !versions.h1;
        caps.full_duplex = h2_only;
        caps.response_trailers = h2_only;
        caps
    }

    /// report the HTTP/1.1 floor for [`Native::new`]'s reason.
    /// Send a `PING` on every **shared** HTTP/2 connection, and close one
    /// whose peer stops answering.
    ///
    /// Off by default, which is the decision rather than an omission: a
    /// client that pings puts traffic on the wire nobody asked for. What
    /// it is for is the path rather than the peer — a NAT or a load
    /// balancer whose flow timer drops a connection that has been quiet,
    /// which is what a long-poll or an SSE stream between events looks
    /// like from the middle of a network.
    ///
    /// **Only reached with [`Native::multiplexed`]**, and that is
    /// structural: an idle pooled connection has no holder at all, so
    /// there is nothing to write a `PING` or read its answer. Set here
    /// without `multiplexed()`, it is inert.
    ///
    /// The peer's failure to answer within
    /// [`H2KeepAlive::within`](http2::H2KeepAlive::within) closes the
    /// connection with [`PingNotAnswered`] —
    /// `ErrorKind::Connect`, because what ended is a connection and not
    /// an exchange.
    #[must_use]
    #[cfg(feature = "http2")]
    pub fn h2_keep_alive(mut self, cfg: http2::H2KeepAlive) -> Self {
        self.h2_keep_alive = Some(cfg);
        self
    }

    #[cfg(feature = "http2")]
    pub fn multiplexed(mut self) -> Self
    where
        R: Spawn<http2::H2Driver<NativeIo<R, T>, H, R>> + Unpin,
        H: Hooks + Unpin,
    {
        self.share_h2 = Some(<R as Spawn<http2::H2Driver<NativeIo<R, T>, H, R>>>::spawn);
        self
    }

    /// Socket parameters for EVERY TCP attempt this transport makes (see
    /// [`hclient_rt::TcpOpts`]) — **refused here, once, if the runtime
    /// cannot apply them.**
    ///
    /// `Result`, not `Self`, and that is the whole point of the method.
    /// W7 gave [`hclient_rt::TcpConnect`] an `APPLIES` constant and
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
    /// source is a [`hclient_rt::UnsupportedTcpOpts`], carried inside an
    /// [`std::io::Error`] exactly as `reject_unsupported` builds it, and
    /// [`UnsupportedTcpOpts::names`](hclient_rt::UnsupportedTcpOpts::names)
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
    /// there — and a caller passing
    /// `TcpOpts::default().keepalive(Some(..))` here turns it back off
    /// along with everything else, because `TcpOpts::default()` is
    /// all-off. That is
    /// deliberate rather than a trap left open: these are *the* socket
    /// parameters for every attempt this transport makes, and a method
    /// that silently kept one field of its own would be a worse surprise
    /// than one that takes the caller at their word. `..transport
    /// .tcp_opts_now()` does not exist for the same reason a getter for a
    /// setting nobody set does not: the value to start from is
    /// `TcpOpts::default().nodelay(true)`, which is what
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

    /// What this client announces in its HTTP/2 `SETTINGS` frame, where it
    /// does not want `h2`'s default — see [`H2Opts`], which is where the
    /// argument for each field is and where the three deliberate absences
    /// are named.
    ///
    /// The one that motivates the rest is the receive window: RFC 9113
    /// §6.9.2 fixes it at 65 535 bytes, and on a long fat pipe a peer can
    /// have at most a window in flight, so the ceiling is `window / RTT`
    /// however much bandwidth there is.
    ///
    /// ```no_run
    /// # use hclient_native::{H2Opts, Native};
    /// # use hclient_rt::{TcpConnect, Timer};
    /// # use hclient_tls::TlsConnect;
    /// # fn f<R: TcpConnect + Timer, T: TlsConnect, D>(t: Native<R, T, D>) -> Native<R, T, D> {
    /// t.h2_opts(H2Opts {
    ///     initial_window_size: Some(4 << 20),
    ///     // Raised together: the connection window is the ceiling for
    ///     // everything sharing the connection, so lifting only the
    ///     // stream one leaves 65 535 in force where streams are shared.
    ///     initial_connection_window_size: Some(8 << 20),
    ///     ..H2Opts::default()
    /// })
    /// # }
    /// ```
    ///
    /// **Infallible, unlike [`tcp_opts`](Self::tcp_opts) beside it**, and
    /// the difference is who applies the value: a socket option is applied
    /// by a runtime that may not have it, so `TcpOpts` needs a refusal and
    /// a per-field support mirror, where a `SETTINGS` frame is written by
    /// this crate and there is nobody to say no. A value out of the RFC's
    /// range is `h2`'s to reject at the handshake, not this method's to
    /// second-guess.
    ///
    /// **It replaces the whole set**, as `tcp_opts` does, and the wart is
    /// smaller here for the same reason the constructor differs: every
    /// field is an `Option` whose `None` means *h2's default*, so a caller
    /// who sets one field and drops another gets the default rather than
    /// an option silently switched off.
    ///
    /// Nothing in [`Capabilities`] changes — a window size is not a
    /// capability, and no caller can ask whether one was raised.
    #[cfg(feature = "http2")]
    #[must_use]
    pub fn h2_opts(mut self, opts: H2Opts) -> Self {
        self.h2_opts = opts;
        self
    }

    /// What this client accepts in an HTTP/1 **response head** — the
    /// header count and the largest head it will buffer. See
    /// [`H1Opts`].
    ///
    /// The head is the one part of a response a client must hold whole
    /// before it can act on any of it, so it is the one part a hostile
    /// server can make expensive without sending a body.
    /// [`h2_opts`](Self::h2_opts)' `max_header_list_size` is the same
    /// guard one protocol over, and neither is complete without the other:
    /// a transport that negotiates ALPN speaks whichever the server
    /// picked.
    ///
    /// **Fallible, unlike [`h2_opts`](Self::h2_opts)**, and the difference
    /// is who would refuse the value. A `SETTINGS` frame is written by
    /// this crate and there is nobody to say no; `max_buf_size` is handed
    /// to hyper, which **panics** below 8192. A caller's number reaching a
    /// `panic!` inside a connect is not a refusal they can act on, so it
    /// is checked here and named.
    /// Send every request over the Unix-domain socket at `path`, whatever
    /// authority its URI names.
    ///
    /// This is how a caller reaches a local daemon that speaks HTTP over a
    /// socket rather than a port — a container runtime, a systemd service,
    /// a package manager. The URI still carries a host, because HTTP needs
    /// one for `Host:` and for the pool: `http://localhost/v1.51/version`
    /// over `/var/run/docker.sock` is the shape, and it is `curl`'s
    /// `--unix-socket` exactly.
    ///
    /// ```no_run
    /// # use hclient_native::Native;
    /// # use hclient_rt::{TcpConnect, Timer};
    /// # use hclient_tls::TlsConnect;
    /// # fn f<R: TcpConnect + Timer, T: TlsConnect, D>(t: Native<R, T, D>)
    /// # -> Result<Native<R, T, D>, hclient_core::Error> {
    /// t.unix_socket("/var/run/docker.sock")
    /// # }
    /// ```
    ///
    /// # What it replaces
    ///
    /// The whole resolve → HTTPS-record discovery → Happy Eyeballs →
    /// `connect` block, which is [`Proxy`]'s slot exactly:
    /// there is no name to resolve, no address family to race and no port.
    /// It is **not** a proxy — nothing is tunnelled and the request head is
    /// written origin-form — which is why it is a setting rather than a
    /// `ProxyProtocol`.
    ///
    /// A proxy and a Unix socket together is a refusal rather than an
    /// order of precedence: both answer *where does this connection go*,
    /// and a rule about which wins would be a rule nobody could guess.
    ///
    /// # `https://` still works, and `TcpOpts` still does not
    ///
    /// The TLS handshake is unchanged and the server name comes from the
    /// URI, so a daemon that speaks TLS over a socket is reachable. Every
    /// [`TcpOpts`] field, on the other hand, is a TCP or IP option that
    /// `AF_UNIX` does not have — they are simply not applied here, which
    /// is what `TcpConnect::connect_unix` taking no options says.
    ///
    /// # It is refused where the runtime says it cannot
    ///
    /// [`hclient_rt::TcpConnect::SUPPORTS_UNIX`],
    /// which both shipped runtimes compute with `cfg!(unix)`
    /// — so this fails at the call that configures it rather than on the
    /// first request, which is `tcp_opts`' rule one method over.
    pub fn unix_socket(mut self, path: impl AsRef<std::path::Path>) -> Result<Self, Error> {
        if !<R as TcpConnect>::SUPPORTS_UNIX {
            return Err(Error::new(
                ErrorKind::Unsupported,
                hclient_rt::UnixSocketsUnsupported,
            ));
        }
        if !self.proxies.is_empty() {
            return Err(Error::new(ErrorKind::Unsupported, ProxyAndUnixSocket));
        }
        self.unix_socket = Some(Arc::from(path.as_ref()));
        Ok(self)
    }

    pub fn h1_opts(mut self, opts: crate::http1::H1Opts) -> Result<Self, Error> {
        if let Some(asked) = opts.max_buf_size
            && asked < crate::http1::MINIMUM_MAX_BUF_SIZE
        {
            return Err(Error::new(
                ErrorKind::Unsupported,
                crate::error::MaxBufSizeTooSmall { asked },
            ));
        }
        self.h1_opts = opts;
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
impl<R, T, D, H, P> Native<R, T, D, H, P>
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
    P: crate::proxy::Handshake + Clone,
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
    /// `identity` is the resolved [`TlsConfigId`] of a named client
    /// identity, where the request asked for one.
    ///
    /// **It goes into the key, and that is the isolation.** Two labels
    /// resolve to two ids, so they cannot share a connection — by
    /// construction rather than by a check, which matters because the
    /// failure mode is presenting one tenant's certificate on another
    /// tenant's behalf and no error would report it. Written as
    /// `let _ = identity_id;` first and caught immediately by the test
    /// that exists for exactly this.
    fn key_parts(
        &self,
        uri: &http::Uri,
        identity: Option<hclient_tls::TlsConfigId>,
    ) -> Result<KeyParts, Error> {
        let host = connect::host(uri)?;
        let use_tls = connect::wants_tls(uri)?;
        let port = connect::port(uri, use_tls);
        let security = if use_tls {
            // The identity of the trust configuration, asked of the TLS
            // backend rather than inferred from its type — see
            // `hclient_tls::TlsConfigId`.
            Security::Tls(identity.unwrap_or_else(|| self.tls.config_id()))
        } else {
            Security::Plaintext
        };
        Ok(KeyParts {
            security,
            host: host.into(),
            port,
            // **The proxy that will actually serve this request**, not
            // "the proxy": with a list, two schemes can route to two
            // different proxies, and a key naming the wrong one would let
            // a tunnel through proxy A be reused for a request routed to
            // proxy B — the security defect `Proxy::key`'s own doc names.
            //
            // **Unreachable today, and structurally so**, which is this
            // change's mutation control: replacing this with
            // `self.proxies.first()` passes the whole suite. `choose` is a
            // pure function of `(use_tls, host, port)` and the key already
            // carries all three — `security` encodes the first — so two
            // requests that agree on the key cannot disagree on the proxy.
            // It is written correctly anyway for the reason the `proxy`
            // field is in `PoolKey` at all: the moment a pool is shared
            // between transports, the two stop being the same question.
            // **The socket path shares `proxy`'s slot in the key**, and it
            // is the same argument: two connections that go to different
            // places must not be interchangeable. They share one field
            // because `Native::unix_socket` refuses to coexist with a
            // proxy, so at most one is ever `Some` — a second field would
            // be a state the constructor forbids.
            //
            // **Unreachable today, like the proxy beside it**, and for the
            // same structural reason: `unix_socket` is constant within one
            // `Native`, so two requests through one transport cannot
            // disagree about it, and two transports have two pools.
            // Removing it passes the whole suite — this work's second
            // mutation control. Written correctly anyway for the moment a
            // pool is shared between transports.
            proxy: self
                .unix_socket
                .as_ref()
                .map(|p| format!("unix:{}", p.display()).into_boxed_str())
                .or_else(|| {
                    crate::proxy::Proxy::choose(
                        &self.proxies,
                        matches!(security, Security::Tls(_)),
                        host,
                        port,
                    )
                    .map(|p| p.key().into_boxed_str())
                }),
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
    /// answer back (`hclient-tls-native-tls` is exactly that) would leave
    /// us speaking HTTP/1 into a connection the server switched to HTTP/2:
    /// not a lost optimisation, a protocol error on every request, and one
    /// arriving through a feature the user may not have enabled — Cargo
    /// unifies features across the whole graph. So the offer is withheld
    /// unless the answer can be read.
    #[cfg(feature = "http2")]
    fn may_speak_h2(&self, parts: &KeyParts) -> bool {
        self.versions.h2 && matches!(parts.security, Security::Tls(_)) && self.tls.reports_alpn()
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
    /// The exchange under `Timeouts::first_byte`, with the
    /// `Expect: 100-continue` bound folded in.
    ///
    /// **Folded rather than wrapped, and that is not tidiness.** A second
    /// combinator around this one held the whole exchange future by value
    /// inside `execute`'s, and `hclient-native`'s hook suite — 56 tests —
    /// aborted with `SIGABRT` on a stack overflow. Written that way once,
    /// measured, and replaced by this: one race, two sleeps, no extra
    /// nesting. The cache work found the same edge one crate over, which
    /// is why `hclient/tests/future_size.rs` exists.
    async fn within_first_byte_gated<F, V>(
        &self,
        d: Option<Duration>,
        gate: Option<Arc<body::ContinueGate>>,
        fut: F,
    ) -> Result<V, established::Failed>
    where
        F: Future<Output = Result<V, established::Failed>>,
    {
        let gate = gate.zip(self.expect_continue);
        // No `first_byte` bound, so the only clock here is the gate's —
        // and where there is neither, this is `fut.await` and nothing
        // else, which is what every request that asked for nothing gets.
        let Some(d) = d else {
            let Some((gate, after)) = gate else {
                return fut.await;
            };
            let mut fut = std::pin::pin!(fut);
            let mut wait = std::pin::pin!(self.rt.sleep(after));
            let mut opened = false;
            return poll_fn(|cx| {
                if let Poll::Ready(r) = fut.as_mut().poll(cx) {
                    return Poll::Ready(r);
                }
                if !opened && wait.as_mut().poll(cx).is_ready() {
                    opened = true;
                    gate.open();
                }
                Poll::Pending
            })
            .await;
        };
        let mut fut = std::pin::pin!(fut);
        // Built only on this branch: `Tokio::sleep` panics outside a
        // runtime, and a request that asked for nothing must not need one.
        let mut sleep_fut = std::pin::pin!(self.rt.sleep(d));
        let mut gate_wait = gate
            .as_ref()
            .map(|(_, after)| Box::pin(self.rt.sleep(*after)));
        let mut gate_opened = false;
        poll_fn(|cx| {
            // The exchange first, so a head that arrived in the same wake
            // as the deadline expiring is a response rather than a
            // timeout — the same ordering rule as `with_connect_timeout`
            // below and `hclient::within`.
            if let Poll::Ready(r) = fut.as_mut().poll(cx) {
                return Poll::Ready(r);
            }
            // The gate's bound is a *release*, never a failure, so it is
            // checked before the one that ends the request and does not
            // return: RFC 9110 §10.1.1 makes an unanswered `Expect` mean
            // "send it anyway".
            if let (false, Some((gate, _)), Some(w)) = (gate_opened, &gate, gate_wait.as_mut())
                && w.as_mut().poll(cx).is_ready()
            {
                gate_opened = true;
                gate.open();
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
        count: Counted<'_>,
    ) -> http::Response<NativeBody<R, T, H>> {
        resp.map(|b| {
            // The counter goes **inside** the `between_bytes` bound, for
            // the same reason that bound is innermost: what it counts is
            // what came off the wire, not what a wrapper above chose to
            // pass on. Nothing observable turns on the order — a body cut
            // by the idle timeout has still yielded what it yielded — and
            // the two are written in the order their arguments give.
            let counted = hclient_core::unversioned::Counting::new(
                b,
                self.hooks.clone(),
                count.id,
                count.request,
                count.uri,
                count.sent,
            );
            IdleTimeout::new(counted, self.rt.clone(), every)
        })
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
        request: RequestId,
        uri: &http::Uri,
        began: Option<R::Instant>,
    ) {
        // `version` is `Some`, because this transport read it: off the
        // status line on HTTP/1, off ALPN with the `http2` feature. That
        // is the same claim `capabilities()` makes with
        // `version_reported: true`, and `Head::version`'s doc says the two
        // must agree.
        self.hooks.on(&Event::Head(
            Head::new(id, uri, resp.status(), since::<R>(&self.rt, began))
                .request(request)
                .version(Some(resp.version())),
        ));
    }

    /// Whether this transport shares HTTP/2 connections — the spawner is
    /// set **and** there is a pool to keep a shared connection in.
    ///
    /// **What the second conjunct actually buys is narrower than it
    /// looks**, and a mutation is what said so. It does *not* make
    /// [`Native::multiplexed`] and [`Native::without_pool`]
    /// order-independent: `share_if_multiplexing` reads the pool's
    /// configuration for the deadline it has to stamp, so a transport with
    /// no pool shares nothing whichever order the two were written in, with
    /// or without this conjunct. What it buys is that such a transport also
    /// does no **single-flight** work: without it, a pool-less multiplexed
    /// transport would take the connect mark and make seven of a burst of
    /// eight wait once for a connection that is never published. No test
    /// asserts that, because what it costs is a wait rather than an
    /// outcome; it is recorded as a surviving mutation rather than
    /// claimed as pinned.
    #[cfg(feature = "http2")]
    fn shares_connections(&self) -> bool {
        self.share_h2.is_some() && self.pool.config().is_some()
    }

    /// Waits for somebody else's connect to this origin, and reports what
    /// is left of `Timeouts::connect`.
    ///
    /// **The budget is spent once**, which is the arithmetic
    /// `hclient-select`'s h3 fallback does one layer up and `Client`'s
    /// `425` replay does one layer down: a caller who set
    /// `connect: Some(C)` must not be made to wait `C` for a neighbour and
    /// then `C` again for a connect of their own. Where the wait uses it
    /// all up, `with_connect_timeout` fails the request with the same
    /// `Timeout(Connect)` a connect of our own would have.
    ///
    /// The clock is read only when there is a bound to spend, so a request
    /// that set none costs no `Timer::now`.
    #[cfg(feature = "http2")]
    async fn wait_for_a_shared_connect(
        &self,
        key: &PoolKey,
        budget: Option<Duration>,
    ) -> Result<Option<Duration>, Error> {
        let waited_from = budget.map(|_| self.rt.now());
        let wait = self.pool.wait_for_connect(key);
        with_connect_timeout(&self.rt, budget, async move {
            wait.await;
            Ok(())
        })
        .await?;
        Ok(match (budget, waited_from) {
            (Some(d), Some(at)) => Some(d.saturating_sub(self.rt.elapsed_since(at))),
            _ => None,
        })
    }

    /// Turns a freshly handshaken HTTP/2 connection into a shared one, on
    /// a transport that asked for that and nowhere else.
    ///
    /// Three things happen together and must: the connection is split into
    /// a `SendRequest` and a driver, the driver is spawned, and a clone of
    /// the sender is published to the pool. Published *here* rather than
    /// when the response body ends, because the pool's copy is what keeps
    /// the connection alive and what every concurrent request borrows —
    /// the check-in an exclusive connection makes at the end of its body
    /// would be a connection shared with nobody until then.
    ///
    /// The [`CheckIn`] comes back as `None` for the same reason: the whole
    /// `Reuse { checkin, sender }` dance exists to survive an exclusive
    /// check-out, and nothing on this path has anything to hand back.
    ///
    /// Everything else — HTTP/1, an exclusive h2 connection, a transport
    /// that never asked — comes back exactly as it went in.
    #[cfg(feature = "http2")]
    fn share_if_multiplexing(
        &self,
        est: established::Established<NativeIo<R, T>>,
        checkin: &mut Option<CheckIn<NativeIo<R, T>>>,
        now: Duration,
        parts_of_key: &KeyParts,
    ) -> established::Established<NativeIo<R, T>> {
        let (Some(spawn), Some(cfg)) = (self.share_h2, self.pool.config()) else {
            return est;
        };
        match est {
            established::Established::H2(h2) => {
                let (shared, driver) = http2::share(
                    *h2,
                    self.hooks.clone(),
                    self.h2_keep_alive.map(|cfg| (cfg, self.rt.clone())),
                );
                spawn(&self.rt, driver);
                self.pool.put(
                    parts_of_key.key(Protocol::H2),
                    established::Established::H2Shared(shared.clone()),
                    now.saturating_add(cfg.idle_timeout),
                );
                *checkin = None;
                established::Established::H2Shared(shared)
            }
            other => other,
        }
    }
    /// A pooled connection that is still worth a request, or `None`.
    ///
    /// Dead candidates are dropped as they are found — dropping closes the
    /// socket — and the loop moves on to the next one, so a burst that left
    /// several dead connections behind costs a walk through them rather
    /// than a failed request. It terminates because every iteration removes
    /// one entry from the pool and `take` returns `None` on an empty
    /// bucket.
    ///
    /// **A shared entry is borrowed rather than taken** (see
    /// [`crate::pool::Pool::take`]), which is what makes the termination
    /// argument above need a second half: the entry a rejected clone came
    /// from is still in the pool, so it is removed here by name before the
    /// loop goes round. Without that line this function would borrow the
    /// same dead connection for ever.
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
            #[cfg(feature = "http2")]
            if let Some(slot) = est.shared_slot() {
                self.pool.forget_shared(key, slot);
            }
            // Reported here rather than at the drop below, and outside
            // the pool's mutex rather than inside it — the two rules the
            // hooks seam is built on (`hclient_core::unversioned::hooks`,
            // module doc). `Stale` is the honest reason: the peer closed
            // it while it sat idle, which is why this loop is walking
            // past it. A connection the pool itself drops for age never
            // reaches this function, and that hole is deliberate.
            self.hooks
                .on(&Event::Closed(Closed::new(est.id(), CloseReason::Stale)));
        }
    }
}

impl<R, T, D, H, P> Native<R, T, D, H, P>
where
    R: TcpConnect + Timer,
    T: TlsConnect,
    P: crate::proxy::Handshake + Clone,
{
    /// How this request's head must be written, which a tunnel does not
    /// change — see [`crate::proxy::Via`].
    fn via(&self, uri: &http::Uri) -> crate::proxy::Via<'_> {
        let use_tls = uri.scheme_str() == Some("https");
        let port = uri.port_u16().unwrap_or(if use_tls { 443 } else { 80 });
        let host = uri.host().unwrap_or_default();
        // The list and its bypasses are asked here too, and they must be:
        // a request that went direct — because it was bypassed, or because
        // no proxy in the list serves its scheme — would otherwise still
        // be written in absolute-form, to an origin that never agreed to
        // be a proxy.
        match crate::proxy::Proxy::choose(&self.proxies, use_tls, host, port) {
            Some(p) if p.protocol().approach(use_tls) == crate::proxy::Approach::Absolute => {
                crate::proxy::Via::AbsoluteForm(p.protocol().proxy_authorization())
            }
            _ => crate::proxy::Via::Direct,
        }
    }
}

impl<R, T, D, H, P> Native<R, T, D, H, P>
where
    R: TcpConnect + Timer,
    T: TlsConnect,
{
    /// A gate for this request, or `None` if it is not one that waits.
    ///
    /// **Both conditions**: the caller asked, by sending the header, and
    /// the transport was configured to honour it. Either alone leaves the
    /// body ungated — a header with no configuration is today's behaviour,
    /// and a configuration with no header has nothing to wait for.
    fn continue_gate(
        &self,
        req: &http::Request<body::OutgoingBody>,
    ) -> Option<Arc<body::ContinueGate>> {
        self.expect_continue?;
        let asked = req
            .headers()
            .get_all(http::header::EXPECT)
            .iter()
            .any(|v| v.as_bytes().eq_ignore_ascii_case(b"100-continue"));
        asked.then(|| Arc::new(body::ContinueGate::default()))
    }
}

/// A pool key with its protocol not yet decided — see
/// [`Native::key_parts`].
struct KeyParts {
    security: Security,
    host: Box<str>,
    port: u16,
    /// Carried from the transport that built these parts, so that a
    /// pooled tunnel is never handed to a request routed through a
    /// different proxy. `None` on every direct transport, which is every
    /// transport whose `P` is `NoProxy`.
    proxy: Option<Box<str>>,
}

impl KeyParts {
    fn key(&self, protocol: Protocol) -> PoolKey {
        PoolKey::new(
            self.security,
            &self.host,
            self.port,
            protocol,
            self.proxy.as_deref(),
        )
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
    h1_opts: crate::http1::H1Opts,
    h2_opts: crate::http2::H2Opts,
) -> Result<established::Established<I>, Error>
where
    I: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
{
    if is_h2(protocol) {
        Ok(established::Established::H2(Box::new(
            http2::handshake(conn, id, h2_opts).await?,
        )))
    } else {
        Ok(established::Established::H1(
            http1::handshake(conn, id, h1_opts).await?,
        ))
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
    h1_opts: crate::http1::H1Opts,
) -> Result<established::Established<I>, Error>
where
    I: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
{
    debug_assert!(!is_h2(protocol));
    Ok(established::Established::H1(
        http1::handshake(conn, id, h1_opts).await?,
    ))
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
/// does not speak, and the standing answer to that is
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
/// type *is* that projection, which is what `hclient-select` already
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
    /// second protocol stack (`hclient-select`) has to read it *before* it
    /// can decide which stack a request belongs to — after which this
    /// transport, left to itself, would ask for the very same record a
    /// second time. Measured, that was two type-65 queries for one request
    /// (`crates/hclient-select/tests/dns_cost.rs`).
    ///
    /// # What it does not become
    ///
    /// **Not a way to tell a transport something.** The answer is fetched
    /// *here*, by the transport, with its own resolver and its own memory,
    /// for the authority of the request handed in — so there is no version
    /// of this call in which a caller supplies the record. See [`Prepared`]
    /// for why that is the whole point.
    ///
    /// **Not a cache.** What comes back is good for the one request it
    /// travels with, and nothing here remembers it — for
    /// `hclient-native`'s reason: an HTTPS record carries no TTL, and
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
impl<R, T, D, H, P> Native<R, T, D, H, P>
where
    R: TcpConnect + Timer + Clone,
    R::Stream: 'static,
    T: TlsConnect,
    T::Stream<R::Stream>: 'static,
    H: Hooks + Clone + Unpin,
    P: crate::proxy::Handshake + Clone,
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

        // `Capabilities` claims `timeouts.connect = true`, so this file
        // must read `Timeouts` from `parts.extensions` — not reading it
        // makes the claimed timeout a silent no-op, exactly the class of
        // defect this channel exists to root out.
        // `Transport::execute`'s doc comment
        // (`hclient-core/src/unversioned/transport.rs`) spells out the
        // correct reading literally: "presence is not intent",
        // `.get::<Timeouts>().copied().unwrap_or_default()`, then field by
        // field — don't branch on the extension's `is_some()` as a whole.
        let timeouts = parts
            .extensions
            .get::<Timeouts>()
            .copied()
            .unwrap_or_default();
        // The same reading, one extension over, and it is read **once**:
        // the request is consumed by the exchange, and the response body
        // that reports `Progress` outlives it, so the value has to be
        // carried rather than looked up again. Gated on `H::WATCHING`
        // inside `identify`, so an unwatched build touches no map.
        let request = hclient_core::unversioned::identify::<H>(&parts.extensions);

        // Which connections may serve this request. Computed BEFORE
        // `origin_form` below rewrites the URI into origin-form and the
        // authority stops being there to read — and, in doing so, this is
        // now where the scheme and host are validated: `key_parts` runs the
        // same `connect::wants_tls`/`connect::host` checks
        // `connect::connect` runs, and fails with the same typed errors, so
        // everything downstream may still assume they passed.
        // **The client identity, read here and resolved before any
        // socket.** A label rather than a certificate, because no
        // representation of a certificate is shared by Windows, macOS,
        // PKCS#11 and Android — see `docs/mtls-design.md`.
        //
        // A name this backend does not know is a **refusal naming it**,
        // never a connection with the default identity: substituting one
        // silently is how one tenant's certificate reaches another
        // tenant's server.
        let identity = parts
            .extensions
            .get::<hclient_core::ClientIdentity>()
            .cloned();
        let identity_id = match identity.as_ref().map(ClientIdentity::name) {
            None => None,
            Some(id) => match hclient_tls::TlsIdentity::config_id_for(&self.tls, id) {
                Some(cfg) => Some(cfg),
                None => {
                    return Err(Error::new(
                        ErrorKind::Tls,
                        UnknownClientIdentity(id.to_string()),
                    ));
                }
            },
        };
        let identity = identity.as_ref().map(ClientIdentity::name);
        let parts_of_key = self.key_parts(&parts.uri, identity_id)?;
        // A transport that has forbidden HTTP/1.1 cannot serve `http://`:
        // plaintext carries no ALPN, and the only route to HTTP/2 there is
        // prior knowledge (RFC 9113 §3.4), which this transport does not
        // do. Refused here rather than quietly served over the protocol
        // the caller just ruled out — which would also make
        // `Capabilities::full_duplex` a lie, since `Native::http1` raises
        // that floor on exactly this guarantee.
        if !self.versions.h1 && !matches!(parts_of_key.security, Security::Tls(_)) {
            return Err(Error::new(ErrorKind::Unsupported, PlaintextNeedsHttp1));
        }
        let uri = parts.uri.clone();

        let outgoing = body::OutgoingBody::from_request_body(body)?;
        // The request body's octet count, made once for the whole
        // operation and not once per attempt: `Native::run` may hand the
        // same request back to a second connection after a `NotSent`, and
        // a counter reset there would report the upload starting over when
        // not a byte of it had reached the wire.
        //
        // `expected` is read off the body's own `size_hint` before
        // anything moves, which is where a `Content-Length` would have
        // come from anyway.
        let outgoing = {
            let meter =
                hclient_core::unversioned::meter::<H>(outgoing.expected()).map(std::sync::Arc::new);
            outgoing.counting(meter)
        };
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

        // 0. Only when connections are shared: if somebody is already
        //    opening one to this origin, wait for it rather than opening a
        //    second. Sharing is what makes this necessary — a burst of N
        //    concurrent requests to a cold origin has no first request to
        //    share from, so without single-flight all N connect and each
        //    shared connection is shared with nobody.
        //
        //    The mark is taken here rather than immediately before the
        //    connect so that the window between "the pool is empty" and
        //    "I am connecting" is not a window at all, and it is released
        //    the moment there is a connection to find — see
        //    `pool::Connecting`. `budget` is what is left of
        //    `Timeouts::connect` afterwards, because a bound a waiter can
        //    be made to pay twice is not a bound.
        #[cfg(feature = "http2")]
        let mut connect_guard = None;
        #[cfg(feature = "http2")]
        let mut budget = timeouts.connect;
        #[cfg(feature = "http2")]
        if self.shares_connections() && self.may_speak_h2(&parts_of_key) {
            let shared_key = parts_of_key.key(Protocol::H2);
            connect_guard = self.pool.begin_connect(&shared_key);
            if connect_guard.is_none() {
                budget = self.wait_for_a_shared_connect(&shared_key, budget).await?;
                // Whoever we waited for is done. If they published a
                // connection the loop below finds it; if they failed,
                // taking the mark now makes this request the one the next
                // arrivals wait for rather than leaving the herd
                // uncoalesced for ever.
                connect_guard = self.pool.begin_connect(&shared_key);
            }
        }
        #[cfg(not(feature = "http2"))]
        let budget = timeouts.connect;

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
            self.hooks.on(&Event::Reused(
                Reused::new(id, &uri, spoken_version(Some(protocol))).request(request),
            ));
            let checkin = self.checkin_for(&key, now);
            // Nobody waiting for a connect should wait for this exchange:
            // what they are waiting for is a connection to exist, and one
            // does. Released here rather than at the end of `run` for the
            // reason `pool::Connecting`'s own doc gives — a slow server
            // must not become a queue.
            #[cfg(feature = "http2")]
            drop(connect_guard.take());
            let via = self.via(&uri);
            let gate = self.continue_gate(&req);
            // Read off the body before it moves into the exchange: the meter
            // was put there when the body was built, so this is one fact
            // travelling rather than two places agreeing about whether to
            // count.
            let sent = req.body().meter();
            let attempt = established::exchange(
                est,
                req,
                checkin,
                &uri,
                self.hooks.clone(),
                established::Dispatch {
                    via,
                    watch_1xx: self.watch_1xx,
                    gate: gate.clone(),
                },
            );
            let attempt =
                std::pin::pin!(self.within_first_byte_gated(timeouts.first_byte, gate, attempt));
            match hclient_core::unversioned::Reporting::new(
                attempt,
                &self.hooks,
                id,
                request,
                &uri,
                sent.clone(),
            )
            .await
            {
                Ok(resp) => {
                    self.report_head(&resp, id, request, &uri, began);
                    return Ok(self.bound_body(
                        resp,
                        timeouts.between_bytes,
                        Counted::new(id, request, &uri, sent),
                    ));
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
        let alpn: &[&[u8]] = match (offered_h2, self.versions.h1) {
            // Order is the preference: RFC 7301 leaves the choice to the
            // server, but every implementation reads the client's list as
            // ranked, and h2 is what we want when it is on offer.
            (true, true) => &[b"h2", b"http/1.1"],
            // **`http1(false)` is a guarantee rather than a preference.**
            // A server that cannot speak h2 finds no overlap and fails the
            // handshake, so no connection this transport makes can carry
            // HTTP/1.1 — which is what lets `Capabilities` raise its floor
            // (see `Native::http1`).
            (true, false) => &[b"h2"],
            (false, true) => &[b"http/1.1"],
            // Unreachable: `http1`/`http2` refuse the state between them,
            // and `may_speak_h2` only ever narrows what `versions.h2`
            // already allowed. Written as the honest arm rather than an
            // `unreachable!`, because the cost of being wrong is a failed
            // handshake and not a panic in a caller's process.
            (false, false) => &[b"http/1.1"],
        };
        // `now` is the same reading of the runtime's clock the pool's
        // bookkeeping above uses, passed on rather than taken again: the
        // negative cache's window and a connection's idle deadline are
        // measured from one epoch on one clock, and two reads a
        // microsecond apart would be two facts where there is one.
        let connect_fut = connect::connect::<R, D, T, P, H>(
            &self.rt,
            &self.dns,
            &self.tls,
            &self.proxies,
            self.unix_socket.as_deref(),
            &uri,
            &self.opts,
            alpn,
            identity,
            &self.svcb_failures,
            now,
            // Whatever `prepare` found, or `NotConsulted` for a request
            // that came through `Transport::execute`. This is the only
            // way a record reaches the connector from outside this
            // function, and it was fetched for this request's own
            // authority — see `Prepared`.
            prefetched,
            timeouts.resolve,
        );
        // `budget` rather than `timeouts.connect`: on the shared path it
        // is what a wait for somebody else's connect left, and on every
        // other path it *is* `timeouts.connect`.
        let (conn, tls_info, attempted) =
            with_connect_timeout(&self.rt, budget, connect_fut).await?;
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
            self.hooks.on(&Event::Connected(
                Connected::new(id, &uri, spoken_version(protocol))
                    .request(request)
                    .remote(attempted.remote)
                    // The handshake's own report, which this transport has
                    // had in hand at this line since TLS became a seam and
                    // has been discarding: `TlsInfo` carries the version,
                    // the suite and the ALPN, and nothing above
                    // `hclient-native` could see any of it.
                    .tls(
                        tls_info
                            .as_ref()
                            .and_then(|i| i.protocol_version.as_deref()),
                        tls_info.as_ref().and_then(|i| i.cipher_suite.as_deref()),
                        tls_info.as_ref().and_then(|i| i.alpn.as_deref()),
                        // Cloned rather than borrowed: it is not a slice of
                        // the `TlsInfo`, and the clone happens only in the
                        // `Asked` arm. Over `http://` there is no `TlsInfo`
                        // at all, which is `Unobserved` — there was no
                        // handshake to watch.
                        tls_info
                            .as_ref()
                            .map(|i| i.client_cert.clone())
                            .unwrap_or_default(),
                    )
                    .timing(
                        ConnectTiming::new()
                            .dns(attempted.dns)
                            .tcp(attempted.tcp)
                            .tls(attempted.tls)
                            .total(connect_took),
                    ),
            ));
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
        // demand existing.
        //
        // Checked before `checkin_for`, so a connection about to be
        // refused is never given a check-in token either. It is dropped
        // here, unpooled, which is right: nothing was spoken on it.
        check_version(req.extensions(), spoken_version(protocol))?;

        #[cfg_attr(not(feature = "http2"), allow(unused_mut))]
        let mut checkin = match protocol {
            Some(p) => self.checkin_for(&parts_of_key.key(p), now),
            None => None,
        };

        let est = handshake_for(
            conn,
            protocol,
            id,
            self.h1_opts,
            #[cfg(feature = "http2")]
            self.h2_opts,
        )
        .await?;
        // The one place a connection becomes a *shared* one. It happens
        // before the exchange, so what this request holds is a clone like
        // any other and the pool's copy owns the connection from the first
        // instant — which is also what lets the waiters released two lines
        // below find it.
        #[cfg(feature = "http2")]
        let est = self.share_if_multiplexing(est, &mut checkin, now, &parts_of_key);
        #[cfg(feature = "http2")]
        drop(connect_guard.take());
        let via = self.via(&uri);
        let gate = self.continue_gate(&req);
        // Read off the body before it moves into the exchange: the meter
        // was put there when the body was built, so this is one fact
        // travelling rather than two places agreeing about whether to
        // count.
        let sent = req.body().meter();
        let attempt = established::exchange(
            est,
            req,
            checkin,
            &uri,
            self.hooks.clone(),
            established::Dispatch {
                via,
                watch_1xx: self.watch_1xx,
                gate: gate.clone(),
            },
        );
        let attempt =
            std::pin::pin!(self.within_first_byte_gated(timeouts.first_byte, gate, attempt));
        let resp = hclient_core::unversioned::Reporting::new(
            attempt,
            &self.hooks,
            id,
            request,
            &uri,
            sent.clone(),
        )
        .await
        .map_err(established::Failed::into_error)?;
        self.report_head(&resp, id, request, &uri, began);
        Ok(self.bound_body(
            resp,
            timeouts.between_bytes,
            Counted::new(id, request, &uri, sent),
        ))
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
impl<R, T, D, H, P> Transport for Native<R, T, D, H, P>
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
    P: crate::proxy::Handshake + Clone,
{
    /// The pooled body, with the `between_bytes` bound wrapped round it.
    ///
    /// The order matters and is the mirror of `hclient::body::ClientBody`'s: the
    /// idle bound is the **innermost** wrapper, next to the socket, so it
    /// measures the gap between reads on the wire. Outside it the client
    /// may add its own `Deadline` and decompression, neither of which can
    /// hide a silent peer from this one.
    type Body = NativeBody<R, T, H>;
    type Error = Error;

    /// `Native::run` with nothing looked up — which is what every
    /// request through this seam is, because the seam has no way to carry
    /// an answer and [deliberately does not gain one](Prefetch::prepare).
    /// The record is fetched inside, when and if a connection is opened.
    async fn execute(
        &self,
        req: http::Request<RequestBody>,
    ) -> Result<http::Response<Self::Body>, Error> {
        #[cfg(feature = "http3")]
        return self.routed(req).await;
        #[cfg(not(feature = "http3"))]
        self.run(Prepared::new(req)).await
    }

    /// Identity: `Self::Error` is already `hclient_core::Error`, and its
    /// category is set wherever the failure happened (`Resolve`/`Connect`/
    /// `Unsupported` in `connect::connect`, `Tls` in `TlsConnect::connect`,
    /// `Body`/`Connect` in `http1::exchange`). The hook's default would do
    /// exactly the same thing (it recognizes our `Error` and passes it
    /// through unchanged) — the line is behaviorally redundant and
    /// semantically needed: it names the intent where it's read, and it
    /// will survive the default changing later. See the doc comment on
    /// `Transport::to_error` in `hclient-core`.
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
impl<R, T, D, H, P> Prefetch for Native<R, T, D, H, P>
where
    R: TcpConnect + Timer + Clone,
    R::Stream: 'static,
    T: TlsConnect,
    T::Stream<R::Stream>: 'static,
    D: Resolve,
    H: Hooks + Clone + Unpin,
    P: crate::proxy::Handshake + Clone,
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

/// Races `fut` against `rt.sleep(d)`: `fut` if it finished first,
/// otherwise `Err(ErrorKind::Timeout(Phase::Connect))`.
///
/// `std::future::poll_fn`, polling both arms by hand, rather than
/// `futures_util::select!`/`select` — the same technique and the same
/// reasoning as `connect::drive` (see its doc comment, the section on why
/// a `select_biased!` didn't fit): there are only two arms here
/// and each is needed exactly once, the macro gives this pair nothing that
/// a direct `poll` doesn't.
///
/// Scope: `fut` is expected to be the whole
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
    poll_fn(|cx| {
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
/// `pub(crate)` items like `connect::race_connect`/`http1::exchange`
/// directly. `#[doc(hidden)]` isn't part of the crate's public API, it's a
/// gap opened for this crate's own integration tests (`tests/connect.rs`,
/// `tests/dual_runtime.rs`, `tests/h1.rs`) and for nothing else.
#[doc(hidden)]
pub mod testing {
    use std::pin::Pin;
    use std::task::Context;
    use std::task::Poll;
    /// Runs Happy Eyeballs over a ready-made address list, bypassing DNS —
    /// a wrapper around `connect::race_connect` with a default `HeConfig`
    /// and `TcpOpts`, exactly what a test that only controls the address
    /// list and port needs.
    pub async fn connect_for_test<R>(
        rt: &R,
        addrs: &[std::net::IpAddr],
        port: u16,
    ) -> Result<R::Stream, hclient_core::Error>
    where
        R: hclient_rt::TcpConnect + hclient_rt::Timer,
    {
        let (v6, v4): (Vec<_>, Vec<_>) = addrs.iter().copied().partition(|a| a.is_ipv6());
        crate::connect::race_connect(
            rt,
            v6,
            v4,
            port,
            &hclient_rt::TcpOpts::default(),
            hclient_proto::happy_eyeballs::HeConfig::default(),
        )
        .await
    }

    /// Which proxy — if any — a request to `host:port` under `use_tls`
    /// would be sent through.
    ///
    /// The chooser is `pub(crate)` because nothing outside this crate
    /// routes a request, and `tests/*.rs` is outside it. What this opens
    /// is the one question a proxy test actually has: an installed proxy
    /// that is never *chosen* would satisfy every assertion that reads
    /// fields back, and fail every request.
    #[cfg(feature = "proxy")]
    pub fn chosen_proxy<'a, R, T, D, H, P>(
        native: &'a crate::Native<R, T, D, H, P>,
        use_tls: bool,
        host: &str,
        port: u16,
    ) -> Option<&'a crate::proxy::Proxy<P>>
    where
        R: hclient_rt::TcpConnect + hclient_rt::Timer,
        T: hclient_tls::TlsConnect,
    {
        native.chosen_proxy(use_tls, host, port)
    }

    pub use crate::body::OutgoingBody;
    pub use crate::established::NativeBody;

    /// An empty request body — what any `http1::exchange` test with nothing
    /// to send needs (a bodyless GET).
    pub fn empty_body() -> crate::body::OutgoingBody {
        crate::body::OutgoingBody::from_request_body(hclient_core::RequestBody::Empty)
            .expect("an Empty body follows no factory chain and cannot exceed the rewind bound")
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
    /// executor_with_no_spawn` actually hanging — it never got as far as
    /// the request's first byte.
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
    fn poll_would_block<T>(cx: &Context<'_>, r: std::io::Result<T>) -> Poll<std::io::Result<T>> {
        match r {
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            other => Poll::Ready(other),
        }
    }

    impl hyper::rt::Read for BlockingIo {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            mut buf: hyper::rt::ReadBufCursor<'_>,
        ) -> Poll<std::io::Result<()>> {
            // No `unsafe`: read into a stack buffer, then copy via
            // `put_slice` — the same path as the safe example in
            // `hyper::rt::Read`'s doc comment.
            let mut scratch = [0u8; 8192];
            let want = buf.remaining().min(scratch.len());
            match poll_would_block(
                cx,
                std::io::Read::read(&mut self.get_mut().0, &mut scratch[..want]),
            ) {
                Poll::Ready(Ok(n)) => {
                    buf.put_slice(&scratch[..n]);
                    Poll::Ready(Ok(()))
                }
                Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
                Poll::Pending => Poll::Pending,
            }
        }
    }

    impl hyper::rt::Write for BlockingIo {
        fn poll_write(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            poll_would_block(cx, std::io::Write::write(&mut self.get_mut().0, buf))
        }
        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            // `TcpStream::flush` is a no-op (no userspace buffering),
            // never blocks and never returns `WouldBlock`.
            Poll::Ready(std::io::Write::flush(&mut self.get_mut().0))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
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
            Poll::Ready(match r {
                Err(e) if e.kind() == std::io::ErrorKind::NotConnected => Ok(()),
                other => other,
            })
        }
    }

    /// One exchange over `io`, with no pool: the handshake and
    /// `http1::exchange` in one call, exactly as `Native::execute` does them
    /// for a fresh connection that is not to be reused.
    ///
    /// `None` for the check-in is not a simplification for tests' sake —
    /// it is the same value `Native::execute` passes when reuse is off,
    /// so what `tests/h1.rs` exercises is a real path rather than one that
    /// exists only for it.
    pub async fn exchange_for_test<I>(
        io: I,
        req: http::Request<crate::body::OutgoingBody>,
    ) -> Result<http::Response<crate::established::NativeBody<I>>, hclient_core::Error>
    where
        I: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
    {
        use hclient_core::unversioned::{ConnectionId, NoHooks};
        let est =
            crate::http1::handshake(io, ConnectionId::UNWATCHED, crate::http1::H1Opts::default())
                .await?;
        crate::http1::exchange(est, req, None, NoHooks, ConnectionId::UNWATCHED)
            .await
            .map(|r| r.map(crate::established::NativeBody::h1))
            .map_err(crate::established::Failed::into_error)
    }

    pub async fn collect<I>(
        b: crate::established::NativeBody<I>,
    ) -> Result<bytes::Bytes, hclient_core::Error>
    where
        I: hyper::rt::Read + hyper::rt::Write + Unpin,
    {
        use http_body_util::BodyExt;
        Ok(b.collect().await?.to_bytes())
    }
}

/// The `Send` half of the seam — what makes `hclient::Client`'s request
/// future crossable, and the reason the four runtime seams carry
/// associated futures at all.
///
/// **Every bound here is nameable, and that is the whole design.** A
/// generic impl must *prove* its future `Send`, which means naming every
/// future it awaits; `impl Future` has no name, so this impl could not
/// have been written while `TcpConnect`, `TlsConnect`, `Blocking` and
/// `Resolve` returned RPITITs. They return associated types now, so the
/// proof is a list of ordinary bounds — and one this transport's own
/// pieces may fail without being excluded from anything: `Native` over
/// `hclient-rt-embassy` is still a `Transport`, over a resolver that
/// cannot promise `Send` it is still a `Transport`, and what it is not is
/// a `SendTransport`.
impl<R, T, D, H, P> hclient_core::unversioned::SendTransport for Native<R, T, D, H, P>
where
    R: TcpConnect + Timer + Clone + Sync + Send, // send-bound-exception: amendment-C16
    R::Stream: 'static + Send,                   // send-bound-exception: amendment-C16
    R::Instant: Send + Sync,                     // send-bound-exception: amendment-C16
    R::Sleep: Send,                              // send-bound-exception: amendment-C16
    for<'a> R::Connecting<'a>: Send,             // send-bound-exception: amendment-C16
    for<'a> R::ConnectingUnix<'a>: Send,         // send-bound-exception: amendment-C16
    T: TlsConnect + Sync + Send,                 // send-bound-exception: amendment-C16
    T::Stream<R::Stream>: 'static + Send,        // send-bound-exception: amendment-C16
    for<'a> T::Handshake<'a, R::Stream>: Send,   // send-bound-exception: amendment-C16
    D: Resolve + Sync + Send,                    // send-bound-exception: amendment-C16
    for<'a> D::Ipv4<'a>: Send,                   // send-bound-exception: amendment-C16
    for<'a> D::Ipv6<'a>: Send,                   // send-bound-exception: amendment-C16
    for<'a> D::Svcb<'a>: Send,                   // send-bound-exception: amendment-C16
    H: Hooks + Clone + Unpin + Sync + Send,      // send-bound-exception: amendment-C16
    P: crate::proxy::Handshake + Clone + Sync + Send, // send-bound-exception: amendment-C16
{
    fn execute_send(
        &self,
        req: http::Request<hclient_core::RequestBody>,
    ) -> hclient_core::unversioned::BoxSendExchange<'_, Self::Body, Error> {
        Box::pin(<Self as hclient_core::unversioned::Transport>::execute(
            self, req,
        ))
    }
}
