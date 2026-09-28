//! An egress filter from outside this crate, erased where it is installed.
//!
//! `Native::egress` is the one place the runtime, the resolver and the TLS
//! backend are known concretely *and* their futures are proven `Send`, so
//! that is where the filter's whole connect path is monomorphised into a
//! function pointer — the IPC dialler's arrangement before it, and
//! `SpawnH2`'s. The connector calls the pointer and names none of those
//! bounds, which is what keeps `Native<Embassy, ..>` a `Transport`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use hclient_core::error::Error;
use hclient_core::hooks::{Event, Hooks, NoHooks};
use hclient_dns::Resolve;
use hclient_proxy::{BoxDial, BoxIo, Opened, SharedDial, SharedFilter, Target};
use hclient_rt::{TcpConnect, TcpOpts, Timer};
use hclient_tls::{TlsConnect, TlsInfo, TlsRequest};

use crate::DialIpc;
use crate::connect::{Attempted, Conn};
use crate::dial::NativeDial;
use crate::{mark, since};

/// A connection an external filter opened, with TLS finished over it.
pub(crate) type Opening<'a, R, L> = Pin<
    Box<
        dyn Future<
                Output = Result<
                    (
                        crate::NativeIo<R, L>,
                        Option<TlsInfo>,
                        Option<Box<Attempted>>,
                    ),
                    Error,
                >,
            > + Send // send-bound-exception: amendment-C15
            + 'a,
    >,
>;

/// Everything the filter's connect path needs from the transport and the
/// request, borrowed for one connect.
pub(crate) struct Call<'a, R: TcpConnect + Timer, D, L> {
    pub(crate) rt: &'a R,
    pub(crate) dns: &'a D,
    pub(crate) tls: &'a L,
    pub(crate) opts: &'a TcpOpts,
    pub(crate) ipc: Option<DialIpc<R>>,
    pub(crate) udp: Option<crate::BindUdp<R>>,
    pub(crate) budget: Option<Duration>,
    /// Whether a hook is watching — `H::WATCHING`, carried as a value so
    /// that the stored pointer names no hook type and `Native::hooks` can
    /// change `H` without dropping the filter.
    pub(crate) watching: bool,
    pub(crate) target: Target<'a>,
    pub(crate) alpn: &'a [&'a [u8]],
    pub(crate) identity: Option<&'a str>,
}

impl<'a, R: TcpConnect + Timer, D, L> Call<'a, R, D, L> {
    /// The connect path this call lends a filter — one construction for
    /// every pointer here, so a stream and a datagram path are opened
    /// through the same context.
    fn dial<H>(&self, began: Option<R::Instant>) -> NativeDial<'a, R, D, L, H> {
        NativeDial::new(
            self.rt,
            self.dns,
            self.tls,
            self.opts,
            self.ipc,
            self.budget,
            began,
            self.udp,
        )
    }
}

pub(crate) type Open<R, D, L> =
    for<'a> fn(&'a SharedFilter, Call<'a, R, D, L>) -> Opening<'a, R, L>;

/// A filter's datagram path, opened through the transport's lent connect
/// path — `F` is an installed filter ([`SharedFilter`]) or the built-in
/// rules.
///
/// A pointer for [`Open`]'s reason: the context the filter is lent is
/// erased where its futures can be proven `Send`, which is where this is
/// monomorphised, so the routing that calls it names none of those bounds.
/// Only `alpn`, `identity` and `watching` of the [`Call`] go unread: a path
/// carries datagrams, and TLS to the origin is QUIC's.
#[cfg(feature = "http3")]
pub(crate) type OpenPath<R, D, L, F> =
    for<'a> fn(&'a F, Call<'a, R, D, L>) -> hclient_proxy::BoxPathOpening<'a>;

/// The installed filter and its monomorphised connect paths.
pub(crate) struct External<R: TcpConnect + Timer, D, L: TlsConnect> {
    pub(crate) filter: Arc<SharedFilter>,
    pub(crate) open: Open<R, D, L>,
    #[cfg(feature = "http3")]
    pub(crate) open_path: OpenPath<R, D, L, SharedFilter>,
}

impl<R: TcpConnect + Timer, D, L: TlsConnect> Clone for External<R, D, L> {
    fn clone(&self) -> Self {
        Self {
            filter: Arc::clone(&self.filter),
            open: self.open,
            #[cfg(feature = "http3")]
            open_path: self.open_path,
        }
    }
}

impl<R: TcpConnect + Timer, D, L: TlsConnect> std::fmt::Debug for External<R, D, L> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("External")
    }
}

/// A hook type that watches and hears nothing: it makes the timing half of
/// the connect path run exactly as it would for a real hook, where the
/// caller has one.
#[derive(Clone, Copy)]
struct Timed;

