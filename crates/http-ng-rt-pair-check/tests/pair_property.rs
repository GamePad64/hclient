//! Reviewer-written pair-property check for Task 4 of vertical 2
//! (`http-ng-rt-smol`). This is the whole point of the task: the same
//! client-shaped code must run on tokio and on smol with no `#[cfg]`
//! anywhere in the shared body.
//!
//! `exercise<R, F>` below is the one shared body. It touches all four
//! capability traits (`Timer`, `TcpConnect`, `Blocking`, `Spawn`), does real
//! work (spawns a background task, sleeps a measurable amount, runs a
//! blocking closure off the reactor, connects to a real loopback listener
//! with an option applied, writes, and reads a real echo back), and is
//! instantiated once for `Tokio` and once for `Smol` below, each in its own
//! `#[test]`. There is no `#[cfg]`, no `Box<dyn Trait>`, and no
//! runtime-specific bound anywhere in `exercise` itself — the two
//! instantiations differ only in which concrete runtime type is passed in
//! and in the harness each test uses to drive its own executor
//! (`#[tokio::test]` vs. `futures_executor::block_on`), which is
//! unavoidable and outside the shared body by construction (a test fn has
//! to pick an executor to run *in*).
//!
//! Ran in a throwaway clone during the Task 4 review: both
//! `pair_property_holds_for_tokio` and `pair_property_holds_for_smol`
//! passed against the workspace at `71cad8d`, first try, with the shared
//! `exercise` body exactly as checked in here - it needed no `#[cfg]`, no
//! boxing, and no bound beyond what `TcpConnect`'s own associated-type
//! bound (`Stream: hyper::rt::Read + hyper::rt::Write + Unpin`) already
//! supplies. This is the strongest evidence available that the runtime
//! seam this vertical exists to prove is real, not decorative.
//!
//! To re-run: this whole directory (`http-ng-rt-pair-check/`) is a
//! standalone crate meant to be added under `crates/` in a scratch clone
//! (workspace members are `crates/*`, so it is picked up automatically),
//! then `cargo test -p http-ng-rt-pair-check --all-features`.
use http_ng_rt::{Blocking, Spawn, TcpConnect, TcpOpts, Timer};
use hyper::rt::{Read as HyperRead, ReadBuf, Write as HyperWrite};
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

fn spawn_echo_listener() -> SocketAddr {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        if let Ok((mut s, _)) = l.accept() {
            use std::io::{Read, Write};
            let mut buf = [0u8; 64];
            if let Ok(n) = s.read(&mut buf) {
                let _ = s.write_all(&buf[..n]);
            }
        }
    });
    addr
}

async fn write_all<S: HyperWrite + Unpin>(s: &mut S, mut buf: &[u8]) -> io::Result<()> {
    while !buf.is_empty() {
        let n = std::future::poll_fn(|cx| Pin::new(&mut *s).poll_write(cx, buf)).await?;
        assert!(n > 0, "poll_write returned 0 for a non-empty buffer");
        buf = &buf[n..];
    }
    Ok(())
}

async fn read_some<S: HyperRead + Unpin>(s: &mut S, out: &mut [u8]) -> io::Result<usize> {
    let mut rb = ReadBuf::new(out);
    std::future::poll_fn(|cx| Pin::new(&mut *s).poll_read(cx, rb.unfilled())).await?;
    Ok(rb.filled().len())
}

/// The one shared body. `R::Stream` already carries `hyper::rt::Read +
/// hyper::rt::Write + Unpin` from `TcpConnect`'s own associated-type bound,
/// so nothing extra needs to be spelled out here for it.
async fn exercise<R, F>(rt: R, addr: SocketAddr, background: F)
where
    R: Timer + TcpConnect + Blocking + Spawn<F>,
    F: Future<Output = ()> + Send + 'static,
{
    // Spawn: fire-and-forget background work.
    rt.spawn(background);

    // Timer: measurable sleep.
    let start = rt.now();
    rt.sleep(Duration::from_millis(5)).await;
    assert!(rt.elapsed_since(start) >= Duration::from_millis(5));

    // Blocking: off-reactor closure.
    let doubled = rt.run(|| 21 * 2).await.expect("blocking task did not run");
    assert_eq!(doubled, 42);

    // TcpConnect: real connect with an option applied, then a real
    // write/read round trip against a loopback echo listener.
    let opts = TcpOpts {
        nodelay: true,
        ..Default::default()
    };
    let mut stream = rt.connect(addr, &opts).await.expect("connect");
    write_all(&mut stream, b"ping").await.expect("write");
    let mut buf = [0u8; 64];
    let n = read_some(&mut stream, &mut buf).await.expect("read");
    assert_eq!(&buf[..n], b"ping");
}

#[tokio::test]
async fn pair_property_holds_for_tokio() {
    use http_ng_rt_tokio::Tokio;
    let addr = spawn_echo_listener();
    let ran = Arc::new(AtomicBool::new(false));
    let ran2 = ran.clone();
    exercise(Tokio, addr, async move {
        ran2.store(true, Ordering::SeqCst);
    })
    .await;
    // give the spawned background task a moment to actually run
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        ran.load(Ordering::SeqCst),
        "spawned background task never ran (Tokio)"
    );
}

#[test]
fn pair_property_holds_for_smol() {
    use http_ng_rt_smol::Smol;
    let addr = spawn_echo_listener();
    let ran = Arc::new(AtomicBool::new(false));
    let ran2 = ran.clone();
    futures_executor::block_on(async {
        exercise(Smol, addr, async move {
            ran2.store(true, Ordering::SeqCst);
        })
        .await;
    });
    // give the spawned background task's dedicated executor thread a moment
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        ran.load(Ordering::SeqCst),
        "spawned background task never ran (Smol)"
    );
}
