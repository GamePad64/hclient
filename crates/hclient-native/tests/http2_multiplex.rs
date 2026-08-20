//! `Native::multiplexed()` — one HTTP/2 connection, several requests
//! (v0.4), observed from outside the client.
//!
//! `docs/h2-multiplexing.md` is the investigation this file measures. What
//! lives here is the **opt-in's own shape and its three prices**; the three
//! yardstick limitations it closes are measured next to the assertions they
//! invert, in `tests/grpc_shape.rs` and `tests/http2.rs`, so that a reader
//! meets the old rule and the new one in one place.
//!
//! # Every count is the server's
//!
//! The observer is a real `h2::server` on a real socket: it performs the
//! connection preface, decodes HPACK, counts the TCP connections it
//! accepted and records the greatest number of request streams it ever had
//! open at once. A client that spoke HTTP/1.1 into it would get no answer
//! at all. **No test below reads anything off the client about itself.**
//!
//! The barrier is what makes an accept count mean anything: `/pair`
//! answers nobody until the expected number of requests has arrived, so
//! the calls provably overlapped at the server. A client that serialised
//! them would hang and fail on a ceiling rather than quietly report `1`.
//!
//! The TLS stub is `tests/http2.rs`'s, for the reason that file's module
//! doc argues at length: ALPN is the only route to HTTP/2 here and
//! `TlsConnect` is what reports it, so the stub encrypts nothing, hands
//! the stream through and reports `h2` — the bytes on the wire really are
//! HTTP/2.
#![cfg(all(feature = "http2", not(target_family = "wasm")))]

use bytes::Bytes;
use hclient::Client;
use hclient_core::unversioned::{CloseReason, Event, Hooks};
use hclient_core::{RequestBody, Timeouts};
use hclient_dns_system::SystemDns;
use hclient_native::{Native, PoolConfig};
use hclient_rt::{Spawn, TcpConnect, TcpOpts, TcpOptsSupport};
use hclient_rt_tokio::Tokio;
use hclient_tls::{TlsConfigId, TlsConnect, TlsIdentity, TlsInfo, TlsRequest};
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;

/// Ceiling for anything that must not hang. Every failure mode in this
/// file is a hang if the client gets it wrong, so this is what turns
/// "wrong" into a named failure instead of a stuck run.
const BOUND: Duration = Duration::from_secs(30);

// ── the server ──────────────────────────────────────────────────────────

#[derive(Default)]
struct Shared {
    /// TCP connections accepted. Every claim in this file about sharing is
    /// this number.
    accepted: AtomicUsize,
    /// Connections whose accept loop has ended — a close, observed rather
    /// than waited out.
    closed: AtomicUsize,
    /// Request streams the server has finished with.
    served: AtomicUsize,
    /// Request streams open at this instant, and the greatest that number
    /// has ever been. The second is what says whether a peer's
    /// `MAX_CONCURRENT_STREAMS` was respected, and it is the server's
    /// reading rather than the client's intention.
    open: AtomicUsize,
    most_open: AtomicUsize,
    /// How many requests must arrive at `/pair` before any of them is
    /// answered.
    expect: AtomicUsize,
    arrived: AtomicUsize,
    /// Request body bytes the server counted, across every stream.
    body_bytes: AtomicUsize,
}

struct Fixture {
    addr: SocketAddr,
    shared: Arc<Shared>,
}

impl Fixture {
    fn url(&self, path: &str) -> String {
        format!("https://{}{}", self.addr, path)
    }

    fn accepted(&self) -> usize {
        self.shared.accepted.load(Ordering::SeqCst)
    }

    fn served(&self) -> usize {
        self.shared.served.load(Ordering::SeqCst)
    }

    fn most_open(&self) -> usize {
        self.shared.most_open.load(Ordering::SeqCst)
    }

    fn body_bytes(&self) -> usize {
        self.shared.body_bytes.load(Ordering::SeqCst)
    }

    fn expect(&self, n: usize) {
        self.shared.expect.store(n, Ordering::SeqCst);
    }

    async fn wait_for_closed(&self, n: usize, within: Duration) -> bool {
        let deadline = std::time::Instant::now() + within;
        while std::time::Instant::now() < deadline {
            if self.shared.closed.load(Ordering::SeqCst) >= n {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }
}

/// An HTTP/2 server and nothing else.
///
/// `limit` is the `MAX_CONCURRENT_STREAMS` it announces — `None` for h2's
/// own default, which is effectively unbounded for these tests.
fn spawn_server(limit: Option<u32>) -> Fixture {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let shared = Arc::new(Shared::default());

    let shared_t = Arc::clone(&shared);
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    continue;
                };
                shared_t.accepted.fetch_add(1, Ordering::SeqCst);
                let shared = Arc::clone(&shared_t);
                tokio::spawn(async move {
                    let _ = serve(tcp, Arc::clone(&shared), limit).await;
                    shared.closed.fetch_add(1, Ordering::SeqCst);
                });
            }
        });
    });

    Fixture { addr, shared }
}

async fn serve(
    tcp: tokio::net::TcpStream,
    shared: Arc<Shared>,
    limit: Option<u32>,
) -> Result<(), h2::Error> {
    serve_io(tcp, shared, limit).await
}

/// The same, over any IO — so a test can put a wrapper between the socket
/// and `h2` without a second copy of the server.
async fn serve_io<I>(io: I, shared: Arc<Shared>, limit: Option<u32>) -> Result<(), h2::Error>
where
    I: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let mut builder = h2::server::Builder::new();
    if let Some(n) = limit {
        builder.max_concurrent_streams(n);
    }
    let mut conn = builder.handshake(io).await?;
    while let Some(accepted) = conn.accept().await {
        let (req, respond) = accepted?;
        let goaway = req.uri().path() == "/goaway";
        let for_handler = Arc::clone(&shared);
        tokio::spawn(async move {
            handle(req, respond, for_handler).await;
        });
        if goaway {
            conn.graceful_shutdown();
        }
    }
    Ok(())
}

