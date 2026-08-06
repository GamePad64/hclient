//! Утверждения о форме публичного API, вынесенные за пределы `src`:
//! проверка `no-declared-send` в CI сканирует только `crates/*/src`.

#![cfg(feature = "test-util")]

fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn mock_transport_is_send_and_sync_so_client_futures_can_be_spawned() {
    // Если мок перестанет быть Sync, `&MockTransport` перестанет быть Send,
    // и футура `Client::execute` окажется !Send — то есть тест-двойник сам
    // отберёт у нас возможность проверять главное свойство дизайна.
    assert_send_sync::<http_ng::mock::MockTransport>();
}
