//! Byte-exact reads and writes over hyper's IO traits, shared by the two
//! SOCKS connectors.
//!
//! `frames` rather than `io`, which is what this was called for about a
//! minute: a module of that name collides with the `use std::io` every
//! file here needs. The name is also the better one — both helpers exist
//! only for frames the RFCs fix the size of, which is what makes a short
//! read a failure to connect rather than a partial success.

use std::io;
use std::pin::Pin;
use std::task::Poll;

use hclient_core::{Error, ErrorKind};
use hyper::rt::{Read, Write};
use std::future::poll_fn;

// --- byte-exact IO over hyper's traits ----------------------------------
//
// Written here rather than reached for: `hclient-tls-native-tls`'s
// `HyperIo` would give `futures_util`'s `read_exact`, but it is that
// crate's private adapter, and these two helpers are shorter than moving
// it would be. Both are used only for fixed-size SOCKS5 frames.

pub(super) async fn write_all<S: Write + Unpin>(io: &mut S, mut buf: &[u8]) -> Result<(), Error> {
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

pub(super) async fn read_exact<S: Read + Unpin>(io: &mut S, buf: &mut [u8]) -> Result<(), Error> {
    let mut at = 0;
    while at < buf.len() {
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
        if n == 0 {
            // A short handshake is a failure to connect, never a partial
            // success: every frame read here is fixed-size by the RFC.
            return Err(conn(io::Error::from(io::ErrorKind::UnexpectedEof)));
        }
        at += n;
    }
    Ok(())
}

pub(super) fn conn(e: io::Error) -> Error {
    Error::new(ErrorKind::Connect, e)
}
