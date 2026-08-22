//! What an HTTP/3 client with no hook pays for the hook's existence,
//! measured rather than asserted.
//!
//! # The measurement, and why it is a clock rather than a stopwatch
//!
//! "Zero cost" here means one specific thing: with `NoHooks`, the transport
//! does not read a clock, does not take a connection id, does not allocate
//! the per-connection state, and has no branch left that a monomorphised
//! build cannot delete. The clock reads are directly countable and they are
//! the ones that would cost a caller something — `Timer::now` is a
//! `clock_gettime` on the request path.
//!
//! So the runtime under the transport is [`Counting`], a `TokioHandle` that
//! delegates everything and keeps a tally of how often it was asked what
//! time it is. Every other figure would be a proxy: a wall-clock benchmark
//! measures the network, and a binary size measures the linker.
//!
//! # It is exact, and deliberately so
//!
//! A `<=` would pass for a build that read the clock four times when it
//! should read it none. The numbers below are equalities.
//!
//! **Both numbers are zero for `NoHooks`, and that is stronger than
//! `hclient-native`'s.** There the fresh-connection figure is `1`, because
//! `connect::drive` stamps the start of the Happy Eyeballs race and RFC
//! 8305's pacing is measured from it. This transport does not race address
//! families — QUIC's connect is not a TCP SYN race, and racing two
//! handshakes would mean two handshakes' worth of crypto for one request —
//! so before this feature there was no `Timer::now` call in `hclient-h3` at
//! all. `grep -rn '\.now()' crates/hclient-h3/src` finds only `crate::hooks`
//! today, and quinn's own timers go through `Timer::sleep` and
//! `std::time::Instant` (see `hclient_quinn`'s `until`), not through this
//! clock.
//!
//! `elapsed_since` is counted too, which `hclient-native`'s equivalent file
//! deliberately does not do — there it is called once per iteration of a
//! scheduler loop whose iteration count no test may pin. Here every call is
//! at a fixed point, so it is deterministic and worth pinning: it is the
//! second half of "the timings cost nothing when nobody is watching".
//!
//! If a legitimate clock read is ever added to this path, this file fails
//! and the number has to be changed **on purpose**. That is the point of the
//! equality: a tripwire that has to be looked at.
#![cfg(all(feature = "http3", not(target_family = "wasm")))]

#[path = "h3_server.rs"]
mod server;

use hclient_core::RequestBody;
use hclient_core::unversioned::{Event, Hooks, NoHooks, Transport};
use hclient_dns::IpLiteralOnly;
use hclient_native::{H3, QuinnTask};
use hclient_rt::{Spawn, Timer, UdpAdoptStd, UdpBind};
use hclient_rt_tokio::TokioHandle;
use http_body_util::BodyExt;
use server::Behaviour;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// `TokioHandle`, with a note kept of every clock read.
///
/// Everything else is delegated verbatim, including the UDP socket type —
/// so quinn is driven by exactly the runtime the other tests in this suite
/// use, and the only difference is that this one counts.
#[derive(Clone)]
struct Counting {
    inner: TokioHandle,
    nows: Arc<AtomicUsize>,
    elapsed: Arc<AtomicUsize>,
}

impl Counting {
    fn new() -> Self {
        Self {
            inner: TokioHandle::current().expect("inside #[tokio::test]"),
            nows: Arc::new(AtomicUsize::new(0)),
            elapsed: Arc::new(AtomicUsize::new(0)),
        }
    }
    fn reads(&self) -> (usize, usize) {
        (
            self.nows.load(Ordering::SeqCst),
            self.elapsed.load(Ordering::SeqCst),
        )
    }
    fn reset(&self) {
        self.nows.store(0, Ordering::SeqCst);
        self.elapsed.store(0, Ordering::SeqCst);
    }
}

impl Timer for Counting {
    type Instant = <TokioHandle as Timer>::Instant;
    type Sleep = <TokioHandle as Timer>::Sleep;

    /// Not counted, and it must not be: quinn arms a timer on essentially
    /// every packet, so the count would be a fact about the network.
    fn sleep(&self, d: Duration) -> Self::Sleep {
        self.inner.sleep(d)
    }

    fn now(&self) -> Self::Instant {
        self.nows.fetch_add(1, Ordering::SeqCst);
        self.inner.now()
    }

    fn elapsed_since(&self, earlier: Self::Instant) -> Duration {
        self.elapsed.fetch_add(1, Ordering::SeqCst);
        self.inner.elapsed_since(earlier)
    }
}

impl UdpBind for Counting {
    type Socket = <TokioHandle as UdpBind>::Socket;
    fn bind(&self, local: std::net::SocketAddr) -> std::io::Result<Self::Socket> {
        self.inner.bind(local)
    }
}

