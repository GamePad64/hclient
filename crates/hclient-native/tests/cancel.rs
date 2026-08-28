//! `Transport::execute`'s drop-cancellation contract, for `Native`.
//!
//! # What this file refuses to test
//!
//! "The future stopped being polled" and "the transfer stopped" are
//! different claims, and only the second one is the contract. A test that
//! drops a future and then observes that nothing further happens *in its
//! own task* proves the first and nothing else — it would pass just as
//! happily against a transport that detached the exchange onto a spawned
//! task and let it run to completion.
//!
//! So every assertion below is made by the **server**, on its own socket,
//! from its own thread: [`silent_server`] reads the request, says so, and
//! then reports whether the client end went away or stayed. `Ok(0)` out of
//! `read` is the kernel telling the server that the peer closed — a fact
//! about the connection, not about anyone's future.
//!
//! # The two halves are equally load-bearing
//!
//! [`dropping_the_execute_future_closes_the_connection_the_server_sees`]
//! alone is not enough: a client that closed the connection for any other
//! reason (a timeout of its own, a bug that drops the socket early, a
//! server-side idle policy) would satisfy it too. The control,
//! [`holding_the_execute_future_leaves_the_connection_open`], drives the
//! *same* request against the *same* server for a window covering the same
//! wall time, and requires the connection to still be there — so the pair
//! together attributes the close to the drop rather than to the passage of
//! time.
//!
//! # Why the server never answers
//!
//! Cancellation is only observable while there is something to cancel. A
//! server that replies immediately leaves no window at all: `execute` would
//! be finished before the test could drop anything, and the drop would be a
//! no-op that still "passes". [`silent_server`] therefore accepts, reads,
//! and says nothing — the exchange stays in flight for exactly as long as
//! the test wants it to.
#![cfg(not(target_family = "wasm"))]

use hclient::Client;
use hclient_core::unversioned::Transport;
use hclient_core::{CancelSupport, RequestBody};
use hclient_dns_system::SystemDns;
use hclient_native::Native;
use hclient_rt_tokio::Tokio;
use hclient_tls_rustls::Rustls;
use std::io::Read;
use std::net::SocketAddr;
use std::time::Duration;

/// Ceiling for anything that must not hang. Long enough that a slow
/// machine never trips it, short enough that a genuine hang is a failed
/// test rather than a stuck CI job.
const BOUND: Duration = Duration::from_secs(30);

/// How long the server waits for the client end to go away before
/// concluding that it is still there.
///
/// Only one number in this file is a timing choice, and this is it — both
/// verdicts come out of the same `read`, so the two tests cannot disagree
/// about what "long enough" means. Loopback close is delivered in
/// microseconds; a hundred milliseconds is four orders of magnitude of
/// headroom for the positive case, and the negative case is not a race at
/// all — nothing is trying to close that connection.
const OBSERVATION_WINDOW: Duration = Duration::from_millis(100);

/// What the server saw at its end of the connection, which is the only
/// thing this file ever asserts on.
#[derive(Debug, PartialEq, Eq)]
enum ClientEnd {
    /// `read` returned `Ok(0)` (or the connection was reset): the peer is
    /// gone.
    WentAway,
    /// `read` hit [`OBSERVATION_WINDOW`] with the connection intact.
    StillThere,
}

/// A server that accepts one connection, reads one request, and then never
/// answers.
///
/// Returns the address plus two `oneshot` receivers, in the order the
/// server fills them:
///
/// 1. `request_seen` fires once the full request head has arrived. That is
///    what lets the tests drop at a *determined* moment rather than after
///    a guessed sleep: before this fires there may be nothing on the wire
///    to cancel, and a test that dropped early could pass without the
///    exchange ever having started.
/// 2. `verdict` carries the [`ClientEnd`] observation.
///
/// After reporting, the server keeps the socket open until the peer goes
/// away. That is not tidiness: closing it at the end of the observation
/// window would end the exchange *from the server side*, and the control
/// test — which is still polling its future at that point — would get a
/// connection error and never reach its assertion.
fn silent_server() -> (
    SocketAddr,
    tokio::sync::oneshot::Receiver<()>,
    tokio::sync::oneshot::Receiver<ClientEnd>,
) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
    let (verdict_tx, verdict_rx) = tokio::sync::oneshot::channel();

    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().expect("accept");
        sock.set_read_timeout(Some(BOUND))
            .expect("set_read_timeout");

        let mut head = Vec::new();
        let mut buf = [0u8; 1024];
        loop {
            let n = sock.read(&mut buf).expect("reading the request head");
            assert_ne!(n, 0, "the client closed before sending a request at all");
            head.extend_from_slice(&buf[..n]);
            if head.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        let _ = seen_tx.send(());

        sock.set_read_timeout(Some(OBSERVATION_WINDOW))
            .expect("set_read_timeout");
        let verdict = match sock.read(&mut buf) {
            Ok(0) => ClientEnd::WentAway,
            // Nothing should follow a bodyless GET, but bytes are not a
            // close, so they read as "still there" rather than as a
            // panic — the assertion belongs in the test, not here.
            Ok(_) => ClientEnd::StillThere,
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                ClientEnd::StillThere
            }
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => ClientEnd::WentAway,
            Err(e) => panic!("unexpected error observing the client end: {e}"),
        };
        let _ = verdict_tx.send(verdict);

        // See the doc comment: hold the connection open until the peer
        // leaves, so that the server is never the one that ends it.
        sock.set_read_timeout(Some(BOUND))
            .expect("set_read_timeout");
        while matches!(sock.read(&mut buf), Ok(n) if n > 0) {}
    });

    (addr, seen_rx, verdict_rx)
}

