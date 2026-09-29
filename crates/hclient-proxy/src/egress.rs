//! Where a connection goes: the seam a transport asks, per request, and
//! the context it lends a filter to act on the answer.
//!
//! A transport asks [`EgressFilter::route`] before it resolves anything.
//! [`Decision::Direct`] means its own path, in full; [`Decision::Filtered`]
//! means the filter carries the request, and the transport calls
//! [`EgressFilter::open_stream`] with a [`Dial`] — its own way of opening
//! connections — for the filter to use. Nothing a filter answers ever
//! sends a filtered request direct.

use std::borrow::Cow;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use futures_io::{AsyncRead, AsyncWrite};
use hclient_core::error::Error;
use hclient_core::error::ErrorKind;
use hclient_rt::Shutdown;

use crate::{BoxPath, BoxUdp, Tunnel, TunnelRequest};

/// What a filter asks of a TLS handshake it has the transport run over one
/// of the transport's own streams — see [`Dial::connect_tls`].
///
/// The certificate is checked by the transport's TLS backend against the
/// transport's trust, exactly as an origin's is; nothing here carries a
/// root store or a verifier. `#[non_exhaustive]` because a field added
/// later must not break an implementor; build one with [`ProxyTls::new`].
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct ProxyTls<'a> {
    /// The name the certificate is checked against, and sent as SNI.
    pub server_name: &'a str,
    /// ALPN protocols to offer. Empty by default: an HTTP proxy spoken to
    /// without ALPN speaks HTTP/1.1.
    pub alpn: &'a [&'a [u8]],
    /// A client identity, by the label the backend knows it by. `None` by
    /// default. A label the backend does not know is refused, never
    /// replaced by the default identity.
    pub identity: Option<&'a str>,
}

impl<'a> ProxyTls<'a> {
    /// A handshake checked against `server_name`, offering no ALPN and no
    /// client certificate.
    #[must_use]
    pub const fn new(server_name: &'a str) -> Self {
        Self {
            server_name,
            alpn: &[],
            identity: None,
        }
    }

    /// Offer these ALPN protocols.
    #[must_use]
    pub const fn alpn(mut self, alpn: &'a [&'a [u8]]) -> Self {
        self.alpn = alpn;
        self
    }

    /// Present the client identity the backend knows by `label`.
    #[must_use]
    pub const fn identity(mut self, label: Option<&'a str>) -> Self {
        self.identity = label;
        self
    }
}

fn lends_no_tls() -> Error {
    Error::new(
        ErrorKind::Unsupported,
        std::io::Error::other("this transport lends no TLS to egress filters"),
    )
}

fn lends_nothing(what: &str) -> Error {
    Error::new(
        ErrorKind::Unsupported,
        std::io::Error::other(format!("this transport lends no {what} to egress filters")),
    )
}

/// What a transport lends a filter: its own way of opening a connection.
///
/// `impl Future` rather than named associated types, and on purpose: a
/// transport calls a filter it holds concretely, where the returned
/// futures' auto traits are *inferred*, so the default filter asks nothing
/// of a runtime that is not `Send`. Erasure goes through [`BoxDial`], a
/// concrete `Dial`, and never needs to name these futures.
pub trait Dial {
    /// The byte stream this transport's runtime produces.
    type Stream: Io;

