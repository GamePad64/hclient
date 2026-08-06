//! Утверждения о форме публичного API `http-ng-wasi`, вынесенные за пределы
//! `src` — тот же приём, что у `http-ng-core/tests/shape.rs` (см. его
//! doc-комментарий): `no-declared-send` в CI сканирует только `crates/*/src`,
//! так что обычный `T: Send` здесь не путается с инвариантом "ядро не
//! объявляет Send/Sync" в продакшн-коде.
//!
//! Компилируется и запускается на любом таргете (не гейтится под
//! `wasm32-wasip2`): тест ниже никогда не поллит футуру, которую строит, —
//! только проверяет её ТИП. Ни `WasiHttp::new()`, ни сборка `http::Request`,
//! ни вызов `async fn execute` сами по себе не трогают ни один вызов
//! `wasi:http`: `execute` — `async fn`, его тело не выполняется, пока футуру
//! не поллят, а конструкторы `WasiHttp`/`http::Request` не делают host-вызовов
//! вовсе.

fn assert_send<T: Send>(_: T) {}

/// Резолюция review (Task 16, находка B-13): `convert::Payload::Streaming`
/// несёт `+ Send` (`send-bound-exception: amendment-C2`) именно затем, чтобы
/// будущее `WasiHttp::execute` оставалось `Send` для потоковых тел запроса.
/// Ревью нашло, что маркер был обоснован — но если бы бонд и маркер убрали
/// ВМЕСТЕ, `no-declared-send` остался бы зелёным (маркер просто исчез бы
/// вместе с тем, что он разрешал), а это свойство сломалось бы молча. Этот
/// тест ловит именно такую регрессию извне: `pub(crate) enum Payload` отсюда
/// не виден (`tests/` видит только публичный API крейта), так что
/// единственный способ проверить его Send-ность — понаблюдать за формой
/// футуры, которую производит `execute`, на реальном потоковом теле.
#[test]
fn execute_future_is_send_even_for_a_streaming_request_body() {
    use http_ng_core::RequestBody;
    use http_ng_core::unversioned::Transport;

    struct OneShot(Option<bytes::Bytes>);
    impl http_body::Body for OneShot {
        type Data = bytes::Bytes;
        type Error = http_ng_core::Error;
        fn poll_frame(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Result<http_body::Frame<bytes::Bytes>, http_ng_core::Error>>>
        {
            std::task::Poll::Ready(self.0.take().map(|b| Ok(http_body::Frame::data(b))))
        }
    }

    let transport = http_ng_wasi::WasiHttp::new();
    let req = http::Request::builder()
        .uri("http://example.invalid/")
        .body(RequestBody::Streaming(Box::new(OneShot(Some(
            bytes::Bytes::from_static(b"x"),
        )))))
        .unwrap();
    let fut = transport.execute(req);
    assert_send(fut);
}

/// Симметрия: тело `Empty` тоже не должно случайно перестать быть `Send` —
/// эта ветка не проходит через `Payload::Streaming` вовсе, так что у неё
/// свой путь до `Send`-ности футуры.
#[test]
fn execute_future_is_send_for_an_empty_request_body() {
    use http_ng_core::RequestBody;
    use http_ng_core::unversioned::Transport;

    let transport = http_ng_wasi::WasiHttp::new();
    let req = http::Request::builder()
        .uri("http://example.invalid/")
        .body(RequestBody::Empty)
        .unwrap();
    let fut = transport.execute(req);
    assert_send(fut);
}