impl UdpAdoptStd for Counting {
    fn adopt(&self, s: std::net::UdpSocket) -> std::io::Result<Self::Socket> {
        self.inner.adopt(s)
    }
}

impl Spawn<QuinnTask> for Counting {
    fn spawn(&self, f: QuinnTask) {
        self.inner.spawn(f);
    }
}

/// A hook that does nothing but exist — the *presence* of a watcher is what
/// this file measures, not what a watcher does with the events.
#[derive(Clone, Copy, Default)]
struct Watching;
impl Hooks for Watching {
    fn on(&self, _event: Event<'_>) {}
}

async fn get(t: &impl Transport, addr: std::net::SocketAddr) {
    let req = http::Request::builder()
        .uri(format!("https://{addr}/"))
        .body(RequestBody::Empty)
        .unwrap();
    let resp = t.execute(req).await.map_err(|_| ()).expect("request");
    assert_eq!(resp.status(), 200);
    let _ = resp
        .into_body()
        .collect()
        .await
        .map_err(|_| ())
        .expect("body");
}

/// The whole claim, in two numbers a caller can check.
///
/// `IpLiteralOnly` is what makes those numbers this crate's: a resolver with
/// a clock of its own would read the counter for reasons that are not ours.
#[tokio::test(flavor = "multi_thread")]
async fn a_client_with_no_hook_reads_no_clock_at_all() {
    let s = server::start(Behaviour::Echo);
    let clock = Counting::new();
    let quiet = H3::new(
        clock.clone(),
        server::client_tls(&s.cert_der),
        IpLiteralOnly,
    )
    .expect("H3::new does no I/O");
    clock.reset();

    get(&quiet, s.addr).await;
    let fresh = clock.reads();
    clock.reset();
    get(&quiet, s.addr).await;
    let pooled = clock.reads();

    assert_eq!(
        s.accepted(),
        1,
        "the second request must be the pooled one this file is about"
    );
    assert_eq!(
        fresh,
        (0, 0),
        "a fresh connection under NoHooks must read no clock: this transport \
         does not race address families, so before the hooks there was no \
         `Timer::now` call in it at all"
    );
    assert_eq!(
        pooled,
        (0, 0),
        "and a pooled request even less so — it does not connect"
    );
}

/// The other side of the same measurement, and the reason the first is not
/// vacuous: with a hook, the same two requests do read the clock, and the
/// counts say exactly which reads the hook is responsible for.
///
/// Without this pair the first test would pass against a transport whose
/// timing code was simply broken — never reading a clock for anybody.
#[tokio::test(flavor = "multi_thread")]
async fn the_same_two_requests_with_a_hook_do_read_it() {
    let s = server::start(Behaviour::Echo);
    let clock = Counting::new();
    let watched = H3::new(
        clock.clone(),
        server::client_tls(&s.cert_der),
        IpLiteralOnly,
    )
    .expect("H3::new does no I/O")
    .hooks(Watching);
    clock.reset();

    get(&watched, s.addr).await;
    let fresh = clock.reads();
    clock.reset();
    get(&watched, s.addr).await;
    let pooled = clock.reads();

    assert_eq!(s.accepted(), 1, "still one connection");
    assert_eq!(
        fresh,
        (2, 4),
        "a fresh connection takes two marks — the request's start \
         (`Head::elapsed` and `ConnectTiming::total`) and the QUIC attempt's \
         launch (`tcp`) — and reads four intervals off them: `dns`, `tcp`, \
         `total`, `Head::elapsed`. There is no fifth because QUIC's \
         handshake is not divisible into a transport phase and a TLS one, \
         which is why `ConnectTiming::tls` is `None` here"
    );
    assert_eq!(
        pooled,
        (1, 2),
        "a pooled request takes one mark and reads two intervals: `dns`, \
         which is measured before the pool is consulted and then thrown \
         away on this path, and `Head::elapsed`"
    );
}

/// `NoHooks` costs nothing to *carry* either: it is zero-sized, so a
/// transport that stores one is the same size as one with no field at all.
#[test]
fn the_no_op_hook_takes_up_no_room_in_the_transport() {
    assert_eq!(std::mem::size_of::<NoHooks>(), 0);
    assert_eq!(
        std::mem::size_of::<H3<TokioHandle, hclient_tls_rustls::Rustls, IpLiteralOnly, NoHooks>>(),
        std::mem::size_of::<H3<TokioHandle, hclient_tls_rustls::Rustls, IpLiteralOnly>>(),
        "the default type parameter must be the same type, not a second one"
    );
}