    /// Resolve `host` and connect to `port` exactly as the transport would
    /// for a direct request — its resolver, Happy Eyeballs, its socket
    /// options, its hooks' timing. A filter that must not resolve a name
    /// locally never passes that name here.
    ///
    /// The future may borrow `host`, as [`connect_tls`](Self::connect_tls)'s
    /// may borrow its request, so an implementor need not copy it.
    ///
    /// # Errors
    ///
    /// Whatever the transport's own connect answers: a name that did not
    /// resolve ([`ErrorKind::Resolve`](hclient_core::error::ErrorKind::Resolve)),
    /// no address that accepted
    /// ([`ErrorKind::Connect`](hclient_core::error::ErrorKind::Connect)), or
    /// the request's connect bound running out
    /// ([`ErrorKind::Timeout`](hclient_core::error::ErrorKind::Timeout)).
    fn connect<'a>(
        &'a self,
        host: &'a str,
        port: u16,
    ) -> impl Future<Output = Result<Self::Stream, Error>> + 'a;

    /// Open a same-machine connection.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::Unsupported`](hclient_core::error::ErrorKind::Unsupported)
    /// when the transport cannot open this kind of address; otherwise what
    /// the connect answers —
    /// [`ErrorKind::Connect`](hclient_core::error::ErrorKind::Connect) for
    /// nothing listening, and
    /// [`ErrorKind::Timeout`](hclient_core::error::ErrorKind::Timeout) for
    /// the bound running out.
    fn connect_ipc<'a>(
        &'a self,
        addr: &'a hclient_rt::IpcAddr,
    ) -> impl Future<Output = Result<Self::Stream, Error>> + 'a;

    /// What is left of the request's connect bound, if it has one. A
    /// filter spends it; it is never handed a fresh one.
    fn remaining(&self) -> Option<Duration>;

    /// Run TLS over `stream`, with the transport's own backend and trust,
    /// and hand back the same stream type — so a filter can layer it again
    /// (TLS to a proxy, then a tunnel, then TLS to the origin) without
    /// naming a TLS type.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::Unsupported`](hclient_core::error::ErrorKind::Unsupported)
    /// by default, for a transport that lends no TLS. A handshake that
    /// fails is the backend's error, [`ErrorKind::Tls`](hclient_core::error::ErrorKind::Tls).
    fn connect_tls<'a>(
        &'a self,
        stream: Self::Stream,
        req: ProxyTls<'a>,
    ) -> impl Future<Output = Result<Self::Stream, Error>> + 'a {
        let _ = (stream, req);
        std::future::ready(Err(lends_no_tls()))
    }

    /// Bind a UDP socket of the transport's runtime at `local`.
    ///
    /// **A receive may be several datagrams.** Where the runtime does GRO, one
    /// `poll_recv` can hand over a run of datagrams coalesced into one buffer,
    /// [`RecvMeta::stride`](hclient_rt::RecvMeta::stride) bytes apart, up to
    /// [`UdpSupport::max_recv_segments`](hclient_rt::UdpSupport::max_recv_segments)
    /// of them. A path built on this socket must split each receive by its
    /// stride before reading a datagram out of it, as the built-in SOCKS5
    /// path does — reading the buffer as one datagram loses every one after
    /// the first.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::Unsupported`](hclient_core::error::ErrorKind::Unsupported)
    /// by default, for a transport that lends no UDP; otherwise whatever
    /// the bind answers.
    fn bind_udp(&self, local: SocketAddr) -> Result<BoxUdp, Error> {
        let _ = local;
        Err(lends_nothing("UDP"))
    }

    /// Resolve a **proxy's** name with the transport's resolver. Never the
    /// origin's: a filter resolving the target locally would name it to the
    /// resolver a proxy is often there to avoid.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::Unsupported`](hclient_core::error::ErrorKind::Unsupported)
    /// by default, for a transport that lends no resolver; otherwise what
    /// the resolver answers,
    /// [`ErrorKind::Resolve`](hclient_core::error::ErrorKind::Resolve) for
    /// a name that does not exist.
    fn resolve<'a>(
        &'a self,
        host: &'a str,
        port: u16,
    ) -> impl Future<Output = Result<Vec<SocketAddr>, Error>> + 'a {
        let _ = (host, port);
        std::future::ready(Err(lends_nothing("name resolution")))
    }

    /// Open a CONNECT or extended CONNECT tunnel through a proxy spoken to
    /// over HTTP/2 or HTTP/3.
    ///
    /// The proxy's response is handed back unjudged in
    /// [`Tunnel::response`]: a `4xx` from the proxy is a tunnel the filter
    /// reads and refuses, not an error here.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::Unsupported`](hclient_core::error::ErrorKind::Unsupported)
    /// by default, for a transport that lends no tunnels, and for a
    /// [`TunnelVersion`](crate::TunnelVersion) or a kind of CONNECT the
    /// transport cannot speak — a refusal a filter may answer by trying
    /// another way. Otherwise the connect's, the TLS handshake's or the
    /// HTTP connection's own error, and
    /// [`ErrorKind::Timeout`](hclient_core::error::ErrorKind::Timeout) for
    /// the bound running out.
    fn connect_tunnel<'a>(
        &'a self,
        req: TunnelRequest<'a>,
    ) -> impl Future<Output = Result<Tunnel, Error>> + 'a {
        let _ = req;
        std::future::ready(Err(lends_nothing("HTTP tunnels")))
    }
}

/// The origin a request is for.
///
/// Built by the transport and only read by a filter, so a field added
/// later must not break one: `#[non_exhaustive]`, and [`Target::new`] for
/// a filter's own tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Target<'a> {
    /// The origin's host, as the request names it.
    pub host: &'a str,
    /// The origin's port, the scheme's default filled in.
    pub port: u16,
    /// Whether the request is `https`.
    pub use_tls: bool,
}

impl<'a> Target<'a> {
    /// The origin `host:port`, over TLS when `use_tls`.
    #[must_use]
    pub const fn new(host: &'a str, port: u16, use_tls: bool) -> Self {
        Self {
            host,
            port,
            use_tls,
        }
    }
}

/// A filter's answer for one target.
///
/// Built by a filter and matched by the transport. Exhaustive on purpose:
/// a third answer a transport met under a wildcard arm would have nothing
/// honest on its right-hand side — taking its own path would send a
/// filtered request direct — so a new one must be a compile error in
/// every transport instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision<'a> {
    /// The transport's ordinary path, in full.
    Direct,
    /// This filter carries the request, this way.
    Filtered(Route<'a>),
}

// Maintainer notes (not rendered):
// A struct rather than the variant's own fields: the filter builds it and
// the transport reads it, and a field the datagram path will want (a key
// of its own, say) would otherwise break every filter written against the
// three named here.
/// How a filter carries one request — [`Decision::Filtered`]'s answer.
///
/// `#[non_exhaustive]`, so build one with [`Route::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Route<'a> {
    /// What this filter can carry for this target.
    pub support: FilterSupport,
    /// This filter's part of the pool key. Two requests whose keys differ
    /// never share a connection.
    ///
    /// Borrowed from the filter where it can be — a transport asks
    /// [`route`](EgressFilter::route) several times per request, and a key
    /// the filter computed once need not be allocated again each time.
    pub pool_key: Cow<'a, str>,
    /// How the request head is written on the stream the filter opens.
    pub form: RequestForm,
}

impl<'a> Route<'a> {
    /// A route carrying what `support` claims, pooled under `pool_key`,
    /// with the request head written as `form` says.
    #[must_use]
    pub fn new(
        support: FilterSupport,
        pool_key: impl Into<Cow<'a, str>>,
        form: RequestForm,
    ) -> Self {
        Self {
            support,
            pool_key: pool_key.into(),
            form,
        }
    }
}

