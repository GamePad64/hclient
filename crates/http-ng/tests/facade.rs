//! Проверка фасада: типы, участвующие в публичном API `http-ng`, обязаны быть
//! достижимы из крейта, который зависит только от `http-ng`.
//!
//! Живёт в `tests/`, а не в `src/`, по двум причинам: во-первых, `tests/`
//! компилируется как внешний потребитель, поэтому видит ровно ту
//! поверхность, что и downstream-пользователь (внутренний `use super::*`
//! этого не проверил бы). Во-вторых, `no-declared-send` в CI сканирует
//! только `crates/*/src` (amendment C3) — здесь это не имеет значения,
//! `Send`/`Sync` тут не объявляются, но место всё равно правильное для
//! любого будущего теста в этом духе.

#[test]
fn public_api_types_are_reachable_from_the_facade() {
    // `Config.redirect` имеет этот тип.
    let _p: http_ng::RedirectPolicy = http_ng::RedirectPolicy::default();
    // `check_supported` принимает это и возвращает вот это.
    let caps: http_ng::Capabilities = http_ng::Capabilities::none();
    let cfg = http_ng::Config::default();
    let _: Result<(), http_ng::UnsupportedCapability> =
        http_ng::check_supported(&cfg, &caps, "probe");
}

/// `Response`, `Collected` и `RequestBuilder` (Task 13) не имели проверки
/// достижимости из фасада (Task 13 fix round 1, Finding 6). В отличие от
/// типов выше, у них нет публичного конструктора без транспорта — значение
/// сконструировать здесь нечем, поэтому достижимость и форма (арность
/// дженериков) проверяются компиляцией никогда не вызываемой функции: если
/// `Response`/`Collected`/`RequestBuilder` перестанут реэкспортироваться из
/// `http_ng::` или сменят число параметров, этот файл — как внешний
/// потребитель — перестанет собираться.
#[allow(dead_code)]
fn response_collected_and_request_builder_are_reachable_from_the_facade<T, B>(
    _r: http_ng::Response<B>,
    _c: http_ng::Collected,
    _b: http_ng::RequestBuilder<'_, T>,
) {
}

/// `SseStream`, `SseEvent` и `DEFAULT_MAX_EVENT_SIZE` (Task 14) на самом деле
/// живут в `http-ng-proto` (`SseEvent`, `DEFAULT_MAX_EVENT_SIZE`) и `http-ng`
/// (`SseStream`), но обязаны быть именуемы из `http_ng::` без прямой
/// зависимости от `http-ng-proto` — тот же контракт, что и выше. Тот же
/// приём для `SseStream`, что и для `Response`/`Collected`/`RequestBuilder`:
/// конструктора без транспорта нет, поэтому достижимость и форма (арность
/// дженерика) проверяются компиляцией никогда не вызываемой функции.
#[allow(dead_code)]
fn sse_types_are_reachable_from_the_facade<B>(_s: http_ng::SseStream<B>) {
    let _event: http_ng::SseEvent = http_ng::SseEvent::Comment(String::new());
    let _limit: usize = http_ng::DEFAULT_MAX_EVENT_SIZE;
}

// ── Task 17 fix round 1 ─────────────────────────────────────────────────
//
// `Error` был именно тем типом, что делает предыдущий раунд теста неполным:
// `Client::execute`, `RequestBuilder::send`, `Response::chunk`/`collect`,
// `Collected::text`, `SseStream::new`/`next` все возвращают его, а
// `public_api_types_are_reachable_from_the_facade` выше ни разу его не
// называет. Тесты ниже не просто проверяют компиляцию (как
// `#[allow(dead_code)]`-функции выше, у которых нет конструктора без
// транспорта) — они реально СОЗДАЮТ и СРАВНИВАЮТ значения каждого нового
// реэкспорта, потому что тест, который лишь называет тип, ничего не говорит
// о том, действительно ли с ним можно работать (сравнивать, деструктурировать,
// передавать по значению).

/// `Error`/`ErrorKind`/`Phase` — типы `Result`, которые фасад обязан уметь
/// назвать сам, без обращения к `http-ng-core`. `Result`-алиас ниже — именно
/// то, что не могла написать функция, возвращающая `Result<_, http_ng::Error>`,
/// до этого раунда фикса (см. отчёт задачи 17, история с примером в брифе,
/// который называл несуществующий `http_ng::Error`).
#[test]
fn error_kind_and_phase_are_reachable_and_matchable_from_the_facade() {
    type FacadeResult<T> = Result<T, http_ng::Error>;

    fn probe() -> FacadeResult<()> {
        Err(http_ng::Error::new(
            http_ng::ErrorKind::Timeout(http_ng::Phase::Connect),
            std::io::Error::other("probe"),
        ))
    }

    let err = probe().expect_err("probe всегда возвращает Err");
    // `ErrorKind` — `#[non_exhaustive]`: снаружи крейта матч обязан иметь
    // catch-all-ветку независимо от того, сколько вариантов перечислено —
    // это само по себе часть проверки достижимости из фасада, не только
    // компиляции.
    match err.kind() {
        http_ng::ErrorKind::Timeout(phase) => assert_eq!(*phase, http_ng::Phase::Connect),
        other => panic!("unexpected kind: {other:?}"),
    }
    assert!(err.is_timeout());
}

