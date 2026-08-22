//! HTTP/3 for hclient: QUIC over the runtime seam's UDP capability.
//!
//! ```no_run
//! # async fn doc() -> Result<(), Box<dyn std::error::Error>> {
//! use hclient_h3::H3;
//! let h3 = H3::new(
//!     hclient_rt_tokio::TokioHandle::current()?,
//!     hclient_tls_rustls::Rustls::with_webpki_roots(),
//!     hclient_dns_system::SystemDns::new(hclient_rt_tokio::TokioHandle::current()?),
//! )?;
//! let client = hclient::Client::builder(h3).build()?;
//! # Ok(()) }
//! ```
//!
//! # Its own crate, and not a feature of `hclient-native`
//!
//! v0.2's design document reserved `http3` as a feature there. It cannot
//! be one, and the reason is the type system rather than the dependency
//! count: this transport's bounds are `R: UdpBind + Spawn<..>` and
//! `T: QuicTlsConnect`, neither of which `Native<R, T, D>` has — and
//! Cargo's features are additive, so a feature would have to make both
//! unconditional for every build in the graph. The 55-crate QUIC stack is
//! the second reason, not the first.
//!
//! # `R: Spawn` is required, and what that costs
//!
//! **A QUIC connection that nobody polls is not idle, it is dying.** The
//! PING that resets a peer's idle timer comes from the connection's driver,
//! not from the kernel, so unlike an HTTP/1 socket in a pool it needs
//! something to drive it between requests. Measured: across a 1500 ms gap
//! under a 1000 ms idle timeout, an undriven connection's second request
//! fails and a driven one's succeeds. That is the whole argument for the
//! `Spawn` bound, and everything h3 is for — multiplexing, 0-RTT on a second
//! visit — pays off across exactly those gaps.
//!
//! The bound excludes only runtimes that were already excluded. `embassy-net`
//! has no descriptor at all, so `quinn-udp` cannot even be asked about
//! GSO/GRO/ECN, and quinn cannot wrap the socket. And the bound is on
//! [`H3`], not on the seam and not on `Native`, so a runtime may still
//! implement as little as it honestly can.
//!
//! # Connections are shared, and requests on one are multiplexed
//!
//! **This is the opposite of v0.2 W2's h2 policy, and deliberately so.**
//! There, an h2 connection is checked out of the pool *exclusively*, one
//! stream at a time, because without a spawner there is nobody to drive a
//! shared connection but the in-flight request futures — so a caller that
//! stopped polling one request would stall its neighbours. The argument is
//! correct and it has no subject here: the driver has moved into
//! [`Spawn`], because it had to for the connection to
//! survive at all, and a driver that is nobody's request future cannot be
//! stalled by any request's polling behaviour.
//!
//! So W1's "cancelling one stream must not tear down the others" holds in
//! this crate for a different reason than it holds in `hclient-native`:
//! there because there are no others, here because dropping an [`H3Body`]
//! sends `STOP_SENDING` for one stream and leaves the connection alone.
//! Both facts are written where the policy is, so that changing one does
//! not silently import the other's justification.
//!
//! # `capabilities()` reports the floor
//!
//! W3's rule, applied to a transport that negotiates exactly one protocol —
//! so "the worst protocol this might negotiate" is HTTP/3 itself, and the
//! floor is not automatically the conservative answer it is for `Native`.
//! Each field is set to what this implementation actually does; see
//! [`H3::new`].
#![forbid(unsafe_code)]

mod body;
mod early;
mod hooks;
mod pump;
mod staged;

pub use body::H3Body;
pub use hclient_quinn::QuinnTask;
pub use pump::{RequestTrailersNotSent, UnknownRequestBodyFrame};
pub use staged::{Refused, Staged, StagedConnect};

use bytes::Bytes;
use hclient_core::unversioned::{
    CloseReason, ConnectionId, Event, Head, Hooks, NoHooks, Transport,
};
use hclient_core::{
    CancelSupport, Capabilities, DecompressionSupport, EarlyDataSupport, Error, ErrorKind, Phase,
    RedirectSupport, RequestBody, ReuseSupport, TimeoutSupport, TlsSupport,
};
use hclient_rt::{Spawn, Timer, UdpAdoptStd, UdpBind};
use hclient_tls::TlsConfigId;
use hclient_tls_quic::{QuicTlsConnect, QuicTlsRequest};
use hooks::{ConnState, Watch, mark, since};
use std::collections::HashMap;
use std::fmt;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The ALPN token HTTP/3 is identified by (RFC 9114 §3.2). Mandatory, and
/// not a fallback: a QUIC connection that negotiates anything else is an
/// error, which is why the QUIC TLS seam has no `reports_alpn`.
const ALPN_H3: &[u8] = b"h3";

/// What a pooled connection is interchangeable for.
///
/// `early_data` is part of the key, not a property looked up afterwards:
/// rustls carries `enable_early_data` on the `ClientConfig`, so a
/// connection built to offer early data and one built not to came from
/// different configurations, and reusing one for the other would be a
/// connection that quietly does not do what its request asked.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PoolKey {
    host: String,
    port: u16,
    tls: TlsConfigId,
    early_data: bool,
}

type SendRequest = h3::client::SendRequest<h3_quinn::OpenStreams, Bytes>;

/// The 0-RTT acceptance verdict, shared by every request on the connection
/// that offered early data.
///
/// **A future, not a field, and that is the finding rather than a style
/// choice.** `TlsInfo::early_data_accepted: Option<bool>` is the right
/// shape for TLS 1.3 over TCP, where the answer is known when the
/// handshake completes. Over QUIC it is not: measured, `into_0rtt()`
/// returns at 1.27 ms, the
/// response arrives at 8.58 ms, and the verdict resolves at **8.63 ms** —
/// after the response body. A field could only hold it by waiting for the
/// handshake, which is the round trip 0-RTT exists to skip.
///
/// `Shared` because the connection is pooled and multiplexed: several
/// requests may need the same one-shot answer, and the *second* request on
/// a 0-RTT connection has as much right to know as the first.
type ZeroRtt = futures_util::future::Shared<quinn::ZeroRttAccepted>;

struct Pooled {
    send: SendRequest,
    conn: quinn::Connection,
    /// `Some` only if this connection actually went out with early data —
    /// `None` covers both "the caller did not ask" and "there was no usable
    /// ticket", which are the same thing to everyone downstream: nothing
    /// was risked, so there is nothing to replay.
    zero_rtt: Option<ZeroRtt>,
    /// This connection's identity for the observability seam, and the flag
    /// that keeps its end from being announced twice — `None` when nobody
    /// is watching. It lives with the connection rather than with a
    /// request because the connection is what outlives both.
    state: Option<Arc<ConnState>>,
}

/// What a checkout did, carried **out of the pool's mutex** so the events
/// can be emitted with no lock held.
///
/// `made` is the branch itself rather than a flag beside it: there is
/// nothing anywhere that says "this one was reused", only the code path it
/// came back on. That is `hclient-native`'s discipline for the same event,
/// and the reason it cannot be right for the wrong reason.
struct CheckedOut {
    send: SendRequest,
    zero_rtt: Option<ZeroRtt>,
    conn: quinn::Connection,
    state: Option<Arc<ConnState>>,
    /// `Some` when this connection was **made**; `None` when it was found.
    made: Option<Made>,
}

