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
