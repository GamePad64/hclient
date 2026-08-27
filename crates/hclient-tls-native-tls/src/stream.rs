//! The handshake and the stream, owned rather than borrowed from
//! `async-native-tls`.
//!
//! # Why this crate has them at all
//!
//! [`hclient_tls::TlsConnect::Handshake`] is an associated type, so that a
//! consumer can **name** the handshake's future and prove its own `Send` —
//! which is what `hclient::Client`'s request future being `Send` rests on.
//! An `async fn` body has no name, so a backend that awaits anything has to
//! write its handshake as a type with a `poll`.
//!
//! `async_native_tls::TlsConnector::connect` is a `pub async fn`, so it
//! could not be named; and driving `native_tls`'s own handshake by hand was
//! not enough either, because the stream it yields must then be one this
//! crate owns — `async_native_tls::TlsStream::new` is `pub(crate)` and its
//! `StdAdapter` is private. So both are here. This crate was a wrapper over
//! a wrapper; it is now a wrapper.
//!
//! **What that bought is not only the `Send`.** Owning the stream means
//! `native_tls::TlsStream::negotiated_alpn` is reachable, and this backend
//! reports ALPN now — a limitation its own documentation called concrete
//! and unavoidable for two verticals, and which was a property of the
//! wrapper rather than of the platform.
//!
//! # Why `native-tls` cannot be driven the way rustls is
//!
//! `hclient-tls-rustls` needs none of this: rustls is sans-io, so its
//! handshake is a loop over buffers this workspace owns and there is no
//! `Context` to smuggle. `native-tls` fronts SChannel, Security.framework
//! and OpenSSL through one synchronous `Read`/`Write` interface, and hands
//! back `HandshakeError::WouldBlock` when the stream underneath is not
//! ready. Bridging that to a poll-based world means giving the synchronous
//! side a `Read`/`Write` that can reach the current task's waker — which
//! is [`StdAdapter`], and which is why this file carries this workspace's
//! second `unsafe` (amendment C17, `docs/exceptions.md`).
//!
//! The shape is upstream's, arrived at by reading async-native-tls 0.6.0
//! (`std_adapter.rs`, `handshake.rs`, `tls_stream.rs`) rather than by
//! inventing a second answer to a solved problem. What is not upstream's
//! is the handshake being a **named type** instead of an `async fn`, which
//! is the entire reason for the port.

use futures_io::{AsyncRead, AsyncWrite};
use native_tls::{HandshakeError, MidHandshakeTlsStream};
use std::future::Future;
use std::io::{self, Read, Write};
use std::pin::Pin;
use std::ptr::null_mut;
use std::task::{Context, Poll};

/// A synchronous `Read`/`Write` over an asynchronous one, carrying the
/// current task's [`Context`] as a raw pointer for the length of one call.
///
/// # The `unsafe`, and what bounds it
///
/// The pointer is set immediately before a call into `native-tls` and
/// cleared immediately after — by a [`Guard`] whose `Drop` runs on the
/// panicking path too — so it is dereferenced only while the `&mut
/// Context` it was taken from is alive on the stack directly above.
/// [`with_context`](Self::with_context) asserts it is not null rather than
/// trusting that, which turns a violated invariant into a panic instead of
/// a use-after-free.
///
/// The two `unsafe impl`s say the pointer does not make this type less
/// `Send`/`Sync` than `S`: it is null except during a call that cannot
/// yield, so there is never a moment at which a value holding a live
/// pointer could be observed from another thread.
#[derive(Debug)]
pub(crate) struct StdAdapter<S> {
    pub(crate) inner: S,
    pub(crate) context: *mut (),
}

#[allow(
    unsafe_code, // unsafe-code-exception: amendment-C17
    reason = "the raw Context is null except during one non-yielding call; see the type's own doc"
)]
// SAFETY: `context` is null at every point a value of this type can be
// observed from anywhere but the call that set it — see the type doc.
unsafe impl<S: Send> Send for StdAdapter<S> {} // unsafe-code-exception: amendment-C17 send-bound-exception: amendment-C17
#[allow(
    unsafe_code, // unsafe-code-exception: amendment-C17
    reason = "as above"
)]
// SAFETY: as above.
unsafe impl<S: Sync> Sync for StdAdapter<S> {} // unsafe-code-exception: amendment-C17 send-bound-exception: amendment-C17

