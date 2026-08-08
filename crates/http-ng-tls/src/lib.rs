//! Pluggable TLS.
//!
//! The trait is typed on `hyper::rt::Read`/`Write`, and **not** on
//! futures-io or tokio-io. Consequence: there is no such thing as a
//! per-runtime TLS glue crate — one adapter (Task 9, rustls) serves every
//! runtime (`http-ng-rt-tokio`, `http-ng-rt-smol`, and any future one),
//! because `hyper::rt::{Read, Write}` is the one point every `S` in this
//! vertical is already normalized to (`http_ng_rt::FuturesIo`, `TokioIo`),
//! not one more layer stacked on top.
#![forbid(unsafe_code)]

use http_ng_core::{Error, ErrorKind, TlsSupport};
use std::future::Future;

/// Parameters for a single TLS connection.
///
/// ALPN lives on the **connect call**, not on the config: version pinning
/// and h2-prior-knowledge each require different ALPN sets for different
/// connections to the same origin (for example, one attempt forces
/// `h2`-only, the next falls back to `http/1.1`). An implementation for
/// which recomputing this on every connect is expensive is free to cache
/// its TLS config per concrete ALPN set internally — that's its own
/// business, not this trait's.
#[derive(Debug, Clone, Copy)]
pub struct TlsRequest<'a> {
    pub server_name: &'a str,
    pub alpn: &'a [&'a [u8]],
    /// RFC 9849 Encrypted Client Hello. The `EchConfigList` comes from an
    /// HTTPS/SVCB record (`http_ng_dns::SvcbEndpoint::ech_config_list`,
    /// Task 6). The slot is reserved up front, not bolted on once the
    /// first implementation needed it: adding a new field to a request
    /// struct later would be a breaking change for every already-written
    /// `TlsConnect` implementation.
    pub ech: Option<&'a [u8]>,
}

/// The outcome of a TLS handshake, as visible to the caller.
///
/// **Every field is `Option`**: native-tls (the only backend available
/// without picking a specific crypto library) hands back only the leaf
/// certificate, ALPN, and tls-server-end-point — not the full chain, not
/// the protocol version, not the cipher suite. Note that this describes
/// `native-tls` itself; `http-ng-tls-native-tls`, the backend built on it,
/// reports even less — its `alpn` is always `None`, because the async
/// wrapper it uses does not expose the negotiated protocol. See that
/// crate's module doc before relying on ALPN from it. The trait must allow for a
/// backend with that reduced a set. Symmetrically: a backend that can't
/// report a field must leave it `None`, not substitute a plausible-looking
/// value — a capability that lies about its own state is worse than a
/// capability that's simply absent (the same principle that split
/// `RedirectSupport::None`/`Transparent` in `http-ng-core` and
/// `supports_svcb()`/the empty stream in `http-ng-dns`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TlsInfo {
    /// The negotiated ALPN protocol for this connection — a single item,
    /// the result of negotiation, not the whole proposed list from
    /// `TlsRequest::alpn`.
    pub alpn: Option<Vec<u8>>,
    /// The peer's certificate chain, DER, in leaf → root order. Backends
    /// like native-tls hand back only the leaf — in that case a
    /// single-element `Vec`, not `None`: there is a certificate, the chain
    /// is just incomplete.
    pub peer_certificates: Option<Vec<Vec<u8>>>,
    /// The TLS protocol version negotiated on this connection.
    ///
    /// A `String`, not an enum defined by this crate: version enums differ
    /// across backends (rustls, native-tls over OpenSSL/SChannel/
    /// SecureTransport) in exactly which variants they carry, and defining
    /// a unifying enum here would mean either lagging behind a new backend
    /// or carrying variants a given backend will never produce. So that
    /// two backends don't name the same version differently, the value
    /// must be a registry-style string, the same one used by both
    /// `openssl`'s `SSL_get_version()` and rustls: `"TLSv1.3"`,
    /// `"TLSv1.2"`, `"TLSv1.1"`, `"TLSv1.0"` — not the `Debug` formatting
    /// of the backend's internal enum (rustls's `Debug` for
    /// `ProtocolVersion::TLSv1_3`, for example, prints `TLSv1_3`, with an
    /// underscore instead of a dot — the implementation must normalize
    /// this to the canonical form, not pass `Debug`'s output through as
    /// is).
    pub protocol_version: Option<String>,
    /// The cipher suite negotiated on this connection.
    ///
    /// The same argument as `protocol_version`, for the same reason — a
    /// `String`, not an enum. The value must be a name from the IANA TLS
    /// Cipher Suites registry, e.g. `"TLS_AES_128_GCM_SHA256"` — the same
    /// name rustls uses (`CipherSuite::TLS13_AES_128_GCM_SHA256` must be
    /// normalized to the registry name with its version prefix stripped,
    /// not passed through `Debug` as is), whereas OpenSSL by default names
    /// the same cipher suite with an alias like
    /// `"ECDHE-RSA-AES128-GCM-SHA256"` — an implementation on top of
    /// OpenSSL must translate the alias to the registry name, or two
    /// backends will report the same cipher as two different strings, and
    /// a caller comparing them will get it wrong.
    pub cipher_suite: Option<String>,
}

