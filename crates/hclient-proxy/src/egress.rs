//! Where a connection goes: the seam a transport asks, per request, and
//! the context it lends a filter to act on the answer.
//!
//! A transport asks [`EgressFilter::route`] before it resolves anything.
//! [`Decision::Direct`] means its own path, in full; [`Decision::Filtered`]
//! means the filter carries the request, and the transport calls
//! [`EgressFilter::open_stream`] with a [`Dial`] — its own way of opening
//! connections — for the filter to use. Nothing a filter answers ever
//! sends a filtered request direct.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use futures_io::{AsyncRead, AsyncWrite};
use hclient_core::error::Error;
#[cfg(test)]
use hclient_core::error::ErrorKind;
use hclient_rt::Shutdown;

/// What a transport lends a filter: its own way of opening a connection.
///
/// `impl Future` rather than named associated types, and on purpose: a
/// transport calls a filter it holds concretely, where the returned
/// futures' auto traits are *inferred*, so the default filter asks nothing
/// of a runtime that is not `Send`. Erasure goes through [`BoxDial`], a
/// concrete `Dial`, and never needs to name these futures.
pub trait Dial {
    /// The byte stream this transport's runtime produces.
    type Stream: AsyncRead + AsyncWrite + Shutdown + Unpin;

    /// Resolve `host` and connect to `port` exactly as the transport would
    /// for a direct request — its resolver, Happy Eyeballs, its socket
    /// options, its hooks' timing. A filter that must not resolve a name
    /// locally never passes that name here.
    fn connect(
        &self,
        host: &str,
        port: u16,
    ) -> impl Future<Output = Result<Self::Stream, Error>> + '_;

    /// Open a same-machine connection.
    ///
    /// Refuses with [`ErrorKind::Unsupported`](hclient_core::error::ErrorKind::Unsupported)
    /// when the transport cannot open this kind of address.
    fn connect_ipc(
        &self,
        addr: &hclient_rt::IpcAddr,
    ) -> impl Future<Output = Result<Self::Stream, Error>> + '_;

    /// What is left of the request's connect bound, if it has one. A
    /// filter spends it; it is never handed a fresh one.
    fn remaining(&self) -> Option<Duration>;
}

/// The origin a request is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target<'a> {
    /// The origin's host, as the request names it.
    pub host: &'a str,
    /// The origin's port, the scheme's default filled in.
    pub port: u16,
    /// Whether the request is `https`.
    pub use_tls: bool,
}

/// A filter's answer for one target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// The transport's ordinary path, in full.
    Direct,
    /// This filter carries the request.
    Filtered {
        /// What this filter can carry for this target.
        support: FilterSupport,
        /// This filter's part of the pool key. Two requests whose keys
        /// differ never share a connection.
        pool_key: Box<str>,
        /// How the request head is written on the stream the filter opens.
        form: RequestForm,
    },
}

/// How the request head is written once the filter's stream is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestForm {
    /// As to the origin — a tunnel, or no proxy at all.
    Origin,
    /// RFC 9112 §3.2.2 absolute-form, to an HTTP proxy acting as the
    /// origin server for an `http://` request.
    Absolute {
        /// `Proxy-Authorization` for the proxy, if it wants one.
        proxy_authorization: Option<http::HeaderValue>,
    },
}

/// What a filter can carry for a target, declared by the filter itself.
///
/// Built by the filter, so — like `hclient_rt::TcpSupport` — it is not
/// `#[non_exhaustive]`; start from [`NONE`](Self::NONE) or
/// [`STREAM`](Self::STREAM).
///
/// `datagrams: true` is honoured by no transport yet: a transport that
/// cannot carry QUIC over a filter treats it as `false`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
}

/// A byte stream the seam can carry, erased.
pub trait Io: AsyncRead + AsyncWrite + Shutdown + Unpin {}
impl<T: AsyncRead + AsyncWrite + Shutdown + Unpin> Io for T {}

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
pub enum Opened<S> {
    /// The context's own stream type: a handshake changes bytes, not the
    /// type, so the transport keeps its concrete connection.
    Raw(S),
    /// A stream a filter wrapped (TLS to a proxy, a custom layer).
    Boxed(BoxIo),
}

impl<S> std::fmt::Debug for Opened<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Raw(_) => "Opened::Raw",
            Self::Boxed(_) => "Opened::Boxed",
        })
    }
}

/// How an attempt through a filter failed.
///
/// Three outcomes because only one of them permits trying another way
/// through the same filter: a proxy that is unreachable, or that refused
/// this target, will not do better by being asked for a stream instead of
/// datagrams, and a proxy that does not support datagrams might.
#[derive(Debug)]
pub enum Attempt {
    /// The filter could not reach its proxy.
    Unreachable(Error),
    /// The proxy refused this target.
    Refused(Error),
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
            Self::Unreachable(e) | Self::Refused(e) | Self::Unsupported(e) => e,
        }
    }
}

