//! The claim: a stack that can cross a thread gets a `Send` connect
//! future, one that cannot gets a working runtime without the claim, and
//! neither is decided by this crate.

mod support;

use hclient_rt::{TcpConnect, TcpOpts};
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
    fn assert_send<T: Send>() {}
    assert_send::<<SendRt as TcpConnect>::Connecting<'static>>();
    assert_send::<<SendRt as TcpConnect>::Stream>();
}

/// And the control, without which the test above would also pass for a
/// macro that claimed `Send` unconditionally: the `!Send` stack goes
/// through `adapt_local!` and is **not** `Send`.
///
/// A real negative rather than `fn assert_not<T>() {}`, which accepts
/// anything: the inherent method is found only when the trait method is
/// not applicable.
#[test]
fn a_local_stack_yields_a_future_that_is_not_send() {
    // Inherent methods win over trait ones, so the answer is `true` only
    // where the bound holds — the shape `hclient-rt-embassy`'s own seam
    // test uses. Written the other way round it always answers `false`
    // and discriminates nothing, which is how this test first failed.
    struct Probe<T>(std::marker::PhantomData<T>);
    trait Fallback {
        fn is() -> bool {
            false
        }
    }
    impl<T> Fallback for Probe<T> {}
    impl<T: Send> Probe<T> {
        fn is() -> bool {
            true
        }
    }

    assert!(
        !Probe::<<LocalRt as TcpConnect>::Connecting<'static>>::is(),
        "a stack holding an Rc must not produce a Send future — if this passes, \
         `adapt_local!` is claiming something it cannot keep"
    );
    // The probe discriminates rather than always answering `false`, which
    // is what would make the assertion above vacuous.
    assert!(Probe::<<SendRt as TcpConnect>::Connecting<'static>>::is());
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