impl<S: Unpin> StdAdapter<S> {
    #[allow(
        unsafe_code, // unsafe-code-exception: amendment-C17
        reason = "reconstitutes the &mut Context set by the caller one frame below; asserted non-null"
    )]
    fn with_context<F, R>(&mut self, f: F) -> R
    where
        F: FnOnce(&mut Context<'_>, Pin<&mut S>) -> R,
    {
        assert!(
            !self.context.is_null(),
            "native-tls called back into the stream outside a poll — see StdAdapter's doc",
        );
        // SAFETY: non-null here means the pointer was set by `Guard::new`
        // one frame below and has not been cleared, so the `Context` it
        // came from is still borrowed and alive.
        let cx = unsafe { &mut *self.context.cast::<Context<'_>>() }; // unsafe-code-exception: amendment-C17
        f(cx, Pin::new(&mut self.inner))
    }
}

impl<S: AsyncRead + Unpin> Read for StdAdapter<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.with_context(|cx, s| s.poll_read(cx, buf)) {
            Poll::Ready(r) => r,
            Poll::Pending => Err(io::Error::from(io::ErrorKind::WouldBlock)),
        }
    }
}

impl<S: AsyncWrite + Unpin> Write for StdAdapter<S> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.with_context(|cx, s| s.poll_write(cx, buf)) {
            Poll::Ready(r) => r,
            Poll::Pending => Err(io::Error::from(io::ErrorKind::WouldBlock)),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.with_context(|cx, s| s.poll_flush(cx)) {
            Poll::Ready(r) => r,
            Poll::Pending => Err(io::Error::from(io::ErrorKind::WouldBlock)),
        }
    }
}

/// Clears the borrowed stream's pointer when it goes out of scope,
/// **including on unwind** — which is the half a plain assignment after
/// the call would miss.
struct Guard<'a, S>(&'a mut TlsStream<S>);

impl<S> Drop for Guard<'_, S> {
    fn drop(&mut self) {
        self.0.0.get_mut().context = null_mut();
    }
}

/// A TLS session over an asynchronous stream.
///
/// `Send`/`Sync` exactly when `S` is, by ordinary inference — which is the
/// property the whole port exists for, and is why this is a struct rather
/// than anything boxed.
#[derive(Debug)]
pub struct TlsStream<S>(native_tls::TlsStream<StdAdapter<S>>);

impl<S: AsyncRead + AsyncWrite + Unpin> TlsStream<S> {
    pub(crate) fn new(inner: native_tls::TlsStream<StdAdapter<S>>) -> Self {
        Self(inner)
    }

    /// The negotiated ALPN protocol, if the peer chose one.
    ///
    /// Reachable only because this crate owns the stream; see the module
    /// doc.
    pub fn negotiated_alpn(&self) -> Option<Vec<u8>> {
        self.0.negotiated_alpn().ok().flatten()
    }

    /// The peer's leaf certificate in DER, if it sent one.
    pub fn peer_certificate_der(&self) -> Option<Vec<u8>> {
        self.0
            .peer_certificate()
            .ok()
            .flatten()
            .and_then(|c| c.to_der().ok())
    }