/// How the request head is written once the filter's stream is open.
///
/// Built by a filter and matched by the transport that writes the head.
/// Exhaustive on purpose, for [`Decision`]'s reason: a form a transport
/// did not know, written as the nearest one it did, would be a request
/// the proxy reads differently from how it was meant — so a new form must
/// be a compile error in every transport. The variant that may grow
/// fields is `#[non_exhaustive]` on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestForm {
    /// As to the origin — a tunnel, or no proxy at all.
    Origin,
    /// RFC 9112 §3.2.2 absolute-form, to an HTTP proxy acting as the
    /// origin server for an `http://` request. Build one with
    /// [`RequestForm::absolute`].
    // `#[non_exhaustive]` because this is where a hop's other headers
    // would go (curl's `--proxy-header`), and an enum variant cannot gain
    // a field otherwise without breaking every filter that builds one.
    #[non_exhaustive]
    Absolute {
        /// `Proxy-Authorization` for the proxy, if it wants one.
        proxy_authorization: Option<http::HeaderValue>,
    },
}

impl RequestForm {
    /// Absolute-form, carrying `proxy_authorization` to the proxy if given.
    #[must_use]
    pub fn absolute(proxy_authorization: Option<http::HeaderValue>) -> Self {
        Self::Absolute {
            proxy_authorization,
        }
    }
}

/// What a filter can carry for a target, declared by the filter itself.
///
/// Start from [`NONE`](Self::NONE) or [`STREAM`](Self::STREAM).
/// `#[non_exhaustive]` so that a way through a filter added later is a new
/// field rather than a break of every filter: built from the constants,
/// the attribute costs a filter nothing.
///
/// `datagrams: true` promises [`EgressFilter::open_datagrams`]; a transport
/// asks it only where it would otherwise carry the request over QUIC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct FilterSupport {
    /// The filter can open a byte stream to the target.
    pub stream: bool,
    /// The filter can open a datagram path to the target.
    pub datagrams: bool,
}

impl FilterSupport {
    /// Claims nothing.
    pub const NONE: Self = Self {
        stream: false,
        datagrams: false,
    };
    /// A byte stream and nothing else.
    pub const STREAM: Self = Self {
        stream: true,
        datagrams: false,
    };

    /// The same claim, plus a datagram path to the target.
    #[must_use]
    pub const fn with_datagrams(mut self) -> Self {
        self.datagrams = true;
        self
    }
}

/// A byte stream the seam can carry — `hclient-rt`'s, re-exported so a
/// filter author needs one dependency.
pub use hclient_rt::Io;

/// A stream whose type grew with a filter's layers, or that crossed an
/// erased filter.
///
/// A newtype rather than a `Pin<Box<dyn Io>>` alias because the seam's
/// `Shutdown` has no impl for `Pin`, and a blanket one would be a change
/// to `hclient-rt`'s stable surface made for one consumer.
pub struct BoxIo(Pin<Box<dyn Io + Send>>); // send-bound-exception: amendment-C16

impl BoxIo {
    /// Erase a stream.
    pub fn new<S>(io: S) -> Self
    where
        S: Io + Send + 'static, // send-bound-exception: amendment-C16
    {
        Self(Box::pin(io))
    }
}

impl std::fmt::Debug for BoxIo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BoxIo")
    }
}

impl AsyncRead for BoxIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        self.0.as_mut().poll_read(cx, buf)
    }
}

impl AsyncWrite for BoxIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.0.as_mut().poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.0.as_mut().poll_flush(cx)
    }
    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.0.as_mut().poll_close(cx)
    }
}

impl Shutdown for BoxIo {
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.0.as_mut().poll_shutdown(cx)
    }
}

/// A stream a filter opened.
///
/// Built by a filter and matched by the transport. Exhaustive on purpose,
/// for [`Decision`]'s reason: a transport cannot carry a request over a
/// kind of stream it has no arm for, so a new kind must be a compile
/// error in every transport rather than a wildcard's guess.
pub enum Opened<S, W> {
    /// The context's own stream type: a handshake changes bytes, not the
    /// type, so the transport keeps its concrete connection.
    Raw(S),
    /// A stream the filter wrapped — TLS to a proxy, a custom layer — of
    /// the filter's own [`EgressFilter::Wrapped`] type.
    Wrapped(W),
}

impl<S, W> std::fmt::Debug for Opened<S, W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Raw(_) => "Opened::Raw",
            Self::Wrapped(_) => "Opened::Wrapped",
        })
    }
}

/// An [`Opened`] over an erased context, with its wrapper erased too —
/// what [`SendEgressFilter::open_stream_send`] hands back.
///
/// Generic over the wrapper so it is called where the wrapper's type is
/// known and its `Send` is inferred: in a filter's own
/// `open_stream_send`.
pub fn erase<W>(opened: Opened<BoxIo, W>) -> Opened<BoxIo, BoxIo>
where
    W: Io + Send + 'static, // send-bound-exception: amendment-C16
{
    match opened {
        Opened::Raw(s) => Opened::Raw(s),
        Opened::Wrapped(w) => Opened::Wrapped(BoxIo::new(w)),
    }
}

// Maintainer notes (not rendered):
// It had three variants — `Unreachable` and `Refused` beside
// `Unsupported` — and nothing could tell the first two apart: neither
// permits a switch, `hclient-native` reads only `into_error`, and the
// built-in rules could sort a refusal from an outage only for their own
// three protocols, so a third-party handshake's refusal came back as
// `Unreachable`. A distinction with one reachable side, taken out before a
// stable version would have promised it. A filter that fails over between
// proxies reads *why* off the error's source, which carries it for every
// protocol alike.
/// How an attempt through a filter failed.
///
/// Two outcomes because only one of them permits trying another way
/// through the same filter: a proxy that could not be reached, or that
/// declined this target, will not do better by being asked for a stream
/// instead of datagrams, and a proxy that does not support datagrams
/// might. Which of those it was — and which proxy — is the error's
/// source.
///
/// Built by a filter and read by the transport through
/// [`permits_switch`](Self::permits_switch) and
/// [`into_error`](Self::into_error), which answer for every variant —
/// so `#[non_exhaustive]`: a third outcome is added with its answers to
/// those two, and nothing that reads through them breaks.
#[derive(Debug)]
#[non_exhaustive]
pub enum Attempt {
    /// The attempt failed: the proxy could not be reached, its handshake
    /// failed, or it declined this target.
    Failed(Error),
    /// The proxy does not support what was asked of it.
    Unsupported(Error),
}