/// A pluggable TLS handshake over an arbitrary transport.
///
/// One method, `connect`, not separate "handshake" and "wrap" steps:
/// there's nothing to gain from splitting them — no caller anywhere in
/// this vertical wants a bare handshake without a wrapped stream, or the
/// reverse.
pub trait TlsConnect {
    /// The wrapped stream after the handshake. `S: hyper::rt::Read + Write
    /// + Unpin` appears in both places (on the type itself and in its
    /// where clause) — an implementation can't promise a wrapper for only
    /// some possible `S`; every `S` capable of `connect` must get back a
    /// working `Stream<S>` too.
    type Stream<S>: hyper::rt::Read + hyper::rt::Write + Unpin
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin;

    /// Performs a TLS handshake over an already established `io` (a TCP
    /// socket from `http_ng_rt::TcpConnect`, wrapped in
    /// `FuturesIo`/`TokioIo` — `connect` itself knows nothing about the
    /// transport) and returns the encrypted stream along with whatever
    /// negotiated parameters the implementation can honestly report.
    fn connect<S>(
        &self,
        io: S,
        req: TlsRequest<'_>,
    ) -> impl Future<Output = Result<(Self::Stream<S>, TlsInfo), Error>>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin;

    /// What a transport built on this implementation should advertise in
    /// [`Capabilities::tls_config`].
    ///
    /// Defaulted to `Full` so that adding this method broke no existing
    /// implementation — every one of them does perform TLS. It exists for
    /// the one that does not: [`NoTls`] returns `None`, and a transport
    /// that asks instead of assuming cannot end up advertising TLS it will
    /// refuse to perform.
    ///
    /// The same shape as [`http_ng_dns::Resolve::supports_svcb`], and for
    /// the same reason: a capability has to come from the component that
    /// knows, not from whoever assembles it.
    fn tls_support(&self) -> TlsSupport {
        TlsSupport::Full
    }
}

/// A [`TlsConnect`] that performs no TLS, for a client built without it.
///
/// For constrained targets that have `std` but no room for a TLS stack:
/// plain HTTP works, and `https://` fails at connect with a typed error
/// instead of failing to link. `Native<R, NoTls, D>` drops rustls,
/// native-tls and their transitive trees from the build entirely.
///
/// It advertises [`TlsSupport::None`], so a caller who reads
/// `Capabilities::tls_config` before making a request learns the truth
/// rather than discovering it at connect time.
///
/// `Stream<S>` is an uninhabited type. That is not a trick: it is the type
/// system carrying the same fact the error does — this implementation
/// cannot produce a TLS stream, and no code path can pretend otherwise,
/// because there is no value to pretend with.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoTls;

/// The stream [`NoTls`] never returns. Uninhabited, so every method is
/// unreachable by construction rather than by a panic.
#[derive(Debug)]
pub enum NoStream {}