fn transport() -> Native<Tokio, Rustls, SystemDns<Tokio>> {
    Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio))
}

fn get(addr: SocketAddr) -> http::Request<RequestBody> {
    http::Request::builder()
        .method(http::Method::GET)
        .uri(format!("http://{addr}/cancel"))
        .body(RequestBody::Empty)
        .expect("request")
}

/// The claim in one sentence: after the request is on the wire, dropping
/// the future closes the connection, and the *server* is the one that says
/// so.
///
/// Mutation that turns this red: give `NativeBody`/`h1::exchange` a way to
/// outlive the future — spawning the `Connection` onto a task, or leaking
/// it with `std::mem::forget` — and the server sits out the whole
/// observation window with the connection intact.
#[tokio::test]
async fn dropping_the_execute_future_closes_the_connection_the_server_sees() {
    let (addr, seen, verdict) = silent_server();
    let t = transport();
    let mut fut = Box::pin(t.execute(get(addr)));

    // Poll the exchange until the server confirms it has the request. Not
    // a sleep: the point of dropping is lost if there was nothing in
    // flight yet, and this is the server saying there was.
    tokio::select! {
        _ = &mut fut => panic!("the silent server must never produce a response"),
        r = seen => r.expect("the server must report the request it read"),
    }

    // `Box::pin`, so this is a real drop of the future itself — dropping a
    // `Pin<&mut F>` from `tokio::pin!` would drop the borrow and leave the
    // future alive on the stack, and every assertion below would then be
    // about nothing.
    drop(fut);

    let seen_by_server = tokio::time::timeout(BOUND, verdict)
        .await
        .expect("the server must report a verdict")
        .expect("the server thread must not die before reporting");
    assert_eq!(
        seen_by_server,
        ClientEnd::WentAway,
        "dropping the execute future must end the exchange on the wire, not merely stop \
         polling it: the server still had the connection open {OBSERVATION_WINDOW:?} after \
         the drop"
    );
}

/// The control, and the half that makes the test above mean anything: same
/// request, same server, same observation window, future *not* dropped.
///
/// If this failed, the other test would be proving that the connection
/// closes — not that dropping closes it.
#[tokio::test]
async fn holding_the_execute_future_leaves_the_connection_open() {
    let (addr, seen, verdict) = silent_server();
    let t = transport();
    let mut fut = Box::pin(t.execute(get(addr)));

    tokio::select! {
        _ = &mut fut => panic!("the silent server must never produce a response"),
        r = seen => r.expect("the server must report the request it read"),
    }

    // Keep polling across the whole observation window, which is what
    // makes this a control for the drop rather than for the wait: the
    // future is alive AND being driven while the server watches.
    let seen_by_server = tokio::select! {
        _ = &mut fut => panic!("the silent server must never produce a response"),
        v = verdict => v.expect("the server thread must not die before reporting"),
    };
    assert_eq!(
        seen_by_server,
        ClientEnd::StillThere,
        "a live, polled execute future must keep its connection: if this end closes on its \
         own, the drop test above is measuring the passage of time and not the drop"
    );
}

/// The same property one level up, where callers actually meet it:
/// `Client::get(..).send()`. The facade wraps the transport's future in its
/// own (redirect stage, timeout merging), and a wrapper that spawned,
/// buffered or otherwise detached the inner future would break the
/// contract without `Native` changing at all.
#[tokio::test]
async fn dropping_a_client_send_future_closes_the_connection_too() {
    let (addr, seen, verdict) = silent_server();
    let client = Client::builder(transport()).build().expect("build");
    let mut fut = Box::pin(client.get(format!("http://{addr}/cancel")).send());

    tokio::select! {
        _ = &mut fut => panic!("the silent server must never produce a response"),
        r = seen => r.expect("the server must report the request it read"),
    }
    drop(fut);

    let seen_by_server = tokio::time::timeout(BOUND, verdict)
        .await
        .expect("the server must report a verdict")
        .expect("the server thread must not die before reporting");
    assert_eq!(
        seen_by_server,
        ClientEnd::WentAway,
        "the contract has to survive the facade: dropping the future `Client::send` returns \
         must end the exchange exactly as dropping the transport's own does"
    );
}

/// The declaration and the behaviour are checked in the same file on
/// purpose.
///
/// `CancelSupport::None` is a legitimate answer for a backend that cannot
/// cancel — and it is therefore also the obvious way to make the two tests
/// above stop applying to this backend without anyone noticing. Asserting
/// the declared value here closes that exit: dropping `Native`'s
/// cancellation would have to be an explicit edit to this test, not a
/// quiet capability flip that leaves a green suite behind.
#[tokio::test]
async fn native_declares_the_cancellation_it_performs() {
    assert_eq!(
        transport().capabilities().cancel_on_drop,
        CancelSupport::Supported,
        "the tests in this file measure a cancellation that `Native` must also declare — a \
         backend is free to declare `None`, but not to declare `None` while behaving \
         otherwise, nor to quietly stop being covered by the measurement"
    );
}
