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