async fn handle(
    req: http::Request<h2::RecvStream>,
    mut respond: h2::server::SendResponse<Bytes>,
    shared: Arc<Shared>,
) {
    let open = shared.open.fetch_add(1, Ordering::SeqCst) + 1;
    shared.most_open.fetch_max(open, Ordering::SeqCst);

    let path = req.uri().path().to_owned();
    let (_, mut body) = req.into_parts();

    // `/duplex` answers BEFORE it reads a byte of the request, which is
    // what lets a caller's body depend on the head having arrived.
    let mut duplex = (path == "/duplex")
        .then(|| {
            let head = http::Response::builder().status(200).body(()).unwrap();
            respond.send_response(head, false).ok()
        })
        .flatten();

    let mut got = 0usize;
    while let Some(chunk) = body.data().await {
        let Ok(chunk) = chunk else { break };
        got += chunk.len();
        let _ = body.flow_control().release_capacity(chunk.len());
    }
    shared.body_bytes.fetch_add(got, Ordering::SeqCst);

    if let Some(send) = duplex.as_mut() {
        // The count is the server's, and it is the whole response: a
        // request body that did not all cross is a different number.
        let _ = send.send_data(Bytes::from(format!("{got} bytes")), true);
        shared.open.fetch_sub(1, Ordering::SeqCst);
        shared.served.fetch_add(1, Ordering::SeqCst);
        return;
    }

    if path == "/pair" {
        // Nobody is answered until the expected number of requests is in
        // flight at once. A client that serialised them would hang here,
        // and the test's ceiling is what would fail.
        shared.arrived.fetch_add(1, Ordering::SeqCst);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while shared.arrived.load(Ordering::SeqCst) < shared.expect.load(Ordering::SeqCst)
            && std::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    if path == "/queue" {
        // Long enough that the waves a stream limit produces are visible
        // as waves rather than as scheduling noise.
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    if path == "/hints" {
        let interim = http::Response::builder()
            .status(103)
            .header("link", "</s.css>; rel=preload")
            .body(())
            .unwrap();
        let _ = respond.send_informational(interim);
    }
    let resp = http::Response::builder().status(200).body(()).unwrap();
    if let Ok(mut send) = respond.send_response(resp, false) {
        let _ = send.send_data(Bytes::from_static(b"ok"), true);
    }
    shared.open.fetch_sub(1, Ordering::SeqCst);
    shared.served.fetch_add(1, Ordering::SeqCst);
}

// ── the TLS stub ────────────────────────────────────────────────────────

#[derive(Clone)]
struct FakeTls(TlsConfigId);

impl Default for FakeTls {
    fn default() -> Self {
        Self(TlsConfigId::new_unique())
    }
}

impl TlsIdentity for FakeTls {
    fn config_id(&self) -> TlsConfigId {
        self.0
    }
}

impl TlsConnect for FakeTls {
    type Stream<S>
        = S
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin;

    fn reports_alpn(&self) -> bool {
        true
    }

    async fn connect<S>(
        &self,
        io: S,
        _req: TlsRequest<'_>,
    ) -> Result<(S, TlsInfo), hclient_core::Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin,
    {
        Ok((
            io,
            TlsInfo {
                alpn: Some(b"h2".to_vec()),
                ..Default::default()
            },
        ))
    }
}

// ── clients ─────────────────────────────────────────────────────────────

type Plain = Native<Tokio, FakeTls, SystemDns<Tokio>>;

fn transport() -> Plain {
    Native::new(Tokio, FakeTls::default(), SystemDns::new(Tokio))
}

fn shared_client() -> Client<Plain> {
    Client::builder(transport().multiplexed()).build().unwrap()
}

fn exclusive_client() -> Client<Plain> {
    Client::builder(transport()).build().unwrap()
}

/// One `GET`, to the point where the server has answered and the body has
/// been read to its end. Written out because "the call is over" is the
/// premise of half the assertions here, and a response whose body was
/// never drained is not over.
async fn call<H>(
    client: &Client<Native<Tokio, FakeTls, SystemDns<Tokio>, H>>,
    url: String,
) -> Result<http::StatusCode, hclient::Error>
where
    H: Hooks + Clone + Unpin,
{
    let resp = client.get(&url).send().await?;
    let status = resp.status();
    assert_eq!(resp.version(), http::Version::HTTP_2);
    resp.collect().await?;
    Ok(status)
}

// ── the headline ────────────────────────────────────────────────────────

/// **Eight concurrent calls, one connection.**
///
/// The barrier makes it causal: `/pair` answers nobody until all eight
/// have arrived, so a client that had opened one connection and used it
/// serially would deadlock and fail on the ceiling rather than report
/// `accepted == 1` for the wrong reason. The server's own high-water mark
/// of open streams is asserted too, because "one connection" and "eight
/// streams at once" are two claims and only the pair is multiplexing.
#[tokio::test(flavor = "multi_thread")]
async fn eight_concurrent_calls_travel_over_one_connection() {
    let server = spawn_server(None);
    server.expect(8);
    let client = shared_client();

    let calls = (0..8).map(|_| call(&client, server.url("/pair")));
    let results = tokio::time::timeout(BOUND, futures_util::future::join_all(calls))
        .await
        .expect("all eight must be in flight at once — the server answers none alone");
    for r in results {
        assert_eq!(r.expect("every call must succeed"), 200);
    }

    assert_eq!(
        server.accepted(),
        1,
        "eight concurrent calls over one shared connection is the whole point"
    );
    assert_eq!(server.served(), 8);
    assert_eq!(
        server.most_open(),
        8,
        "and all eight streams were open at once, which is what makes it \
         multiplexing rather than a very fast queue"
    );
}

/// **The control, and it is this client one call earlier.** The same eight
/// calls without `multiplexed()` take eight connections — the limitation
/// `docs/grpc-yardstick.md` records as L1, measured here so that the test
/// above is a difference rather than an observation.
#[tokio::test(flavor = "multi_thread")]
async fn without_multiplexing_the_same_eight_calls_take_eight_connections() {
    let server = spawn_server(None);
    server.expect(8);
    let client = exclusive_client();

    let calls = (0..8).map(|_| call(&client, server.url("/pair")));
    let results = tokio::time::timeout(BOUND, futures_util::future::join_all(calls))
        .await
        .expect("must not hang");
    for r in results {
        assert_eq!(r.expect("every call must succeed"), 200);
    }

    assert_eq!(
        server.accepted(),
        8,
        "an h2 connection is checked out exclusively without the opt-in"
    );
    assert_eq!(server.most_open(), 8);
}

// ── price 1: a spawner nobody drives ────────────────────────────────────

/// A runtime that is `tokio` in every respect except that its `Spawn`
/// **keeps** what it is given and never polls it.
///
/// Not a contrived type: it is `async_executor::Executor` with no `run`,
/// or a `TokioHandle` for a runtime that has been shut down — the mistake
/// `pool.rs`'s module doc names as the third thing no bound can catch. The
/// point of writing it here is that the failure it produces is different
/// in kind from the reaper's.
type HeldTasks = Arc<Mutex<Vec<Pin<Box<dyn Future<Output = ()> + Send>>>>>;

#[derive(Clone, Default)]
struct HoldsTasks(HeldTasks);

impl hclient_core::unversioned::Timer for HoldsTasks {
    type Instant = <Tokio as hclient_core::unversioned::Timer>::Instant;
    type Sleep = <Tokio as hclient_core::unversioned::Timer>::Sleep;
    fn sleep(&self, d: Duration) -> Self::Sleep {
        Tokio.sleep(d)
    }
    fn now(&self) -> Self::Instant {
        hclient_core::unversioned::Timer::now(&Tokio)
    }
    fn elapsed_since(&self, earlier: Self::Instant) -> Duration {
        Tokio.elapsed_since(earlier)
    }
}

impl TcpConnect for HoldsTasks {
    type Stream = <Tokio as TcpConnect>::Stream;
    const APPLIES: TcpOptsSupport = <Tokio as TcpConnect>::APPLIES;
    fn connect(
        &self,
        addr: SocketAddr,
        opts: &TcpOpts,
    ) -> impl Future<Output = std::io::Result<Self::Stream>> {
        Tokio.connect(addr, opts)
    }
}

impl<F: Future<Output = ()> + Send + 'static> Spawn<F> for HoldsTasks {
    fn spawn(&self, f: F) {
        // Held, not dropped. Dropping it would fail the request with a
        // broken pipe; holding it is the worse of the two failures and the
        // one an un-run executor actually produces.
        self.0.lock().unwrap().push(Box::pin(f));
    }
}

/// **A spawner nobody drives hangs the request, and `first_byte` is the
/// only thing that cuts it.**
///
/// An A/B on one fixture. The arm with no bound is the price stated on
/// `Native::multiplexed` — worse than the reaper's version of the same
/// mistake, which only leaks sockets. The arm with a bound is the whole of
/// the available mitigation, and it is not a default.
///
/// Causal rather than timed in the direction that matters: the driver is
/// *provably* never polled, because the executor under it is a `Vec`.
#[tokio::test(flavor = "multi_thread")]
async fn a_spawner_that_never_runs_hangs_the_request_and_first_byte_is_what_cuts_it() {
    let server = spawn_server(None);
    let rt = HoldsTasks::default();
    let transport =
        Native::new(rt.clone(), FakeTls::default(), SystemDns::new(Tokio)).multiplexed();
    let client = Client::builder(transport).build().unwrap();

    // A. No bound: no verdict at all.
    let hung = tokio::time::timeout(
        Duration::from_millis(500),
        client.get(&format!("https://{}/a", server.addr)).send(),
    )
    .await;
    assert!(
        hung.is_err(),
        "a request on a connection whose driver is never polled has nothing \
         that can resolve it — not an error, no verdict"
    );
    assert_eq!(
        rt.0.lock().unwrap().len(),
        1,
        "and the driver really was handed over and really was never polled"
    );

    // B. The same client, the same server, with `Timeouts::first_byte`.
    let mut req = http::Request::builder()
        .method("GET")
        .uri(format!("https://{}/b", server.addr))
        .body(RequestBody::Empty)
        .unwrap();
    req.extensions_mut().insert(Timeouts {
        resolve: None,
        first_byte: Some(Duration::from_millis(200)),
        ..Timeouts::default()
    });
    let err = tokio::time::timeout(BOUND, client.execute(req))
        .await
        .expect("the bound must turn the hang into an answer")
        .expect_err("nothing can answer this request");
    assert!(
        matches!(
            err.kind(),
            hclient_core::ErrorKind::Timeout(hclient_core::Phase::FirstByte)
        ),
        "the one bound that reaches this failure must be the one that \
         reports it: {err:?}"
    );
}

// ── price 2: the peer's stream limit ────────────────────────────────────

/// **Beyond the peer's `MAX_CONCURRENT_STREAMS` requests queue, and no
/// second connection is opened.**
///
/// The decision, measured rather than argued. h2 accepts every request and
/// opens the streams as capacity frees up, so six concurrent calls against
/// a server allowing two run in three waves on one socket; without the
/// opt-in the fifth and sixth would each get a connection of their own and
/// finish in one wave.
///
/// The assertion is the server's high-water mark, not a clock: `most_open
/// == 2` is what "the limit was respected" means, and `accepted == 1` is
/// what "and no second connection was opened for the overflow" means.
/// Timing is deliberately not asserted — `docs/h2-multiplexing.md`'s
/// 203/405/607 ms is a measurement of this machine and three timing-based
/// assertions in this workspace have already turned out to be flakes.
#[tokio::test(flavor = "multi_thread")]
async fn beyond_the_peers_stream_limit_requests_queue_on_one_connection() {
    let server = spawn_server(Some(2));
    let client = shared_client();

    let calls = (0..6).map(|_| call(&client, server.url("/queue")));
    let results = tokio::time::timeout(BOUND, futures_util::future::join_all(calls))
        .await
        .expect("queued requests must still finish");
    for r in results {
        assert_eq!(r.expect("every call must succeed"), 200);
    }

    assert_eq!(
        server.accepted(),
        1,
        "a full connection does not get a second one — the policy is that \
         requests queue, because `poll_ready` cannot be asked whether a \
         connection is full and no honest threshold is measurable here"
    );
    assert_eq!(server.served(), 6);
    assert_eq!(
        server.most_open(),
        2,
        "the peer's limit was respected: six calls, never more than two \
         streams open at once"
    );
}

// ── price 3: an `Rc` hook cannot multiplex ──────────────────────────────
//
// The refusal is a compile error and lives where the compiler can be asked
// about it: `Native::multiplexed`'s own `compile_fail` doctests, in
// `src/lib.rs`. A runtime test cannot observe a program that does not
// build.

// ── the ordering rules ──────────────────────────────────────────────────

/// Counts what it is told about, and nothing else.
#[derive(Clone, Default)]
struct Counting(Arc<Mutex<Vec<String>>>);

impl Hooks for Counting {
    fn on(&self, event: Event<'_>) {
        let line = match event {
            Event::Connected(_) => "connected".to_owned(),
            Event::Reused(r) => format!("reused:{:?}", r.version),
            Event::Closed(c) => match c.reason {
                CloseReason::Ended => "closed:ended".to_owned(),
                CloseReason::Stale => "closed:stale".to_owned(),
                CloseReason::Failed(_) => "closed:failed".to_owned(),
            },
            _ => return,
        };
        self.0.lock().unwrap().push(line);
    }
}

impl Counting {
    fn lines(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

/// **`.hooks(..)` must come before `.multiplexed()`, and the pair is what
/// says so.**
///
/// The spawner is a `fn(&R, H2Driver<_, H>)` — it names the hook, because
/// the driver carries it — so `hooks()` cannot carry that pointer across a
/// change of `H`. Either half alone reads as an accident: a client that
/// never multiplexed passes the second, and one that always did passes the
/// first.
#[tokio::test(flavor = "multi_thread")]
async fn hooks_before_multiplexed_shares_and_hooks_after_it_does_not() {
    for (order_shares, label) in [
        (true, "hooks then multiplexed"),
        (false, "multiplexed then hooks"),
    ] {
        let server = spawn_server(None);
        server.expect(2);
        let hook = Counting::default();
        let base = Native::new(Tokio, FakeTls::default(), SystemDns::new(Tokio));
        let client = if order_shares {
            Client::builder(base.hooks(hook.clone()).multiplexed())
                .build()
                .unwrap()
        } else {
            Client::builder(base.multiplexed().hooks(hook.clone()))
                .build()
                .unwrap()
        };

        let calls = (0..2).map(|_| call(&client, server.url("/pair")));
        let results = tokio::time::timeout(BOUND, futures_util::future::join_all(calls))
            .await
            .expect("must not hang");
        for r in results {
            assert_eq!(r.expect("every call must succeed"), 200);
        }

        assert_eq!(
            server.accepted(),
            if order_shares { 1 } else { 2 },
            "{label}: the spawner names the hook, so a later `hooks()` \
             cannot carry it — see `Native::hooks`"
        );
        assert!(
            hook.lines().iter().any(|l| l == "connected"),
            "{label}: and the hook is installed either way"
        );
    }
}

/// **There is nowhere to share a connection without a pool, whichever
/// order the two are written in.**
///
/// `Native::with_reaper`'s third bullet, one seam over — except that this
/// one is order-independent rather than last-call-wins, because the shared
/// path is entered only where a pool exists rather than remembered as a
/// flag.
#[tokio::test(flavor = "multi_thread")]
async fn without_a_pool_there_is_nothing_to_share_in_either_order() {
    for (label, build) in [
        ("multiplexed then without_pool", 0),
        ("without_pool then multiplexed", 1),
    ] {
        let server = spawn_server(None);
        server.expect(2);
        let base = Native::new(Tokio, FakeTls::default(), SystemDns::new(Tokio));
        let transport = if build == 0 {
            base.multiplexed().without_pool()
        } else {
            base.without_pool().multiplexed()
        };
        let client = Client::builder(transport).build().unwrap();

        let calls = (0..2).map(|_| call(&client, server.url("/pair")));
        let results = tokio::time::timeout(BOUND, futures_util::future::join_all(calls))
            .await
            .expect("must not hang");
        for r in results {
            assert_eq!(r.expect("every call must succeed"), 200);
        }

        assert_eq!(
            server.accepted(),
            2,
            "{label}: reuse off means no shared entry to borrow"
        );
    }
}

// ── what the driver makes reportable ────────────────────────────────────

/// **A shared connection's end is reported, and nothing else in this crate
/// could report it.**
///
/// `established::Inner::H2`'s own doc records the gap: the h2 response body
/// emits no `Closed`, because the end of an h2 connection can arrive in
/// three places where HTTP/1's arrives in two. A shared connection dies
/// inside its **driver**, which is one place, so the driver reports it —
/// which is the whole reason it carries `H`.
///
/// Causal: the server sends `GOAWAY` after answering, and the test waits
/// for the server's own connection task to have ended before looking.
#[tokio::test(flavor = "multi_thread")]
async fn the_driver_reports_the_end_of_a_shared_connection() {
    let server = spawn_server(None);
    let hook = Counting::default();
    let transport = Native::new(Tokio, FakeTls::default(), SystemDns::new(Tokio))
        .hooks(hook.clone())
        .multiplexed();
    let client = Client::builder(transport).build().unwrap();

    assert_eq!(
        tokio::time::timeout(BOUND, call(&client, server.url("/goaway")))
            .await
            .expect("must not hang")
            .expect("the call the GOAWAY names must still be answered"),
        200
    );
    assert!(
        server.wait_for_closed(1, BOUND).await,
        "the server's connection task must have finished"
    );

    let deadline = std::time::Instant::now() + BOUND;
    while std::time::Instant::now() < deadline {
        if hook.lines().iter().any(|l| l == "closed:ended") {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "the driver must report the end of the connection it was driving: {:?}",
        hook.lines()
    );
}

/// **A shared entry the peer has finished with is removed, not borrowed
/// again.**
///
/// The other half of borrowing rather than taking, and it is not optional:
/// a clone that `is_reusable` rejects leaves the entry it came from in the
/// pool, so a checkout that only dropped the clone would borrow the same
/// dead connection for ever — this test would not fail, it would **hang**,
/// which is why it has a ceiling.
///
/// Causal: the server's connection task must have ended before the second
/// call is made, so that call provably faces a dead pool entry rather than
/// racing the `GOAWAY` across the wire.
#[tokio::test(flavor = "multi_thread")]
async fn a_dead_shared_entry_is_removed_rather_than_borrowed_for_ever() {
    let server = spawn_server(None);
    let hook = Counting::default();
    let transport = Native::new(Tokio, FakeTls::default(), SystemDns::new(Tokio))
        .hooks(hook.clone())
        .multiplexed();
    let client = Client::builder(transport).build().unwrap();

    assert_eq!(
        tokio::time::timeout(BOUND, call(&client, server.url("/goaway")))
            .await
            .expect("must not hang")
            .expect("the call the GOAWAY names must still be answered"),
        200
    );
    assert!(
        server.wait_for_closed(1, BOUND).await,
        "the server's connection task must have finished — that is what \
         makes the next call face a dead pool entry rather than race one"
    );

    assert_eq!(
        tokio::time::timeout(BOUND, call(&client, server.url("/next")))
            .await
            .expect("a dead shared entry must not be borrowed for ever")
            .expect("the call after a GOAWAY must go elsewhere, not fail"),
        200
    );
    assert_eq!(server.accepted(), 2, "elsewhere is a second connection");
    assert!(
        hook.lines().iter().any(|l| l == "closed:stale"),
        "and checkout said why it walked past it — `Stale` is the same \
         reason an exclusive entry the peer closed while idle gets, and a \
         checkout that never rejected the clone would report nothing: {:?}",
        hook.lines()
    );
}

// ── the pool's own bookkeeping ──────────────────────────────────────────

/// The idle timeout these two tests are an A/B on. Short enough to be
/// worth waiting for and far longer than a loopback exchange.
const SHORT_IDLE: Duration = Duration::from_millis(300);

/// **A shared connection under continuous load outlives its idle
/// timeout.**
///
/// `PoolConfig::idle_timeout` is measured *"from when the connection was
/// last handed out"*, and a borrow is a hand-out — so the deadline is
/// restamped every time a request takes a clone. Without that, a shared
/// entry is never checked in again (there is nothing to check in), so its
/// deadline would be frozen at the first request's and the connection
/// would be dropped one idle timeout after the traffic *started* rather
/// than after it stopped.
///
/// Six calls 100 ms apart under a 300 ms timeout: one connection.
#[tokio::test(flavor = "multi_thread")]
async fn a_shared_connection_that_keeps_being_used_is_not_dropped_for_age() {
    let server = spawn_server(None);
    let transport = Native::new(Tokio, FakeTls::default(), SystemDns::new(Tokio))
        .pool(PoolConfig {
            idle_timeout: SHORT_IDLE,
            ..PoolConfig::default()
        })
        .multiplexed();
    let client = Client::builder(transport).build().unwrap();

    for _ in 0..6 {
        assert_eq!(
            tokio::time::timeout(BOUND, call(&client, server.url("/tick")))
                .await
                .expect("must not hang")
                .expect("every call must succeed"),
            200
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    assert_eq!(
        server.accepted(),
        1,
        "600 ms of traffic under a 300 ms idle timeout is one connection, \
         because every borrow restamps the deadline"
    );
}

/// The control: the same client, the same timeout, one gap longer than it.
///
/// Without this, the test above would also pass for a pool that ignored
/// `idle_timeout` on shared entries altogether.
#[tokio::test(flavor = "multi_thread")]
async fn a_shared_connection_idle_past_its_deadline_is_not_handed_out_again() {
    let server = spawn_server(None);
    let transport = Native::new(Tokio, FakeTls::default(), SystemDns::new(Tokio))
        .pool(PoolConfig {
            idle_timeout: SHORT_IDLE,
            ..PoolConfig::default()
        })
        .multiplexed();
    let client = Client::builder(transport).build().unwrap();

    assert_eq!(
        tokio::time::timeout(BOUND, call(&client, server.url("/one")))
            .await
            .expect("must not hang")
            .expect("must succeed"),
        200
    );
    tokio::time::sleep(SHORT_IDLE + Duration::from_millis(200)).await;
    assert_eq!(
        tokio::time::timeout(BOUND, call(&client, server.url("/two")))
            .await
            .expect("must not hang")
            .expect("must succeed"),
        200
    );

    assert_eq!(
        server.accepted(),
        2,
        "past its deadline a shared entry is dropped as it is walked past, \
         exactly as an exclusive one is"
    );
}

// ── the single flight this needed ───────────────────────────────────────

/// **A connect that fails does not strand the requests waiting for it.**
///
/// Single-flight is what makes a cold burst one connection, and its
/// failure mode is the one worth pinning: the mark is released by
/// `Connecting`'s `Drop`, so a connect that fails wakes the herd instead of
/// holding it. Against a port nothing is listening on, all eight calls must
/// come back as errors rather than hanging.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_shared_connect_releases_everyone_waiting_for_it() {
    // A port with nothing behind it: bound and dropped, so the connect is
    // refused rather than black-holed.
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap()
    };
    let client = shared_client();

    let client = &client;
    let calls = (0..8).map(move |_| {
        let url = format!("https://{dead}/x");
        async move { client.get(&url).send().await }
    });
    let results = tokio::time::timeout(BOUND, futures_util::future::join_all(calls))
        .await
        .expect("a failed connect must not strand the requests waiting on it");
    for r in results {
        assert!(r.is_err(), "there is nothing there to answer");
    }
}

// ── the budget the wait spends ──────────────────────────────────────────

/// A `TlsConnect` whose handshake takes time, and whose **first** handshake
/// fails.
///
/// The two together are what make the arithmetic below observable at all:
/// the request that holds the connect mark has to spend a measurable part
/// of `Timeouts::connect` and then **fail**, so that the request waiting
/// behind it still needs a connect of its own and has only what is left.
#[derive(Clone)]
struct SlowTls {
    id: TlsConfigId,
    handshakes: Arc<AtomicUsize>,
}

impl Default for SlowTls {
    fn default() -> Self {
        Self {
            id: TlsConfigId::new_unique(),
            handshakes: Arc::new(AtomicUsize::new(0)),
        }
    }
}

/// What the first handshake spends before failing, and what every later one
/// spends before succeeding. Both are this stub's own sleeps rather than a
/// property of the network, so the arms below are causal in everything
/// except the sleeps they name.
const FIRST_TLS: Duration = Duration::from_millis(300);
const LATER_TLS: Duration = Duration::from_millis(150);

impl TlsIdentity for SlowTls {
    fn config_id(&self) -> TlsConfigId {
        self.id
    }
}

impl TlsConnect for SlowTls {
    type Stream<S>
        = S
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin;

    fn reports_alpn(&self) -> bool {
        true
    }

    async fn connect<S>(
        &self,
        io: S,
        _req: TlsRequest<'_>,
    ) -> Result<(S, TlsInfo), hclient_core::Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin,
    {
        let nth = self.handshakes.fetch_add(1, Ordering::SeqCst);
        if nth == 0 {
            tokio::time::sleep(FIRST_TLS).await;
            return Err(hclient_core::Error::new(
                hclient_core::ErrorKind::Connect,
                std::io::Error::other("the first handshake fails, slowly"),
            ));
        }
        tokio::time::sleep(LATER_TLS).await;
        Ok((
            io,
            TlsInfo {
                alpn: Some(b"h2".to_vec()),
                ..Default::default()
            },
        ))
    }
}

fn slow_client(tls: SlowTls) -> Client<Native<Tokio, SlowTls, SystemDns<Tokio>>> {
    Client::builder(Native::new(Tokio, tls, SystemDns::new(Tokio)).multiplexed())
        .build()
        .unwrap()
}

fn bounded(url: &str, connect: Duration) -> http::Request<RequestBody> {
    let mut req = http::Request::builder()
        .method("GET")
        .uri(url)
        .body(RequestBody::Empty)
        .unwrap();
    req.extensions_mut().insert(Timeouts {
        resolve: None,
        connect: Some(connect),
        ..Timeouts::default()
    });
    req
}

/// **`Timeouts::connect` is one bound for one request, and waiting for
/// somebody else's connect spends it.**
///
/// The same arithmetic `hclient-select`'s h3 fallback does one layer up and
/// `Client`'s `425` replay does one layer down: a caller who set
/// `connect: Some(C)` must not be made to wait `C` for a neighbour and then
/// be given a fresh `C` for a connect of their own.
///
/// **An A/B, and neither arm asserts on a duration.** Same fixture, same
/// two requests, same failing-then-slow TLS; what differs is the bound:
///
/// - `FIRST_TLS + LATER_TLS` and more is enough for both, so the waiter's
///   own connect fits in what is left and it gets its `200`;
/// - a bound with room for the first handshake and 50 ms over — less than
///   `LATER_TLS` — leaves the waiter with nothing to connect with, and the
///   honest answer is its own `Timeout(Connect)`.
///
/// A wait handed a fresh copy of the bound passes the first arm and fails
/// the second: 350 ms is more than the 150 ms its own handshake needs, so
/// it would connect and succeed where the caller's bound had no room for
/// it. The margins are the reason those two numbers are what they are —
/// 50 ms against 150 ms one way and 350 ms against 150 ms the other.
#[tokio::test(flavor = "multi_thread")]
async fn waiting_for_a_shared_connect_spends_the_callers_connect_bound() {
    // Arm A — room for both handshakes: the waiter succeeds.
    {
        let server = spawn_server(None);
        let tls = SlowTls::default();
        let client = slow_client(tls.clone());
        let url = server.url("/a");
        let (first, second) = tokio::time::timeout(
            BOUND,
            futures_util::future::join(
                client.execute(bounded(
                    &url,
                    FIRST_TLS + LATER_TLS + Duration::from_millis(300),
                )),
                client.execute(bounded(
                    &url,
                    FIRST_TLS + LATER_TLS + Duration::from_millis(300),
                )),
            ),
        )
        .await
        .expect("must not hang");
        assert!(
            first.is_err() != second.is_err(),
            "exactly one of the two met the failing handshake; the other \
             waited for it and then connected for itself"
        );
        assert!(tls.handshakes.load(Ordering::SeqCst) >= 2);
    }

    // Arm B — room for the first handshake and not for a second: the
    // waiter has nothing left.
    {
        let server = spawn_server(None);
        let tls = SlowTls::default();
        let client = slow_client(tls.clone());
        let url = server.url("/b");
        let bound = FIRST_TLS + Duration::from_millis(50);
        assert!(
            bound - FIRST_TLS < LATER_TLS && bound > LATER_TLS,
            "the arm is only a discriminator between these two: what is \
             LEFT of the bound must be too little for a second handshake, \
             and the WHOLE bound must be enough for one"
        );
        let (first, second) = tokio::time::timeout(
            BOUND,
            futures_util::future::join(
                client.execute(bounded(&url, bound)),
                client.execute(bounded(&url, bound)),
            ),
        )
        .await
        .expect("must not hang");
        for r in [first, second] {
            let e = r.expect_err(
                "the caller's connect bound has room for one handshake, and \
                 the first one failed",
            );
            assert!(
                matches!(
                    e.kind(),
                    hclient_core::ErrorKind::Connect
                        | hclient_core::ErrorKind::Timeout(hclient_core::Phase::Connect)
                ),
                "the failure is the connect's, not something further on: {e:?}"
            );
        }
    }
}

// ── request bodies on a shared connection ───────────────────────────────

/// **A request body crosses a shared connection in frames the server
/// counts.**
///
/// `exchange_shared` runs the same `Pump` the exclusive path does, and it
/// is the one part of that path with no `Connection` beside it to poll: the
/// DATA frames the pump queues reach the socket from inside the **driver**,
/// woken by the queueing itself. More than one flow-control window's worth,
/// so the capacity loop is exercised rather than skipped.
///
/// It is also the reason this test exists rather than being assumed: every
/// other test in this file sends a `GET`, so without it the pump on the
/// shared path would be entirely unexercised.
#[tokio::test(flavor = "multi_thread")]
async fn a_request_body_crosses_a_shared_connection_whole() {
    let server = spawn_server(None);
    let client = shared_client();

    let payload = vec![b'x'; 128 * 1024];
    let resp = tokio::time::timeout(
        BOUND,
        client
            .post(&server.url("/upload"))
            .body(RequestBody::Full(payload.clone().into()))
            .send(),
    )
    .await
    .expect("must not hang")
    .expect("request must succeed");
    assert_eq!(resp.status(), 200);
    resp.collect().await.expect("body");

    assert_eq!(
        server.body_bytes(),
        payload.len(),
        "every byte of the body must have crossed, which needs more than one \
         DATA frame and therefore a working capacity loop on a connection \
         this exchange does not poll"
    );
    assert_eq!(server.accepted(), 1);
}

/// A body that hands out what it is given and nothing else, counting the
/// frames taken off it.
struct Feed {
    rx: tokio::sync::mpsc::UnboundedReceiver<Bytes>,
}

impl http_body::Body for Feed {
    type Data = Bytes;
    type Error = hclient_core::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
        match self.rx.poll_recv(cx) {
            std::task::Poll::Ready(Some(b)) => {
                std::task::Poll::Ready(Some(Ok(http_body::Frame::data(b))))
            }
            std::task::Poll::Ready(None) => std::task::Poll::Ready(None),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

/// **A shared connection is full duplex, and the proof is causal rather
/// than timed.**
///
/// The caller's body holds one chunk; the second is put into its channel
/// only **after** `send()` has returned a head. A transport that wrote the
/// whole request before reading the response would be waiting for a chunk
/// that is waiting for it, at any speed. `/duplex` answers before it reads
/// a byte, which is what makes the head available that early.
///
/// What this pins on the shared path specifically is the duplex line — a
/// pump that cannot proceed must not stop the response from arriving — and
/// the leftover pump moving into `H2Body`, which is what finishes the
/// upload once the response body is read. Neither is shared with
/// `exchange`: `exchange_shared` is its own loop, because the connection
/// poll that loop is built around belongs to the driver here.
#[tokio::test(flavor = "multi_thread")]
async fn a_shared_connection_carries_a_duplex_exchange() {
    const A: usize = 4096;
    const B: usize = 8192;

    let server = spawn_server(None);
    let client = shared_client();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tx.send(Bytes::from(vec![b'a'; A])).unwrap();

    let resp = tokio::time::timeout(
        BOUND,
        client
            .post(&server.url("/duplex"))
            .body(RequestBody::Streaming(Box::new(Feed { rx })))
            .send(),
    )
    .await
    .expect(
        "the head is on the wire and the second chunk is not: a transport \
         that waits for the body before reading the head deadlocks here",
    )
    .expect("request must succeed");
    assert_eq!(resp.status(), 200);

    // Only now does the rest of the request body exist.
    tx.send(Bytes::from(vec![b'b'; B])).unwrap();
    drop(tx);

    // Collecting the response is what drives the remaining upload: the pump
    // moved into the body, and nothing else polls it.
    let text = tokio::time::timeout(BOUND, resp.collect())
        .await
        .expect("the rest of the upload rides on the response body being read")
        .expect("the body must arrive")
        .text()
        .unwrap();
    assert_eq!(
        text,
        format!("{} bytes", A + B),
        "the count is the server's: both chunks crossed"
    );
    assert_eq!(server.accepted(), 1);
}

/// **A shared connection is in the bucket a `RequireVersion(HTTP_2)`
/// request looks in.**
///
/// `PoolKey` needs nothing new for sharing — a `Native` either shares its
/// h2 connections or does not, decided once at construction — but the
/// entry still has to go into the **right** bucket, and this is the one
/// thing that notices. An ordinary request would not: `pooled_candidates`
/// offers `[H2, Http11]` and `established::exchange` dispatches on the
/// connection rather than on the key, so a shared entry filed under
/// `Http11` is found and spoken to correctly anyway. A **demand** is
/// different: `protocol_admissible` skips the `Http11` bucket outright, so
/// a misfiled entry costs a connection every time.
///
/// The `Reused` event is the second half and the reason it is asserted
/// here rather than taken on trust: its `version` is read off the bucket
/// the connection came out of, so a misfiled shared connection would
/// report a browser's h2 traffic as HTTP/1.1 — the same wrong-answer shape
/// `Head::version` became an `Option` for.
#[tokio::test(flavor = "multi_thread")]
async fn a_demand_for_http2_is_served_by_the_shared_connection() {
    let server = spawn_server(None);
    let hook = Counting::default();
    let transport = Native::new(Tokio, FakeTls::default(), SystemDns::new(Tokio))
        .hooks(hook.clone())
        .multiplexed();
    let client = Client::builder(transport).build().unwrap();

    // Warm it: one ordinary call makes the shared connection.
    assert_eq!(
        tokio::time::timeout(BOUND, call(&client, server.url("/warm")))
            .await
            .expect("must not hang")
            .expect("must succeed"),
        200
    );

    let mut req = http::Request::builder()
        .method("GET")
        .uri(server.url("/demand"))
        .body(RequestBody::Empty)
        .unwrap();
    req.extensions_mut()
        .insert(hclient_core::RequireVersion(http::Version::HTTP_2));
    let resp = tokio::time::timeout(BOUND, client.execute(req))
        .await
        .expect("must not hang")
        .expect("h2 was negotiated and shared, so the demand is satisfied");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.version(), http::Version::HTTP_2);
    drop(resp);

    assert_eq!(
        server.accepted(),
        1,
        "the demanding request found the shared connection, which it can \
         only do if the entry is in the HTTP/2 bucket"
    );
    assert_eq!(
        hook.lines(),
        vec!["connected", "reused:HTTP/2.0"],
        "and the reuse names the protocol the connection actually speaks"
    );
}

/// **A `1xx` on a SHARED connection reaches the hook too.**
///
/// The third code path — `exchange_shared`, which `Native::multiplexed`
/// turns on — and it was wired without a fixture reaching it: the
/// mutation that removes its poll survived the whole suite until this
/// test existed. A capability saying `informational_1xx` while a
/// multiplexed transport reported none would be a capability lying, which
/// is what `.watching_1xx()` promises not to be.
#[tokio::test(flavor = "multi_thread")]
async fn a_1xx_on_a_shared_connection_reaches_the_hook() {
    #[derive(Debug, Clone, Default)]
    struct Hints(std::sync::Arc<std::sync::Mutex<Vec<u16>>>);

    impl hclient_core::unversioned::Hooks for Hints {
        fn on(&self, event: hclient_core::unversioned::Event<'_>) {
            if let hclient_core::unversioned::Event::Informational(e) = event {
                self.0.lock().unwrap().push(e.status.as_u16());
            }
        }
    }

    let server = spawn_server(None);
    server.expect(2);
    let hints = Hints::default();
    // `.hooks(..)` first, then the two opt-ins — the order both of them
    // require, and the reason is the same pointer-names-`H` rule.
    let client = Client::builder(
        Native::new(Tokio, FakeTls::default(), SystemDns::new(Tokio))
            .hooks(hints.clone())
            .watching_1xx()
            .multiplexed(),
    )
    .build()
    .expect("build");

    let calls = (0..2).map(|_| call(&client, server.url("/hints")));
    let results = tokio::time::timeout(BOUND, futures_util::future::join_all(calls))
        .await
        .expect("must not hang");
    for r in results {
        assert_eq!(r.expect("every call must succeed"), 200);
    }
    assert_eq!(
        server.accepted(),
        1,
        "one connection, or this is not the shared path"
    );
    assert_eq!(
        hints.0.lock().unwrap().clone(),
        vec![103, 103],
        "one interim head per request, on the connection they share"
    );
}

// ── the stream limit before the peer has stated one ─────────────────────

/// Holds the first write for `delay`, so a server's `SETTINGS` frame
/// arrives after the client has had to decide how many streams to open.
///
/// The first write and no other: what is being delayed is the frame that
/// carries `MAX_CONCURRENT_STREAMS`, and `h2::server::handshake` writes it
/// before anything else. Delaying every write would also delay the
/// `RST_STREAM`s this test is about seeing or not seeing.
struct DelayFirstWrite<S> {
    inner: S,
    sleep: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl<S> DelayFirstWrite<S> {
    fn new(inner: S, delay: Duration) -> Self {
        Self {
            inner,
            sleep: Some(Box::pin(tokio::time::sleep(delay))),
        }
    }

    /// `Pending` until the delay has elapsed, then never again.
    fn gate(&mut self, cx: &mut std::task::Context<'_>) -> Poll<()> {
        let Some(sleep) = self.sleep.as_mut() else {
            return Poll::Ready(());
        };
        std::task::ready!(sleep.as_mut().poll(cx));
        self.sleep = None;
        Poll::Ready(())
    }
}

impl<S: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for DelayFirstWrite<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for DelayFirstWrite<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        std::task::ready!(self.gate(cx));
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        std::task::ready!(self.gate(cx));
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// **A burst against a connection whose peer has not stated a limit opens
/// one stream, not six.**
///
/// This is the deterministic form of a flake: under load, four full-suite
/// runs in forty had
/// `beyond_the_peers_stream_limit_requests_queue_on_one_connection` fail
/// with `RST_STREAM(REFUSED_STREAM)` on stream 5 — the client had opened
/// six streams against a server allowing two, because `h2`'s
/// `initial_max_send_streams` is `usize::MAX` and the burst beat the
/// `SETTINGS` frame. `send_request` consumes the head, so that failure
/// cannot be retried by this client even though RFC 9113 §8.7 says the
/// request was never processed.
///
/// Here the frame is held back on purpose, so the client's decision is
/// forced rather than raced: the assertion is the server's own high-water
/// mark, which is `2` — the limit it advertised — and never `6`.
#[tokio::test(flavor = "multi_thread")]
async fn a_burst_before_the_peers_settings_does_not_open_streams_on_a_guess() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let shared = Arc::new(Shared::default());
    let shared_t = Arc::clone(&shared);

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            while let Ok((tcp, _)) = listener.accept().await {
                shared_t.accepted.fetch_add(1, Ordering::SeqCst);
                let shared = Arc::clone(&shared_t);
                tokio::spawn(async move {
                    // 300 ms is far longer than a loopback round trip, so
                    // the client cannot have seen the frame by accident —
                    // and the assertion is on a count rather than on the
                    // duration, so a slow machine changes nothing.
                    let io = DelayFirstWrite::new(tcp, Duration::from_millis(300));
                    let _ = serve_io(io, Arc::clone(&shared), Some(2)).await;
                    shared.closed.fetch_add(1, Ordering::SeqCst);
                });
            }
        });
    });

    let fixture = Fixture { addr, shared };
    let client = shared_client();
    let calls = (0..6).map(|_| call(&client, fixture.url("/queue")));
    let results = tokio::time::timeout(BOUND, futures_util::future::join_all(calls))
        .await
        .expect("the queued requests must still finish");
    for r in results {
        assert_eq!(
            r.expect("no call may be refused: the client must not open a stream it has no evidence it may"),
            200
        );
    }
    assert_eq!(fixture.served(), 6);
    assert_eq!(
        fixture.most_open(),
        2,
        "the peer's advertised limit, never the six the client could have \
         guessed at before the frame carrying it arrived"
    );
}