/// `RetryKind` — вариант возврата `RequestBody::retry_kind()`, `RewindFactory`
/// — тип поля `RequestBody::Rewindable`. Строится и `Full`, и `Rewindable`,
/// а не только один вариант — иначе тест доказал бы достижимость `RetryKind`,
/// но не `RewindFactory`.
#[test]
fn retry_kind_and_rewind_factory_are_reachable_from_the_facade() {
    let full = http_ng::RequestBody::Full(bytes::Bytes::from_static(b"x"));
    assert_eq!(full.retry_kind(), http_ng::RetryKind::Free);

    let factory: http_ng::RewindFactory =
        std::sync::Arc::new(|| http_ng::RequestBody::Full(bytes::Bytes::from_static(b"y")));
    let rewindable = http_ng::RequestBody::Rewindable(factory);
    assert_eq!(rewindable.retry_kind(), http_ng::RetryKind::ViaFactory);
    let replay = rewindable
        .rewind()
        .expect("Rewindable всегда переигрывается");
    assert!(matches!(replay, http_ng::RequestBody::Full(ref b) if &b[..] == b"y"));
}

/// `RedirectSupport`/`TlsSupport`/`TimeoutSupport`/`UpgradeSupport` — поля
/// `Capabilities`. Нужны не только для чтения (`Capabilities` читаема и без
/// них — `#[non_exhaustive]` на структуре не блокирует доступ к `pub`-полям),
/// а для ЗАПИСИ: собрать свой `Capabilities` для мок-транспорта (например,
/// `MockTransport::with_capabilities`) без них нельзя — тип поля должен быть
/// именуем на стороне вызывающего.
#[test]
fn capability_support_types_are_reachable_from_the_facade() {
    let mut caps = http_ng::Capabilities::none();
    caps.redirects = http_ng::RedirectSupport::Configurable;
    caps.tls_config = http_ng::TlsSupport::Full;
    caps.upgrade = http_ng::UpgradeSupport::H1;
    caps.timeouts = http_ng::TimeoutSupport {
        connect: true,
        first_byte: true,
        between_bytes: false,
    };
    assert_eq!(caps.redirects, http_ng::RedirectSupport::Configurable);
    assert_eq!(caps.tls_config, http_ng::TlsSupport::Full);
    assert_eq!(caps.upgrade, http_ng::UpgradeSupport::H1);
    assert!(caps.timeouts.connect && caps.timeouts.first_byte && !caps.timeouts.between_bytes);
}

/// Сквозной прогон через `mock`: не набор изолированных проверок
/// достижимости, а то, ради чего достижимость вообще нужна — реальный внешний
/// потребитель, зависящий только от `http-ng` (с фичей `test-util`), строит
/// клиент на `MockTransport`, шлёт запрос и читает как успешный, так и
/// оборванный ошибкой ответ, ни разу не написав `http_ng_core::`/
/// `use http_ng_core::unversioned::Transport` — `Client::builder`,
/// `RequestBuilder::send`, `MockTransport::requests()` и
/// `MockTransport::push_response_frames_then_error()` — все обычные
/// (не трейтовые) методы, вызывать которые трейт `Transport` в области
/// видимости не требуется.
#[cfg(feature = "test-util")]
#[test]
fn mock_transport_round_trip_uses_only_facade_types() {
    let m = http_ng::mock::MockTransport::new();
    m.push_response(http::Response::builder().status(200).body("ok").unwrap());

    let client = http_ng::Client::builder(m)
        .build()
        .expect("mock supports the default config");
    let resp = futures_executor::block_on(
        client
            .post("https://a/")
            .body(http_ng::RequestBody::Full(bytes::Bytes::from_static(b"x")))
            .send(),
    )
    .expect("mock replies");
    assert_eq!(resp.status(), 200);

    // `RecordedRequest::retry_kind` — поле, набранное отдельно от
    // `retry_kind_and_rewind_factory_are_reachable_from_the_facade` выше:
    // там `RetryKind` пришёл из `RequestBody::retry_kind()` напрямую, здесь —
    // из поля структуры, которую собрал транспорт. Разные пути к одному и
    // тому же типу, оба обязаны быть именуемы.
    let recorded = client.transport().requests();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].retry_kind, http_ng::RetryKind::Free);

    // `push_response_frames_then_error` — единственное место в публичном API
    // `http-ng`, где `Error` приходит ПАРАМЕТРОМ, а не результатом. Кадр
    // ошибки долетает до `Response::chunk()`, который заново оборачивает его
    // в `Error::new(ErrorKind::Body, ..)` — отсюда `ErrorKind::Body`, а не
    // `ErrorKind::Other`, которым он был на входе.
    let m2 = http_ng::mock::MockTransport::new();
    let empty_frames: Vec<&'static str> = Vec::new();
    m2.push_response_frames_then_error(
        http::Response::builder()
            .status(200)
            .body(empty_frames)
            .unwrap(),
        http_ng::Error::new(
            http_ng::ErrorKind::Other,
            std::io::Error::other("mock probe"),
        ),
    );
    let client2 = http_ng::Client::builder(m2)
        .build()
        .expect("mock supports the default config");
    let mut resp2 =
        futures_executor::block_on(client2.get("https://a/").send()).expect("mock replies");
    match futures_executor::block_on(resp2.chunk()) {
        Some(Err(e)) => assert_eq!(*e.kind(), http_ng::ErrorKind::Body),
        other => panic!("expected a terminal error frame, got {other:?}"),
    }
}