impl Attempt {
    /// Whether a transport may try the same filter another way.
    pub fn permits_switch(&self) -> bool {
        matches!(self, Self::Unsupported(_))
    }

    /// The error a caller sees.
    pub fn into_error(self) -> Error {
        match self {
            Self::Failed(e) | Self::Unsupported(e) => e,
        }
    }
}

/// Decides where each request's connection goes, and opens it.
pub trait EgressFilter {
    /// The stream this filter hands back when it wraps the one it was
    /// lent — a TLS session to a proxy, a custom layer. A filter that only
    /// runs handshakes over the transport's stream (the built-in proxies)
    /// sets it to `S` and never builds [`Opened::Wrapped`].
    ///
    /// An associated type rather than an erased box so that whether the
    /// wrapper is `Send` is answered by the concrete filter and the
    /// concrete stream, not demanded of every transport.
    type Wrapped<S: Io>: Io
    where
        Self: Sized;

    /// Where a request to `target` goes, asked before anything is resolved.
    ///
    /// **It must be a pure function of `target`.** A transport may ask it
    /// several times for one request — for the pool key, for how the
    /// request line is written, for whether HTTP/3 is possible, and again
    /// when it connects — and acts on each answer as though they were the
    /// same. A filter whose answer changed between those calls would have a
    /// request keyed for one proxy and dialled through another, or kept
    /// off HTTP/3 as filtered and then sent direct. Rotation, health checks
    /// and failover belong inside [`open_stream`](Self::open_stream), behind
    /// a key that names the pool they share.
    fn route(&self, target: &Target<'_>) -> Decision<'_>;

    /// Open a byte stream to `target`, for a request [`route`](Self::route)
    /// answered `Filtered` with `support.stream`.
    ///
    /// `where Self: Sized` keeps [`SendEgressFilter`] usable as `dyn`: a
    /// generic method cannot be in a vtable, and the erased path calls
    /// [`SendEgressFilter::open_stream_send`] instead.
    ///
    /// # Errors
    ///
    /// An [`Attempt`] saying which of its two ways it failed.
    #[allow(
        clippy::type_complexity,
        reason = "the return type is the seam: a stream of the context's type or of this filter's own wrapper type, and naming it through an alias would hide which is which"
    )]
    fn open_stream<'a, C: Dial + 'a>(
        &'a self,
        target: Target<'a>,
        ctx: &'a C,
    ) -> impl Future<Output = Result<Opened<C::Stream, Self::Wrapped<C::Stream>>, Attempt>> + 'a
    where
        Self: Sized;

    /// Open a datagram path to `target`, for a request [`route`](Self::route)
    /// answered `Filtered` with `support.datagrams`.
    ///
    /// Refuses with [`Attempt::Unsupported`] by default — the one answer that
    /// lets a transport send the request over [`open_stream`](Self::open_stream)
    /// instead.
    ///
    /// # Errors
    ///
    /// [`Attempt::Unsupported`] when this filter or its proxy carries no
    /// datagrams to `target`; [`Attempt::Failed`] when the proxy could not be
    /// reached or refused.
    fn open_datagrams<'a, C: Dial + 'a>(
        &'a self,
        target: Target<'a>,
        ctx: &'a C,
    ) -> impl Future<Output = Result<BoxPath, Attempt>> + 'a
    where
        Self: Sized,
        // A path is `Send + Sync` because its one consumer is a QUIC
        // stack, and a path that holds the proxy's control connection (a
        // SOCKS5 association does) holds it for the path's life.
        C::Stream: Send + 'static, // send-bound-exception: amendment-C16
    {
        let _ = (target, ctx);
        std::future::ready(Err(no_datagrams()))
    }
}

fn no_datagrams() -> Attempt {
    Attempt::Unsupported(Error::new(
        ErrorKind::Unsupported,
        std::io::Error::other("this filter opens no datagram path"),
    ))
}

/// [`SendEgressFilter::open_datagrams_send`]'s future.
pub type BoxPathOpening<'a> = Pin<Box<dyn Future<Output = Result<BoxPath, Attempt>> + Send + 'a>>; // send-bound-exception: amendment-C16

/// [`DynDial`]'s futures.
pub type BoxDialing<'a> = Pin<Box<dyn Future<Output = Result<BoxIo, Error>> + Send + 'a>>; // send-bound-exception: amendment-C16
/// [`DynDial::resolve_boxed`]'s future.
pub type BoxResolving<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<SocketAddr>, Error>> + Send + 'a>>; // send-bound-exception: amendment-C16
/// [`DynDial::connect_tunnel_boxed`]'s future.
pub type BoxTunnelling<'a> = Pin<Box<dyn Future<Output = Result<Tunnel, Error>> + Send + 'a>>; // send-bound-exception: amendment-C16