    /// Sets the pointer, runs `f`, and clears it — the clear by a
    /// [`Guard`], so an unwind out of `native-tls` cannot leave a dangling
    /// one behind. **No `unsafe` here**: the guard borrows this whole
    /// stream, so `f` reaches the inner one through the guard rather than
    /// through a second pointer.
    fn with_context<F, R>(&mut self, cx: &mut Context<'_>, f: F) -> R
    where
        F: FnOnce(&mut native_tls::TlsStream<StdAdapter<S>>) -> R,
        StdAdapter<S>: Read + Write,
    {
        self.0.get_mut().context = std::ptr::from_mut(cx).cast::<()>();
        let guard = Guard(self);
        f(&mut guard.0.0)
    }
}

fn cvt<T>(r: io::Result<T>) -> Poll<io::Result<T>> {
    match r {
        Ok(v) => Poll::Ready(Ok(v)),
        Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => Poll::Pending,
        Err(e) => Poll::Ready(Err(e)),
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncRead for TlsStream<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        self.with_context(cx, |s| cvt(s.read(buf)))
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncWrite for TlsStream<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.with_context(cx, |s| cvt(s.write(buf)))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.with_context(cx, |s| cvt(s.flush()))
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.with_context(cx, native_tls::TlsStream::shutdown) {
            Ok(()) => Poll::Ready(Ok(())),
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => Poll::Pending,
            Err(e) => Poll::Ready(Err(e)),
        }
    }
}

/// [`crate::NativeTls`]'s handshake, as a type.
///
/// `Send` exactly when `S` is, derived rather than declared — which is the
/// whole reason this is a struct and not an `async fn`. See the module doc.
///
/// # Three states, and the first one is not a mistake
///
/// `native_tls::TlsConnector::connect` must be called with a live
/// [`Context`] in hand, so it cannot run in [`crate::NativeTls::connect`]
/// — it runs on the first poll. What *can* fail before that (an unusable
/// ALPN string, ECH asked for) is refused in `connect`, and arrives here
/// as [`Handshaking2::Failed`].
#[derive(Debug)]
pub struct Handshaking<S> {
    state: Handshaking2<S>,
}

#[derive(Debug)]
enum Handshaking2<S> {
    Failed(Option<hclient_core::Error>),
    /// Nothing has touched the socket yet: the connector and the name to
    /// verify against, waiting for a poll to supply a `Context`.
    Start(Option<(native_tls::TlsConnector, String, S)>),
    /// `native-tls` asked for more bytes.
    Mid(Option<MidHandshakeTlsStream<StdAdapter<S>>>),
}

impl<S> Handshaking<S> {
    pub(crate) fn failed(e: hclient_core::Error) -> Self {
        Self {
            state: Handshaking2::Failed(Some(e)),
        }
    }

    pub(crate) fn start(connector: native_tls::TlsConnector, name: String, io: S) -> Self {
        Self {
            state: Handshaking2::Start(Some((connector, name, io))),
        }
    }
}

impl<S> Future for Handshaking<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    type Output = Result<TlsStream<S>, hclient_core::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // `S: Unpin` is on this impl, so a plain `get_mut` and no
        // projection — the same reason `hclient-tls-rustls`'s handshake
        // needs none.
        let me = self.get_mut();
        let tls = |e: native_tls::Error| hclient_core::Error::new(hclient_core::ErrorKind::Tls, e);
        let ptr = std::ptr::from_mut(cx).cast::<()>();
        let polled_again = "a Future is not polled after it returns Ready";

        // Every arm returns: `Start` becomes `Mid` only by way of
        // `Poll::Pending`, so there is no state this can fall through into
        // and no loop.
        match &mut me.state {
            Handshaking2::Failed(e) => Poll::Ready(Err(e.take().expect(polled_again))),

            Handshaking2::Start(taken) => {
                let (connector, name, io) = taken.take().expect(polled_again);
                let adapter = StdAdapter {
                    inner: io,
                    context: ptr,
                };
                // The adapter is moved in, so the pointer is cleared on
                // whichever value comes back rather than by a `Guard` —
                // and on the `Failure` arm nothing comes back at all,
                // because `native-tls` keeps the stream only when it may
                // still be resumed.
                match connector.connect(&name, adapter) {
                    Ok(mut done) => {
                        done.get_mut().context = null_mut();
                        Poll::Ready(Ok(TlsStream::new(done)))
                    }
                    Err(HandshakeError::WouldBlock(mut mid)) => {
                        mid.get_mut().context = null_mut();
                        me.state = Handshaking2::Mid(Some(mid));
                        Poll::Pending
                    }
                    Err(HandshakeError::Failure(e)) => Poll::Ready(Err(tls(e))),
                }
            }

            Handshaking2::Mid(taken) => {
                let mut mid = taken.take().expect(polled_again);
                mid.get_mut().context = ptr;
                match mid.handshake() {
                    Ok(mut done) => {
                        done.get_mut().context = null_mut();
                        Poll::Ready(Ok(TlsStream::new(done)))
                    }
                    Err(HandshakeError::WouldBlock(mut mid)) => {
                        mid.get_mut().context = null_mut();
                        me.state = Handshaking2::Mid(Some(mid));
                        Poll::Pending
                    }
                    Err(HandshakeError::Failure(e)) => Poll::Ready(Err(tls(e))),
                }
            }
        }
    }
}