impl hyper::rt::Read for NoStream {
    fn poll_read(
        self: core::pin::Pin<&mut Self>,
        _: &mut core::task::Context<'_>,
        _: hyper::rt::ReadBufCursor<'_>,
    ) -> core::task::Poll<std::io::Result<()>> {
        match *self {}
    }
}

impl hyper::rt::Write for NoStream {
    fn poll_write(
        self: core::pin::Pin<&mut Self>,
        _: &mut core::task::Context<'_>,
        _: &[u8],
    ) -> core::task::Poll<std::io::Result<usize>> {
        match *self {}
    }
    fn poll_flush(
        self: core::pin::Pin<&mut Self>,
        _: &mut core::task::Context<'_>,
    ) -> core::task::Poll<std::io::Result<()>> {
        match *self {}
    }
    fn poll_shutdown(
        self: core::pin::Pin<&mut Self>,
        _: &mut core::task::Context<'_>,
    ) -> core::task::Poll<std::io::Result<()>> {
        match *self {}
    }
}

impl TlsConnect for NoTls {
    type Stream<S>
        = NoStream
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin;

    async fn connect<S>(&self, _io: S, req: TlsRequest<'_>) -> Result<(NoStream, TlsInfo), Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin,
    {
        Err(Error::new(
            ErrorKind::Tls,
            std::io::Error::other(format!(
                "this client was built without TLS support (NoTls); cannot secure a connection to {}",
                req.server_name
            )),
        ))
    }

    fn tls_support(&self) -> TlsSupport {
        TlsSupport::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::rt::{Read, ReadBufCursor, Write};
    use std::collections::VecDeque;
    use std::io;
    use std::pin::{Pin, pin};
    use std::task::{Context, Poll, Waker};

    /// Polls a single `Future`/`poll_fn` synchronously and demands
    /// immediate readiness. Every future in this module's tests is built
    /// on `Loopback`, which never returns `Pending` — a real executor
    /// would add nothing here; `Waker::noop()` (stable since 1.85, well under this
    /// vertical's MSRV) settles the matter without an extra dependency
    /// like `futures-executor` for a single synchronous poll.
    fn poll_once<F: Future>(mut fut: Pin<&mut F>) -> F::Output {
        let mut cx = Context::from_waker(Waker::noop());
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(v) => v,
            Poll::Pending => panic!("test I/O must not return Pending"),
        }
    }

    /// `hyper::rt::Read + Write` with zero third-party dependencies:
    /// writes into a shared buffer, reads from that same buffer. Not a
    /// call-counting mock — working I/O, enough to actually push bytes
    /// through `TlsConnect::Stream<S>` and back.
    #[derive(Default)]
    struct Loopback {
        buf: VecDeque<u8>,
    }

    impl Read for Loopback {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            mut buf: ReadBufCursor<'_>,
        ) -> Poll<io::Result<()>> {
            let n = buf.remaining().min(self.buf.len());
            let chunk: Vec<u8> = self.buf.drain(..n).collect();
            buf.put_slice(&chunk);
            Poll::Ready(Ok(()))
        }
    }

    impl Write for Loopback {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            data: &[u8],
        ) -> Poll<io::Result<usize>> {
            self.buf.extend(data.iter().copied());
            Poll::Ready(Ok(data.len()))
        }
        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    /// A pass-through implementation of `TlsConnect::Stream<S>` — a
    /// wrapper around `S`, not `type Stream<S> = S`: a real adapter (Task
    /// 9, rustls) must wrap `S` in TLS session state, and an identity GAT
    /// wouldn't exercise that shape at all. Encrypts nothing, just
    /// forwards.
    struct PassThrough<S>(S);

