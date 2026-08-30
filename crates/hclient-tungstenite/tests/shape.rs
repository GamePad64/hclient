//! Assertions about this crate's public API shape, kept outside `src` —
//! the same technique as `hclient-core/tests/shape.rs` and
//! `hclient-native/tests/shape.rs` (see their doc comments and spec
//! amendment-C3): CI's `no-send-or-sync` scans every `crates/*/src`, and
//! an ordinary `T: Send` here does not get confused with the production
//! invariant, because this file is not `src`.
#![cfg(not(target_family = "wasm"))]

/// Auto traits reach an open WebSocket, and it is the whole reason
/// `hyper::upgrade::Upgraded` was not used one crate over: that type is
/// `Rewind<Box<dyn Io + Send>>`, so taking it would have made the IO carry
/// a *declared* `Send` bound and shut out single-threaded runtimes.
/// `hclient_native::Upgrading::finish` hands back the concrete `I`
/// instead, and `Send` is inferred here rather than required anywhere.
///
/// `Sync` is deliberately not asserted: a `Stream + Sink` is used through
/// `&mut`, and nothing in this workspace shares one.
///
/// The second type argument is the keep-alive's clock, and it carries a
/// second claim of the same kind: `TungsteniteWebSocket` holds a
/// `Pin<Box<Tm::Sleep>>`, and a box around a **concrete** type lets auto
/// traits through where a `Pin<Box<dyn Future>>` would have stopped them.
/// `Timer::Sleep` being a named associated type rather than an RPITIT is
/// what makes that available — the same property `IdleTimeout` relies on.
#[test]
fn auto_traits_reach_the_websocket() {
    type Rt = hclient_rt_tokio::Tokio;
    type Tls = hclient_tls_rustls::Rustls;

    static_assertions::assert_impl_all!(
        hclient_tungstenite::TungsteniteWebSocket<hclient_native::NativeIo<Rt, Tls>, Rt>: Send
    );
}
