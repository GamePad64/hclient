//! HTTP/3 for http-ng: QUIC over the runtime seam's UDP capability.
//!
//! ```no_run
//! # async fn doc() -> Result<(), Box<dyn std::error::Error>> {
//! use http_ng_h3::H3;
//! let h3 = H3::new(
//!     http_ng_rt_tokio::TokioHandle::current()?,
//!     http_ng_tls_rustls::Rustls::with_webpki_roots(),
//!     http_ng_dns_system::SystemDns::new(http_ng_rt_tokio::TokioHandle::current()?),
//! )?;
//! let client = http_ng::Client::builder(h3).build()?;
//! # Ok(()) }
//! ```
//!
//! # Its own crate, and not a feature of `http-ng-native`
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
//! something to drive it between requests. Measured
//! (`docs/h3-research.md` §1.5): across a 1500 ms gap under a 1000 ms idle
//! timeout, an undriven connection's second request fails and a driven
//! one's succeeds. That is the whole argument for the `Spawn` bound, and
//! everything h3 is for — multiplexing, 0-RTT on a second visit — pays off
//! across exactly those gaps.
//!
//! The bound excludes only runtimes that were already excluded. `embassy-net`
//! has no descriptor at all, so `quinn-udp` cannot even be asked about
//! GSO/GRO/ECN (`docs/h3-research.md` §2.4 quotes the `E0277`), and quinn
//! cannot wrap the socket. And the bound is on [`H3`], not on the seam and
//! not on `Native`, so W7's direction — a runtime may implement as little
//! as it honestly can — is untouched.
//!
//! # Connections are shared, and requests on one are multiplexed
//!
//! **This is the opposite of v0.2 W2's h2 policy, and deliberately so.**
//! There, an h2 connection is checked out of the pool *exclusively*, one
//! stream at a time, because without a spawner there is nobody to drive a
//! shared connection but the in-flight request futures — so a caller that
//! stopped polling one request would stall its neighbours. The argument is
//! correct and it has no subject here: the driver has moved into
//! [`Spawn`](http_ng_rt::Spawn), because it had to for the connection to
//! survive at all, and a driver that is nobody's request future cannot be
//! stalled by any request's polling behaviour.
//!
//! So W1's "cancelling one stream must not tear down the others" holds in
//! this crate for a different reason than it holds in `http-ng-native`:
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
mod pump;
mod runtime;

pub use body::H3Body;
pub use pump::{RequestTrailersNotSent, UnknownRequestBodyFrame};
pub use runtime::QuinnTask;

use bytes::Bytes;
use http_ng_core::{
    CancelSupport, Capabilities, DecompressionSupport, EarlyDataSupport, Error, ErrorKind, Phase,
    RedirectSupport, RequestBody, ReuseSupport, TimeoutSupport, Timeouts, TlsSupport,
    check_version, unversioned::Transport,
};
use http_ng_rt::{Spawn, Timer, UdpAdoptStd, UdpBind};
use http_ng_tls::TlsConfigId;
use http_ng_tls_quic::{QuicTlsConnect, QuicTlsRequest};
use std::collections::HashMap;
use std::fmt;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

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
/// choice.** W3 reserved `TlsInfo::early_data_accepted: Option<bool>`, which
/// is the right shape for TLS 1.3 over TCP where the answer is known when
/// the handshake completes. Over QUIC it is not: measured in
/// `docs/h3-research.md` §3.2, `into_0rtt()` returns at 1.27 ms, the
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
/// The research (`docs/h3-research.md` §1.5) established that an unpolled
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
pub struct H3<R, T, D> {
    rt: R,
    tls: T,
    dns: D,
    caps: Capabilities,
    keep_alive: Option<std::time::Duration>,
    shared: Arc<Shared>,
}

// Hand-written rather than derived, and not requiring `R: Debug` etc.: a
// derive would put a `Debug` bound on all three parameters, which is a
// bound on the runtime, the TLS backend and the resolver for the benefit of
// a formatter. What is worth printing is the capability set anyway.
impl<R, T, D> fmt::Debug for H3<R, T, D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("H3")
            .field("early_data", &self.caps.early_data)
            .finish_non_exhaustive()
    }
}

