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

#[test]
fn client_future_is_send_when_the_transport_is() {
    fn assert_send<T: Send>(_: T) {}
    let c = http_ng::Client::builder(http_ng::mock::MockTransport::new())
        .build()
        .expect("mock supports the default config");
    // Именно это свойство ломалось дважды за проект — стиранием типа в
    // `Error` и в `RequestBody`. Оно и есть причина, по которой в ядре
    // не объявлен ни один бонд Send.
    assert_send(c.execute(http::Request::new(http_ng_core::RequestBody::Empty)));
}
