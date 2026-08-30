//! The claim: a stack that can cross a thread gets a `Send` connect
//! future, one that cannot gets a working runtime without the claim, and
//! neither is decided by this crate.

mod support;

use hclient_rt::{TcpConnect, TcpOpts};
use static_assertions::{assert_impl_all, assert_not_impl_all};
use std::sync::{Arc, Mutex};
use support::{LocalStack, Log, SendStack};

hclient_rt_nal::adapt!(SendRt, support::SendStack);
hclient_rt_nal::adapt_local!(LocalRt, support::LocalStack);

fn send_stack(body: &'static [u8]) -> (&'static SendStack, Arc<Mutex<Log>>) {
    // A `&'static` stack, which is what `TcpConnect::Stream` carrying no
    // lifetime requires and what a `StaticCell` gives an embassy program.
    let log = Arc::new(Mutex::new(Log::default()));
    let leaked: &'static SendStack = Box::leak(Box::new(SendStack {
        body,
        log: Arc::clone(&log),
    }));
    (leaked, log)
}

fn addr() -> std::net::SocketAddr {
    "127.0.0.1:80".parse().expect("a literal")
}

/// The whole point. A generic adapter could not have this for any stack;
/// the macro has it for the stacks that deserve it.
#[test]
fn a_send_capable_stack_yields_a_send_connect_future() {
    assert_impl_all!(<SendRt as TcpConnect>::Connecting<'static>: Send);
    assert_impl_all!(<SendRt as TcpConnect>::Stream: Send);
}

/// And the control, without which the test above would also pass for a
/// macro that claimed `Send` unconditionally: the `!Send` stack goes
/// through `adapt_local!` and is **not** `Send`.
///
/// A compile-time negative, and the pair is the assertion. This was an
/// autoref probe — 25 lines, and correct — replaced because a probe is
/// itself weakenable: written the other way round it always answers
/// `false` and discriminates nothing, which is how this test first
/// failed. `assert_not_impl_all!` cannot be written the other way round
/// from here.
#[test]
fn a_local_stack_yields_a_future_that_is_not_send() {
    // A stack holding an `Rc` must not produce a `Send` future — if this
    // stops failing, `adapt_local!` is claiming something it cannot keep.
    assert_not_impl_all!(<LocalRt as TcpConnect>::Connecting<'static>: Send);
    assert_impl_all!(<SendRt as TcpConnect>::Connecting<'static>: Send);
}

/// The adapter is a real `TcpConnect`, not just a type that satisfies the
/// bounds: bytes go in and come back out through hyper's IO traits.
#[test]
fn bytes_travel_through_the_bridge() {
    use hyper::rt::{Read as _, Write as _};
    use std::future::poll_fn;
    use std::pin::Pin;

    let (stack, log) = send_stack(b"hello from the stack");
    let rt = SendRt(stack);

    let mut io = futures_executor::block_on(rt.connect(addr(), &TcpOpts::default()))
        .expect("the synthetic stack always connects");

    let n = futures_executor::block_on(poll_fn(|cx| {
        Pin::new(&mut io).poll_write(cx, b"GET / HTTP/1.1\r\n\r\n")
    }))
    .expect("write");
    assert_eq!(n, 18);
    assert_eq!(log.lock().unwrap().written, b"GET / HTTP/1.1\r\n\r\n");

    let mut store = [0u8; 64];
    let mut rb = hyper::rt::ReadBuf::new(&mut store);
    futures_executor::block_on(poll_fn(|cx| Pin::new(&mut io).poll_read(cx, rb.unfilled())))
        .expect("read");
    assert_eq!(rb.filled(), b"hello from the stack");
}

/// A read is bounded by the caller's chunk, not by the caller's buffer —
/// which is what makes the size a per-device decision rather than a
/// constant.
#[test]
fn the_read_chunk_is_the_callers_and_is_taken_at_run_time() {
    use hyper::rt::Read as _;
    use std::future::poll_fn;
    use std::pin::Pin;

    let (stack, _) = send_stack(b"0123456789");
    let conn = futures_executor::block_on(embedded_nal_async::TcpConnect::connect(stack, addr()))
        .expect("connect");
    let mut io = hclient_rt_nal::NalIo::with_capacity(conn, 4);

    let mut store = [0u8; 64];
    let mut rb = hyper::rt::ReadBuf::new(&mut store);
    futures_executor::block_on(poll_fn(|cx| Pin::new(&mut io).poll_read(cx, rb.unfilled())))
        .expect("read");
    // Four bytes, because the chunk said four — the 64-byte destination
    // did not decide it.
    assert_eq!(rb.filled(), b"0123");
}

/// A zero chunk would make every read report end of stream, which is the
/// worst way for a mis-sized buffer to fail.
#[test]
fn a_zero_chunk_is_raised_rather_than_becoming_a_silent_eof() {
    use hyper::rt::Read as _;
    use std::future::poll_fn;
    use std::pin::Pin;

    let (stack, _) = send_stack(b"abc");
    let conn = futures_executor::block_on(embedded_nal_async::TcpConnect::connect(stack, addr()))
        .expect("connect");
    let mut io = hclient_rt_nal::NalIo::with_capacity(conn, 0);

    let mut store = [0u8; 8];
    let mut rb = hyper::rt::ReadBuf::new(&mut store);
    futures_executor::block_on(poll_fn(|cx| Pin::new(&mut io).poll_read(cx, rb.unfilled())))
        .expect("read");
    assert_eq!(rb.filled(), b"a", "one byte, not zero");
}

/// `embedded-nal-async` exposes no socket options, so the runtime must say
/// it applies none — the understating direction, which turns a caller's
/// `TcpOpts` into a named refusal one layer up instead of an option
/// silently dropped.
#[test]
fn no_socket_option_is_claimed() {
    assert_eq!(
        <SendRt as TcpConnect>::APPLIES,
        hclient_rt::TcpOptsSupport::NONE
    );
    const { assert!(!<SendRt as TcpConnect>::SUPPORTS_UNIX) };
}

/// `adapt_local!` produces a **working** runtime, not merely one that
/// type-checks — the point of the split is that a stack which cannot
/// promise `Send` keeps everything below `Client`.
///
/// The `Box::leak` is the interesting part rather than test scaffolding: a
/// `!Sync` stack cannot live in a `static` item at all, so the `&'static`
/// the adapter needs comes from a leak (or from embassy's own
/// `StaticCell` machinery), never from `static FOO: MyStack = ..`. That is
/// written on the macro too, because it is where a user meets it.
#[test]
fn the_local_stack_is_a_real_runtime_too() {
    let stack: &'static LocalStack = Box::leak(Box::new(LocalStack(std::rc::Rc::new(()))));
    let rt = LocalRt(stack);
    let io = futures_executor::block_on(rt.connect(addr(), &TcpOpts::default()));
    assert!(io.is_ok(), "adapt_local! must produce a working runtime");
}

// Both visibilities compile, which is what makes the parameter real rather
// than accepted-and-ignored: a `pub` adapter over a `pub` stack, and a
// private one over a private stack — the shape the crate's own first doc
// example needs, and which forcing `pub` used to reject with
// `E0446: private type in public interface`.
mod visibility {
    struct PrivateStack(#[allow(dead_code, reason = "only its type matters")] std::rc::Rc<()>);
    #[allow(dead_code, reason = "declared to be adapted, never connected")]
    struct PrivateConn;
    impl embedded_io_async::ErrorType for PrivateConn {
        type Error = embedded_io_async::ErrorKind;
    }
    impl embedded_io_async::Read for PrivateConn {
        async fn read(&mut self, _b: &mut [u8]) -> Result<usize, Self::Error> {
            Ok(0)
        }
    }
    impl embedded_io_async::Write for PrivateConn {
        async fn write(&mut self, b: &[u8]) -> Result<usize, Self::Error> {
            Ok(b.len())
        }
        async fn flush(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }
    impl embedded_nal_async::TcpConnect for PrivateStack {
        type Error = embedded_io_async::ErrorKind;
        type Connection<'a>
            = PrivateConn
        where
            Self: 'a;
        async fn connect<'a>(
            &'a self,
            _r: core::net::SocketAddr,
        ) -> Result<Self::Connection<'a>, Self::Error> {
            Ok(PrivateConn)
        }
    }

    // Private stack, private adapter — the default.
    hclient_rt_nal::adapt_local!(PrivateRt, PrivateStack);
    // And the public form, over this file's public double.
    hclient_rt_nal::adapt!(pub PublicRt, crate::support::SendStack);

    #[test]
    fn both_visibilities_expand() {
        fn is_connect<T: hclient_rt::TcpConnect>() {}
        is_connect::<PrivateRt>();
        is_connect::<PublicRt>();
    }
}
