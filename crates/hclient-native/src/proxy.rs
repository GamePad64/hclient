//! Driving a [`Handshake`] over a socket — the whole of what proxying
//! costs a transport.
//!
//! The protocols themselves are `hclient-proxy`'s and are **sans-io**: state machines that are
//! handed the bytes that arrived and answer with the bytes to send. This
//! file is the thirty lines that know what a `poll_read` is, and it is
//! the same thirty lines for all three of them.
//!
//! # Why the protocols are a crate and this is not
//!
//! Because the split falls where a dependency does. The protocols carry
//! none — which is why this feature has no `dep:` line — but the
//! machine's own settings carry `proxy_cfg`, and through it `url` and the
//! ICU tables. A feature on this crate would put those into every build
//! in any graph that switched it on, which is the argument that keeps
//! `quinn-proto` in `hclient-tls-quic` and `tungstenite` in
//! `hclient-tungstenite`.
//!
//! What the split bought beyond that is measurable in this file: driving
//! `CONNECT` used to mean driving **hyper's h1 dispatcher** through
//! `crate::upgrade`, because writing one request and reading one response
//! needed an HTTP client. It does not any more — see
//! `hclient_proxy::connect` — so this file has one loop rather than a
//! special case.
//!
//! # Why this is not a `TcpConnect` wrapper, which would have cost nothing
//!
//! [`TcpConnect::connect`](hclient_rt::TcpConnect::connect) takes a
//! `SocketAddr` and nothing else, so a wrapper implementing it could never
//! hand the proxy the origin's **name** — the client would resolve it
//! locally and leak exactly the DNS a proxy user is often there to hide,
//! and `http://` could never take absolute-form, because that is decided
//! where the request head is written. A proxy replaces the resolve →
//! Happy-Eyeballs → connect block; it does not decorate the socket.

use std::io;
use std::pin::Pin;
use std::task::Poll;

pub use crate::error::{ProxyAndUnixSocket, ProxySpokeFirst};
use bytes::{Bytes, BytesMut};

use hclient_core::{Error, ErrorKind};
use hyper::rt::{Read, Write};
use std::future::poll_fn;

#[cfg(feature = "system-proxy")]
#[doc(inline)]
pub use hclient_proxy::system;
pub use hclient_proxy::{Approach, Handshake, NoProxy, Proxy, ProxyScheme, Step};
/// The three protocols, behind the `proxy` feature exactly as they were
/// before they moved: the seam above is unconditional because `Native`'s
/// own `P = NoProxy` default names one of its types.
#[cfg(feature = "proxy")]
pub use hclient_proxy::{
    ConnectError, HttpConnect, ProxyRefused, Socks4, Socks4HandshakeError, Socks4Refused, Socks5,
    Socks5HandshakeError, Socks5Refused,
};

/// How the *request* is written, which is the one thing a proxy changes
/// above the socket.
///
/// [`AbsoluteForm`](Via::AbsoluteForm) is only ever the answer for an
/// HTTP proxy carrying an `http://` request, where the proxy is an origin
/// server for this request — RFC 9112 §3.2.2's absolute-form.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Via<'a> {
    Direct,
    AbsoluteForm(Option<&'a http::HeaderValue>),
}

/// Run `h` over `io` until the tunnel is open.
///
/// Returns whatever the proxy sent past the end of its own handshake,
/// which the caller decides what to do with — see [`ProxySpokeFirst`].
pub(crate) async fn drive<S, H>(
    io: &mut S,
    h: &mut H,
    host: &str,
    port: u16,
) -> Result<Bytes, Error>
where
    S: Read + Write + Unpin,
    H: Handshake,
{
    write_all(io, &h.begin(host, port)?).await?;
    // One buffer for the whole exchange, and it is the driver's: a
    // handshake that answers `NeedMore` has consumed nothing, so the
    // fragment it could not use is still here when the next read appends
    // to it. That contract is what makes *not yet* free.
    let mut buf = BytesMut::new();
    loop {
        match h.advance(&mut buf)? {
            Step::Done => return Ok(buf.freeze()),
            Step::Write(bytes) => write_all(io, &bytes).await?,
            Step::NeedMore => read_some(io, &mut buf).await?,
        }
    }
}

// --- byte IO over hyper's traits ---------------------------------------
//
// Written here rather than reached for: `hclient-tls-native-tls`'s
// `HyperIo` would give `futures_util`'s helpers, but it is that crate's
// private adapter and these two are shorter than moving it would be.

async fn write_all<S: Write + Unpin>(io: &mut S, mut buf: &[u8]) -> Result<(), Error> {
    while !buf.is_empty() {
        let n = poll_fn(|cx| Pin::new(&mut *io).poll_write(cx, buf))
            .await
            .map_err(conn)?;
        if n == 0 {
            return Err(conn(io::Error::from(io::ErrorKind::WriteZero)));
        }
        buf = &buf[n..];
    }
    poll_fn(|cx| Pin::new(&mut *io).poll_flush(cx))
        .await
        .map_err(conn)
}

