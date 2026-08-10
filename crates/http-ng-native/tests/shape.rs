//! Assertions about `http-ng-native`'s public API shape, kept outside
//! `src` — the same technique as `http-ng-core/tests/shape.rs` and
//! `http-ng-wasi/tests/shape.rs` (see their doc comments and spec
//! amendment-C3): CI's `no-declared-send` has scanned
//! `crates/http-ng-native/src` since Task 13 (vertical 2) — before that,
//! the crate exported nothing public besides `testing`, so there was
//! nothing yet to protect. An ordinary `T: Send` here doesn't get
//! confused with the production invariant, because this file isn't
//! `src`.
use http_ng_native::testing::OutgoingBody;

/// Used to live in `src/body.rs`'s `#[cfg(test)] mod tests` as
/// `error_type_satisfies_hypers_send_sync_bound` — the same assertion,
/// the same meaning (`hyper::client::conn::http1::handshake<T, B>`
/// requires `B::Error: Into<Box<dyn StdError + Send + Sync>>` and
/// `B::Data: Send`, see `body.rs`'s module doc comment), moved here once
/// `no-declared-send` started scanning this crate's `src`.
/// Auto traits reach the transport and its response body, rather than
/// being cut off by a `dyn`.
///
/// Both halves of this were untrue or fragile before v0.2 W2 and are
/// asserted here because both are now claims the documentation makes.
/// `Native` was `Send + Sync` by luck — a pool holding `Box<dyn Future>`
/// connections would have taken both away, and nothing would have noticed.
/// `NativeBody` was `!Send` outright, so a response could not be handed to
/// `tokio::spawn`; storing hyper's concrete `Connection<I, B>` instead of a
/// boxed future is what changed that (`h1.rs`, "Nothing here is boxed behind
/// `dyn`").
///
/// These are `Send`/`Sync` *assertions*, not declared bounds — the
/// distinction CI's `no-declared-send` cares about, and the reason this file
/// lives outside `src` (see the module doc above).
#[test]
fn auto_traits_reach_the_transport_and_its_body() {
    fn assert_send<T: Send>() {}
    fn assert_send_sync<T: Send + Sync>() {}

    type Rt = http_ng_rt_tokio::Tokio;
    type Tls = http_ng_tls_rustls::Rustls;
    type Dns = http_ng_dns_system::SystemDns<Rt>;

    assert_send_sync::<http_ng_native::Native<Rt, Tls, Dns>>();
    assert_send::<http_ng_native::testing::NativeBody<http_ng_native::NativeIo<Rt, Tls>>>();
}

#[test]
fn outgoing_bodys_error_satisfies_hypers_send_sync_bound() {
    fn assert_bound<B: http_body::Body>()
    where
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
        B::Data: bytes::Buf + Send,
    {
    }
    assert_bound::<OutgoingBody>();
}

/// The same property for the WebSocket seam (v0.3 W4), and it is the whole
/// reason `hyper::upgrade::Upgraded` was not used: that type is
/// `Rewind<Box<dyn Io + Send>>`, so taking it would have made this crate's
/// IO carry a *declared* `Send` bound and shut out single-threaded
/// runtimes. `poll_without_shutdown` + `into_parts` hand back the concrete
/// `I` instead, and `Send` is inferred here rather than required
/// anywhere — exactly as for `NativeBody` above.
///
/// `Sync` is deliberately not asserted: a `Stream + Sink` is used through
/// `&mut`, and nothing in this workspace shares one.
#[cfg(feature = "websocket")]
#[test]
fn auto_traits_reach_the_websocket() {
    fn assert_send<T: Send>() {}

    type Rt = http_ng_rt_tokio::Tokio;
    type Tls = http_ng_tls_rustls::Rustls;

    assert_send::<http_ng_native::NativeWebSocket<http_ng_native::NativeIo<Rt, Tls>>>();
}
