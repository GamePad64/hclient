//! What a client with no hook pays for the hook's existence, measured
//! rather than asserted.
//!
//! # The measurement, and why it is a clock rather than a stopwatch
//!
//! "Zero cost" here means one specific thing: with [`NoHooks`], the
//! transport does not read a clock, does not take a connection id, and has
//! no branch left that a monomorphised build cannot delete. The first of
//! those is directly countable, and it is the one that would actually cost
//! a caller something — `Timer::now` is a `clock_gettime` on the request
//! path.
//!
//! So the runtime under the transport is [`Counting`], a `Tokio` that
//! delegates everything and keeps a tally of how often it was asked what
//! time it is. Every other figure would be a proxy: a wall-clock benchmark
//! measures the network, and a binary size measures the linker.
//!
//! # It is exact, and deliberately so
//!
//! A `<=` would pass for a build that read the clock four times when it
//! should read it none. The numbers below are equalities, and the second
//! request is the interesting one: **served from the pool, a `NoHooks`
//! request reads the clock zero times.** There is nothing to be careful
//! about in that number — it cannot drift by a microsecond or by a
//! scheduler.
//!
//! The first request reads it exactly once with no hook, and that read is
//! not the hook's: `connect::drive` stamps the start of the Happy Eyeballs
//! race, because RFC 8305's pacing is measured from it. It was there
//! before this feature and it is there without it.
//!
//! If a legitimate clock read is ever added to this path, this file fails
//! and the number has to be changed **on purpose**. That is the point of
//! the equality: a tripwire that has to be looked at.
#![cfg(not(target_family = "wasm"))]

use hclient_core::RequestBody;
use hclient_core::unversioned::{Event, Hooks, NoHooks, Transport};
use hclient_dns::IpLiteralOnly;
use hclient_native::Native;
use hclient_rt::{TcpConnect, TcpOpts, TcpOptsSupport, Timer};
use hclient_rt_tokio::Tokio;
use hclient_tls::NoTls;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// `Tokio`, with a note kept of every `now()`.
///
/// Only `now()` is counted, and that is not laziness: `elapsed_since` is
/// called once per iteration of `connect::drive`'s scheduler loop, and how
/// many iterations that loop takes depends on whether a loopback connect
/// completes on its first poll — a number no test may pin. `now()` is
/// called at fixed points, so counting it is deterministic.
#[derive(Clone, Default)]
struct Counting(Arc<AtomicUsize>);

