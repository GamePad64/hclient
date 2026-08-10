//! Connection reuse (v0.2 W2), observed from outside the client.
//!
//! # Every claim here is the server's, not ours
//!
//! A pool test that passes whether or not anything was reused is worse than
//! no test, and a counter written by the same code that does the pooling
//! would be exactly that. So the only thing asserted below is a number the
//! **server** produced: how many TCP connections it accepted. Two requests
//! over one accepted connection is reuse; two requests over two is not; and
//! neither answer can be produced by a client-side variable being
//! incremented in the wrong place.
//!
//! # Both halves are load-bearing
//!
//! `two_requests_travel_over_one_connection` on its own would also pass
//! against a client that opened one connection and then failed the second
//! request, or against a server that only ever managed to accept once. Its
//! control, `without_a_pool_each_request_gets_its_own_connection`, runs the
//! *same* two requests against the *same* server through a transport whose
//! only difference is `Native::without_pool`, and requires two. The pair
//! attributes the single connection to the pool rather than to anything
//! about the fixture.
#![cfg(not(target_family = "wasm"))]

use http_ng::Client;
use http_ng_core::ReuseSupport;
use http_ng_core::unversioned::Transport;
use http_ng_dns_system::SystemDns;
use http_ng_native::{Native, PoolConfig};
use http_ng_rt_tokio::Tokio;
use http_ng_tls_rustls::Rustls;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// Ceiling for anything that must not hang.
const BOUND: Duration = Duration::from_secs(30);

/// What the fixture server does, beyond answering.
#[derive(Clone, Default)]
struct Behaviour {
    /// Close the connection once it has served this many responses, the
    /// way a server whose keep-alive budget has run out does. Note this is
    /// a bare close — there is no `Connection: close` header — so the
    /// client cannot learn about it from the response it already has.
    responses_before_close: Option<usize>,
    /// Wait before writing the response, so a test has a window in which to
    /// drop something.
    delay: Duration,
    /// Send `Connection: close` with every response — the polite form of
    /// the bare close above, and a different code path: the client learns
    /// from the response itself that this connection is over, so hyper's
    /// `Connection` future completes rather than the socket simply going
    /// quiet.
    connection_close_header: bool,
    /// Wait this long between the last response and dropping the socket.
    ///
    /// A scheduler delay, made explicit. `responses_before_close` writes a
    /// response and closes in the next few instructions, and on an idle
    /// machine the `FIN` therefore follows the response by microseconds —
    /// which is what let every test below wait for the close with a
    /// `sleep` and get away with it. It is not a guarantee: the two are
    /// separate operations by a thread the OS may deschedule between them,
    /// and under load it does. See [`closes`](Behaviour::closes).
    close_delay: Duration,
    /// The second observer: bumped once per socket **after** it has been
    /// dropped.
    ///
    /// An observer rather than a behaviour, and it lives in this struct
    /// only because this struct is what already reaches [`serve`].
    /// `accepted` says how many connections the server took; this says how
    /// many it has finished with, which is the fact a test needs before it
    /// may call a pooled connection dead. Waiting on it is the difference
    /// between "the server has probably closed by now" and "the server has
    /// closed" — and the first of those is a race the client loses as a
    /// `ConnectionReset`, see
    /// [`checkout_walks_past_a_dead_connection_to_a_live_one`].
    closes: Option<Arc<AtomicUsize>>,
}

/// The observer: a server that counts the connections it accepts and can
/// answer several requests on each of them.
///
/// Returns the address and that counter. Nothing else about it is asserted
/// on anywhere in this file.
fn counting_server(behaviour: Behaviour) -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&accepted);
    std::thread::spawn(move || {
        for sock in listener.incoming() {
            let Ok(sock) = sock else { continue };
            counter.fetch_add(1, Ordering::SeqCst);
            let behaviour = behaviour.clone();
            std::thread::spawn(move || serve(sock, behaviour));
        }
    });
    (addr, accepted)
}

