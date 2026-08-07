//! Утверждения о форме публичного API `http-ng-native`, вынесенные за
//! пределы `src` — тот же приём, что у `http-ng-core/tests/shape.rs` и
//! `http-ng-wasi/tests/shape.rs` (см. их doc-комментарии и spec amendment
//! C3): `no-declared-send` в CI сканирует `crates/http-ng-native/src` с
//! Task 13 (вертикаль 2) — до неё крейт не экспортировал ничего публично,
//! кроме `testing`, так что защищать было ещё нечего. Обычный `T: Send`
//! здесь не путается с продакшн-инвариантом, потому что этот файл — не
//! `src`.
use http_ng_native::testing::OutgoingBody;

/// Раньше жил в `src/body.rs`'s `#[cfg(test)] mod tests` как
/// `error_type_satisfies_hypers_send_sync_bound` — тот же ассерт, тот же
/// смысл (`hyper::client::conn::http1::handshake<T, B>` требует `B::Error:
/// Into<Box<dyn StdError + Send + Sync>>` и `B::Data: Send`, см.
/// doc-комментарий модуля `body.rs`), перенесён сюда, когда `no-declared-send`
/// начала сканировать `src` этого крейта.
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