/// Decides where each request's connection goes, and opens it.
pub trait EgressFilter {
    /// Asked once per request, before anything is resolved.
    fn route(&self, target: &Target<'_>) -> Decision;

    /// Open a byte stream to `target`, for a request [`route`](Self::route)
    /// answered `Filtered` with `support.stream`.
    ///
    /// `where Self: Sized` keeps [`SendEgressFilter`] usable as `dyn`: a
    /// generic method cannot be in a vtable, and the erased path calls
    /// [`SendEgressFilter::open_stream_send`] instead.
    ///
    /// # Errors
    ///
    /// An [`Attempt`] saying which of the three ways it failed.
    fn open_stream<'a, C: Dial + 'a>(
        &'a self,
        target: Target<'a>,
        ctx: &'a C,
    ) -> impl Future<Output = Result<Opened<C::Stream>, Attempt>> + 'a
    where
        Self: Sized;
}

/// [`DynDial`]'s futures.
pub type BoxDialing<'a> = Pin<Box<dyn Future<Output = Result<BoxIo, Error>> + Send + 'a>>; // send-bound-exception: amendment-C16

/// An object-safe [`Dial`] whose streams and futures are erased and
/// `Send`, for a transport to lend an erased filter.
///
/// Declares no auto traits, by this workspace's rule for a seam: `Send`
/// and `Sync` are demanded where it is stored, by [`SharedDial`].
pub trait DynDial {
    /// [`Dial::connect`], erased.
    fn connect_boxed<'a>(&'a self, host: &'a str, port: u16) -> BoxDialing<'a>;
    /// [`Dial::connect_ipc`], erased.
    fn connect_ipc_boxed<'a>(&'a self, addr: &'a hclient_rt::IpcAddr) -> BoxDialing<'a>;
    /// [`Dial::remaining`].
    fn remaining(&self) -> Option<Duration>;
}

/// A [`DynDial`] that can cross threads — what [`BoxDial`] holds.
pub type SharedDial = dyn DynDial + Send + Sync; // send-bound-exception: amendment-C16

/// The concrete [`Dial`] an erased filter is handed.
#[derive(Clone, Copy)]
pub struct BoxDial<'a>(pub &'a SharedDial);

impl std::fmt::Debug for BoxDial<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BoxDial")
    }
}

impl Dial for BoxDial<'_> {
    type Stream = BoxIo;

    fn connect(&self, host: &str, port: u16) -> impl Future<Output = Result<BoxIo, Error>> + '_ {
        // Owned, because `connect_boxed` ties the host's lifetime to its
        // future and this signature does not: one allocation per connect,
        // on the erased path only.
        let host = host.to_owned();
        async move { self.0.connect_boxed(&host, port).await }
    }

    fn connect_ipc(
        &self,
        addr: &hclient_rt::IpcAddr,
    ) -> impl Future<Output = Result<BoxIo, Error>> + '_ {
        let addr = addr.clone();
        async move { self.0.connect_ipc_boxed(&addr).await }
    }

    fn remaining(&self) -> Option<Duration> {
        self.0.remaining()
    }
}

/// A [`SendEgressFilter`] a transport can share across threads.
pub type SharedFilter = dyn SendEgressFilter + Send + Sync; // send-bound-exception: amendment-C16

/// [`SendEgressFilter::open_stream_send`]'s future.
pub type BoxOpening<'a, S> = Pin<Box<dyn Future<Output = Result<Opened<S>, Attempt>> + Send + 'a>>; // send-bound-exception: amendment-C16

// Maintainer notes (not rendered):
// The `Transport`/`SendTransport` split, amendment C16.
/// A filter a transport can erase.
///
/// The one method is written where every type is concrete — `Self` and
/// [`BoxDial`] — so `Send` is inferred rather than proven, the shape of
/// `hclient_core::transport::SendTransport`. Every implementation is the
/// one line below:
///
/// ```no_run
/// use hclient_proxy::{
///     Attempt, BoxDial, BoxIo, BoxOpening, Decision, Dial, EgressFilter, Opened, Rules,
///     SendEgressFilter, Target,
/// };
///
/// /// A filter that adds nothing to the built-in rules.
/// struct Mine(Rules);
///
/// impl EgressFilter for Mine {
///     fn route(&self, t: &Target<'_>) -> Decision {
///         self.0.route(t)
///     }
///     async fn open_stream<'a, C: Dial + 'a>(
///         &'a self,
///         t: Target<'a>,
///         ctx: &'a C,
///     ) -> Result<Opened<C::Stream>, Attempt>
///     where
///         Self: Sized,
///     {
///         self.0.open_stream(t, ctx).await
///     }
/// }
///
/// impl SendEgressFilter for Mine {
///     fn open_stream_send<'a>(&'a self, t: Target<'a>, ctx: &'a BoxDial<'a>) -> BoxOpening<'a, BoxIo> {
///         Box::pin(self.open_stream(t, ctx))
///     }
/// }
/// ```
///
/// Declares no auto traits; a transport stores one as [`SharedFilter`].
pub trait SendEgressFilter: EgressFilter {
    /// [`EgressFilter::open_stream`], over an erased context, boxed.
    fn open_stream_send<'a>(
        &'a self,
        target: Target<'a>,
        ctx: &'a BoxDial<'a>,
    ) -> BoxOpening<'a, BoxIo>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_unsupported_permits_a_switch() {
        let e = || Error::new(ErrorKind::Connect, std::io::Error::other("x"));
        assert!(!Attempt::Unreachable(e()).permits_switch());
        assert!(!Attempt::Refused(e()).permits_switch());
        assert!(Attempt::Unsupported(e()).permits_switch());
    }

    #[test]
    fn every_outcome_keeps_its_error() {
        for a in [
            Attempt::Unreachable(Error::new(ErrorKind::Connect, std::io::Error::other("u"))),
            Attempt::Refused(Error::new(ErrorKind::Connect, std::io::Error::other("r"))),
            Attempt::Unsupported(Error::new(
                ErrorKind::Unsupported,
                std::io::Error::other("s"),
            )),
        ] {
            let expected = match &a {
                Attempt::Unsupported(_) => ErrorKind::Unsupported,
                _ => ErrorKind::Connect,
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
}