/// The two intervals a fresh connection can report, measured where they
/// happen. `total` is not here: it ends after `checkout` returns, and is
/// stamped by `execute` from the same mark `Head::elapsed` uses.
struct Made {
    remote: SocketAddr,
    dns: Duration,
    handshake: Duration,
}

struct Shared {
    /// One endpoint per address family, bound on first use.
    ///
    /// **Not one dual-stack v6 endpoint serving both**, which is the
    /// tempting shape and the wrong one. A wildcard v6 socket reaches a v4
    /// peer only through v4-mapped addresses, and this workspace has just
    /// finished documenting what dual-stack costs on that path: `IP_RECVTOS`
    /// is unsupported on dual-stack sockets on macOS and iOS
    /// (`quinn-udp-0.5.15/src/unix.rs:114`), so such an endpoint reports
    /// `ecn: false` there — for *every* connection, including the v4 ones
    /// that would have had ECN on a socket of their own.
    ///
    /// Two endpoints cost one extra `UdpSocket` on a host that talks to
    /// both families, and nothing at all on a host that talks to one.
    endpoints: Mutex<HashMap<bool, quinn::Endpoint>>,
    conns: Mutex<HashMap<PoolKey, Pooled>>,
}

impl fmt::Debug for Shared {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Shared")
    }
}

/// How often a pooled connection sends a PING when nothing else is
/// travelling on it.
///
/// # A spawned driver is necessary and not sufficient, which is a finding
///
/// The research established that an unpolled
/// QUIC connection dies across an idle gap, and that a *driven* one
/// survives. Building it here turned up the other half: **the driver alone
/// is not enough.** With a driver spawned and no keep-alive configured, the
/// same 1500 ms gap under a 1000 ms idle timeout still killed the
/// connection — because driving a connection is what lets it *send* a PING,
/// not what makes it *decide* to. The decision is
/// `TransportConfig::keep_alive_interval`, and quinn leaves it unset by
/// default.
///
/// `an_idle_connection_survives_only_because_of_the_keep_alive` in
/// `tests/live.rs` is that pair, with the driver spawned in both arms so the
/// keep-alive is the only difference.
///
/// # Why five seconds, and what it does not promise
///
/// It has to be comfortably under the *peer's* idle timeout, and the peer's
/// idle timeout is not something a client can read: QUIC negotiates the
/// effective value as the minimum of the two ends' `max_idle_timeout`
/// transport parameters, and quinn exposes no accessor for the result. Five
/// seconds is under every common server default (nginx and Caddy both sit
/// at 30 s), and a server that idles a connection out faster than this will
/// still drop it — at which point the next request opens a fresh
/// connection, which is a cost rather than a failure.
///
/// The trade is real and worth stating: a pooled QUIC connection either
/// gets pinged or dies, so a client holding one idle for an hour sends 720
/// PINGs. [`H3::keep_alive_interval`] and [`H3::without_keep_alive`] exist
/// for callers who would rather pay the handshake.
pub const DEFAULT_KEEP_ALIVE: std::time::Duration = std::time::Duration::from_secs(5);

/// The HTTP/3 transport.
///
/// # `H`, the observability hook
///
/// [`NoHooks`] by default, a zero-sized type whose `Hooks::WATCHING` is
/// `false` — so `H3<R, T, D>` still names the transport it always named,
/// and a build that asks for nothing reads no clock, allocates no
/// connection state and takes no connection id. [`H3::hooks`] is how a
/// caller asks; what comes back is a *different type*, which is the whole
/// of the zero-cost claim rather than an inconvenience.
///
/// What this transport can and cannot say is in `crate::hooks`: `tcp`
/// holds the QUIC attempt and `tls` is always `None`, because QUIC's
/// handshake is TLS and `into_0rtt` hands back a usable connection before
/// it finishes; `CloseReason::Ended` has no emitter, because nothing in
/// HTTP/3 ends a connection by finishing an exchange.
pub struct H3<R, T, D, H = NoHooks> {
    rt: R,
    tls: T,
    dns: D,
    /// Where the events go. `NoHooks` is a ZST, so this field costs a
    /// build that wants nothing exactly nothing.
    hooks: H,
    caps: Capabilities,
    keep_alive: Option<std::time::Duration>,
    shared: Arc<Shared>,
}

// Hand-written rather than derived, and not requiring `R: Debug` etc.: a
// derive would put a `Debug` bound on all three parameters, which is a
// bound on the runtime, the TLS backend and the resolver for the benefit of
// a formatter. What is worth printing is the capability set anyway.
impl<R, T, D, H> fmt::Debug for H3<R, T, D, H> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("H3")
            .field("early_data", &self.caps.early_data)
            .finish_non_exhaustive()
    }
}

impl<R, T, D> H3<R, T, D, NoHooks>
where
    T: QuicTlsConnect,
{
    /// # The capability values, one by one
    ///
    /// - `streaming_request_body: true` and `full_duplex: true`. HTTP/3
    ///   supports both — the request and response halves of a stream are
    ///   independent — and as of this change **so does this
    ///   implementation**: `one_attempt` splits the
    ///   stream, writes the body from a future polled beside
    ///   `recv_response`, and hands the unfinished write to [`H3Body`].
    ///   Neither field is set from what the protocol can do; both were
    ///   `false` for as long as `execute` wrote the whole body first, and
    ///   `full_duplex` is the one whose over-claim costs a deadlock rather
    ///   than a degradation, so it is measured from outside rather than
    ///   argued: `a_response_head_arrives_while_the_request_body_is_still_
    ///   going_out` in `tests/streaming.rs` deadlocks if it is not true.
    /// - `response_trailers: true`. [`H3Body`] yields them as a trailers
    ///   frame; `request_trailers` stays `false` because nothing here sends
    ///   any — and now that a caller can supply a body that produces them,
    ///   that `false` is enforced rather than merely declared:
    ///   [`RequestTrailersNotSent`].
    /// - `connection_reuse: Supported`, and here it means more than it does
    ///   for HTTP/1: requests share a connection *concurrently*, not only
    ///   in sequence.
    /// - `early_data` follows the TLS backend's own
    ///   [`offers_early_data`](QuicTlsConnect::offers_early_data), which
    ///   defaults to `false`. It is never set from a constant here: the
    ///   capability has to come from the component that knows.
    /// - `version_reported: true`, and `version_select: true` — which is
    ///   not a claim to choose anything. This transport speaks exactly one
    ///   version and *honours* a per-request
    ///   [`RequireVersion`](hclient_core::RequireVersion): `HTTP_3`
    ///   proceeds, anything else is
    ///   [`VersionNotAvailable`](hclient_core::VersionNotAvailable) before
    ///   a packet goes out. `false` would make `Client` refuse the one
    ///   demand this transport meets by construction.
    /// - `timeouts`: `connect` is `true` and enforced in `execute` — it
    ///   bounds resolution, the QUIC handshake and h3's settings exchange
    ///   together, the same scope `hclient-native` gives the same setting.
    ///   `first_byte` and `between_bytes` stay `false`, and the comment on
    ///   `capabilities` says what each of them would cost.
    pub fn new(rt: R, tls: T, dns: D) -> Result<Self, Error> {
        let early_data = if tls.offers_early_data() {
            EarlyDataSupport::Supported
        } else {
            EarlyDataSupport::None
        };
        let client_certs = tls.presents_client_certs();
        Ok(Self {
            rt,
            tls,
            dns,
            hooks: NoHooks,
            caps: capabilities(early_data, client_certs),
            keep_alive: Some(DEFAULT_KEEP_ALIVE),
            shared: Arc::new(Shared {
                endpoints: Mutex::new(HashMap::new()),
                conns: Mutex::new(HashMap::new()),
            }),
        })
    }
}