/// One connection: read a request head, answer it, repeat until the peer
/// goes away (or until the first response, when the behaviour says so).
fn serve(mut sock: std::net::TcpStream, behaviour: Behaviour) {
    sock.set_read_timeout(Some(BOUND))
        .expect("set_read_timeout");
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 1024];
    let mut served = 0usize;
    loop {
        // A request head ends at the first blank line. Every request this
        // file sends is a bodyless GET, so that is the whole request.
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

        if !behaviour.delay.is_zero() {
            std::thread::sleep(behaviour.delay);
        }
        let response: &[u8] = if behaviour.connection_close_header {
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
        } else {
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok"
        };
        if sock.write_all(response).is_err() {
            return;
        }
        served += 1;
        if Some(served) == behaviour.responses_before_close {
            std::thread::sleep(behaviour.close_delay);
            // Dropped explicitly, and the counter bumped only afterwards,
            // so a test that has seen the bump has seen a socket that is
            // shut — not one that is about to be.
            drop(sock);
            if let Some(closes) = &behaviour.closes {
                closes.fetch_add(1, Ordering::SeqCst);
            }
            return;
        }
    }
}

/// Waits for the server to report `n` closed sockets.
///
/// The counterpart to [`Behaviour::closes`]. Bounded, and the bound is a
/// failure rather than a `return`: a test whose premise never arrived has
/// not passed.
async fn server_has_closed(closes: &AtomicUsize, n: usize) {
    tokio::time::timeout(BOUND, async {
        while closes.load(Ordering::SeqCst) < n {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the server never closed {n} connection(s)"));
}

fn native() -> Native<Tokio, Rustls, SystemDns<Tokio>> {
    Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio))
}

async fn get_ok(client: &Client<Native<Tokio, Rustls, SystemDns<Tokio>>>, addr: SocketAddr) {
    let resp = tokio::time::timeout(BOUND, client.get(&format!("http://{addr}/")).send())
        .await
        .expect("must not hang")
        .expect("request must succeed");
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.collect().await.expect("body").text().expect("text"),
        "ok"
    );
}

#[tokio::test]
async fn two_requests_travel_over_one_connection() {
    let (addr, accepted) = counting_server(Behaviour::default());
    let client = Client::builder(native()).build().unwrap();

    get_ok(&client, addr).await;
    get_ok(&client, addr).await;

    assert_eq!(
        accepted.load(Ordering::SeqCst),
        1,
        "the server must have accepted ONE connection for two requests"
    );
}

/// The control for the test above — see the module doc comment.
#[tokio::test]
async fn without_a_pool_each_request_gets_its_own_connection() {
    let (addr, accepted) = counting_server(Behaviour::default());
    let client = Client::builder(native().without_pool()).build().unwrap();

    get_ok(&client, addr).await;
    get_ok(&client, addr).await;

    assert_eq!(
        accepted.load(Ordering::SeqCst),
        2,
        "without a pool every request must open its own connection"
    );
}

/// The capability is not a second place where the same decision is written
/// down — it is read off the pool. Both values have to be reachable, and
/// each one has to match what the two tests above measured from outside.
#[tokio::test]
async fn the_capability_reports_what_the_pool_actually_does() {
    assert_eq!(
        native().capabilities().connection_reuse,
        ReuseSupport::Supported,
        "reuse is on by default, and the pair of tests above measured it"
    );
    assert_eq!(
        native().without_pool().capabilities().connection_reuse,
        ReuseSupport::None,
    );
    assert_eq!(
        native()
            .pool(PoolConfig {
                idle_timeout: Duration::from_secs(1),
                max_idle_per_key: 2,
            })
            .capabilities()
            .connection_reuse,
        ReuseSupport::Supported,
    );
}

/// A connection older than the idle timeout is not handed out.
///
/// The window is real time rather than a mocked clock, because the point of
/// the assertion is the server's count, and 150ms of it costs less than a
/// fixture that could disagree with the runtime's own timer.
#[tokio::test]
async fn a_connection_past_its_idle_timeout_is_not_reused() {
    let (addr, accepted) = counting_server(Behaviour::default());
    let client = Client::builder(native().pool(PoolConfig {
        idle_timeout: Duration::from_millis(50),
        ..PoolConfig::default()
    }))
    .build()
    .unwrap();

    get_ok(&client, addr).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    get_ok(&client, addr).await;

    assert_eq!(
        accepted.load(Ordering::SeqCst),
        2,
        "a connection past its idle timeout must not be handed out again"
    );
}

