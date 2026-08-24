//! How big `Native::execute`'s future is, asserted rather than discovered.
//!
//! **`hclient`'s guard of the same name did not catch the growth that made
//! this one necessary**, and the reason is worth stating: it measures
//! `Client<MockTransport>`, whose transport future is a stub. The
//! `Expect: 100-continue` bound grew **`Native`'s** future instead — it was
//! first written as a combinator wrapping the exchange — and 56 tests in
//! this crate's hook suite aborted with `SIGABRT` on a stack overflow
//! while that guard stayed green.
//!
//! One future is not the whole story either. The test that noticed,
//! `pool::checkout_walks_past_a_dead_connection_to_a_live_one`, holds two
//! at once through `tokio::join!`, and the debug-build call chain carries
//! the value down every frame. So the ceiling here is what keeps a
//! reasonable caller inside a 2 MiB stack rather than a number anybody
//! measured a stack against.
#![cfg(not(target_family = "wasm"))]

use hclient_core::RequestBody;
use hclient_core::unversioned::Transport;
use hclient_dns::IpLiteralOnly;
use hclient_native::Native;
use hclient_rt_tokio::Tokio;
use hclient_tls::NoTls;
use std::time::Duration;

/// Measured **15,480 bytes** on x86-64 Linux, debug, `--all-features` —
/// three and a half times `Client::execute`'s, which is the other half of
/// why that guard could not stand in for this one. This future holds the
/// connect machinery: Happy Eyeballs over two address families, the
/// discovery lookup, the pool checkout and the retry.
///
/// **The ceiling is 24 KiB, and it is measured rather than rounded.** One
/// extra `async fn` layer around the exchange — the shape that caused the
/// aborts — takes this future to **28,088 bytes**, so a ceiling of 32 KiB
/// would have been a guard that could not fire. 24 KiB sits below that and
/// 55% above today's, which is room for an ordinary field and none for a
/// wrapper.
///
/// Checked in both directions: reintroducing that layer fails this test,
/// and removing it passes.
const CEILING: usize = 24 * 1024;

/// A pinned ceiling would fail on every unrelated edit and be raised
/// without thought; this one is meant to be reached only by a mistake.
#[test]
fn the_execute_future_stays_under_its_ceiling() {
    let t = Native::new(Tokio, NoTls, IpLiteralOnly);
    let fut = t.execute(http::Request::new(RequestBody::Empty));
    let size = std::mem::size_of_val(&fut);
    assert!(
        size <= CEILING,
        "`Native::execute`'s future is {size} bytes, over the {CEILING}-byte \
         ceiling. Something is held by value that should be folded into an \
         existing combinator or boxed — see `Native::within_first_byte_gated`, \
         which is folded rather than wrapped for exactly this reason and \
         records the 56 tests that aborted when it was not."
    );
}

/// **The same future with every opt-in switched on**, because each one
/// adds a field and the ceiling has to hold for the transport a caller
/// actually configures rather than the smallest one.
#[test]
fn the_ceiling_holds_with_the_opt_ins_switched_on() {
    let t = Native::new(Tokio, NoTls, IpLiteralOnly).expect_continue(Duration::from_secs(1));
    let fut = t.execute(http::Request::new(RequestBody::Empty));
    let size = std::mem::size_of_val(&fut);
    assert!(size <= CEILING, "with the opt-ins on it is {size} bytes");
}