/// Everything a `H3` can be configured with, whatever its hook — separated
/// from [`H3::new`] above only because `new` is the one method that names a
/// *particular* `H` ([`NoHooks`]), and putting it in this block would make
/// `H3::<_, _, _, MyHook>::new` a thing a caller could write and get a
/// hookless transport from. The same split `hclient-native` makes, for the
/// same reason.
impl<R, T, D, H> H3<R, T, D, H>
where
    T: QuicTlsConnect,
{
    /// Send this transport's events to `hooks` — see
    /// [`hclient_core::unversioned::Hooks`] for what it hears and what it
    /// costs, [`Event`] for the vocabulary, and `crate::hooks` for the
    /// two things QUIC cannot say in it.
    ///
    /// **It returns a different type**, and that is the zero-cost
    /// mechanism: the hook is a type parameter, so the `NoHooks` build
    /// monomorphises to code with no clock reads in it at all, where a
    /// `Box<dyn Hooks>` field would leave every no-hook build carrying a
    /// null check on the request path.
    ///
    /// The hook may be `!Send`: nothing on this path declares it, so an
    /// `Rc` inside a hook makes this transport `!Send` and leaves it
    /// working (P13; `crates/hclient-core/tests/shape.rs`). What that
    /// costs here is written down in `crate::hooks` — the spawned
    /// connection driver is `Send` because quinn says so, so a hook cannot
    /// be called from it, and a close is discovered rather than observed.
    ///
    /// The pool travels across this call, because a connection's identity
    /// lives with the connection rather than with the hook.
    pub fn hooks<H2>(self, hooks: H2) -> H3<R, T, D, H2> {
        H3 {
            rt: self.rt,
            tls: self.tls,
            dns: self.dns,
            hooks,
            caps: self.caps,
            keep_alive: self.keep_alive,
            shared: self.shared,
        }
    }

    /// Ping an idle pooled connection this often. See
    /// [`DEFAULT_KEEP_ALIVE`], which is what this starts at.
    pub fn keep_alive_interval(mut self, d: std::time::Duration) -> Self {
        self.keep_alive = Some(d);
        self
    }

    /// Send no keep-alive at all.
    ///
    /// **This does not make pooled connections cheaper, it makes them
    /// shorter-lived**, and the difference is measured rather than
    /// asserted: `tests/live.rs`'s idle pair runs both arms with the driver
    /// spawned, and the arm without a keep-alive loses its connection
    /// across a gap the other one survives. For a client that makes one
    /// burst of requests and then goes quiet for a long time, that is the
    /// right trade — the connection was going to be replaced anyway, and
    /// this way it is not being pinged in the meantime.
    pub fn without_keep_alive(mut self) -> Self {
        self.keep_alive = None;
        self
    }
}

/// Built from [`Capabilities::none()`] and turned on field by field, not
/// written out as a struct literal — `Capabilities` is `#[non_exhaustive]`,
/// so a literal would not compile from outside `hclient-core`, and the
/// consequence is the one that matters here: a field added to the struct
/// later arrives at this transport as the conservative default rather than
/// as a compile error somebody silences by copying the neighbouring value.
fn capabilities(early_data: EarlyDataSupport, client_certs: bool) -> Capabilities {
    let mut c = Capabilities::none();
    // Both are `true` because `one_attempt` does both, not because HTTP/3
    // does. They were `false` for as long as `execute` wrote the whole
    // request body and then read the head, which is the arrangement the
    // capability model exists to describe honestly; they moved in the
    // change that split the stream and made the write a future polled
    // beside `recv_response`.
    //
    // `full_duplex` is the one that costs a deadlock when over-claimed —
    // v0.2 W3's floor rule — so it is not declared on the strength of the
    // code reading right. `a_response_head_arrives_while_the_request_body_
    // is_still_going_out` in `tests/streaming.rs` is causal rather than
    // timed: the caller's body will not produce its second chunk until the
    // head has been seen, so a transport that read the head only after the
    // body finished cannot complete that exchange at all.
    c.streaming_request_body = true;
    c.full_duplex = true;
    // Nothing here sends request trailers; `H3Body` does yield response
    // ones, as a trailers frame. With a streaming request body a caller can
    // now actually produce a trailers frame, so this `false` is enforced —
    // `RequestTrailersNotSent`, a typed error, rather than a silent drop.
    c.request_trailers = false;
    c.response_trailers = true;
    // A 3xx arrives as an ordinary response and following it is `Client`'s
    // job — `Transparent`, not `None`, which would be the stronger claim
    // that redirects are impossible.
    c.redirects = RedirectSupport::Transparent;
    // Dropping the `execute` future or the body sends `STOP_SENDING` for
    // that stream, through `RequestStream`'s own `Drop`.
    c.cancel_on_drop = CancelSupport::Supported;
    // And here it means more than it does over HTTP/1: requests share a
    // connection *concurrently*, not merely in sequence.
    c.connection_reuse = ReuseSupport::Supported;
    c.response_decompression = DecompressionSupport::None;
    // Read from the TLS backend, never from a constant: the capability has
    // to come from the component that knows, and this one defaults to
    // `None` in the trait for a reason whose cost is replay exposure.
    c.early_data = early_data;
    c.tls_config = TlsSupport::Full;
    // The same rule as `early_data` three lines up, and it took until v0.4
    // to obey it here: this was `true` unconditionally, which over-claimed
    // for every `T` that presents no certificate — including the one this
    // module's own tests use. `TlsIdentity::presents_client_certs` is
    // where the component that knows now answers.
    c.client_certs = client_certs;
    // One version — and honouring a demand is not the same as choosing
    // one. `execute` reads `RequireVersion` before it does anything else:
    // `HTTP_3` proceeds, everything else is `VersionNotAvailable` with not
    // a packet sent. `false` here would make `Client` refuse
    // `RequireVersion(HTTP_3)` at the `UnsupportedCapability` gate, which
    // is the one demand this transport satisfies by construction.
    c.version_select = true;
    c.version_reported = true;
    // `connect` is declared and enforced in the same change, which is v0.2
    // W4's rule and the reason it was `false` until now rather than
    // "cheapest to add" and added: `execute` reads `Timeouts::connect` from
    // the request's extensions and races the whole
    // resolve-plus-QUIC-plus-h3-handshake against `Timer::sleep`, ending in
    // `ErrorKind::Timeout(Phase::Connect)` with a `ConnectTimedOut` source.
    // `tests/live.rs` measures it against a UDP black hole, with the
    // control that must still be waiting when the bounded one has already
    // given up.
    //
    // The other two stay honestly `false`. Neither is a line of code away:
    // `first_byte` would have to bound `one_attempt` and then decide what a
    // 0-RTT replay does to the budget, and `between_bytes` needs a body
    // wrapper holding a sleep, the shape `hclient_native::IdleTimeout` has.
    // Declaring either without that is the silent no-op this field exists
    // to make impossible.
    c.timeouts = TimeoutSupport {
        resolve: true,
        connect: true,
        first_byte: false,
        between_bytes: false,
    };
    c
}