impl Hooks for Timed {
    const WATCHING: bool = true;
    fn on(&self, _event: &Event<'_>) {}
}

/// [`Open`]'s one body, instantiated in `Native::egress`.
pub(crate) fn open<'a, R, D, L>(filter: &'a SharedFilter, c: Call<'a, R, D, L>) -> Opening<'a, R, L>
where
    R: TcpConnect + Timer + Sync,     // send-bound-exception: amendment-C15
    R::Stream: Send + 'static,        // send-bound-exception: amendment-C15
    R::Instant: Send + Sync,          // send-bound-exception: amendment-C15
    R::Sleep: Send,                   // send-bound-exception: amendment-C15
    for<'x> R::Connecting<'x>: Send,  // send-bound-exception: amendment-C15
    D: Resolve + Sync,                // send-bound-exception: amendment-C15
    for<'x> D::Records<'x>: Send,     // send-bound-exception: amendment-C15
    L: TlsConnect + Sync,             // send-bound-exception: amendment-C15
    L::Stream<BoxIo>: Send + 'static, // send-bound-exception: amendment-C15
    for<'x> L::Handshake<'x, BoxIo>: Send, // send-bound-exception: amendment-C15
{
    if c.watching {
        Box::pin(run::<R, D, L, Timed>(filter, c))
    } else {
        Box::pin(run::<R, D, L, NoHooks>(filter, c))
    }
}

async fn run<'a, R, D, L, H>(
    filter: &'a SharedFilter,
    c: Call<'a, R, D, L>,
) -> Result<
    (
        crate::NativeIo<R, L>,
        Option<TlsInfo>,
        Option<Box<Attempted>>,
    ),
    Error,
>
where
    R: TcpConnect + Timer + Sync,     // send-bound-exception: amendment-C15
    R::Stream: Send + 'static,        // send-bound-exception: amendment-C15
    R::Instant: Send + Sync,          // send-bound-exception: amendment-C15
    R::Sleep: Send,                   // send-bound-exception: amendment-C15
    for<'x> R::Connecting<'x>: Send,  // send-bound-exception: amendment-C15
    D: Resolve + Sync,                // send-bound-exception: amendment-C15
    for<'x> D::Records<'x>: Send,     // send-bound-exception: amendment-C15
    L: TlsConnect + Sync,             // send-bound-exception: amendment-C15
    L::Stream<BoxIo>: Send + 'static, // send-bound-exception: amendment-C15
    for<'x> L::Handshake<'x, BoxIo>: Send, // send-bound-exception: amendment-C15
    H: Hooks,
{
    let began = mark::<H, R>(c.rt);
    let dial = c.dial::<H>(began);
    let erased: &SharedDial<'_> = &dial;
    let boxed = BoxDial::new(erased);
    let opened = filter
        .open_stream_send(c.target, &boxed)
        .await
        .map_err(hclient_proxy::Attempt::into_error)?;
    let io = match opened {
        Opened::Raw(io) | Opened::Wrapped(io) => io,
    };
    let mut attempted = dial.take_attempted().or_else(|| {
        began.map(|b| {
            Box::new(Attempted {
                remote: None,
                dns: Duration::ZERO,
                tcp: since::<R>(c.rt, Some(b)),
                tls: None,
            })
        })
    });
    if !c.target.use_tls {
        return Ok((Conn::boxed(io), None, attempted));
    }
    // The origin's name, whatever carried the bytes: the certificate is
    // checked against who the caller asked for.
    let req =
        TlsRequest::new(hclient_core::url::bare_host(c.target.host), c.alpn).identity(c.identity);
    let handshake_began = mark::<H, R>(c.rt);
    let (tls_stream, info) = c.tls.connect(io, req).await?;
    if let Some(a) = attempted.as_mut() {
        a.tls = Some(since::<R>(c.rt, handshake_began));
    }
    Ok((Conn::boxed(BoxIo::new(tls_stream)), Some(info), attempted))
}

/// [`OpenPath`]'s one body, instantiated in `Native::egress` for an
/// installed filter and in `Native::http3` for the built-in rules.
///
/// The rules go through the erased context too, rather than being called
/// concretely as their streams are: `open_datagrams` demands a `Send`
/// stream of the context it is lent, and the concrete context's stream is
/// generic over the runtime and cannot be proven one there.
#[cfg(feature = "http3")]
pub(crate) fn open_path<'a, R, D, L, F>(
    filter: &'a F,
    c: Call<'a, R, D, L>,
) -> hclient_proxy::BoxPathOpening<'a>
where
    F: hclient_proxy::SendEgressFilter + Sync + ?Sized, // send-bound-exception: amendment-C15
    R: TcpConnect + Timer + Sync,                       // send-bound-exception: amendment-C15
    R::Stream: Send + 'static,                          // send-bound-exception: amendment-C15
    R::Instant: Send + Sync,                            // send-bound-exception: amendment-C15
    R::Sleep: Send,                                     // send-bound-exception: amendment-C15
    for<'x> R::Connecting<'x>: Send,                    // send-bound-exception: amendment-C15
    D: Resolve + Sync,                                  // send-bound-exception: amendment-C15
    for<'x> D::Records<'x>: Send,                       // send-bound-exception: amendment-C15
    L: TlsConnect + Sync,                               // send-bound-exception: amendment-C15
    L::Stream<BoxIo>: Send + 'static,                   // send-bound-exception: amendment-C15
    for<'x> L::Handshake<'x, BoxIo>: Send,              // send-bound-exception: amendment-C15
{
    Box::pin(async move {
        // No hook watches a path's opening: the connection it carries is
        // reported by the QUIC arm, with no remote address.
        let dial = c.dial::<NoHooks>(None);
        let erased: &SharedDial<'_> = &dial;
        let boxed = BoxDial::new(erased);
        filter.open_datagrams_send(c.target, &boxed).await
    })
}