/// A server that closes after answering. The client cannot know from the
/// response that this happened — there is no `Connection: close` header —
/// so it finds out at checkout, and the second request must still succeed.
///
/// What would break without the poll in `h1::is_reusable`: the second
/// request would be written onto a socket the server has already closed.
#[tokio::test]
async fn a_connection_the_server_closed_while_idle_is_not_handed_out() {
    let closes = Arc::new(AtomicUsize::new(0));
    let (addr, accepted) = counting_server(Behaviour {
        responses_before_close: Some(1),
        closes: Some(Arc::clone(&closes)),
        ..Behaviour::default()
    });
    let client = Client::builder(native()).build().unwrap();

    get_ok(&client, addr).await;
    // The same barrier as
    // `checkout_walks_past_a_dead_connection_to_a_live_one`, and for the
    // same reason: this test's premise is a socket the server has closed,
    // and "the response arrived" does not imply it. Not separately
    // measured here — this test has never been seen to fail — but the race
    // is the identical one and costs nothing to remove.
    server_has_closed(&closes, 1).await;
    // The `FIN` is on its way; nothing polls the connection, so nobody has
    // read it yet. Long enough that it has certainly arrived at the kernel,
    // which is what makes the checkout poll's job the deterministic one.
    tokio::time::sleep(Duration::from_millis(100)).await;
    get_ok(&client, addr).await;

    assert_eq!(
        accepted.load(Ordering::SeqCst),
        2,
        "the second request must be on a new connection, and must succeed"
    );
}

/// What the poll at checkout is for, as distinct from the look
/// `h1::exchange` takes one instant later.
///
/// Both notice a connection the server closed while nobody was watching,
/// and for a pool holding a single connection they are interchangeable —
/// the request would be handed over, come straight back, and go out on a
/// fresh connection. What only the checkout poll can do is **move on to the
/// next candidate**: it runs before the request is committed to any one
/// connection, so a dead entry costs a look rather than the whole pool.
///
/// The fixture arranges exactly that: two connections parked, the one that
/// would be tried first (most recently used) already closed by the server,
/// and a live one behind it. Reusing that live one means the server never
/// accepts a third connection. Without the checkout poll the dead one is
/// handed the request, hands it back, and a third connection is opened
/// while the live one stays parked, unused.
///
/// # The dead one has to be dead, and waiting is not the same as knowing
///
/// This test was flaky on `main` — roughly one run in forty under load,
/// always `Connect / hyper::Error(Io, ConnectionReset)` — and the cause
/// was in this fixture rather than in the pool. "Already closed by the
/// server" used to be arranged by sleeping 100 ms after the response that
/// exhausts the connection, on the reasoning that a `FIN` follows its
/// response by microseconds. It does, on an idle machine. Writing the
/// response and dropping the socket are two operations by a thread the OS
/// may deschedule between them, though, and under load it does — and a
/// checkout that lands in that gap finds a connection which is *not yet*
/// closed, which no poll can tell from a live one. The request then goes
/// out on it, the server closes with those bytes still unread, its kernel
/// answers `RST`, and hyper reports the failure with the request already
/// on the wire: `Failed::Sent`, which the pool's retry deliberately does
/// not cover, because resending bytes a server may have acted on is a
/// different promise. Reproduced deterministically by giving the server's
/// close a 150 ms delay: the same error, five runs out of five, against a
/// control with the close prompt that passes.
///
/// So the wait below is on [`Behaviour::closes`] — the server saying it
/// has dropped the socket — and only then on the clock, for the `FIN` to
/// be visible to the runtime. [`Behaviour::close_delay`] is what keeps
/// that honest: the server here closes 300 ms late *on purpose*, so the
/// barrier is load-bearing rather than decorative, and deleting it fails
/// this test every run instead of one in forty.
#[tokio::test]
async fn checkout_walks_past_a_dead_connection_to_a_live_one() {
    // Each connection is closed by the server after its SECOND response,
    // which is what lets the test decide which of the two parked
    // connections is the dead one.
    let closes = Arc::new(AtomicUsize::new(0));
    let (addr, accepted) = counting_server(Behaviour {
        responses_before_close: Some(2),
        delay: Duration::from_millis(200),
        close_delay: Duration::from_millis(300),
        closes: Some(Arc::clone(&closes)),
        ..Behaviour::default()
    });
    let client = Client::builder(native()).build().unwrap();

    // Two connections, both parked, both having served one request. The
    // delay is what keeps them concurrent, so neither can serve both.
    let (a, b) = tokio::join!(get_ok(&client, addr), get_ok(&client, addr));
    let _ = (a, b);
    assert_eq!(accepted.load(Ordering::SeqCst), 2, "two, not one");

    // One more request takes the most recently parked connection and gives
    // it its second response — after which the server closes it. It is
    // parked again, on top, still looking fine from here.
    get_ok(&client, addr).await;
    // The server has dropped that socket. Not "has had time to": has.
    server_has_closed(&closes, 1).await;
    // The `FIN` is on its way; nothing polls the connection, so nobody has
    // read it yet. This is now slack after a fact rather than a guess at
    // one — see the doc comment.
    tokio::time::sleep(Duration::from_millis(100)).await;

    get_ok(&client, addr).await;

    assert_eq!(
        accepted.load(Ordering::SeqCst),
        2,
        "the live connection behind the dead one must have been used, rather \
         than a third connection opened"
    );
}