/// The runtime capabilities this transport needs, in one place.
///
/// Four bounds, three of which are `quinn`'s and not this workspace's:
/// `Send`, `Sync` and `'static` are declared on `quinn::{Runtime,
/// AsyncTimer, AsyncUdpSocket}` and are paid **here**, by the crate that
/// wants QUIC, rather than on [`UdpBind`] where every implementer would pay
/// them. See `hclient_quinn`'s crate doc.
pub trait H3Runtime:
    Timer
    + UdpBind
    + UdpAdoptStd
    + Spawn<QuinnTask>
    + Clone
    + Send // send-bound-exception: amendment-C10
    + Sync // send-bound-exception: amendment-C10
    + 'static
{
}

impl<R> H3Runtime for R
where
    R: Timer + UdpBind + UdpAdoptStd + Spawn<QuinnTask> + Clone + Send + Sync + 'static, // send-bound-exception: amendment-C10
    R::Sleep: Send + 'static, // send-bound-exception: amendment-C10
    R::Socket: fmt::Debug + Send + Sync + 'static, // send-bound-exception: amendment-C10
{
}

impl<R, T, D, H> H3<R, T, D, H>
where
    R: H3Runtime,
    R::Sleep: Send + 'static, // send-bound-exception: amendment-C10
    R::Socket: fmt::Debug + Send + Sync + 'static, // send-bound-exception: amendment-C10
    T: QuicTlsConnect,
    D: hclient_dns::Resolve,
    // `H: Clone` for the reason `hclient-native` gives: the response body
    // outlives `execute` and reports the connection's end from
    // `poll_frame`, so it needs a hook of its own rather than a borrow of
    // this transport's. There is deliberately **no `H: Unpin`** here, which
    // is where this backend is cheaper than that one: `H3Body` holds its
    // hook behind an `Option<Box<..>>`, and a `Box` is `Unpin` whatever it
    // contains.
    H: Hooks + Clone,
{
    /// The endpoint, built on first use.
    ///
    /// Lazily, and inside the request future rather than in `new`: binding
    /// a socket registers it with a reactor, which on tokio's ZST runtime
    /// requires being inside the runtime — and a client is usually built
    /// outside one. `TokioHandle` carries its handle and does not have that
    /// constraint, but `H3` is generic and cannot assume it.
    fn endpoint(&self, peer: SocketAddr) -> Result<quinn::Endpoint, Error> {
        let v6 = peer.is_ipv6();
        let mut slots = self
            .shared
            .endpoints
            .lock()
            .expect("endpoint mutex poisoned");
        if let Some(e) = slots.get(&v6) {
            return Ok(e.clone());
        }
        let wildcard = if v6 {
            SocketAddr::from(([0u8; 16], 0))
        } else {
            SocketAddr::from(([0, 0, 0, 0], 0))
        };
        let bound = hclient_quinn::endpoint(&self.rt, wildcard)
            .map_err(|e| Error::new(ErrorKind::Connect, e))?;
        slots.insert(v6, bound.clone());
        Ok(bound)
    }

    /// A connection for this key, from the pool or newly made.
    ///
    /// The check on the way out is `close_reason()`: quinn reports a
    /// connection the peer or a timer already closed, and a pool that
    /// handed one out would fail the request it was reused for. This is the
    /// same "poll at checkout" W2's HTTP/1 pool does, in the form this
    /// stack offers it.
    ///
    /// # Where the `Stale` event comes from, and why not from a `Drop`
    ///
    /// A dead entry is **removed under the lock** and reported after it,
    /// which buys two things at once: exactly one caller can report a given
    /// connection even when several arrive at the same instant, and no hook
    /// runs with the pool's mutex held — the rule
    /// `hclient_core::unversioned::hooks` states, and the one a panicking
    /// hook would otherwise turn into a poisoned pool.
    ///
    /// It is reported here rather than carried out to `execute`, because a
    /// connect that then fails or times out must not swallow the fact that
    /// the pooled connection really was found dead.
    async fn checkout(
        &self,
        key: &PoolKey,
        addr: SocketAddr,
        dns: Duration,
    ) -> Result<CheckedOut, Error> {
        let stale = {
            let mut pool = self.shared.conns.lock().expect("pool mutex poisoned");
            if let Some(p) = pool.get(key)
                && p.conn.close_reason().is_none()
            {
                // A clone, not a removal: `SendRequest` is `Clone` precisely so
                // several requests can be in flight on one connection, which is
                // the multiplexing this transport exists for. Contrast
                // `hclient-native`'s h2, which takes the connection OUT of the
                // pool for the duration of one exchange.
                //
                // So `Reused` here says something `hclient-native`'s cannot:
                // the connection may be carrying somebody else's request
                // right now. The event's own words — "a connection somebody
                // else already made is being used again" — stay true; what
                // changes is that a caller must not read two `Reused` events
                // as two consecutive uses.
                return Ok(CheckedOut {
                    send: p.send.clone(),
                    zero_rtt: p.zero_rtt.clone(),
                    conn: p.conn.clone(),
                    state: p.state.clone(),
                    made: None,
                });
            }
            pool.remove(key).and_then(|p| p.state)
        };
        if let Some(dead) = stale {
            dead.closed(&self.hooks, CloseReason::Stale);
        }
        let (send, conn, zero_rtt, handshake) = self.connect(key, addr).await?;
        let state = ConnState::new::<H>();
        // quinn's own answer rather than the address that was dialled: the
        // seam asks for "the address that answered".
        let remote = conn.remote_address();
        self.shared
            .conns
            .lock()
            .expect("pool mutex poisoned")
            .insert(
                key.clone(),
                Pooled {
                    send: send.clone(),
                    conn: conn.clone(),
                    zero_rtt: zero_rtt.clone(),
                    state: state.clone(),
                },
            );
        Ok(CheckedOut {
            send,
            zero_rtt,
            conn,
            state,
            made: Some(Made {
                remote,
                dns,
                handshake,
            }),
        })
    }

    /// Make one — with the **one fallback the early-data path owes**.
    ///
    /// The fourth element of the answer is what [`ConnectTiming`]'s `tcp`
    /// field holds here: **the attempt itself**, from `connect_with` to a
    /// connection that can carry a request. Binding the endpoint and
    /// building the crypto configuration are before the mark and h3's
    /// SETTINGS exchange is after it, so both land in `total` and in no
    /// phase — which is exactly what `ConnectTiming` means by "three
    /// measurements, not a decomposition". `crate::hooks` says why there is
    /// no `tls` figure to go with it.
    ///
    /// # A 0-RTT rejection can land on h3's control stream, and it did
    ///
    /// [`crate::early`]'s table says a server rejecting the 0-RTT keys is
    /// *"replayed on the same connection once the handshake completes; the
    /// caller sees a normal response"*, and [`Self::finish`] is where that
    /// happens. It covers a rejection that arrives on the **request**
    /// stream. One that arrives a few microseconds earlier arrives on the
    /// **control** stream instead — h3 opens it in early data like every
    /// other stream on a connection `into_0rtt` handed back — and RFC 9114
    /// §6.2.1 obliges h3 to answer that with `H3_CLOSED_CRITICAL_STREAM`,
    /// which **closes the QUIC connection**. [`Self::finish`] never runs;
    /// there is no request stream for it to replay.
    ///
    /// So the rejection reached the caller as an `ErrorKind::Connect`, on a
    /// request whose only sin was carrying [`hclient_core::AllowEarlyData`].
    /// It was found as a flake — 2 failures in 277 concurrent runs of this
    /// crate's suite, 0 in 846 after.
    ///
    /// The fallback below is one step
    /// later, and its sentence holds verbatim: *"nothing was sent, so
    /// falling through to a full handshake risks nothing"*. Nothing had been
    /// sent — the request has not been formed at this point, let alone
    /// written — so this is not a retry and needs no `RetryKind`.
    ///
    /// Three things about it are decisions rather than mechanics:
    ///
    /// - **The condition is "we took the shortcut and the h3 client could
    ///   not be built on it", and not the 0-RTT verdict.** Awaiting
    ///   `quinn::ZeroRttAccepted` here would not discriminate: it resolves
    ///   `false` for a rejection *and* for a connection that died some other
    ///   way, because `terminate` sends `false` on a connection lost before
    ///   the handshake (`quinn-0.11.11/src/connection.rs:1224`). A condition
    ///   whose two arms cannot be told apart is one no test can pin, so this
    ///   asks the question it can answer.
    /// - **Exactly one extra dial, and only for a marked request.** Every
    ///   other failure — the crypto configuration, the endpoint, the
    ///   `connect_with`, the full handshake on the refusal path — returns
    ///   as it always did, because doubling a failing connect is how a
    ///   caller's `Timeouts::connect` gets spent twice.
    /// - **One mark for both attempts.** `ConnectTiming::tcp` is "the
    ///   attempt", and an attempt that spent a discarded early-data
    ///   connection spent it. Re-marking would report the fallback's
    ///   handshake as the whole cost.
    async fn connect(
        &self,
        key: &PoolKey,
        addr: SocketAddr,
    ) -> Result<(SendRequest, quinn::Connection, Option<ZeroRtt>, Duration), Error> {
        // The attempt's launch — see this function's doc comment. Under
        // `NoHooks` this is a compile-time `None` and no clock is read.
        let launched = mark::<H, R>(&self.rt);
        if key.early_data {
            match self.dial(key, addr, launched, true).await {
                Ok(made) => return Ok(made),
                Err(DialFailed::EarlyDataLost(_)) => {}
                Err(DialFailed::Fatal(e)) => return Err(e),
            }
        }
        self.dial(key, addr, launched, false)
            .await
            .map_err(DialFailed::into_error)
    }

    /// One dial, with the 0-RTT shortcut taken or not taken.
    ///
    /// `early` is *not* `key.early_data`: the key says what kind of
    /// connection this is (and is what a later request has to match to
    /// reuse it), while this says whether **this attempt** may put anything
    /// into early data. [`Self::connect`]'s fallback is the one place they
    /// differ, and the difference is the whole of RFC 8470's *"MUST NOT be
    /// sent in early data"* here: the only way application data can reach a
    /// 0-RTT packet is through a `Connection` that `into_0rtt` handed back,
    /// so not calling it is not a promise but an absence.
    async fn dial(
        &self,
        key: &PoolKey,
        addr: SocketAddr,
        launched: Option<R::Instant>,
        early: bool,
    ) -> Result<(SendRequest, quinn::Connection, Option<ZeroRtt>, Duration), DialFailed> {
        let crypto = self.tls.quic_client_config(QuicTlsRequest {
            alpn: &[ALPN_H3],
            ech: None,
            early_data: key.early_data,
        })?;
        let endpoint = self.endpoint(addr)?;
        let mut cfg = quinn::ClientConfig::new(crypto);
        if let Some(d) = self.keep_alive {
            let mut transport = quinn::TransportConfig::default();
            transport.keep_alive_interval(Some(d));
            cfg.transport_config(Arc::new(transport));
        }
        // `bare_host`, because `key.host` is `Uri::host()`'s answer and an
        // IPv6 literal still wears the brackets RFC 3986 §3.2.2 gives the
        // authority. quinn turns this argument into a
        // `rustls_pki_types::ServerName`, which reads `[::1]` as neither a
        // name nor an address — `InvalidServerName`, before a packet
        // leaves. The same duty `hclient_tls::TlsRequest::server_name`
        // states for the TCP seam; the QUIC seam has no field for the name
        // at all, so this call is where it lands.
        //
        // The key keeps the bracketed form: it is the pool's identity, and
        // matching it against the URI a request arrives with is what a
        // second request has to do to be reused. Normalising twice, once
        // for the key and once here, would be the two-places-drifting
        // problem `bare_host`'s doc is about.
        let connecting = endpoint
            .connect_with(cfg, addr, hclient_core::bare_host(&key.host))
            .map_err(|e| Error::new(ErrorKind::Connect, e))?;

        // The round trip 0-RTT exists to skip, actually skipped.
        //
        // `into_0rtt` hands back a usable `Connection` *before the
        // handshake completes* when a remembered ticket supplies key
        // material, and hands the `Connecting` back untouched when it does
        // not. The second case is the first of the three failure paths and
        // the only free one: nothing was sent, so falling through to a full
        // handshake risks nothing and tells the caller nothing.
        //
        // It is reached only when `early` is set, which is only when the
        // caller marked this request — see `crate::early` — and only on the
        // first of this connect's at most two attempts.
        let (conn, zero_rtt) = if early {
            match connecting.into_0rtt() {
                Ok((conn, accepted)) => (conn, Some(futures_util::FutureExt::shared(accepted))),
                Err(connecting) => (
                    connecting
                        .await
                        .map_err(|e| Error::new(ErrorKind::Connect, e))?,
                    None,
                ),
            }
        } else {
            (
                connecting
                    .await
                    .map_err(|e| Error::new(ErrorKind::Connect, e))?,
                None,
            )
        };

        // The connection can carry a request from here — which for a 0-RTT
        // connection is *before its handshake has completed*, and is
        // precisely why there is no `tls` duration to report beside this
        // one: there is no completed handshake yet to have timed.
        let handshake = since::<R>(&self.rt, launched);

        // The control stream, and the failure [`Self::connect`]'s fallback
        // exists for. `zero_rtt.is_some()` is exactly "this connection's
        // streams went out in early data", so it is the whole condition:
        // on any other connection an h3 client that cannot be built is a
        // connect failure and nothing else.
        let (mut driver, send) = match h3::client::builder()
            .build(h3_quinn::Connection::new(conn.clone()))
            .await
        {
            Ok(built) => built,
            Err(e) => {
                let e = Error::new(ErrorKind::Connect, std::io::Error::other(e.to_string()));
                return Err(match zero_rtt {
                    Some(_) => DialFailed::EarlyDataLost(e),
                    None => DialFailed::Fatal(e),
                });
            }
        };

        // The driver, spawned. This is the whole reason for the `Spawn`
        // bound: an h3 connection whose control streams nobody polls stops
        // answering, and a pooled one stops answering between requests,
        // which is when a pool is the only thing holding it.
        //
        // Boxed into `QuinnTask` rather than given a `Spawn` bound of its
        // own: `Spawn<F>` puts the future in the TRAIT, so a bound must
        // name it and an `async` block has no name — `Pin<Box<dyn Future +
        // Send>>` does, and it is the same type quinn itself hands over.
        self.rt.spawn(Box::pin(async move {
            let _ = std::future::poll_fn(|cx| driver.poll_close(cx)).await;
        }) as QuinnTask);

        Ok((send, conn, zero_rtt, handshake))
    }

    /// Open a stream, start writing the request body, and read the head —
    /// **both at once**.
    ///
    /// Separate from `execute` because it is the unit a rejected 0-RTT
    /// request is replayed as — see there.
    ///
    /// # This is where `full_duplex` is true or false
    ///
    /// The stream is [`split`](h3::client::RequestStream::split) into its
    /// two independent halves and the write side becomes a future
    /// ([`crate::pump`]) polled beside `recv_response`, rather than awaited
    /// before it. The head is returned the moment it arrives, with the
    /// unfinished write handed to [`H3Body`] to carry on. Writing the body
    /// first and reading the head afterwards — the arrangement this
    /// replaced — is precisely what `full_duplex: false` described, and
    /// putting the two `await`s back in sequence would silently withdraw
    /// the capability while leaving its declaration standing.
    ///
    /// **The pump is polled first, and its errors return before the
    /// response is looked at.** That keeps the meaning the sequential code
    /// had: everything except the one tolerated write failure is the
    /// request failing, and it is reported as such rather than as a
    /// response that happens to be missing its request.
    async fn one_attempt(
        send: &mut SendRequest,
        head: http::Request<()>,
        body: RequestBody,
        watch: Option<Box<Watch<H>>>,
    ) -> Result<http::Response<H3Body<H>>, Error> {
        let stream = send.send_request(head).await.map_err(body::stream_error)?;
        // From here on the head is on the wire, and a write-side failure
        // stops meaning what it meant a line ago. See `write_after_head`.
        let (writer, mut reader) = stream.split();
        let mut pump = Some(pump::pump(writer, body));
        // The borrow of `reader` ends with this block, which is what lets
        // the same value move into `H3Body` two lines later.
        let resp = {
            let mut head = std::pin::pin!(reader.recv_response());
            std::future::poll_fn(|cx| {
                if let Some(p) = pump.as_mut() {
                    match p.as_mut().poll(cx) {
                        std::task::Poll::Ready(Ok(())) => pump = None,
                        std::task::Poll::Ready(Err(e)) => {
                            pump = None;
                            return std::task::Poll::Ready(Err(e));
                        }
                        // Not returned: a write that cannot proceed must
                        // not stop the response from arriving, which is
                        // the entire point of the two halves being
                        // independent.
                        std::task::Poll::Pending => {}
                    }
                }
                head.as_mut().poll(cx).map_err(body::stream_error)
            })
            .await?
        };
        let (parts, ()) = resp.into_parts();
        Ok(http::Response::from_parts(
            parts,
            H3Body::new(reader, pump, watch),
        ))
    }

    /// The hook, the connection and its state, packed for a body — or
    /// `None` when nobody is watching, which is the only case in which the
    /// box is not allocated.
    fn watch(
        &self,
        conn: &quinn::Connection,
        state: &Option<Arc<ConnState>>,
    ) -> Option<Box<Watch<H>>> {
        let state = state.clone()?;
        Some(Box::new(Watch::new(
            self.hooks.clone(),
            conn.clone(),
            state,
        )))
    }

    /// The `Head` event, from the one place all three attempts reach it —
    /// the first, the 0-RTT replay, and nothing else.
    ///
    /// A method rather than three copies, because a caller counting heads
    /// must not be able to tell those paths apart: a rejected 0-RTT request
    /// that was replayed is one request that got one response, and a second
    /// `Head` for it would report a request the caller never made.
    /// A request that failed, told to the hook **only if the connection
    /// under it went away**.
    ///
    /// The discrimination is `Watch::failed`'s, and it is the whole
    /// difference between this transport and one that does not share
    /// connections: over HTTP/1 a failed exchange is a failed connection,
    /// and here it usually is not.
    fn report_failed(&self, watch: &Option<Box<Watch<H>>>, e: &Error) {
        if let Some(w) = watch.as_deref() {
            w.failed(e);
        }
    }

    fn report_head(
        &self,
        resp: &http::Response<H3Body<H>>,
        id: ConnectionId,
        uri: &http::Uri,
        began: Option<R::Instant>,
    ) {
        self.hooks.on(Event::Head(Head {
            id,
            uri,
            status: resp.status(),
            // `Some`, because this transport knows: it speaks HTTP/3 and
            // refuses every other demand. That is the same claim
            // `capabilities()` makes with `version_reported: true`, and
            // `Head::version`'s doc says the two must agree.
            version: Some(resp.version()),
            elapsed: since::<R>(&self.rt, began),
        }));
    }

    async fn resolve(&self, host: &str, port: u16) -> Result<SocketAddr, Error> {
        use futures_util::StreamExt;
        // `bare_host` first: `host` came from `Uri::host()`, so `[::1]`
        // arrives with its brackets and `str::parse::<IpAddr>` rejects it.
        // Without this the shortcut misses every IPv6 literal and the
        // address goes to the resolver instead — where `getaddrinfo("[::1]")`
        // fails and the request with it. `hclient-native` does not have
        // this half of the defect only because `IpLiteralOnly::literal`
        // strips on the way in; a shortcut in front of the resolver has to
        // strip for itself.
        if let Ok(ip) = hclient_core::bare_host(host).parse::<std::net::IpAddr>() {
            return Ok(SocketAddr::new(ip, port));
        }
        // v6 first, then v4. Not happy eyeballs: QUIC's connect is not a
        // TCP SYN race, and racing two QUIC handshakes would mean two
        // handshakes' worth of crypto for one request. `hclient-native`'s
        // `happy_eyeballs` is the right tool over TCP and the wrong one
        // here, which is why it is not reused.
        let mut v6 = Box::pin(self.dns.lookup_ipv6(host));
        while let Some(r) = v6.next().await {
            if let Ok(a) = r {
                return Ok(SocketAddr::new(a.addr, port));
            }
        }
        let mut v4 = Box::pin(self.dns.lookup_ipv4(host));
        while let Some(r) = v4.next().await {
            if let Ok(a) = r {
                return Ok(SocketAddr::new(a.addr, port));
            }
        }
        Err(Error::new(
            ErrorKind::Resolve,
            std::io::Error::other(format!("no address for {host}")),
        ))
    }
}