/// An object-safe [`Dial`] whose streams and futures are erased and
/// `Send`, for a transport to lend an erased filter.
///
/// Declares no auto traits, by this workspace's rule for a seam: `Send`
/// and `Sync` are demanded where it is stored, by [`SharedDial`].
pub trait DynDial {
    /// [`Dial::connect`], erased.
    ///
    /// # Errors
    ///
    /// See [`Dial::connect`].
    fn connect_boxed<'a>(&'a self, host: &'a str, port: u16) -> BoxDialing<'a>;
    /// [`Dial::connect_ipc`], erased.
    ///
    /// # Errors
    ///
    /// See [`Dial::connect_ipc`].
    fn connect_ipc_boxed<'a>(&'a self, addr: &'a hclient_rt::IpcAddr) -> BoxDialing<'a>;
    /// [`Dial::remaining`].
    fn remaining(&self) -> Option<Duration>;
    /// [`Dial::connect_tls`], erased. Refuses by default.
    ///
    /// # Errors
    ///
    /// See [`Dial::connect_tls`].
    fn connect_tls_boxed<'a>(&'a self, stream: BoxIo, req: ProxyTls<'a>) -> BoxDialing<'a> {
        let _ = (stream, req);
        Box::pin(std::future::ready(Err(lends_no_tls())))
    }
    /// [`Dial::bind_udp`]. Refuses by default.
    ///
    /// # Errors
    ///
    /// See [`Dial::bind_udp`].
    fn bind_udp(&self, local: SocketAddr) -> Result<BoxUdp, Error> {
        let _ = local;
        Err(lends_nothing("UDP"))
    }
    /// [`Dial::resolve`], erased. Refuses by default.
    ///
    /// # Errors
    ///
    /// See [`Dial::resolve`].
    fn resolve_boxed<'a>(&'a self, host: &'a str, port: u16) -> BoxResolving<'a> {
        let _ = (host, port);
        Box::pin(std::future::ready(Err(lends_nothing("name resolution"))))
    }
    /// [`Dial::connect_tunnel`], erased. Refuses by default.
    ///
    /// # Errors
    ///
    /// See [`Dial::connect_tunnel`].
    fn connect_tunnel_boxed<'a>(&'a self, req: TunnelRequest<'a>) -> BoxTunnelling<'a> {
        let _ = req;
        Box::pin(std::future::ready(Err(lends_nothing("HTTP tunnels"))))
    }
}

/// A [`DynDial`] that can cross threads — what [`BoxDial`] holds.
pub type SharedDial<'a> = dyn DynDial + Send + Sync + 'a; // send-bound-exception: amendment-C16

/// The concrete [`Dial`] an erased filter is handed.
#[derive(Clone, Copy)]
pub struct BoxDial<'a>(&'a SharedDial<'a>);

impl<'a> BoxDial<'a> {
    /// Lend `dial` to an erased filter.
    #[must_use]
    pub const fn new(dial: &'a SharedDial<'a>) -> Self {
        Self(dial)
    }
}

impl std::fmt::Debug for BoxDial<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BoxDial")
    }
}

impl Dial for BoxDial<'_> {
    type Stream = BoxIo;

    fn connect<'b>(
        &'b self,
        host: &'b str,
        port: u16,
    ) -> impl Future<Output = Result<BoxIo, Error>> + 'b {
        self.0.connect_boxed(host, port)
    }

    fn connect_ipc<'b>(
        &'b self,
        addr: &'b hclient_rt::IpcAddr,
    ) -> impl Future<Output = Result<BoxIo, Error>> + 'b {
        self.0.connect_ipc_boxed(addr)
    }

    fn remaining(&self) -> Option<Duration> {
        self.0.remaining()
    }

    fn connect_tls<'b>(
        &'b self,
        stream: BoxIo,
        req: ProxyTls<'b>,
    ) -> impl Future<Output = Result<BoxIo, Error>> + 'b {
        self.0.connect_tls_boxed(stream, req)
    }

    fn bind_udp(&self, local: SocketAddr) -> Result<BoxUdp, Error> {
        DynDial::bind_udp(self.0, local)
    }

    fn resolve<'b>(
        &'b self,
        host: &'b str,
        port: u16,
    ) -> impl Future<Output = Result<Vec<SocketAddr>, Error>> + 'b {
        self.0.resolve_boxed(host, port)
    }

    fn connect_tunnel<'b>(
        &'b self,
        req: TunnelRequest<'b>,
    ) -> impl Future<Output = Result<Tunnel, Error>> + 'b {
        self.0.connect_tunnel_boxed(req)
    }
}

/// A [`SendEgressFilter`] a transport can share across threads.
pub type SharedFilter = dyn SendEgressFilter + Send + Sync; // send-bound-exception: amendment-C16

/// [`SendEgressFilter::open_stream_send`]'s future.
pub type BoxOpening<'a> =
    Pin<Box<dyn Future<Output = Result<Opened<BoxIo, BoxIo>, Attempt>> + Send + 'a>>; // send-bound-exception: amendment-C16

