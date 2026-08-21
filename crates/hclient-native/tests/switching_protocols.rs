//! What a `101 Switching Protocols` does to the connection pool.
//!
//! # The question, and why it had to be measured
//!
//! Nothing in `hclient-native` mentions 101 or upgrades: grep the crate.
//! A server that answers `101` therefore produces an ordinary response
//! whose body ends immediately — and `H1Body` hands a connection back to
//! the pool at exactly one place, a body ending cleanly. Read that far,
//! the pool looks like it is being handed a socket that has stopped
//! speaking HTTP, which would be a bug today rather than a missing
//! WebSocket feature. `docs/v03-design.md` §W4 wrote it down as the first
//! thing to run, and this file is that run.
//!
//! # The answer: no — and what happens first is one poll's ordering
//!
//! hyper marks a 101 as `wants_upgrade` with a zero-length body and
//! `keep_alive = false` (`proto/h1/role.rs:1273`, `:1169-1177`), so its
//! dispatcher is finished the moment it has delivered the head: the same
//! `Connection::poll` that reads the response reaches
//! `Dispatched::Upgrade`, calls `pending.manual()` and returns
//! `Ready(Ok(()))` (`client/conn/http1.rs:313-320`). `h1::exchange` polls
//! the connection **before** the request future in every iteration of its
//! `poll_fn`, and the request future can only resolve from inside that
//! very poll — so by the time the response exists, `conn_done` is already
//! `true`, and the `H1Body` is built with no connection and no check-in
//! token. There is nothing left that could reach the pool, and the socket
//! is dropped when `exchange` returns. `h1.rs`'s own
//! `a_101_is_never_offered_to_the_pool` pins that mechanism from inside;
//! the tests here pin the consequence from outside. It is what happens
//! first rather than the whole reason the outcome is safe — see the
//! section after next, which is the more useful half of the answer.
//!
//! Note what this does *not* say. The upgrade is destroyed — that is what
//! `pending.manual()` means — and "the exchange succeeded" and "the
//! upgraded connection was thrown away" remain the same observation from
//! hyper. That is W4's problem to solve when the WebSocket seam is built;
//! it is not the pool's, and this file exists so that nobody has to
//! re-derive which of the two it was.
//!
//! # Why no single mutation makes these tests fail, and what that is
//!
//! Worth stating plainly, because a test nothing kills is normally a test
//! that measures nothing. Here it is a property of the subject: the fact
//! that decides the question — "this hyper `Connection` has finished" — is
//! asked for at **five** places, and a `101` makes it true at the first of
//! them. Measured by removing them one at a time, on this tree:
//!
//! 1. `h1::exchange`'s `conn_done`, which is why the body is built with no
//!    connection and no check-in token. Forcing it to `false` is caught by
//!    `h1::tests::a_101_is_never_offered_to_the_pool` and by nothing here.
//! 2. `H1Body::poll_frame` clearing `self.conn`, after which
//!    `hand_back_to_pool`'s `(Some(conn), Some(reuse))` pattern cannot
//!    match. (Its sibling `self.reuse = None` in the same arm is redundant
//!    with it — deleting that line alone kills no test in the workspace,
//!    including `tests/pool.rs`'s `Connection: close` one, which is
//!    carried by `self.conn` too.)
//! 3. `h1::is_reusable` polling the connection once at checkout.
//! 4. `h1::is_reusable` then asking `SendRequest::poll_ready`, which a
//!    finished dispatcher answers `Err` to for ever. **This one alone is
//!    enough**: with 1, 2, 3 and 5 all disabled, the upgraded connection
//!    really does get parked in the pool, and the next request still opens
//!    a socket of its own rather than writing onto it — so the assertions
//!    below stay green.
//! 5. `h1::exchange`'s pre-flight look, which would turn a handed-out dead
//!    connection into a retryable `Failed::NotSent`.
//!
//! So these tests are not here to catch a local edit. They are here to
//! catch the premise: everything above rests on hyper decoding a `101` as
//! a finished connection, which is hyper's decision and not ours, and the
//! only way to notice it changing is to ask a real hyper over a real
//! socket. That is also why the fixture is a plain `TcpListener` rather
//! than anything of ours.
//!
//! # The observer is the server, and it has been shown to see
//!
//! Every assertion below belongs to the fixture: how many connections it
//! accepted, and what arrived on the upgraded socket after it sent the
//! `101`. A client-side counter would be written by the same code that
//! does the pooling. The second test's first assertion is the control —
//! `accepted == 1` across two requests, i.e. this observer does see reuse
//! when reuse happens — so the `2` that follows the `101` is a difference
//! it measured rather than a number it cannot tell from any other.
//!
//! One thing the observer deliberately does not claim: it reports an
//! `EOF`, and an `EOF` is not proof that the client dropped the socket.
//! hyper half-closes the write side of a connection it has finished, so
//! the server sees a `FIN` whether the value was dropped or parked. The
//! assertion that discriminates is the absence of bytes and the accept
//! count, and those are the ones the failure messages are written about.
#![cfg(not(target_family = "wasm"))]

