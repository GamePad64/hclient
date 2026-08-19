//! The same code, two runtimes, zero cfg. If this file needs a single
//! `#[cfg]`, the runtime seam is decorative and the vertical has failed.
//!
//! `fetch_once` below is the one generic body: it mentions `R` (the
//! runtime) through the bounds `http_ng_rt::{TcpConnect, Timer, Blocking} +
//! Clone`, assembles `Native<R, Rustls, SystemDns<R>>`, and drives a real
//! HTTP/1.1 request through it against a real TCP server on loopback. The
//! two tests below instantiate it — once with `http_ng_rt_tokio::Tokio`
//! under `tokio::runtime::Runtime`, once with `http_ng_rt_smol::Smol`
//! under a bare `futures_executor::block_on` (no spawn, no smol reactor at
//! all — only the capabilities `Smol` itself implements on top of
//! `async-io`) — and that's the only difference between the two runs. The
//! vertical's acceptance criterion: not a single `#[cfg]` in this file.
//!
//! The test's property is proven by mutation (see task 14's report):
//! adding `+ Send` to the `R` bound on `fetch_once` doesn't break either
//! instantiation (both capabilities are `Send`) — expected, `Send` isn't
//! the seam being checked here (the only Send asymmetry known in this
//! vertical is `Blocking::run`, not the runtime type itself). The real
//! asymmetry, and correspondingly the real sensitivity check, is adding a
//! bound like `R: PartialEq<std::time::Instant>` via
//! `http_ng_rt::Timer::Instant`: it breaks `Tokio` (`Instant =
//! tokio::time::Instant`, a wrapper) and doesn't break `Smol` (`Instant =
//! std::time::Instant`), see `http-ng-rt-pair-check`'s `pair_property.rs`,
//! from which the mutation trick itself was borrowed.
//!
//! # The one `#[cfg]` in this file, and why it isn't the one forbidden above
//!
//! "Zero cfg" above is a claim about the RUNTIME SEAM: nothing in this file
//! may branch on which runtime is being used, because the whole point is
//! that `fetch_once` is one body serving both. The whole-file gate below
//! makes no such distinction — it excludes `wasm32-*` entirely, where
//! NEITHER runtime exists (`http-ng-rt-tokio` and `http-ng-rt-smol` are
//! equally absent, along with the real `std::net::TcpListener` this file
//! spawns), so it cannot make the seam decorative: there is no seam to
//! decorate on a target where neither side of it is present. Every line
//! that follows still compiles for every target this test can run on at
//! all, with no branch between tokio and smol anywhere.
//!
//! Introduced by Task 8 of vertical 3, which made `http-ng` buildable for
//! `wasm32-unknown-unknown` (`DefaultTransport = http_ng_fetch::Fetch`) and
//! added `tests/wasm_default.rs` to be run there by `wasm-pack test` — a
//! command that builds EVERY test target of the crate. It pairs with the
//! target gate on this file's dependencies in `Cargo.toml`
//! (`[target.'cfg(not(target_family = "wasm"))'.dev-dependencies]`, see the
//! comment there): the gate and this line only work together, and removing
//! either brings back a `mio`/`socket2` compile failure that has nothing to
//! do with anything in this workspace.
#![cfg(not(target_family = "wasm"))]

use http_ng::{Client, Timeouts};
use http_ng_dns_system::SystemDns;
use http_ng_native::Native;
use http_ng_tls_rustls::Rustls;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

fn spawn_server() -> std::net::SocketAddr {
    use std::io::{Read, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let mut b = [0u8; 2048];
            let _ = s.read(&mut b);
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nsame");
        }
    });
    addr
}

/// The generic function: its body is the actual "one codebase for every
/// runtime".
async fn fetch_once<R>(rt: R, addr: std::net::SocketAddr) -> String
where
    R: http_ng_rt::TcpConnect + http_ng_rt::Timer + http_ng_rt::Blocking + Clone,
    R::Stream: 'static,
{
    let t = Native::new(rt.clone(), Rustls::with_webpki_roots(), SystemDns::new(rt));
    let c = Client::builder(t)
        .timeouts(Timeouts {
            resolve: None,
            connect: Some(Duration::from_secs(5)),
            ..Default::default()
        })
        .build()
        .unwrap();
    c.get(&format!("http://{addr}/"))
        .send()
        .await
        .unwrap()
        .collect()
        .await
        .unwrap()
        .text()
        .unwrap()
}

/// Bounds an arbitrary blocking `run` (`tokio::Runtime::block_on` or
/// `futures_executor::block_on`) with a watchdog thread — the same trick
/// as `http_ng_native::connect::tests::bounded_block_on` and
/// `tests/h1.rs`, `tests/dual_runtime.rs` in this same workspace: a
/// regression that stops the runtime seam from driving `fetch_once`
/// forward (say, `Native` silently waiting on a reactor that doesn't exist
/// on a bare `futures` executor) must produce a `FAILED` with a diagnosis,
/// not hang the CI runner mutely. The wrapped work is passed as a closure
/// rather than `F: std::future::Future` directly — the boundary is
/// crossed via `Arc<AtomicBool>`, so `fetch_once` itself, inside the
/// closure, picks up no `Send` bound at all.
fn with_watchdog<T>(run: impl FnOnce() -> T) -> T {
    const BOUND: Duration = Duration::from_secs(30);
    let done = Arc::new(AtomicBool::new(false));
    let watchdog_done = done.clone();
    std::thread::spawn(move || {
        std::thread::sleep(BOUND);
        if !watchdog_done.load(Ordering::SeqCst) {
            eprintln!(
                "two_runtimes: did not finish within {BOUND:?} - looks like the runtime seam \
                 is broken (fetch_once stopped making progress); failing instead of hanging \
                 CI with no test name and no diagnosis"
            );
            std::process::exit(101);
        }
    });
    let result = run();
    done.store(true, Ordering::SeqCst);
    result
}

#[test]
fn identical_code_on_tokio() {
    let addr = spawn_server();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let text = with_watchdog(|| rt.block_on(fetch_once(http_ng_rt_tokio::Tokio, addr)));
    assert_eq!(text, "same");
}

#[test]
fn identical_code_on_smol() {
    let addr = spawn_server();
    let text =
        with_watchdog(|| futures_executor::block_on(fetch_once(http_ng_rt_smol::Smol, addr)));
    assert_eq!(text, "same");
}