impl<R, T, D> H3<R, T, D>
where
    T: QuicTlsConnect,
{
    /// # The capability values, one by one
    ///
    /// - `streaming_request_body: true` and `full_duplex: true`. HTTP/3
    ///   supports both — the request and response halves of a stream are
    ///   independent — and as of this change **so does this
    ///   implementation**: [`one_attempt`](H3::one_attempt) splits the
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
    ///   [`RequireVersion`](http_ng_core::RequireVersion): `HTTP_3`
    ///   proceeds, anything else is
    ///   [`VersionNotAvailable`](http_ng_core::VersionNotAvailable) before
    ///   a packet goes out. `false` would make `Client` refuse the one
    ///   demand this transport meets by construction.
    /// - `timeouts`: `connect` is `true` and enforced in `execute` — it
    ///   bounds resolution, the QUIC handshake and h3's settings exchange
    ///   together, the same scope `http-ng-native` gives the same setting.
    ///   `first_byte` and `between_bytes` stay `false`, and the comment on
    ///   [`capabilities`] says what each of them would cost.
    pub fn new(rt: R, tls: T, dns: D) -> Result<Self, Error> {
        let early_data = if tls.offers_early_data() {
            EarlyDataSupport::Supported
        } else {
            EarlyDataSupport::None
        };
        Ok(Self {
            rt,
            tls,
            dns,
            caps: capabilities(early_data),
            keep_alive: Some(DEFAULT_KEEP_ALIVE),
            shared: Arc::new(Shared {
                endpoints: Mutex::new(HashMap::new()),
                conns: Mutex::new(HashMap::new()),
            }),
        })
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
/// so a literal would not compile from outside `http-ng-core`, and the
/// consequence is the one that matters here: a field added to the struct
/// later arrives at this transport as the conservative default rather than
/// as a compile error somebody silences by copying the neighbouring value.
fn capabilities(early_data: EarlyDataSupport) -> Capabilities {
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
    c.client_certs = true;
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
    // wrapper holding a sleep, the shape `http_ng_native::IdleTimeout` has.
    // Declaring either without that is the silent no-op this field exists
    // to make impossible.
    c.timeouts = TimeoutSupport {
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
/// them. See `crate::runtime`'s module doc.
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

impl<R, T, D> H3<R, T, D>
where
    R: H3Runtime,
    R::Sleep: Send + 'static, // send-bound-exception: amendment-C10
    R::Socket: fmt::Debug + Send + Sync + 'static, // send-bound-exception: amendment-C10
    T: QuicTlsConnect,
    D: http_ng_dns::Resolve,
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
        let bound =
            runtime::endpoint(&self.rt, wildcard).map_err(|e| Error::new(ErrorKind::Connect, e))?;
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
    async fn checkout(
        &self,
        key: &PoolKey,
        addr: SocketAddr,
    ) -> Result<(SendRequest, Option<ZeroRtt>), Error> {
        if let Some(p) = self
            .shared
            .conns
            .lock()
            .expect("pool mutex poisoned")
            .get(key)
            && p.conn.close_reason().is_none()
        {
            // A clone, not a removal: `SendRequest` is `Clone` precisely so
            // several requests can be in flight on one connection, which is
            // the multiplexing this transport exists for. Contrast
            // `http-ng-native`'s h2, which takes the connection OUT of the
            // pool for the duration of one exchange.
            return Ok((p.send.clone(), p.zero_rtt.clone()));
        }
        let (send, conn, zero_rtt) = self.connect(key, addr).await?;
        self.shared
            .conns
            .lock()
            .expect("pool mutex poisoned")
            .insert(
                key.clone(),
                Pooled {
                    send: send.clone(),
                    conn,
                    zero_rtt: zero_rtt.clone(),
                },
            );
        Ok((send, zero_rtt))
    }

    async fn connect(
        &self,
        key: &PoolKey,
        addr: SocketAddr,
    ) -> Result<(SendRequest, quinn::Connection, Option<ZeroRtt>), Error> {
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
        // leaves. The same duty `http_ng_tls::TlsRequest::server_name`
        // states for the TCP seam; the QUIC seam has no field for the name
        // at all, so this call is where it lands.
        //
        // The key keeps the bracketed form: it is the pool's identity, and
        // matching it against the URI a request arrives with is what a
        // second request has to do to be reused. Normalising twice, once
        // for the key and once here, would be the two-places-drifting
        // problem `bare_host`'s doc is about.
        let connecting = endpoint
            .connect_with(cfg, addr, http_ng_core::bare_host(&key.host))
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
        // It is reached only when `key.early_data` is set, which is only
        // when the caller marked this request — see `crate::early`.
        let (conn, zero_rtt) = if key.early_data {
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

        let (mut driver, send) = h3::client::builder()
            .build(h3_quinn::Connection::new(conn.clone()))
            .await
            .map_err(|e| Error::new(ErrorKind::Connect, std::io::Error::other(e.to_string())))?;

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

        Ok((send, conn, zero_rtt))
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
    ) -> Result<http::Response<H3Body>, Error> {
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
        Ok(http::Response::from_parts(parts, H3Body::new(reader, pump)))
    }

    async fn resolve(&self, host: &str, port: u16) -> Result<SocketAddr, Error> {
        use futures_util::StreamExt;
        // `bare_host` first: `host` came from `Uri::host()`, so `[::1]`
        // arrives with its brackets and `str::parse::<IpAddr>` rejects it.
        // Without this the shortcut misses every IPv6 literal and the
        // address goes to the resolver instead — where `getaddrinfo("[::1]")`
        // fails and the request with it. `http-ng-native` does not have
        // this half of the defect only because `IpLiteralOnly::literal`
        // strips on the way in; a shortcut in front of the resolver has to
        // strip for itself.
        if let Ok(ip) = http_ng_core::bare_host(host).parse::<std::net::IpAddr>() {
            return Ok(SocketAddr::new(ip, port));
        }
        // v6 first, then v4. Not happy eyeballs: QUIC's connect is not a
        // TCP SYN race, and racing two QUIC handshakes would mean two
        // handshakes' worth of crypto for one request. `http-ng-native`'s
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
/// `Err(_) => Ok(true)` leaves the whole `http-ng-h3` suite green, all 44
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

impl<R, T, D> Transport for H3<R, T, D>
where
    R: H3Runtime,
    R::Sleep: Send + 'static, // send-bound-exception: amendment-C10
    R::Socket: fmt::Debug + Send + Sync + 'static, // send-bound-exception: amendment-C10
    T: QuicTlsConnect,
    D: http_ng_dns::Resolve,
{
    type Body = H3Body;
    type Error = Error;

    async fn execute(
        &self,
        req: http::Request<RequestBody>,
    ) -> Result<http::Response<H3Body>, Error> {
        // **A `RequireVersion` demand, answered before anything else** —
        // before the scheme check, before resolution, before a QUIC
        // packet. This transport speaks exactly one version, so the answer
        // is a pure function of the request and there is no reason to
        // learn anything else first.
        //
        // The two halves matter equally, and it is the second that makes
        // `Capabilities::version_select` `true` here rather than `false`:
        //
        // - `RequireVersion(HTTP_3)` is **satisfied**, and must not fail.
        //   Declaring `version_select: false` would have `Client` refuse
        //   it at the `UnsupportedCapability` gate — a transport turning
        //   down the one demand it meets by definition.
        // - Anything else is refused with the same `VersionNotAvailable`
        //   `http-ng-native` raises, so a caller who moved from one
        //   backend to the other reads the same type rather than
        //   discovering a second vocabulary.
        //
        // A transport that cannot *choose* still *honours*: see the field's
        // doc in `http-ng-core`, and `http-ng-fetch`/`http-ng-wasi`, which
        // keep `false` because they neither choose the version nor learn
        // it.
        check_version(req.extensions(), http::Version::HTTP_3)?;

        let uri = req.uri().clone();
        if uri.scheme_str() != Some("https") {
            return Err(Error::new(
                ErrorKind::Connect,
                std::io::Error::other(format!(
                    "HTTP/3 runs over QUIC, which is always TLS; `{}` has no plaintext form",
                    uri.scheme_str().unwrap_or("(no scheme)")
                )),
            ));
        }
        let host = uri
            .host()
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::Connect,
                    std::io::Error::other("request URI has no host"),
                )
            })?
            .to_string();
        let port = uri.port_u16().unwrap_or(443);

        let wants_early = early::admits_early_data(&req);
        if req
            .extensions()
            .get::<http_ng_core::AllowEarlyData>()
            .is_some()
            && self.caps.early_data == EarlyDataSupport::None
        {
            return Err(early::refuse_early_data("http-ng-h3"));
        }

        // `Timeouts` is read field by field, from a copy, and never branched
        // on as a whole — `Transport::execute`'s doc comment states the
        // reading literally, and "presence is not intent" is what it means:
        // an extension carrying only `first_byte` must not be read as a
        // request for a `connect` bound, nor its absence as a refusal.
        // Only `connect` is honoured here; the other two are declared
        // `false` and would be silent no-ops, which is the defect this
        // whole channel exists to prevent.
        let timeouts = req
            .extensions()
            .get::<Timeouts>()
            .copied()
            .unwrap_or_default();

        // Everything between "a URI" and "a connection that can carry a
        // request" — address resolution, the QUIC handshake, and h3's own
        // settings exchange on top of it — under one bound.
        //
        // The same scope `http-ng-native` gives `connect`, deliberately:
        // there `with_connect_timeout` wraps `connect::connect`, which is
        // DNS plus the TCP race plus the TLS handshake. A portable setting
        // that meant "DNS included" on one transport and "handshake only"
        // on another would be a capability that lies in the most tiresome
        // way — by being true.
        //
        // A pooled checkout does no I/O at all, so it is inside the bound
        // for want of a reason to write a second path rather than because
        // it needs one: `checkout` returns a live `SendRequest` clone
        // without awaiting anything the timer could beat.
        let connect = async {
            let addr = self.resolve(&host, port).await?;
            let key = PoolKey {
                host,
                port,
                tls: self.tls.config_id(),
                early_data: wants_early,
            };
            self.checkout(&key, addr).await
        };
        let (mut send, zero_rtt) = match timeouts.connect {
            Some(d) => within_connect(&self.rt, d, connect).await?,
            None => connect.await?,
        };

        let (parts, body) = req.into_parts();
        // Taken before the first attempt, because after it the body is
        // gone — but **only when there is something a replay could be
        // needed for**. `rewind` on a `Rewindable` calls the caller's
        // factory, and calling it on every request to hold a spare that
        // almost never gets used would make every `Rewindable` body cost
        // twice what it should.
        //
        // `zero_rtt.is_some()` is exactly the condition: it is `Some` only
        // when this connection really went out with early data, which is
        // the only way a request can be rejected in a way replaying fixes.
        let spare = if zero_rtt.is_some() {
            body.rewind()
        } else {
            None
        };
        let head = http::Request::from_parts(parts.clone(), ());
        let first = Self::one_attempt(&mut send, head, body).await;

        // The second of the three 0-RTT failure paths (`crate::early` has
        // the table). If the server refused the early keys, the streams
        // opened before the handshake completed are reset with
        // `ZeroRttRejected` while the connection itself is fine — quinn
        // documents exactly this. The request must be replayed on that same
        // connection, and **the caller must never see `ZeroRttRejected`**:
        // from outside, offering early data and having it refused is not an
        // outcome, it is a detail of how the response was obtained.
        //
        // The rejection is detected by AWAITING THE VERDICT, not by
        // matching on an error string. Two reasons, and the second is the
        // one that makes it correct rather than merely tidy: h3 surfaces
        // the QUIC error as an opaque `Undefined(..)` whose `Display` is
        // not a stable interface, and the verdict future is the authority
        // on the question anyway — it is what `into_0rtt` handed back for
        // this purpose. By the time a stream has failed this way the
        // handshake has completed, so the await does not stall.
        let Err(e) = first else {
            return first;
        };
        let Some(verdict) = zero_rtt else {
            // Nothing went into early data, so this error is the caller's.
            return Err(e);
        };
        if verdict.await {
            // Early data was accepted; the failure is a real one.
            return Err(e);
        }
        let Some(body) = spare else {
            // Unreachable through `execute`: `admits_early_data` refuses a
            // `RetryKind::Impossible` body, which is the only kind `rewind`
            // returns `None` for. Kept as a typed error rather than an
            // `unwrap`, because the two checks live in different files and
            // the invariant between them is not one the compiler holds.
            return Err(e);
        };
        let head = http::Request::from_parts(parts, ());
        Self::one_attempt(&mut send, head, body).await
    }

    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
}

/// The failure [`within_connect`] ends in when the timer wins.
///
/// A named type rather than a string, for the reason
/// `http_ng_native::FirstByteTimedOut` gives: a caller must be able to tell
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
/// `http_ng_native::with_connect_timeout` and `http_ng::within`.
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

    /// Never called: `H3::new` reads `offers_early_data` and nothing else,
    /// so a stub is enough to construct one and look at what it decided.
    #[derive(Debug)]
    struct StubTls(bool);

    impl http_ng_tls::TlsIdentity for StubTls {
        fn config_id(&self) -> TlsConfigId {
            TlsConfigId::no_tls()
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
            self.0
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
        assert_eq!(h3(StubTls(false)).keep_alive, Some(DEFAULT_KEEP_ALIVE));
        assert!(h3(StubTls(false)).without_keep_alive().keep_alive.is_none());
        assert_eq!(
            h3(StubTls(false))
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
        let h3: H3<_, _, http_ng_dns::IpLiteralOnly> = H3::new(
            http_ng_rt_tokio::TokioHandle::current().unwrap(),
            StubTls(false),
            http_ng_dns::IpLiteralOnly,
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
            h3(StubTls(true)).caps.early_data,
            EarlyDataSupport::Supported
        );
        assert_eq!(h3(StubTls(false)).caps.early_data, EarlyDataSupport::None);
    }
}
