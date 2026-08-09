//! `Native::with_reaper` closes an idle connection at its deadline —
//! measured by the server whose socket it is.
//!
//! # The observer is the server, and the number is its own
//!
//! Nothing in this file asks the client what it did. The pool has no
//! public surface to ask, and a counter we incremented ourselves would
//! prove that some code ran, not that a socket closed. So the whole
//! assertion is one duration the **server** produced: how long after it
//! answered a request its own `read` on that same socket returned `0`,
//! which is the client's `FIN` and cannot be anything else — a timeout is
//! `Err`, data is `Ok(n > 0)`.
//!
//! # Both halves are load-bearing
//!
//! `a_reaper_closes_an_idle_connection_on_*` on its own would also pass
//! against a client that closed the connection for a reason having nothing
//! to do with a reaper — a failed request, a pool that never parked
//! anything, a `Native` dropped early. Its control,
//! `without_a_reaper_the_same_connection_stays_open_on_*`, runs the same
//! request against the same server through a transport differing in
//! exactly one call (`with_reaper(cfg)` where the control has `pool(cfg)`,
//! the same `PoolConfig` either way), and requires that the server see
//! **nothing** for four times the idle timeout. The pair attributes the
//! close to the reaper rather than to anything about the fixture.
//!
//! # Two runtimes, and why that is not belt-and-braces here
//!
//! `Spawn` is the capability this feature stands on, and it is the one the
//! two shipped runtimes implement most differently:
//! `http_ng_rt_tokio::Tokio` hands the task to an ambient multi-threaded
//! runtime read out of a thread-local; `http_ng_rt_smol::Smol` starts a
//! dedicated executor thread of its own the first time anybody spawns, and
//! the request itself runs on a bare `futures_executor::block_on` with no
//! ambient runtime at all. A reaper that worked only on the first would
//! look exactly like a reaper that worked, right up to the day somebody
//! ran it on the second. Each claim below is one generic function
//! instantiated twice, with no `#[cfg]` and no second copy of the
//! assertions; the only thing [`Harness`] abstracts is which executor
//! drives the request.
//!
//! In both cases the test thread then blocks on the server's channel, not
//! on the runtime — so whatever keeps the reaper going afterwards is the
//! runtime's own doing, which is the property under test.
#![cfg(not(target_family = "wasm"))]

use http_ng::Client;
use http_ng_dns_system::SystemDns;
use http_ng_native::{Native, NativeIo, PoolConfig, Reaper};
use http_ng_rt::{Blocking, Spawn, TcpConnect, Timer};
use http_ng_tls_rustls::Rustls;
use std::future::Future;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// The idle timeout under test. Long enough that "closed at its deadline"
/// and "closed the moment the request finished" are far apart on any
/// machine, short enough that the control costs about a second.
const IDLE: Duration = Duration::from_millis(300);

/// How long these tests wait for the server to report a close. Four times
/// the idle timeout: a reaper that fired at its deadline is well inside
/// it, and a transport with no reaper has had four chances to be late.
const WINDOW: Duration = Duration::from_millis(1200);

/// The observer: answers exactly one request, then watches that same
/// socket and reports how long after answering the client closed it.
fn watching_server() -> (SocketAddr, mpsc::Receiver<Duration>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let Ok((mut sock, _)) = listener.accept() else {
            return;
        };
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 1024];
        // A request head ends at the first blank line, and every request
        // this file sends is a bodyless GET.
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
            match sock.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
        if sock
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
            .is_err()
        {
            return;
        }
        let answered = Instant::now();
        // Outlives the tests' own `WINDOW`, so this thread ends on its own
        // without ever being the thing that decides a verdict.
        sock.set_read_timeout(Some(WINDOW * 3))
            .expect("set_read_timeout");
        let mut one = [0u8; 1];
        if let Ok(0) = sock.read(&mut one) {
            let _ = tx.send(answered.elapsed());
        }
    });
    (addr, rx)
}

/// Which executor drives a request, and which runtime the transport is
/// built on — the only two things the tokio and smol runs differ in.
///
/// A trait rather than a closure because building the client has to happen
/// **inside** `run` on tokio: `with_reaper` spawns, and the shipped ZST
/// `Tokio` reads its runtime out of a thread-local, so a client built off
/// a runtime thread would panic there. Handing `run` a whole future lets
/// the generic body below be one body.
trait Harness {
    /// The capabilities `Native` and `SystemDns` need of a runtime.
    /// `Stream: 'static` is hyper's handshake requirement, the same bound
    /// `http-ng/tests/two_runtimes.rs` carries; it is written as an
    /// associated-type bound here because a trait alias carrying it does
    /// not elaborate through `Self::Rt` (E0310).
    type Rt: TcpConnect<Stream: 'static> + Timer + Blocking + Clone;
    fn rt(&self) -> Self::Rt;
    fn run<F: Future>(&self, f: F) -> F::Output;
}