    impl<S: Read + Unpin> Read for PassThrough<S> {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: ReadBufCursor<'_>,
        ) -> Poll<io::Result<()>> {
            Pin::new(&mut self.0).poll_read(cx, buf)
        }
    }

    impl<S: Write + Unpin> Write for PassThrough<S> {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            data: &[u8],
        ) -> Poll<io::Result<usize>> {
            Pin::new(&mut self.0).poll_write(cx, data)
        }
        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.0).poll_flush(cx)
        }
        fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.0).poll_shutdown(cx)
        }
    }

    /// Encrypts nothing: reports the first proposed ALPN as "negotiated"
    /// and a fixed protocol version — exactly enough for the test to have
    /// something to check in `TlsInfo`, nothing more
    /// (`peer_certificates`/`cipher_suite` stay `None` — honestly, the
    /// stub has no way to produce them).
    struct NoOpTls;

    impl TlsConnect for NoOpTls {
        type Stream<S>
            = PassThrough<S>
        where
            S: Read + Write + Unpin;

        fn connect<S>(
            &self,
            io: S,
            req: TlsRequest<'_>,
        ) -> impl Future<Output = Result<(Self::Stream<S>, TlsInfo), Error>>
        where
            S: Read + Write + Unpin,
        {
            let alpn = req.alpn.first().map(|proto| proto.to_vec());
            async move {
                Ok((
                    PassThrough(io),
                    TlsInfo {
                        alpn,
                        peer_certificates: None,
                        protocol_version: Some("TLSv1.3".to_string()),
                        cipher_suite: None,
                    },
                ))
            }
        }
    }

    #[test]
    fn connect_wraps_the_stream_and_negotiates_alpn() {
        // The ALPN bytes are built from LOCAL `Vec<u8>`s, not `&'static
        // [u8]` literals — proof that `TlsRequest<'a>`, with the SAME `'a`
        // on both the outer slice and each element's bytes, is actually
        // constructible without `'static` and without `req` needing to be
        // stored anywhere longer than the `connect` call it was designed
        // for ("ALPN lives on the connect call" — see the field's doc
        // comment).
        let h2 = b"h2".to_vec();
        let http11 = b"http/1.1".to_vec();
        let alpn = [h2.as_slice(), http11.as_slice()];
        let req = TlsRequest {
            server_name: "example.com",
            alpn: &alpn,
            ech: None,
        };

        // `io` already contains data BEFORE the handshake — proves below
        // that the returned `Stream<S>` actually wraps THIS `io`, rather
        // than substituting an independent source that just happens to
        // also implement `Read`/`Write`.
        let mut io = Loopback::default();
        io.buf.extend(*b"preexisting");

        let fut = NoOpTls.connect(io, req);
        let mut fut = pin!(fut);
        let (mut stream, info) = poll_once(fut.as_mut()).unwrap();

        assert_eq!(info.alpn.as_deref(), Some(b"h2".as_slice()));
        assert_eq!(info.protocol_version.as_deref(), Some("TLSv1.3"));
        assert!(info.peer_certificates.is_none());
        assert!(info.cipher_suite.is_none());

        // Data that was sitting in `io` BEFORE `connect` is visible
        // through the returned `Stream<S>` — meaning it's a wrapper over
        // the passed-in `io`, not a new, disconnected stream.
        let mut preexisting = [0u8; 11];
        let mut rb = hyper::rt::ReadBuf::new(&mut preexisting);
        let read = std::future::poll_fn(|cx| Pin::new(&mut stream).poll_read(cx, rb.unfilled()));
        poll_once(pin!(read).as_mut()).unwrap();
        assert_eq!(&preexisting, b"preexisting");

        // `Stream<S>` actually implements `hyper::rt::Write`, not just
        // types as one: write and read it back through the same shared
        // `Loopback` buffer.
        let write = std::future::poll_fn(|cx| Pin::new(&mut stream).poll_write(cx, b"ping"));
        let n = poll_once(pin!(write).as_mut()).unwrap();
        assert_eq!(n, 4);

        let mut echoed = [0u8; 4];
        let mut rb = hyper::rt::ReadBuf::new(&mut echoed);
        let read = std::future::poll_fn(|cx| Pin::new(&mut stream).poll_read(cx, rb.unfilled()));
        poll_once(pin!(read).as_mut()).unwrap();
        assert_eq!(&echoed, b"ping");
    }
}