impl Counting {
    fn reads(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
    fn reset(&self) {
        self.0.store(0, Ordering::SeqCst);
    }
}

impl Timer for Counting {
    type Instant = <Tokio as Timer>::Instant;
    type Sleep = <Tokio as Timer>::Sleep;
    fn sleep(&self, d: Duration) -> Self::Sleep {
        Tokio.sleep(d)
    }
    fn now(&self) -> Self::Instant {
        self.0.fetch_add(1, Ordering::SeqCst);
        Tokio.now()
    }
    /// Delegated **without** counting — see the type's doc comment. It
    /// still reads a clock underneath, which is exactly why this file
    /// cannot count it and stay deterministic.
    fn elapsed_since(&self, earlier: Self::Instant) -> Duration {
        Tokio.elapsed_since(earlier)
    }
}

impl TcpConnect for Counting {
    type Stream = <Tokio as TcpConnect>::Stream;
    const APPLIES: TcpOptsSupport = <Tokio as TcpConnect>::APPLIES;
    fn connect(
        &self,
        addr: SocketAddr,
        opts: &TcpOpts,
    ) -> impl std::future::Future<Output = std::io::Result<Self::Stream>> {
        Tokio.connect(addr, opts)
    }
}

/// A hook that does nothing but exist — the *presence* of a watcher is
/// what this file measures, not what a watcher does with the events.
#[derive(Clone, Copy, Default)]
struct Watching;
impl Hooks for Watching {
    fn on(&self, _event: Event<'_>) {}
}

/// An HTTP/1.1 server that answers and keeps the connection.
fn server() -> SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    std::thread::spawn(move || {
        for sock in listener.incoming() {
            let Ok(mut sock) = sock else { continue };
            std::thread::spawn(move || {
                let mut buf: Vec<u8> = Vec::new();
                let mut chunk = [0u8; 1024];
                loop {
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
                    if sock
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                        .is_err()
                    {
                        return;
                    }
                }
            });
        }
    });
    addr
}

async fn get(t: &impl Transport, addr: SocketAddr) {
    let req = http::Request::builder()
        .uri(format!("http://{addr}/"))
        .body(RequestBody::Empty)
        .unwrap();
    let resp = t.execute(req).await.map_err(|_| ()).expect("request");
    assert_eq!(resp.status(), 200);
    // The body must be read to the end, or the connection is never
    // checked in and the second request below would not be the pooled
    // one this file is about.
    let _ = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .map_err(|_| ())
        .expect("body");
}

/// The whole claim, in two numbers a caller can check.
///
/// `NoTls` and `IpLiteralOnly` are what make those numbers this crate's:
/// a real TLS backend or a resolver with a clock of its own would read
/// the counter for reasons that are not ours.
#[tokio::test]
async fn a_client_with_no_hook_reads_no_clock_the_hook_asked_for() {
    let addr = server();

    let clock = Counting::default();
    let quiet = Native::new(clock.clone(), NoTls, IpLiteralOnly);
    // `Native::new` reads the clock once for its epoch, before any
    // request. Not counted here: it happens once per transport and is not
    // on the request path at all.
    clock.reset();

    get(&quiet, addr).await;
    let fresh = clock.reads();
    clock.reset();
    get(&quiet, addr).await;
    let pooled = clock.reads();

    assert_eq!(
        pooled, 0,
        "a pooled request under NoHooks must not read the clock at all — \
         it does not connect, so not even Happy Eyeballs has a reason to"
    );
    assert_eq!(
        fresh, 1,
        "a fresh connection reads it exactly once, and that read is \
         `connect::drive`'s Happy Eyeballs epoch, which predates this \
         feature — see this file's module doc"
    );
}

/// The other side of the same measurement, and the reason the first is
/// not vacuous: with a hook, the same two requests read the clock, and
/// the counts say which reads the hook is responsible for.
///
/// Without this pair the first test would pass against a transport whose
/// timing code was simply broken — never reading a clock for anybody.
#[tokio::test]
async fn the_same_two_requests_with_a_hook_do_read_it() {
    let addr = server();

    let clock = Counting::default();
    let watched = Native::new(clock.clone(), NoTls, IpLiteralOnly).hooks(Watching);
    clock.reset();

    get(&watched, addr).await;
    let fresh = clock.reads();
    clock.reset();
    get(&watched, addr).await;
    let pooled = clock.reads();

    assert_eq!(
        pooled, 1,
        "a pooled request times its head, and nothing else: one mark at \
         the start of the request, read back with `elapsed_since`"
    );
    assert_eq!(
        fresh, 4,
        "a fresh connection reads it four times, and each one is a mark \
         some figure is measured from: the request's start (`Head::elapsed` \
         and `ConnectTiming::total`), the connect's start (`dns`), Happy \
         Eyeballs' own epoch (which is not the hook's — the `NoHooks` run \
         above pays it too), and the winning attempt's launch (`tcp`). \
         There is no fifth because `http://` has no handshake to time"
    );
}

/// `NoHooks` costs nothing to *carry* either: it is zero-sized, so a
/// transport that stores one is the same size as one with no field at
/// all.
#[test]
fn the_no_op_hook_takes_up_no_room_in_the_transport() {
    assert_eq!(std::mem::size_of::<NoHooks>(), 0);
    assert_eq!(
        std::mem::size_of::<Native<Counting, NoTls, IpLiteralOnly, NoHooks>>(),
        std::mem::size_of::<Native<Counting, NoTls, IpLiteralOnly>>(),
        "the default type parameter must be the same type, not a second one"
    );
}