/// One read, appended. **Not `read_exact`**, which is what this was
/// before the protocols became state machines: how many bytes a frame
/// needs is now the handshake's question, and a driver that decided it
/// would be a second copy of every protocol's framing.
async fn read_some<S: Read + Unpin>(io: &mut S, buf: &mut BytesMut) -> Result<(), Error> {
    let at = buf.len();
    buf.resize(at + 4096, 0);
    let n = poll_fn(|cx| {
        let mut rb = hyper::rt::ReadBuf::new(&mut buf[at..]);
        match Pin::new(&mut *io).poll_read(cx, rb.unfilled()) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(rb.filled().len())),
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    })
    .await
    .map_err(conn)?;
    buf.truncate(at + n);
    if n == 0 {
        // A handshake that still wants bytes and a peer that has stopped
        // sending them is a failure to connect, never a partial success.
        return Err(conn(io::Error::from(io::ErrorKind::UnexpectedEof)));
    }
    Ok(())
}

fn conn(e: io::Error) -> Error {
    Error::new(ErrorKind::Connect, e)
}

/// Gated on `proxy` because the tests drive the **protocols**, which are;
/// the driver above is not, because `connect.rs` calls it whichever
/// features are on.
#[cfg(all(test, feature = "proxy"))]
mod tests {
    use super::*;
    use std::task::Context;

    /// A socket whose answers are decided in advance and whose writes are
    /// kept. Enough for a handshake, which is all `drive` does before it
    /// hands the stream back.
    #[derive(Debug)]
    struct ScriptIo {
        reply: Vec<u8>,
        at: usize,
        written: Vec<u8>,
        /// How much one read hands over. `1` is the shape that breaks a
        /// driver assuming a frame arrives whole; a large value is what a
        /// real socket does, and it is the only way the *leftover* is
        /// reachable at all — the driver stops reading the moment a
        /// handshake is done, so bytes past the head exist only when they
        /// came in the same flight.
        chunk: usize,
    }

    impl ScriptIo {
        fn byte_at_a_time(reply: &[u8]) -> Self {
            Self {
                reply: reply.to_vec(),
                at: 0,
                written: Vec::new(),
                chunk: 1,
            }
        }

        fn one_flight(reply: &[u8]) -> Self {
            Self {
                reply: reply.to_vec(),
                at: 0,
                written: Vec::new(),
                chunk: usize::MAX,
            }
        }
    }

    impl Read for ScriptIo {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            mut buf: hyper::rt::ReadBufCursor<'_>,
        ) -> Poll<io::Result<()>> {
            let left = &self.reply[self.at..];
            if left.is_empty() {
                return Poll::Ready(Ok(()));
            }
            let n = self.chunk.min(left.len()).min(buf.remaining());
            buf.put_slice(&left[..n]);
            self.at += n;
            Poll::Ready(Ok(()))
        }
    }

    impl Write for ScriptIo {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            self.written.extend_from_slice(buf);
            Poll::Ready(Ok(buf.len()))
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[test]
    fn a_handshake_arriving_one_byte_at_a_time_still_completes() {
        // The driver's half of the `NeedMore` contract, over a socket
        // that never delivers a whole frame at once.
        let mut io =
            ScriptIo::byte_at_a_time(&b"HTTP/1.1 200 Connection established\r\nVia: p\r\n\r\n"[..]);
        let mut h = HttpConnect::new();
        let leftover =
            futures_executor::block_on(drive(&mut io, &mut h, "example.com", 443)).expect("open");

        assert!(leftover.is_empty());
        assert!(
            io.written
                .starts_with(b"CONNECT example.com:443 HTTP/1.1\r\n"),
            "{:?}",
            String::from_utf8_lossy(&io.written)
        );
    }

    #[test]
    fn a_peer_that_stops_mid_handshake_is_a_connect_failure() {
        // Never a partial success: a truncated reply leaves the tunnel in
        // a state nothing can use.
        let mut io = ScriptIo::byte_at_a_time(&b"HTTP/1.1 200 OK\r\n"[..]);
        let mut h = HttpConnect::new();
        let err = futures_executor::block_on(drive(&mut io, &mut h, "example.com", 443))
            .expect_err("truncated");
        assert_eq!(*err.kind(), ErrorKind::Connect);
    }

    #[test]
    fn the_driver_is_the_same_loop_for_a_multi_round_trip_protocol() {
        // SOCKS5 writes twice and reads twice; `CONNECT` writes once and
        // reads once. That both run through one loop with no protocol
        // knowledge in it is the property the sans-io split exists for.
        let mut io = ScriptIo::byte_at_a_time(
            &[
                &[0x05, 0x00][..],
                &[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0][..],
            ]
            .concat(),
        );
        let mut h = Socks5::new();
        let leftover =
            futures_executor::block_on(drive(&mut io, &mut h, "example.com", 443)).expect("open");

        assert!(leftover.is_empty());
        // The greeting, then the CONNECT-by-name.
        assert_eq!(&io.written[..3], &[0x05, 0x01, 0x00]);
        assert!(io.written.windows(11).any(|w| w == b"example.com"));
    }

    #[test]
    fn bytes_past_the_handshake_reach_the_caller_who_decides() {
        // The driver reports them; `connect.rs` refuses them. Split that
        // way because *what to make of them* is a question about what
        // happens next, not about the protocol.
        let mut io = ScriptIo::one_flight(b"HTTP/1.1 200 OK\r\n\r\nthe proxy spoke first");
        let mut h = HttpConnect::new();
        let leftover =
            futures_executor::block_on(drive(&mut io, &mut h, "example.com", 443)).expect("open");
        assert_eq!(&leftover[..], b"the proxy spoke first");
    }
}