use hclient::Client;
use hclient_dns_system::SystemDns;
use hclient_native::Native;
use hclient_rt_tokio::Tokio;
use hclient_tls_rustls::Rustls;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Ceiling for anything that must not hang.
const BOUND: Duration = Duration::from_secs(30);

/// What the server saw on one accepted connection once it had answered
/// `101` — the whole observation this file makes.
#[derive(Debug, Default, Clone)]
struct AfterUpgrade {
    /// Bytes the client sent after the `101`. A second HTTP request here
    /// is the poisoning this file is about.
    bytes: Vec<u8>,
    /// Whether the server saw an `EOF`. Reported, and deliberately not
    /// asserted on as "the client dropped it" — see the module doc: hyper
    /// half-closes a finished connection either way.
    closed: bool,
}

/// The observer: counts accepted connections, answers `n` ordinary
/// requests on each and then upgrades, and records what follows.
fn upgrading_server(
    requests_before_upgrade: usize,
) -> (SocketAddr, Arc<AtomicUsize>, Arc<Mutex<Vec<AfterUpgrade>>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let accepted = Arc::new(AtomicUsize::new(0));
    let upgraded: Arc<Mutex<Vec<AfterUpgrade>>> = Arc::new(Mutex::new(Vec::new()));
    let counter = Arc::clone(&accepted);
    let sink = Arc::clone(&upgraded);
    std::thread::spawn(move || {
        for sock in listener.incoming() {
            let Ok(sock) = sock else { continue };
            counter.fetch_add(1, Ordering::SeqCst);
            let sink = Arc::clone(&sink);
            std::thread::spawn(move || serve(sock, requests_before_upgrade, sink));
        }
    });
    (addr, accepted, upgraded)
}

fn serve(
    mut sock: std::net::TcpStream,
    requests_before_upgrade: usize,
    sink: Arc<Mutex<Vec<AfterUpgrade>>>,
) {
    sock.set_read_timeout(Some(BOUND)).expect("read timeout");
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 1024];
    let mut served = 0usize;
    loop {
        // Every request this file sends is a bodyless GET, so the head is
        // the whole request.
        let head_end = loop {
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
            match sock.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        };
        buf.drain(..head_end);

        if served < requests_before_upgrade {
            if sock
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .is_err()
            {
                return;
            }
            served += 1;
            continue;
        }
        break;
    }

    let slot = {
        let mut v = sink.lock().expect("upgrade log poisoned");
        v.push(AfterUpgrade::default());
        v.len() - 1
    };
    if sock
        .write_all(
            b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: chat\r\nConnection: Upgrade\r\n\r\n",
        )
        .is_err()
    {
        return;
    }

    // From here the socket is no longer HTTP, and the server deliberately
    // keeps it open: whatever the client does with it is the measurement.
    loop {
        match sock.read(&mut chunk) {
            Ok(0) => {
                sink.lock().expect("upgrade log poisoned")[slot].closed = true;
                return;
            }
            Ok(n) => sink.lock().expect("upgrade log poisoned")[slot]
                .bytes
                .extend_from_slice(&chunk[..n]),
            Err(_) => return,
        }
    }
}