/// A server that says `Connection: close` is believed, and the connection
/// it said it about is not offered to the next request.
///
/// Not the same path as the bare close two tests up, and that is the point
/// of having both: here the client learns from the response itself, so
/// hyper's `Connection` future completes during the body rather than the
/// socket going quiet with nobody watching. `NativeBody::poll_frame` gives
/// up its check-in the moment that happens, which is the only reason this
/// connection is not parked — there is deliberately no second check asking
/// the same question afterwards.
#[tokio::test]
async fn a_connection_the_server_asked_to_close_is_not_reused() {
    let (addr, accepted) = counting_server(Behaviour {
        connection_close_header: true,
        ..Behaviour::default()
    });
    let client = Client::builder(native()).build().unwrap();

    get_ok(&client, addr).await;
    get_ok(&client, addr).await;

    assert_eq!(
        accepted.load(Ordering::SeqCst),
        2,
        "a connection the server declared finished must not be handed out again"
    );
}

/// v0.2 W1's rule, made structural: a cancelled exchange leaves the
/// connection in a state nobody can describe, so it must not go back into
/// the pool.
///
/// The exchange is cancelled by dropping the future while the server is
/// still holding the response back, so there genuinely is something to
/// cancel — a server that answered at once would leave no window and the
/// test would pass without ever exercising the rule.
#[tokio::test]
async fn a_cancelled_exchange_does_not_return_its_connection() {
    let (addr, accepted) = counting_server(Behaviour {
        delay: Duration::from_secs(3),
        ..Behaviour::default()
    });
    let client = Client::builder(native()).build().unwrap();

    let cancelled = tokio::time::timeout(
        Duration::from_millis(150),
        client.get(&format!("http://{addr}/")).send(),
    )
    .await;
    assert!(cancelled.is_err(), "the request must still be in flight");

    let second = tokio::time::timeout(
        Duration::from_secs(10),
        client.get(&format!("http://{addr}/")).send(),
    )
    .await
    .expect("must not hang");
    assert!(second.is_ok(), "the second request must still work");

    assert_eq!(
        accepted.load(Ordering::SeqCst),
        2,
        "a cancelled exchange must not have handed its connection back"
    );
}

/// The same rule one step later: a response body the caller abandoned
/// half-read leaves unread bytes on the socket, so that connection is not
/// reusable either. Check-in happens at exactly one place — the body
/// reporting a clean end — and dropping the body is not it.
#[tokio::test]
async fn an_abandoned_response_body_does_not_return_its_connection() {
    let (addr, accepted) = counting_server(Behaviour::default());
    let client = Client::builder(native()).build().unwrap();

    let resp = tokio::time::timeout(BOUND, client.get(&format!("http://{addr}/")).send())
        .await
        .expect("must not hang")
        .expect("request must succeed");
    assert_eq!(resp.status(), 200);
    // Dropped with its body unread: no `collect`, no `poll_frame` ever
    // reporting the end.
    drop(resp);

    get_ok(&client, addr).await;

    assert_eq!(
        accepted.load(Ordering::SeqCst),
        2,
        "a body that never reported a clean end must not have been pooled"
    );
}

