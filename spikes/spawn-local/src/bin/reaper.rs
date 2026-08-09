//! Spike 3a: **does the shape carry a reaper for idle pooled connections?**
//!
//! `cargo run --bin reaper`
//!
//! Not "does a background task run" — that was spike 1. The question W2
//! answered "no" to is whether a *real socket* held in a pool with the
//! real pool's storage shape gets *closed* while the client is idle, with
//! no request in flight. The server is the observer: it reports the
//! wall-clock moment `read()` returns 0.
//!
//! Four runs, and the control is the point:
//!
//! - A: no reaper (today's `Native`) — the socket is still open at the end.
//! - B: reaper on `Tokio`, i.e. **the shipped runtime, unmodified**.
//! - C: reaper on `TokioLocal` with a `!Send` connection in the pool.
//! - D: reaper on `SmolLocal`, same.

use spawn_local_spike::minipool::Pool;
use spawn_local_spike::reaper::{start_reaper, start_reaper_local};
use spawn_local_spike::{SmolLocal, TokioLocal};
use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A connection that is `!Send` by construction — the same `Rc<()>` trick
/// `http-ng-native`'s `connect.rs::FakeStream` uses, wrapped around a real
/// socket so that dropping it really does close the connection.
struct NotSendConn {
    _sock: TcpStream,
    _proof: std::rc::Rc<()>,
}

/// One server, one connection, one answer: how long after the client
/// connected did the socket close?
fn server() -> (std::net::SocketAddr, mpsc::Receiver<Option<Duration>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let t0 = Instant::now();
        sock.set_read_timeout(Some(Duration::from_millis(2500))).unwrap();
        let mut buf = [0u8; 16];
        let verdict = match sock.read(&mut buf) {
            Ok(0) => Some(t0.elapsed()), // EOF: the client closed it
            Ok(_) => None,
            Err(_) => None, // timed out: still open
        };
        let _ = tx.send(verdict);
    });
    (addr, rx)
}

const IDLE: Duration = Duration::from_millis(300);
const PERIOD: Duration = Duration::from_millis(50);
const OBSERVE: Duration = Duration::from_millis(1200);

fn report(label: &str, rx: &mpsc::Receiver<Option<Duration>>, swept: usize) {
    match rx.recv_timeout(Duration::from_secs(4)) {
        Ok(Some(t)) => println!(
            "  {label}: server saw EOF after {:.0}ms (idle_timeout {}ms), entries reaped = {swept}",
            t.as_secs_f64() * 1000.0,
            IDLE.as_millis()
        ),
        Ok(None) => println!(
            "  {label}: server saw NO close within 2.5s — connection still open, entries reaped = {swept}"
        ),
        Err(e) => println!("  {label}: server never answered ({e})"),
    }
}

fn main() {
    println!("idle_timeout = {}ms, reaper period = {}ms, observation window = {}ms\n",
        IDLE.as_millis(), PERIOD.as_millis(), OBSERVE.as_millis());

    // --- A. the control: today's pool, no reaper ---------------------------
    println!("A. no reaper (this is what `Native` does today)");
    {
        let (addr, rx) = server();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let pool: Pool<TcpStream> = Pool::new();
            pool.put("k", TcpStream::connect(addr).unwrap(), IDLE);
            tokio::time::sleep(OBSERVE).await;
            println!("  pool still holds {} connection(s)", pool.len());
        });
        report("A", &rx, 0);
    }

    // --- B. reaper on the SHIPPED Tokio ------------------------------------
    println!("\nB. reaper on `http_ng_rt_tokio::Tokio`, unmodified — Send connection");
    {
        let (addr, rx) = server();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let swept = Arc::new(AtomicUsize::new(0));
        let s = swept.clone();
        rt.block_on(async move {
            let pool: Pool<TcpStream> = Pool::new();
            pool.put("k", TcpStream::connect(addr).unwrap(), IDLE);
            start_reaper(http_ng_rt_tokio::Tokio, pool.weak(), PERIOD, s);
            tokio::time::sleep(OBSERVE).await;
            println!("  pool now holds {} connection(s)", pool.len());
        });
        report("B", &rx, swept.load(Ordering::SeqCst));
    }

    // --- C. reaper on TokioLocal, !Send connection -------------------------
    println!("\nC. reaper on `TokioLocal` — the pooled connection holds an Rc, so it is !Send");
    {
        let (addr, rx) = server();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let swept = Arc::new(AtomicUsize::new(0));
        let s = swept.clone();
        rt.block_on(async move {
            let local = TokioLocal::new();
            let pool: Pool<NotSendConn> = Pool::new();
            pool.put(
                "k",
                NotSendConn { _sock: TcpStream::connect(addr).unwrap(), _proof: std::rc::Rc::new(()) },
                IDLE,
            );
            // Shape B, the named-future one: the SAME `start_reaper` as run B.
            start_reaper(local.clone(), pool.weak(), PERIOD, s);
            local.run_until(tokio::time::sleep(OBSERVE)).await;
            println!("  pool now holds {} connection(s)", pool.len());
        });
        report("C", &rx, swept.load(Ordering::SeqCst));
    }

    // --- D. reaper on SmolLocal, !Send connection, shape C -----------------
    println!("\nD. reaper on `SmolLocal` — same !Send connection, written as a plain async block");
    {
        let (addr, rx) = server();
        let swept = Arc::new(AtomicUsize::new(0));
        let s = swept.clone();
        let local = SmolLocal::new();
        let pool: Pool<NotSendConn> = Pool::new();
        pool.put(
            "k",
            NotSendConn { _sock: TcpStream::connect(addr).unwrap(), _proof: std::rc::Rc::new(()) },
            IDLE,
        );
        // Shape C: no named sleep, no struct — a generic method quantifies
        // the future instead.
        start_reaper_local(local.clone(), pool.weak(), PERIOD, s);
        local.block_on(async { async_io::Timer::after(OBSERVE).await; });
        println!("  pool now holds {} connection(s)", pool.len());
        report("D", &rx, swept.load(Ordering::SeqCst));
    }

    // --- E. reaper on the SHIPPED Smol -------------------------------------
    // `Smol::spawn` reaches a global executor on a *different* thread, so
    // the reaper runs off-thread. That needs the pool to be `Sync`, not
    // just `Send` — which the real `Pool`'s `Arc<Mutex<..>>` gives.
    println!("\nE. reaper on `http_ng_rt_smol::Smol`, unmodified — off-thread executor, Send+Sync connection");
    {
        let (addr, rx) = server();
        let swept = Arc::new(AtomicUsize::new(0));
        let s = swept.clone();
        let pool: Pool<std::net::TcpStream> = Pool::new();
        pool.put("k", TcpStream::connect(addr).unwrap(), IDLE);
        start_reaper(http_ng_rt_smol::Smol, pool.weak(), PERIOD, s);
        futures_executor::block_on(async { async_io::Timer::after(OBSERVE).await });
        println!("  pool now holds {} connection(s)", pool.len());
        report("E", &rx, swept.load(Ordering::SeqCst));
    }
}