fn native() -> Native<Tokio, Rustls, SystemDns<Tokio>> {
    Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio))
}

async fn get(client: &Client, addr: SocketAddr) -> u16 {
    let resp = tokio::time::timeout(BOUND, client.get(&format!("http://{addr}/")).send())
        .await
        .expect("must not hang")
        .expect("request must succeed");
    let status = resp.status().as_u16();
    // Reading the body to its end is what would hand the connection back
    // to the pool, if anything were going to.
    tokio::time::timeout(BOUND, resp.collect())
        .await
        .expect("collecting must not hang")
        .expect("body");
    status
}

/// Waits until the server has recorded `n` upgraded connections and the
/// one at `index` has been closed by the client — or gives up and returns
/// what it has, so the assertion rather than the timeout describes the
/// failure.
async fn settle(log: &Mutex<Vec<AfterUpgrade>>, index: usize) -> AfterUpgrade {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        {
            let v = log.lock().expect("upgrade log poisoned");
            if let Some(entry) = v.get(index)
                && (entry.closed || !entry.bytes.is_empty() || Instant::now() > deadline)
            {
                return entry.clone();
            }
        }
        if Instant::now() > deadline {
            return log
                .lock()
                .expect("upgrade log poisoned")
                .get(index)
                .cloned()
                .unwrap_or_default();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A `101` on a fresh connection: the socket is dropped, and the next
/// request dials again rather than writing HTTP onto a protocol that is no
/// longer HTTP.
#[tokio::test]
async fn a_101_is_not_kept_for_the_next_request() {
    let (addr, accepted, log) = upgrading_server(0);
    let client = Client::builder(native()).build().unwrap();

    assert_eq!(get(&client, addr).await, 101);
    let first = settle(&log, 0).await;
    assert!(
        first.bytes.is_empty(),
        "nothing may be written onto a connection that stopped speaking HTTP, and the \
         server received {:?}",
        first.bytes
    );
    assert!(
        first.closed,
        "the client must at least have stopped writing to the upgraded connection; the \
         server has seen neither bytes nor a close, which means it is still being held \
         open as if it were an ordinary idle HTTP connection"
    );

    assert_eq!(get(&client, addr).await, 101);
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        2,
        "the second request must have opened a connection of its own"
    );
}

/// The same on a connection the pool really did hand out — and its own
/// control.
///
/// The first request is answered normally, so the connection is parked;
/// the second reuses it (`accepted == 1` is the control: this fixture's
/// connections *are* pooled, so the count after the third request is
/// about the `101` and not about the fixture) and is answered `101`; the
/// third must dial again.
#[tokio::test]
async fn a_101_takes_a_pooled_connection_back_out_of_the_pool() {
    let (addr, accepted, log) = upgrading_server(1);
    let client = Client::builder(native()).build().unwrap();

    assert_eq!(get(&client, addr).await, 200);
    assert_eq!(get(&client, addr).await, 101);
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        1,
        "the control: the second request must have reused the parked connection, or the \
         count below would say nothing about the 101"
    );

    let upgraded = settle(&log, 0).await;
    assert!(
        upgraded.bytes.is_empty(),
        "nothing may be written onto a connection that stopped speaking HTTP, and the \
         server received {:?}",
        upgraded.bytes
    );
    assert!(
        upgraded.closed,
        "a connection that was in the pool and then answered 101 must not go back into it"
    );

    assert_eq!(get(&client, addr).await, 200);
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        2,
        "the third request must have opened a connection of its own"
    );
}