/// `PoolConfig::max_idle_per_key`, measured the same way as everything
/// else here: by how many connections the server has to accept.
///
/// Three requests are started before any of them can finish — the server
/// holds its responses back — so all three open connections of their own
/// and all three are offered back afterwards. What the bound decides is how
/// many of those survive to serve the *next* three: with room for one, two
/// more connections have to be opened; with room for eight, none do.
///
/// The two halves differ in exactly one field, and an unbounded pool would
/// pass the second and fail the first.
async fn accepted_for_two_rounds_of_three(max_idle_per_key: usize) -> usize {
    let (addr, accepted) = counting_server(Behaviour {
        delay: Duration::from_millis(200),
        ..Behaviour::default()
    });
    let client = Client::builder(native().pool(PoolConfig {
        max_idle_per_key,
        ..PoolConfig::default()
    }))
    .build()
    .unwrap();

    for _ in 0..2 {
        // Concurrent, not sequential: sequential requests would hand the
        // same one connection back and forth and never fill the pool.
        // Nothing is spawned — three futures on one task, which is also a
        // small proof that the pool does not need an executor.
        let (a, b, c) = tokio::join!(
            get_ok(&client, addr),
            get_ok(&client, addr),
            get_ok(&client, addr),
        );
        let _ = (a, b, c);
    }
    accepted.load(Ordering::SeqCst)
}

#[tokio::test]
async fn the_per_key_bound_limits_how_many_connections_are_kept() {
    assert_eq!(
        accepted_for_two_rounds_of_three(1).await,
        5,
        "room for one connection: the second round reuses one and opens two"
    );
}

/// The control for the test above: with room for all three, the second
/// round opens nothing.
#[tokio::test]
async fn room_for_every_connection_means_the_second_round_opens_none() {
    assert_eq!(accepted_for_two_rounds_of_three(8).await, 3);
}

// ── The retry, and the race it exists for ───────────────────────────────
//
// A pool introduces a failure that did not exist before it: a request that
// did nothing wrong, on a connection the server closed while nobody was
// polling it. The checkout poll catches almost all of those, and "almost"
// is the problem — a `FIN` that lands in the instant between the checkout
// poll and the request being handed to hyper would fail a request that a
// client without a pool would have completed.
//
// That instant cannot be hit from outside: it is microseconds wide and
// nothing about a server can aim at it. So the fixture below moves the
// window rather than the timing — a runtime whose sockets report their
// first EOF one poll later than it happened. The socket really is closed,
// the server really did close it, and the client really does discover it
// exactly where the race would have put it. Everything else in the test is
// the same server counting the same connections.

/// [`Tokio`], with sockets that hide their first `n` EOFs, one per poll.
///
/// `n` is where in the sequence the server's close becomes visible. `1`
/// hides it from the checkout poll and reveals it to `h1::exchange`'s look
/// before the request is handed over — the retryable window, and the one
/// this file is about. The window *after* that (hyper already has the
/// request) cannot be reached this way, because how many times hyper reads
/// per `Connection` poll is its own business and not a count a test may
/// pin; it is covered deterministically by `h1.rs`'s
/// `a_connection_that_ends_with_the_request_queued_fails_instead_of_hanging`,
/// where every poll is driven by hand.
#[derive(Clone, Debug)]
struct LateEof(Tokio, u8);

impl http_ng_rt::Timer for LateEof {
    type Instant = <Tokio as http_ng_rt::Timer>::Instant;
    /// Delegation stays exact: the wrapper's sleep IS the inner clock's.
    type Sleep = <Tokio as http_ng_rt::Timer>::Sleep;
    fn sleep(&self, d: Duration) -> Self::Sleep {
        self.0.sleep(d)
    }
    fn now(&self) -> Self::Instant {
        self.0.now()
    }
    fn elapsed_since(&self, earlier: Self::Instant) -> Duration {
        self.0.elapsed_since(earlier)
    }
}

impl http_ng_rt::TcpConnect for LateEof {
    type Stream = HideFirstEof<<Tokio as http_ng_rt::TcpConnect>::Stream>;
    async fn connect(
        &self,
        addr: SocketAddr,
        opts: &http_ng_rt::TcpOpts,
    ) -> std::io::Result<Self::Stream> {
        Ok(HideFirstEof {
            inner: self.0.connect(addr, opts).await?,
            hide_remaining: self.1,
        })
    }
}

