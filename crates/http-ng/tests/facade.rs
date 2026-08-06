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