/// A write on the request stream **after the head is on the wire**, and the
/// one failure of it that is not a failure of the request.
///
/// `Ok(true)` means the peer stopped reading; there is nothing more to
/// write, and the response is still coming.
///
/// # RFC 9114 §4.1, and the mechanism that makes it a live case
///
/// *"When the server does not need to receive the remainder of the request,
/// it MAY abort reading the request stream, send a complete response, and
/// cleanly close the sending part of the stream. Clients **MUST NOT**
/// discard complete responses as a result of having their request
/// terminated abruptly."* A `404`, a `401`, a `413` — every server that
/// answers without reading the body does this.
///
/// On quinn it happens without the server intending anything: dropping a
/// `quinn::RecvStream` that has not been read to the end sends
/// `STOP_SENDING(0)` (`quinn-0.11.11/src/recv_stream.rs:534`), which
/// `h3-quinn` maps to `StreamTerminated` (`h3-quinn-0.0.10/src/lib.rs:425`)
/// and h3 to [`h3::error::StreamError::RemoteTerminate`]. **It reaches
/// even an empty-bodied request**, because h3 writes a grease frame in
/// `RequestStream::finish` on the first request of a connection
/// (`h3-0.0.8/src/connection.rs:1101-1119`) — so `finish()` is a real write
/// and can lose the race with the server's `STOP_SENDING`.
///
/// That is how this was found: not by reading the RFC, but as a test that
/// failed roughly once in twenty under load and never once in isolation,
/// with `Remote reset: 0x0` and a response the client had thrown away.
///
/// # With a streaming body this is the ordinary case, not the rare one
///
/// The paragraphs above were written when every request body was one
/// buffered chunk, so "the peer stopped reading" could only interrupt a
/// single `send_data` or the grease frame in `finish`. A streaming body
/// meets it in the middle of a loop, at whichever frame the
/// `STOP_SENDING` happens to land on, and the second duty falls out of
/// that: `Ok(true)` must **stop the pump**, not merely skip `finish`.
/// Continuing to pull frames from the caller's body would be feeding a
/// stream nobody is reading, for as long as the producer keeps producing.
/// `crate::pump::write_stream` returns on the `true`, and
/// `a_streaming_body_stops_when_the_server_stops_reading_and_the_response_
/// still_arrives` in `tests/streaming.rs` is the RFC 9114 §4.1 guarantee
/// across many frames rather than one.
///
/// # Why only this variant, and only here
///
/// `STOP_SENDING` acts on one direction. The response stream is untouched
/// — since the streams are split it is a different object entirely — so
/// the read side can still run and is still fully checked; the tolerance
/// is on the write and nowhere else.
///
/// Every other [`h3::error::StreamError`] propagates unchanged — and the
/// honest statement about that half is that **no test here pins it, and
/// none can**.
///
/// Measured, not assumed, and **re-measured after streaming landed**, since
/// a claim about which mutations a suite catches goes stale the moment the
/// suite grows: replacing this match's last two arms with
/// `Err(_) => Ok(true)` leaves the whole `hclient-h3` suite green, all 44
/// tests, `a_connection_that_dies_mid_body_is_still_an_error` in
/// `tests/stop_sending.rs` included — and now also the six streaming tests
/// that go through this function on every frame. The reasoning that expected a red line
/// there — "swallowing a `ConnectionError` means `recv_response` hangs on a
/// dead connection" — is wrong: `recv_response` on a connection whose peer
/// has gone returns an error of its own, and it arrives as the same
/// `ErrorKind::Body` the narrow version produces. From outside, the two
/// spellings are indistinguishable.
///
/// A white-box test cannot close the gap either: every `StreamError`
/// variant is `#[non_exhaustive]` outside h3's
/// `i-implement-a-third-party-backend-…` feature, so a `ConnectionError`
/// cannot be synthesised to feed this function directly.
///
/// So the narrowness is right *by construction* rather than by measurement,
/// and the construction is the only argument for it: `RemoteTerminate` is
/// the one variant whose meaning we know here — one direction stopped, the
/// other untouched — and for every other variant we do not know that the
/// response stream survived, so we do not claim it. Widening the match
/// would not break a test today; it would replace a statement we can defend
/// with one we cannot.
///
/// The tolerance also cannot slide *before* the head by accident: this
/// takes a `Result<(), _>` and `send_request` yields the stream, so the
/// compiler refuses the move.
pub(crate) fn write_after_head(r: Result<(), h3::error::StreamError>) -> Result<bool, Error> {
    match r {
        Ok(()) => Ok(false),
        Err(h3::error::StreamError::RemoteTerminate { .. }) => Ok(true),
        Err(e) => Err(body::stream_error(e)),
    }
}