struct HideFirstEof<S> {
    inner: S,
    hide_remaining: u8,
}

impl<S: hyper::rt::Read + Unpin> hyper::rt::Read for HideFirstEof<S> {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        mut buf: hyper::rt::ReadBufCursor<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        // Reads into a scratch buffer and copies, the same shape as
        // `testing::blocking_io`: `ReadBufCursor` moves into the inner
        // call, so there is no way to look at how much it was filled
        // afterwards.
        let this = self.get_mut();
        let mut scratch = [0u8; 8192];
        let want = buf.remaining().min(scratch.len());
        let mut rb = hyper::rt::ReadBuf::new(&mut scratch[..want]);
        match std::pin::Pin::new(&mut this.inner).poll_read(cx, rb.unfilled()) {
            std::task::Poll::Ready(Ok(())) => {
                let filled = rb.filled();
                if filled.is_empty() && this.hide_remaining > 0 {
                    this.hide_remaining -= 1;
                    cx.waker().wake_by_ref();
                    return std::task::Poll::Pending;
                }
                buf.put_slice(filled);
                std::task::Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}

impl<S: hyper::rt::Write + Unpin> hyper::rt::Write for HideFirstEof<S> {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// The request that loses the race must still succeed, on a fresh
/// connection, because the pool is what put it in that position.
///
/// Without the retry this is not a slow request or a degraded one — it is
/// an error returned to a caller who did nothing wrong, on a client that
/// would have worked before v0.2 W2. That is why the retry ships with the
/// pool rather than after it.
#[tokio::test]
async fn a_request_that_loses_the_race_is_retried_on_a_fresh_connection() {
    let closes = Arc::new(AtomicUsize::new(0));
    let (addr, accepted) = counting_server(Behaviour {
        responses_before_close: Some(1),
        closes: Some(Arc::clone(&closes)),
        ..Behaviour::default()
    });
    let transport = Native::new(
        LateEof(Tokio, 1),
        Rustls::with_webpki_roots(),
        SystemDns::new(Tokio),
    );
    let client = Client::builder(transport).build().unwrap();

    let url = format!("http://{addr}/");
    let first = tokio::time::timeout(BOUND, client.get(&url).send())
        .await
        .expect("must not hang")
        .expect("the first request must succeed");
    assert_eq!(first.collect().await.unwrap().text().unwrap(), "ok");

    // The server has closed — waited for rather than assumed, because the
    // whole fixture below is about *when* the `FIN` becomes visible, and
    // a close that has not happened yet is not a late `FIN`, it is a live
    // connection. The same barrier as
    // `checkout_walks_past_a_dead_connection_to_a_live_one`.
    server_has_closed(&closes, 1).await;
    // The `FIN` is in the kernel, and the socket will hide it for exactly
    // one poll — which is the poll at checkout.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let second = tokio::time::timeout(BOUND, client.get(&url).send())
        .await
        .expect("must not hang");
    let second = second.expect(
        "the request lost the race against the server's close and must have been \
         retried on a fresh connection, not returned as an error",
    );
    assert_eq!(second.status(), 200);
    assert_eq!(second.collect().await.unwrap().text().unwrap(), "ok");

    assert_eq!(
        accepted.load(Ordering::SeqCst),
        2,
        "the retry must be on a second, fresh connection"
    );
}

/// The pool key's protocol component, and the guard that keeps it honest:
/// a connection whose negotiated ALPN is not the protocol this transport
/// speaks does not go into the pool at all.
///
/// Both halves run through the same stub TLS backend against the same
/// server; the only difference is what the stub reports as negotiated. The
/// `http/1.1` half must reuse, so the test cannot pass by the connection
/// having been unusable for some unrelated reason.
mod alpn_guard {
    use super::*;
    use std::sync::OnceLock;

    /// Encrypts nothing and hands the stream straight back — so the
    /// "TLS" connection below is really the same plaintext HTTP the
    /// fixture server speaks. What it does do is report a negotiated
    /// ALPN, which is the one input the guard reads.
    struct ReportsAlpn(&'static [u8]);

    impl http_ng_tls::TlsIdentity for ReportsAlpn {
        fn config_id(&self) -> http_ng_tls::TlsConfigId {
            static ID: OnceLock<http_ng_tls::TlsConfigId> = OnceLock::new();
            *ID.get_or_init(http_ng_tls::TlsConfigId::new_unique)
        }
    }

    impl http_ng_tls::TlsConnect for ReportsAlpn {
        type Stream<S>
            = S
        where
            S: hyper::rt::Read + hyper::rt::Write + Unpin;
        async fn connect<S>(
            &self,
            io: S,
            _req: http_ng_tls::TlsRequest<'_>,
        ) -> Result<(S, http_ng_tls::TlsInfo), http_ng_core::Error>
        where
            S: hyper::rt::Read + hyper::rt::Write + Unpin,
        {
            Ok((
                io,
                http_ng_tls::TlsInfo {
                    alpn: Some(self.0.to_vec()),
                    ..Default::default()
                },
            ))
        }
    }

    async fn accepted_for(negotiated: &'static [u8]) -> usize {
        let (addr, accepted) = counting_server(Behaviour::default());
        let client = Client::builder(Native::new(
            Tokio,
            ReportsAlpn(negotiated),
            SystemDns::new(Tokio),
        ))
        .build()
        .unwrap();
        let url = format!("https://{addr}/");
        for _ in 0..2 {
            let resp = tokio::time::timeout(BOUND, client.get(&url).send())
                .await
                .expect("must not hang")
                .expect("request must succeed");
            assert_eq!(resp.status(), 200);
            assert_eq!(resp.collect().await.unwrap().text().unwrap(), "ok");
        }
        accepted.load(Ordering::SeqCst)
    }

    /// The half of the key's security component that can be reached from
    /// outside today: a plaintext connection must never serve an `https://`
    /// request, nor the reverse.
    ///
    /// The other half — two different TLS *configurations* to the same
    /// origin — cannot be reached this way, because a pool belongs to one
    /// `Native` and a `Native` owns one `TlsConnect`, so within any one pool
    /// the identity is a constant. That is written down on
    /// `pool::PoolKey`, along with why the component is there anyway; the
    /// identities themselves are checked at the `TlsConnect` level, in
    /// `http-ng-tls-rustls/tests/config_id.rs`.
    ///
    /// The stub performs no encryption, so both requests really do speak
    /// plain HTTP/1.1 to the same fixture server on the same port. Nothing
    /// but the key stops the second one from reusing the first one's
    /// connection.
    #[tokio::test]
    async fn a_plaintext_connection_is_not_handed_to_an_https_request() {
        let (addr, accepted) = counting_server(Behaviour::default());
        let client = Client::builder(Native::new(
            Tokio,
            ReportsAlpn(b"http/1.1"),
            SystemDns::new(Tokio),
        ))
        .build()
        .unwrap();

        for url in [format!("http://{addr}/"), format!("https://{addr}/")] {
            let resp = tokio::time::timeout(BOUND, client.get(&url).send())
                .await
                .expect("must not hang")
                .expect("request must succeed");
            assert_eq!(resp.status(), 200);
            assert_eq!(resp.collect().await.unwrap().text().unwrap(), "ok");
        }

        assert_eq!(
            accepted.load(Ordering::SeqCst),
            2,
            "`http://` and `https://` to the same host and port are different \
             pool keys, whatever the socket underneath happens to be"
        );
    }

    #[tokio::test]
    async fn a_connection_negotiated_as_http11_is_pooled() {
        assert_eq!(accepted_for(b"http/1.1").await, 1);
    }

    /// `h2` is the reported protocol here, and it is the right choice in
    /// **both** feature configurations, for two different reasons that
    /// happen to demand the same behaviour.
    ///
    /// Without the `http2` feature it is simply a protocol this transport
    /// does not speak. With the feature it is one it does — but `ReportsAlpn`
    /// leaves `TlsConnect::reports_alpn` at its `false` default, so `h2` was
    /// never in the offer, and a peer cannot select what was never proposed.
    /// Either way the connection is one this client must not go on using, and
    /// pooling it would hand it to the next request.
    #[tokio::test]
    async fn a_connection_negotiated_as_something_else_is_not_pooled() {
        assert_eq!(
            accepted_for(b"h2").await,
            2,
            "a connection reporting a protocol this transport did not offer \
             must not be handed to a later request"
        );
    }
}