/// Multi-threaded on purpose: once `run` has returned, the test thread
/// blocks on the server's channel, and the reaper has to make progress on
/// tokio's own worker threads with nothing else driving it — which is what
/// it will have to do in any real program.
struct OnTokio(tokio::runtime::Runtime);

impl Harness for OnTokio {
    type Rt = http_ng_rt_tokio::Tokio;
    fn rt(&self) -> Self::Rt {
        http_ng_rt_tokio::Tokio
    }
    fn run<F: Future>(&self, f: F) -> F::Output {
        self.0.block_on(f)
    }
}

/// No ambient runtime at all: the request runs on a bare
/// `futures_executor::block_on`, and `Spawn` starts an executor thread of
/// its own the first time it is used. Nothing drives the reaper but that
/// thread.
struct OnSmol;

impl Harness for OnSmol {
    type Rt = http_ng_rt_smol::Smol;
    fn rt(&self) -> Self::Rt {
        http_ng_rt_smol::Smol
    }
    fn run<F: Future>(&self, f: F) -> F::Output {
        futures_executor::block_on(f)
    }
}

fn config() -> PoolConfig {
    PoolConfig {
        idle_timeout: IDLE,
        ..PoolConfig::default()
    }
}

fn transport<R: TcpConnect<Stream: 'static> + Timer + Blocking + Clone>(
    rt: R,
) -> Native<R, Rustls, SystemDns<R>> {
    Native::new(rt.clone(), Rustls::with_webpki_roots(), SystemDns::new(rt))
}

/// One request, body collected, so the connection is parked in the pool.
///
/// Collecting matters: a connection is handed back when its response body
/// ends cleanly and at no other moment, so a test that stopped at the head
/// would be watching an abandoned connection rather than an idle one.
async fn park_one_connection<R: TcpConnect<Stream: 'static> + Timer + Blocking + Clone>(
    client: &Client<Native<R, Rustls, SystemDns<R>>>,
    addr: SocketAddr,
) {
    let resp = client
        .get(&format!("http://{addr}/"))
        .send()
        .await
        .expect("request must succeed");
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.collect().await.expect("body").text().expect("text"),
        "ok"
    );
}

/// The claim: with a reaper, the server sees the socket close at about the
/// idle timeout — not at once, and not never.
fn a_reaper_closes_an_idle_connection<H: Harness>(h: H)
where
    H::Rt: Spawn<Reaper<H::Rt, NativeIo<H::Rt, Rustls>>>,
    // Restated rather than inherited from `Harness::Rt`'s own bound: an
    // associated-type bound does not elaborate through `H::Rt` (E0310).
    <H::Rt as TcpConnect>::Stream: 'static,
{
    let (addr, closed) = watching_server();
    let client = h.run(async {
        let c = Client::builder(transport(h.rt()).with_reaper(config()))
            .build()
            .expect("build");
        park_one_connection(&c, addr).await;
        c
    });

    let after = closed
        .recv_timeout(WINDOW)
        .expect("the reaper must have closed the idle connection within the window");
    assert!(
        after >= IDLE / 2,
        "closed {after:?} after the response, far short of the {IDLE:?} deadline — that is a \
         connection being dropped rather than reaped, and it would make the pool useless"
    );
    // Held to here on purpose: dropping the client drops the pool, which
    // closes the socket, which is the one other way the server could have
    // seen what it saw.
    drop(client);
}

/// The control. Same server, same request, same `PoolConfig` — the only
/// difference is `pool` where the claim above has `with_reaper`.
fn without_a_reaper_the_connection_stays_open<H: Harness>(h: H)
where
    <H::Rt as TcpConnect>::Stream: 'static,
{
    let (addr, closed) = watching_server();
    let client = h.run(async {
        let c = Client::builder(transport(h.rt()).pool(config()))
            .build()
            .expect("build");
        park_one_connection(&c, addr).await;
        c
    });

    let verdict = closed.recv_timeout(WINDOW);
    assert!(
        matches!(verdict, Err(mpsc::RecvTimeoutError::Timeout)),
        "with no reaper the idle timeout is a checkout filter, so the socket must still be \
         open {WINDOW:?} after a {IDLE:?} deadline; got {verdict:?}"
    );
    drop(client);
}

fn tokio_harness() -> OnTokio {
    OnTokio(tokio::runtime::Runtime::new().expect("runtime"))
}

#[test]
fn a_reaper_closes_an_idle_connection_on_tokio() {
    a_reaper_closes_an_idle_connection(tokio_harness());
}

#[test]
fn a_reaper_closes_an_idle_connection_on_smol() {
    a_reaper_closes_an_idle_connection(OnSmol);
}

#[test]
fn without_a_reaper_the_same_connection_stays_open_on_tokio() {
    without_a_reaper_the_connection_stays_open(tokio_harness());
}

#[test]
fn without_a_reaper_the_same_connection_stays_open_on_smol() {
    without_a_reaper_the_connection_stays_open(OnSmol);
}