impl<R, T, D, H> Transport for H3<R, T, D, H>
where
    R: H3Runtime,
    R::Sleep: Send + 'static, // send-bound-exception: amendment-C10
    R::Socket: fmt::Debug + Send + Sync + 'static, // send-bound-exception: amendment-C10
    T: QuicTlsConnect,
    D: hclient_dns::Resolve,
    // Argued where the sibling impl above declares it. `Unpin` is not
    // among them, which is the one bound this backend does not have to
    // charge a hook that `hclient-native` does.
    H: Hooks + Clone,
{
    type Body = H3Body<H>;
    type Error = Error;

    /// `H3::stage` then `H3::finish` — the same two halves
    /// [`crate::StagedConnect`] hands a caller separately, in one call.
    ///
    /// One sequencing with two entry points, for `Native::run`'s reason:
    /// the alternative is two orders of the same steps, and the two would
    /// drift into two transports.
    async fn execute(
        &self,
        req: http::Request<RequestBody>,
    ) -> Result<http::Response<H3Body<H>>, Error> {
        let staged = self.stage(req).await.map_err(|(e, _)| e)?;
        self.finish(staged).await
    }

    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
}

/// Why one [`H3::dial`] did not produce a connection, and whether
/// [`H3::connect`] may try again without early data.
///
/// Two variants rather than a `bool` beside an `Error`, because the
/// distinction is the fallback's whole precondition and a `bool` would let
/// a caller ignore it by accident. It never leaves this module: `connect`
/// resolves it into an `Error`.
enum DialFailed {
    /// The h3 client could not be built on a connection whose streams went
    /// out in early data — see [`H3::connect`]. Nothing of the caller's
    /// request exists yet, so this is the connect's to absorb.
    EarlyDataLost(Error),
    /// Everything else, including every failure of a dial that never
    /// offered early data.
    Fatal(Error),
}