// Maintainer notes (not rendered):
// The `Transport`/`SendTransport` split, amendment C16.
/// A filter a transport can erase.
///
/// The two methods are written where every type is concrete — `Self` and
/// [`BoxDial`] — so `Send` is inferred rather than proven, the shape of
/// `hclient_core::transport::SendTransport`. Every implementation is the
/// few lines below:
///
/// ```no_run
/// use hclient_proxy::{
///     Attempt, BoxDial, BoxOpening, BoxPathOpening, Decision, Dial, EgressFilter, Io, Opened,
///     Rules, SendEgressFilter, Target,
/// };
///
/// /// A filter that adds nothing to the built-in rules.
/// struct Mine(Rules);
///
/// impl EgressFilter for Mine {
///     type Wrapped<S: Io> = S;
///
///     fn route(&self, t: &Target<'_>) -> Decision<'_> {
///         self.0.route(t)
///     }
///     async fn open_stream<'a, C: Dial + 'a>(
///         &'a self,
///         t: Target<'a>,
///         ctx: &'a C,
///     ) -> Result<Opened<C::Stream, C::Stream>, Attempt>
///     where
///         Self: Sized,
///     {
///         self.0.open_stream(t, ctx).await
///     }
/// }
///
/// impl SendEgressFilter for Mine {
///     fn open_stream_send<'a>(&'a self, t: Target<'a>, ctx: &'a BoxDial<'a>) -> BoxOpening<'a> {
///         Box::pin(async move { self.open_stream(t, ctx).await.map(hclient_proxy::erase) })
///     }
///     fn open_datagrams_send<'a>(&'a self, t: Target<'a>, ctx: &'a BoxDial<'a>) -> BoxPathOpening<'a> {
///         Box::pin(self.open_datagrams(t, ctx))
///     }
/// }
/// ```
///
/// Both methods are required, and the second is the same one line for
/// every filter. A filter that left it out would compile against a
/// default refusal and the erased path would never reach its own
/// [`EgressFilter::open_datagrams`] — so a filter that opens datagram
/// paths would be sent over a stream instead, and remembered as unable
/// to carry HTTP/3. Leaving it out is a compile error:
///
/// ```compile_fail,E0046
/// use hclient_proxy::{
///     Attempt, BoxDial, BoxOpening, Decision, Dial, EgressFilter, Io, Opened, Rules,
///     SendEgressFilter, Target,
/// };
///
/// struct Forgot(Rules);
///
/// impl EgressFilter for Forgot {
///     type Wrapped<S: Io> = S;
///     fn route(&self, t: &Target<'_>) -> Decision<'_> {
///         self.0.route(t)
///     }
///     async fn open_stream<'a, C: Dial + 'a>(
///         &'a self,
///         t: Target<'a>,
///         ctx: &'a C,
///     ) -> Result<Opened<C::Stream, C::Stream>, Attempt>
///     where
///         Self: Sized,
///     {
///         self.0.open_stream(t, ctx).await
///     }
/// }
///
/// impl SendEgressFilter for Forgot {
///     fn open_stream_send<'a>(&'a self, t: Target<'a>, ctx: &'a BoxDial<'a>) -> BoxOpening<'a> {
///         Box::pin(async move { self.open_stream(t, ctx).await.map(hclient_proxy::erase) })
///     }
/// }
/// ```
///
/// Declares no auto traits; a transport stores one as [`SharedFilter`].
pub trait SendEgressFilter: EgressFilter {
    /// [`EgressFilter::open_stream`], over an erased context, boxed.
    fn open_stream_send<'a>(&'a self, target: Target<'a>, ctx: &'a BoxDial<'a>) -> BoxOpening<'a>;

