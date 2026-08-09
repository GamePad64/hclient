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
mod runtime;

pub use body::H3Body;
pub use runtime::QuinnTask;

use bytes::Bytes;
use http_ng_core::{
    CancelSupport, Capabilities, DecompressionSupport, EarlyDataSupport, Error, ErrorKind,
    RedirectSupport, RequestBody, ReuseSupport, TimeoutSupport, TlsSupport, UpgradeSupport,
    unversioned::Transport,
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

struct Pooled {
    send: SendRequest,
    conn: quinn::Connection,
}

struct Shared {
    endpoint: Mutex<Option<quinn::Endpoint>>,
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
    /// - `streaming_request_body: false` and `full_duplex: false`. HTTP/3
    ///   supports both — the request and response halves of a stream are
    ///   independent — and **this implementation does neither**: `execute`
    ///   writes the whole request body before reading the response head.
    ///   Declaring the protocol's ability rather than the implementation's
    ///   is the mistake the capability model exists to prevent, and for
    ///   `full_duplex` it is the one whose cost is a deadlock rather than a
    ///   degradation.
    /// - `response_trailers: true`. [`H3Body`] yields them as a trailers
    ///   frame; `request_trailers` stays `false` because nothing here sends
    ///   any.
    /// - `connection_reuse: Supported`, and here it means more than it does
    ///   for HTTP/1: requests share a connection *concurrently*, not only
    ///   in sequence.
    /// - `early_data` follows the TLS backend's own
    ///   [`offers_early_data`](QuicTlsConnect::offers_early_data), which
    ///   defaults to `false`. It is never set from a constant here: the
    ///   capability has to come from the component that knows.
    /// - `version_reported: true`, `version_select: false` — this transport
    ///   speaks exactly one version and has nothing to select.
    /// - `timeouts`: all three `false`, honestly. `connect` is the one that
    ///   would be cheapest to add and it is not added, because declaring it
    ///   and enforcing it belong in the same change.
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
                endpoint: Mutex::new(None),
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
    // Both stay `false`, and neither is a limitation of HTTP/3: a QUIC
    // stream's halves are independent, so the protocol does full duplex and
    // streaming request bodies. `execute` below does not — it writes the
    // whole request body, then reads the head. The capability describes the
    // implementation, which is the distinction this model exists for.
    c.streaming_request_body = false;
    c.full_duplex = false;
    // Nothing here sends request trailers; `H3Body` does yield response
    // ones, as a trailers frame.
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
    // One version, nothing to select, and `Response::version()` reports it.
    c.version_select = false;
    c.version_reported = true;
    // All three honestly `false`. `connect` would be the cheapest to add
    // and is not added, because a declaration and its enforcement belong in
    // the same change.
    c.timeouts = TimeoutSupport {
        connect: false,
        first_byte: false,
        between_bytes: false,
    };
    c.upgrade = UpgradeSupport::None;
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
    Timer + UdpBind + UdpAdoptStd + Spawn<QuinnTask> + Clone + Send + Sync + 'static
{
}

impl<R> H3Runtime for R
where
    R: Timer + UdpBind + UdpAdoptStd + Spawn<QuinnTask> + Clone + Send + Sync + 'static,
    R::Sleep: Send + 'static,
    R::Socket: fmt::Debug + Send + Sync + 'static,
{
}

impl<R, T, D> H3<R, T, D>
where
    R: H3Runtime,
    R::Sleep: Send + 'static,
    R::Socket: fmt::Debug + Send + Sync + 'static,
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
    fn endpoint(&self) -> Result<quinn::Endpoint, Error> {
        let mut slot = self
            .shared
            .endpoint
            .lock()
            .expect("endpoint mutex poisoned");
        if let Some(e) = slot.as_ref() {
            return Ok(e.clone());
        }
        // `[::]:0` with a v4-mapped fallback: one endpoint socket serves
        // every destination, so it is bound once and to the wildcard.
        let bound = runtime::endpoint(&self.rt, SocketAddr::from(([0u8; 16], 0)))
            .or_else(|_| runtime::endpoint(&self.rt, SocketAddr::from(([0, 0, 0, 0], 0))))
            .map_err(|e| Error::new(ErrorKind::Connect, e))?;
        *slot = Some(bound.clone());
        Ok(bound)
    }

    /// A connection for this key, from the pool or newly made.
    ///
    /// The check on the way out is `close_reason()`: quinn reports a
    /// connection the peer or a timer already closed, and a pool that
    /// handed one out would fail the request it was reused for. This is the
    /// same "poll at checkout" W2's HTTP/1 pool does, in the form this
    /// stack offers it.
    async fn checkout(&self, key: &PoolKey, addr: SocketAddr) -> Result<SendRequest, Error> {
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
            return Ok(p.send.clone());
        }
        let (send, conn) = self.connect(key, addr).await?;
        self.shared
            .conns
            .lock()
            .expect("pool mutex poisoned")
            .insert(
                key.clone(),
                Pooled {
                    send: send.clone(),
                    conn,
                },
            );
        Ok(send)
    }

    async fn connect(
        &self,
        key: &PoolKey,
        addr: SocketAddr,
    ) -> Result<(SendRequest, quinn::Connection), Error> {
        let crypto = self.tls.quic_client_config(QuicTlsRequest {
            alpn: &[ALPN_H3],
            ech: None,
            early_data: key.early_data,
        })?;
        let endpoint = self.endpoint()?;
        let mut cfg = quinn::ClientConfig::new(crypto);
        if let Some(d) = self.keep_alive {
            let mut transport = quinn::TransportConfig::default();
            transport.keep_alive_interval(Some(d));
            cfg.transport_config(Arc::new(transport));
        }
        let conn = endpoint
            .connect_with(cfg, addr, &key.host)
            .map_err(|e| Error::new(ErrorKind::Connect, e))?
            .await
            .map_err(|e| Error::new(ErrorKind::Connect, e))?;

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

        Ok((send, conn))
    }

    async fn resolve(&self, host: &str, port: u16) -> Result<SocketAddr, Error> {
        use futures_util::StreamExt;
        if let Ok(ip) = host.parse::<std::net::IpAddr>() {
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

impl<R, T, D> Transport for H3<R, T, D>
where
    R: H3Runtime,
    R::Sleep: Send + 'static,
    R::Socket: fmt::Debug + Send + Sync + 'static,
    T: QuicTlsConnect,
    D: http_ng_dns::Resolve,
{
    type Body = H3Body;
    type Error = Error;

    async fn execute(
        &self,
        req: http::Request<RequestBody>,
    ) -> Result<http::Response<H3Body>, Error> {
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

        let addr = self.resolve(&host, port).await?;
        let key = PoolKey {
            host,
            port,
            tls: self.tls.config_id(),
            early_data: wants_early,
        };
        let mut send = self.checkout(&key, addr).await?;

        let (parts, body) = req.into_parts();
        let head = http::Request::from_parts(parts, ());
        let mut stream = send.send_request(head).await.map_err(body::stream_error)?;

        // The whole request body, then the head. `full_duplex` and
        // `streaming_request_body` are declared `false` because of exactly
        // this, and the declaration moves when the code does.
        match body {
            RequestBody::Empty => {}
            RequestBody::Full(b) => {
                if !b.is_empty() {
                    stream.send_data(b).await.map_err(body::stream_error)?;
                }
            }
            RequestBody::Rewindable(f) => {
                if let RequestBody::Full(b) = f()
                    && !b.is_empty()
                {
                    stream.send_data(b).await.map_err(body::stream_error)?;
                }
            }
            RequestBody::Streaming(_) => {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    std::io::Error::other(
                        "http-ng-h3 does not stream request bodies yet; \
                         Capabilities::streaming_request_body reports false",
                    ),
                ));
            }
        }
        stream.finish().await.map_err(body::stream_error)?;

        let resp = stream.recv_response().await.map_err(body::stream_error)?;
        let (parts, ()) = resp.into_parts();
        Ok(http::Response::from_parts(parts, H3Body::new(stream)))
    }

    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
}