impl DialFailed {
    fn into_error(self) -> Error {
        match self {
            Self::EarlyDataLost(e) | Self::Fatal(e) => e,
        }
    }
}

/// So that `dial`'s ordinary `?`s stay ordinary. The conversion is to
/// [`DialFailed::Fatal`] deliberately: `EarlyDataLost` is claimed at
/// exactly one site, which is what keeps the fallback from widening by
/// accident into a retry of every failing connect.
impl From<Error> for DialFailed {
    fn from(e: Error) -> Self {
        Self::Fatal(e)
    }
}

/// The failure `within_connect` ends in when the timer wins.
///
/// A named type rather than a string, for the reason
/// `hclient_native::FirstByteTimedOut` gives: a caller must be able to tell
/// the phases apart with `Error::source().downcast_ref()`, and to read the
/// bound that was actually in force rather than parse it back out of a
/// message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("no HTTP/3 connection within the connect timeout of {0:?}")]
pub struct ConnectTimedOut(pub std::time::Duration);

/// Races `fut` against `rt.sleep(d)`, and reports `ErrorKind::
/// Timeout(Phase::Connect)` if the sleep wins.
///
/// `poll_fn` polling both arms by hand rather than a `select` combinator,
/// and **`fut` first**, which is the part that is a decision rather than a
/// style: a handshake that completed in the same wake as the timer expiring
/// is a handshake that completed. The opposite order turns every bound into
/// a race the caller loses at the boundary. The same ordering rule as
/// `hclient_native::with_connect_timeout` and `hclient::within`.
async fn within_connect<R, F, T>(rt: &R, d: std::time::Duration, fut: F) -> Result<T, Error>
where
    R: Timer,
    F: Future<Output = Result<T, Error>>,
{
    let mut fut = std::pin::pin!(fut);
    let mut sleep = std::pin::pin!(rt.sleep(d));
    std::future::poll_fn(|cx| {
        if let std::task::Poll::Ready(r) = fut.as_mut().poll(cx) {
            return std::task::Poll::Ready(r);
        }
        if sleep.as_mut().poll(cx).is_ready() {
            return std::task::Poll::Ready(Err(Error::new(
                ErrorKind::Timeout(Phase::Connect),
                ConnectTimedOut(d),
            )));
        }
        std::task::Poll::Pending
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Never called: `H3::new` reads `offers_early_data` and
    /// `presents_client_certs` and nothing else, so a stub is enough to
    /// construct one and look at what it decided.
    #[derive(Debug, Default)]
    struct StubTls {
        early: bool,
        certs: bool,
    }

    impl StubTls {
        /// Named constructors rather than a two-`bool` tuple, so each test
        /// says which axis it is varying and the other is visibly at rest.
        fn early(v: bool) -> Self {
            Self {
                early: v,
                ..Self::default()
            }
        }
        fn certs(v: bool) -> Self {
            Self {
                certs: v,
                ..Self::default()
            }
        }
    }

    impl hclient_tls::TlsIdentity for StubTls {
        fn config_id(&self) -> TlsConfigId {
            TlsConfigId::no_tls()
        }
        fn presents_client_certs(&self) -> bool {
            self.certs
        }
    }

    impl QuicTlsConnect for StubTls {
        fn quic_client_config(
            &self,
            _: QuicTlsRequest<'_>,
        ) -> Result<Arc<dyn quinn_proto::crypto::ClientConfig>, Error> {
            unreachable!("this stub never connects")
        }
        fn offers_early_data(&self) -> bool {
            self.early
        }
    }

    fn h3(tls: StubTls) -> H3<(), StubTls, ()> {
        H3::new((), tls, ()).expect("H3::new does no I/O")
    }

    #[test]
    fn a_pooled_connection_is_kept_alive_by_default() {
        // The DEFAULT, which `tests/live.rs`'s idle A/B cannot reach: both
        // of its arms set the interval explicitly, because a 1000 ms server
        // idle timeout is the only way to run that test in three seconds
        // and `DEFAULT_KEEP_ALIVE` is five. So without this, flipping the
        // default to `None` passes every live test — measured, mutation M9.
        //
        // The default matters as much as the mechanism: a pooled QUIC
        // connection that nobody pings dies between requests, and a
        // transport that pooled connections and did not keep them alive
        // would pay for a pool it could not use.
        assert_eq!(
            h3(StubTls::early(false)).keep_alive,
            Some(DEFAULT_KEEP_ALIVE)
        );
        assert!(
            h3(StubTls::early(false))
                .without_keep_alive()
                .keep_alive
                .is_none()
        );
        assert_eq!(
            h3(StubTls::early(false))
                .keep_alive_interval(std::time::Duration::from_millis(7))
                .keep_alive,
            Some(std::time::Duration::from_millis(7))
        );
    }

    #[tokio::test]
    async fn an_endpoint_is_bound_in_the_peers_address_family() {
        // Not observable from outside the crate, and that is the whole
        // reason it is tested from inside: a wildcard v6 socket reaches
        // 127.0.0.1 through a v4-mapped address and every live test here
        // passes either way (measured — mutation M16 survives the entire
        // integration suite). What it costs is invisible in the same way:
        // on macOS a dual-stack socket cannot ask for `IP_RECVTOS`, so one
        // shared v6 endpoint would report `ecn: false` for the v4
        // connections too.
        let h3: H3<_, _, hclient_dns::IpLiteralOnly> = H3::new(
            hclient_rt_tokio::TokioHandle::current().unwrap(),
            StubTls::early(false),
            hclient_dns::IpLiteralOnly,
        )
        .unwrap();

        let v4 = h3
            .endpoint(SocketAddr::from(([127, 0, 0, 1], 443)))
            .expect("a v4 wildcard bind");
        assert!(
            v4.local_addr().unwrap().is_ipv4(),
            "a v4 peer must be reached from a v4 socket, not through a mapped address"
        );

        let Ok(v6) = h3.endpoint(SocketAddr::from(([0u8; 16], 443))) else {
            eprintln!("skipped the v6 half: this host has no IPv6");
            return;
        };
        assert!(v6.local_addr().unwrap().is_ipv6());
        assert_ne!(
            v4.local_addr().unwrap(),
            v6.local_addr().unwrap(),
            "two families, two endpoints"
        );

        // And each is bound once: a second ask for the same family returns
        // the same socket rather than leaking one per request.
        let again = h3
            .endpoint(SocketAddr::from(([127, 0, 0, 1], 8443)))
            .unwrap();
        assert_eq!(again.local_addr().unwrap(), v4.local_addr().unwrap());
    }

    #[test]
    fn early_data_is_read_from_the_tls_backend_not_from_a_constant() {
        // Both directions, because a constant would be right half the time
        // and this is the capability whose over-claim costs replay
        // exposure rather than a lost optimisation.
        assert_eq!(
            h3(StubTls::early(true)).caps.early_data,
            EarlyDataSupport::Supported
        );
        assert_eq!(
            h3(StubTls::early(false)).caps.early_data,
            EarlyDataSupport::None
        );
    }

    #[test]
    fn client_certs_is_read_from_the_tls_backend_not_from_a_constant() {
        // This was `c.client_certs = true` unconditionally, two lines
        // under the comment stating the rule it broke — so the arm that
        // matters is the `false` one, which no constant can produce.
        assert!(!h3(StubTls::certs(false)).caps.client_certs);
        assert!(h3(StubTls::certs(true)).caps.client_certs);
    }
}