    /// [`EgressFilter::open_datagrams`], over an erased context, boxed.
    ///
    /// Required, and always `Box::pin(self.open_datagrams(target, ctx))`:
    /// there is no default, so that this cannot silently answer something
    /// other than what [`EgressFilter::open_datagrams`] answers.
    fn open_datagrams_send<'a>(
        &'a self,
        target: Target<'a>,
        ctx: &'a BoxDial<'a>,
    ) -> BoxPathOpening<'a>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TunnelVersion;

    /// A `Dial` that lends no TLS: `connect_tls` is left to its default.
    struct Bare;
    impl Dial for Bare {
        type Stream = BoxIo;
        fn connect<'a>(
            &'a self,
            _: &'a str,
            _: u16,
        ) -> impl Future<Output = Result<BoxIo, Error>> + 'a {
            std::future::ready(Err(Error::new(
                ErrorKind::Connect,
                std::io::Error::other("no"),
            )))
        }
        fn connect_ipc<'a>(
            &'a self,
            _: &'a hclient_rt::IpcAddr,
        ) -> impl Future<Output = Result<BoxIo, Error>> + 'a {
            std::future::ready(Err(Error::new(
                ErrorKind::Connect,
                std::io::Error::other("no"),
            )))
        }
        fn remaining(&self) -> Option<Duration> {
            None
        }
    }

    /// A stream that reads nothing and swallows writes — enough to hand
    /// `connect_tls` something to refuse or forward.
    struct Empty;
    impl AsyncRead for Empty {
        fn poll_read(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: &mut [u8],
        ) -> Poll<std::io::Result<usize>> {
            Poll::Ready(Ok(0))
        }
    }
    impl AsyncWrite for Empty {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            b: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Poll::Ready(Ok(b.len()))
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }
    impl Shutdown for Empty {
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    fn empty_io() -> BoxIo {
        BoxIo::new(Empty)
    }

    #[test]
    fn a_dial_that_lends_no_tls_refuses_it_as_unsupported() {
        let Err(err) = futures_executor::block_on(Bare.connect_tls(empty_io(), ProxyTls::new("p")))
        else {
            panic!("a transport that lends no TLS must refuse it");
        };
        assert_eq!(*err.kind(), ErrorKind::Unsupported);
    }

    type Asked = (String, Vec<Vec<u8>>, Option<String>);

    /// Records what an erased `connect_tls` was asked.
    #[derive(Default)]
    struct Recorded(std::sync::Mutex<Vec<Asked>>);
    impl DynDial for Recorded {
        fn connect_boxed<'a>(&'a self, _: &'a str, _: u16) -> BoxDialing<'a> {
            Box::pin(async { Ok(empty_io()) })
        }
        fn connect_ipc_boxed<'a>(&'a self, _: &'a hclient_rt::IpcAddr) -> BoxDialing<'a> {
            Box::pin(async { Ok(empty_io()) })
        }
        fn connect_tls_boxed<'a>(&'a self, s: BoxIo, req: ProxyTls<'a>) -> BoxDialing<'a> {
            self.0.lock().unwrap().push((
                req.server_name.to_owned(),
                req.alpn.iter().map(|a| a.to_vec()).collect(),
                req.identity.map(ToOwned::to_owned),
            ));
            Box::pin(async move { Ok(s) })
        }
        fn remaining(&self) -> Option<Duration> {
            None
        }
    }

    #[test]
    fn the_erased_dial_forwards_connect_tls_with_every_field() {
        let rec = Recorded::default();
        let shared: &SharedDial<'_> = &rec;
        let dial = BoxDial::new(shared);
        let alpn: &[&[u8]] = &[b"http/1.1"];
        futures_executor::block_on(dial.connect_tls(
            empty_io(),
            ProxyTls::new("proxy.test").alpn(alpn).identity(Some("t1")),
        ))
        .expect("forwarded");
        assert_eq!(
            *rec.0.lock().unwrap(),
            [(
                "proxy.test".to_owned(),
                vec![b"http/1.1".to_vec()],
                Some("t1".to_owned())
            )]
        );
    }

    #[test]
    fn a_proxy_tls_request_defaults_to_understating() {
        let r = ProxyTls::new("p");
        assert_eq!(r.server_name, "p");
        assert!(r.alpn.is_empty());
        assert_eq!(r.identity, None);
    }

    #[test]
    fn only_unsupported_permits_a_switch() {
        let e = || Error::new(ErrorKind::Connect, std::io::Error::other("x"));
        assert!(!Attempt::Failed(e()).permits_switch());
        assert!(Attempt::Unsupported(e()).permits_switch());
    }

    #[test]
    fn every_outcome_keeps_its_error() {
        for a in [
            Attempt::Failed(Error::new(ErrorKind::Connect, std::io::Error::other("f"))),
            Attempt::Unsupported(Error::new(
                ErrorKind::Unsupported,
                std::io::Error::other("s"),
            )),
        ] {
            let expected = match &a {
                Attempt::Unsupported(_) => ErrorKind::Unsupported,
                Attempt::Failed(_) => ErrorKind::Connect,
            };
            assert_eq!(*a.into_error().kind(), expected);
        }
    }

    #[test]
    fn the_starting_points_claim_what_they_say() {
        assert_eq!(
            FilterSupport::NONE,
            FilterSupport {
                stream: false,
                datagrams: false
            }
        );
        assert_eq!(
            FilterSupport::STREAM,
            FilterSupport {
                stream: true,
                datagrams: false
            }
        );
    }

    #[test]
    fn with_datagrams_adds_the_claim_and_keeps_the_rest() {
        let s = FilterSupport::STREAM.with_datagrams();
        assert!(s.stream && s.datagrams);
        let n = FilterSupport::NONE.with_datagrams();
        assert!(!n.stream && n.datagrams);
    }

    /// A stream whose every answer is distinguishable from a default, so a
    /// wrapper that forwards nothing is caught by the value it hands back.
    struct Marked;
    impl AsyncRead for Marked {
        fn poll_read(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &mut [u8],
        ) -> Poll<std::io::Result<usize>> {
            buf[..3].copy_from_slice(b"abc");
            Poll::Ready(Ok(3))
        }
    }
    fn marked(what: &str) -> std::io::Error {
        std::io::Error::other(what.to_owned())
    }
    impl AsyncWrite for Marked {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            b: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Poll::Ready(Ok(b.len()))
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Err(marked("flush")))
        }
        fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Err(marked("close")))
        }
    }
    impl Shutdown for Marked {
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Err(marked("shutdown")))
        }
    }

    #[test]
    fn a_boxed_stream_forwards_every_call_to_the_stream_inside() {
        let mut io = BoxIo::new(Marked);
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut buf = [0u8; 8];
        let Poll::Ready(Ok(n)) = Pin::new(&mut io).poll_read(&mut cx, &mut buf) else {
            panic!("read")
        };
        assert_eq!(&buf[..n], b"abc");
        let Poll::Ready(Ok(n)) = Pin::new(&mut io).poll_write(&mut cx, b"hello") else {
            panic!("write")
        };
        assert_eq!(n, 5);
        let err = |p: Poll<std::io::Result<()>>| match p {
            Poll::Ready(Err(e)) => e.to_string(),
            other => panic!("{other:?}"),
        };
        assert_eq!(err(Pin::new(&mut io).poll_flush(&mut cx)), "flush");
        assert_eq!(err(Pin::new(&mut io).poll_close(&mut cx)), "close");
        assert_eq!(err(Pin::new(&mut io).poll_shutdown(&mut cx)), "shutdown");
    }

    #[test]
    fn a_dial_that_lends_no_datagrams_refuses_all_three_as_unsupported() {
        let udp = Dial::bind_udp(&Bare, "0.0.0.0:0".parse().unwrap()).unwrap_err();
        assert_eq!(*udp.kind(), ErrorKind::Unsupported);
        let res = futures_executor::block_on(Bare.resolve("p", 1)).unwrap_err();
        assert_eq!(*res.kind(), ErrorKind::Unsupported);
        let tun = futures_executor::block_on(Bare.connect_tunnel(TunnelRequest::new(
            "p",
            443,
            ProxyTls::new("p"),
            "o:443",
        )))
        .unwrap_err();
        assert_eq!(*tun.kind(), ErrorKind::Unsupported);
    }

    #[test]
    fn a_tunnel_request_defaults_to_plain_connect_over_h2() {
        let r = TunnelRequest::new("p", 443, ProxyTls::new("p"), "o:443");
        assert_eq!(
            (r.proxy_host, r.proxy_port, r.authority),
            ("p", 443, "o:443")
        );
        assert_eq!(r.path, None);
        assert_eq!(r.protocol, None);
        assert!(r.headers.is_empty());
        assert_eq!(r.effective_version(), TunnelVersion::Http2);
        let r = r
            .path(Some("/x"))
            .protocol(Some("connect-udp"))
            .version(TunnelVersion::Http2)
            .header(
                http::header::HeaderName::from_static("capsule-protocol"),
                http::HeaderValue::from_static("?1"),
            );
        assert_eq!(
            (r.path, r.protocol, r.version),
            (Some("/x"), Some("connect-udp"), Some(TunnelVersion::Http2))
        );
        assert_eq!(r.headers["capsule-protocol"], "?1");
    }

    #[test]
    fn the_erased_dial_forwards_resolve_and_tunnel() {
        struct Lends;
        impl DynDial for Lends {
            fn connect_boxed<'a>(&'a self, _: &'a str, _: u16) -> BoxDialing<'a> {
                unreachable!()
            }
            fn connect_ipc_boxed<'a>(&'a self, _: &'a hclient_rt::IpcAddr) -> BoxDialing<'a> {
                unreachable!()
            }
            fn remaining(&self) -> Option<Duration> {
                None
            }
            fn resolve_boxed<'a>(&'a self, host: &'a str, port: u16) -> BoxResolving<'a> {
                assert_eq!((host, port), ("proxy.test", 1080));
                Box::pin(async { Ok(vec!["192.0.2.7:1080".parse().unwrap()]) })
            }
            fn connect_tunnel_boxed<'a>(&'a self, req: TunnelRequest<'a>) -> BoxTunnelling<'a> {
                assert_eq!(req.protocol, Some("connect-udp"));
                Box::pin(async {
                    let head = http::Response::builder()
                        .status(200)
                        .body(())
                        .unwrap()
                        .into_parts()
                        .0;
                    Ok(Tunnel::new(head, empty_io(), None))
                })
            }
        }
        let shared: &SharedDial<'_> = &Lends;
        let dial = BoxDial::new(shared);
        let got = futures_executor::block_on(dial.resolve("proxy.test", 1080)).unwrap();
        assert_eq!(got, ["192.0.2.7:1080".parse().unwrap()]);
        let t = futures_executor::block_on(dial.connect_tunnel(
            TunnelRequest::new("p", 443, ProxyTls::new("p"), "o:443").protocol(Some("connect-udp")),
        ))
        .unwrap();
        assert_eq!(t.response.status, 200);
    }

    impl DynDial for Bare {
        fn connect_boxed<'a>(&'a self, _: &'a str, _: u16) -> BoxDialing<'a> {
            unreachable!()
        }
        fn connect_ipc_boxed<'a>(&'a self, _: &'a hclient_rt::IpcAddr) -> BoxDialing<'a> {
            unreachable!()
        }
        fn remaining(&self) -> Option<Duration> {
            None
        }
    }

    #[test]
    fn a_filter_that_declares_no_datagrams_refuses_them_as_unsupported() {
        struct StreamOnly;
        impl EgressFilter for StreamOnly {
            type Wrapped<S: Io> = S;
            fn route(&self, _: &Target<'_>) -> Decision<'_> {
                Decision::Direct
            }
            #[allow(
                clippy::unused_async_trait_impl,
                reason = "open_stream returns `impl Future` (an RPITIT), and `async fn` is the idiomatic way to implement one; this test never calls it, so the body needs no `.await`"
            )]
            async fn open_stream<'a, C: Dial + 'a>(
                &'a self,
                _: Target<'a>,
                _: &'a C,
            ) -> Result<Opened<C::Stream, C::Stream>, Attempt>
            where
                Self: Sized,
            {
                unreachable!()
            }
        }
        impl SendEgressFilter for StreamOnly {
            fn open_stream_send<'a>(&'a self, _: Target<'a>, _: &'a BoxDial<'a>) -> BoxOpening<'a> {
                unreachable!()
            }
            fn open_datagrams_send<'a>(
                &'a self,
                t: Target<'a>,
                ctx: &'a BoxDial<'a>,
            ) -> BoxPathOpening<'a> {
                Box::pin(self.open_datagrams(t, ctx))
            }
        }
        let t = Target::new("o", 443, true);
        let a = futures_executor::block_on(StreamOnly.open_datagrams(t, &Bare)).unwrap_err();
        assert!(a.permits_switch());
        let shared: &SharedDial<'_> = &Bare;
        let erased = BoxDial::new(shared);
        let a = futures_executor::block_on(StreamOnly.open_datagrams_send(t, &erased)).unwrap_err();
        assert_eq!(*a.into_error().kind(), ErrorKind::Unsupported);
    }

    #[test]
    fn the_erased_dial_reports_the_budget_it_was_lent() {
        struct Budget;
        impl DynDial for Budget {
            fn connect_boxed<'a>(&'a self, _: &'a str, _: u16) -> BoxDialing<'a> {
                unreachable!()
            }
            fn connect_ipc_boxed<'a>(&'a self, _: &'a hclient_rt::IpcAddr) -> BoxDialing<'a> {
                unreachable!()
            }
            fn remaining(&self) -> Option<Duration> {
                Some(Duration::from_millis(7))
            }
        }
        let shared: &SharedDial<'_> = &Budget;
        assert_eq!(
            Dial::remaining(&BoxDial::new(shared)),
            Some(Duration::from_millis(7))
        );
    }
}
