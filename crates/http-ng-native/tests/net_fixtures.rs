//! Общие сетевые фикстуры для интеграционных тестов этого крейта. Не
//! содержит `#[test]` сама — подключается через `mod net_fixtures;` в
//! `connect.rs` и `dual_runtime.rs` (тот же приём, что `server.rs` в
//! `http-ng-tls-rustls/tests/`: обычный файл `tests/*.rs`, который cargo
//! всё равно скомпилирует как отдельный — пустой — тестовый бинарник, но
//! который также подключается как модуль там, где нужна общая логика).
//!
//! # Почему это отдельный файл, а не просто "будь внимательнее в каждом"
//!
//! Review round 1 нашло один и тот же класс бага дважды в одной задаче:
//! `tests/connect.rs`'s первая версия `falls_over_from_a_dead_address_to_a_live_one`
//! комбинировала IP закрытого порта с портом ЖИВОГО слушателя (у обоих
//! адресов один и тот же порт нужен — `connect_for_test`/`connect` берут
//! ОДИН порт на все кандидатовские адреса, Happy Eyeballs пробует один и
//! тот же порт на разных IP, не разные порты на одном), что было поймано
//! и исправлено. Двадцать минут спустя та же ошибка независимо
//! воспроизвелась в `tests/dual_runtime.rs`'s `dead_and_live()`: она
//! возвращала IP закрытого слушателя вместе с портом ОТДЕЛЬНОГО живого
//! слушателя — оба слушателя были на `127.0.0.1`, так что "мёртвый" адрес
//! на самом деле совпадал с живым (`dead == live.ip()`), и тест реально
//! соединялся с живым слушателем за ~230µs, не проверяя отказ вообще.
//!
//! Вывод не "быть внимательнее" — бдительность уже не сработала один раз
//! в тех же двадцати минутах. Вывод: ловушка была в ФОРМЕ конструктора
//! (звать `TcpListener::bind` дважды и вручную комбинировать `.ip()` с
//! чужим `.port()`), а не в конкретном месте, где её один раз забыли
//! проверить. `dead_and_live()` ниже устраняет саму форму: "мёртвый" IP —
//! ЛИТЕРАЛЬНАЯ константа (`127.0.0.2`), не производная от какого-либо
//! слушателя, так что скомбинировать её с портом живого слушателя и
//! случайно получить живой адрес попросту невозможно — не проверить
//! перед использованием, а невозможно написать. Оба файла берут эту
//! функцию отсюда, а не пишут свою.
// `#![allow(dead_code)]`, не `#[cfg(test)]`-манипуляции и не `#[expect]`:
// каждый `tests/*.rs`-файл, включающий этот через `mod net_fixtures;`,
// компилирует его заново как ОТДЕЛЬНЫЙ бинарник, и не каждый использует
// ОБЕ функции (`dual_runtime.rs` берёт только `dead_and_live`, не
// `closed_port`) — `#[expect(dead_code)]` был бы то удовлетворён, то нет
// в зависимости от того, какой бинарник компилируется, и заменил бы одно
// предупреждение другим (`unfulfilled_lint_expectations`) в тех
// бинарниках, где функция как раз используется.
#![allow(dead_code)]

use std::net::{IpAddr, SocketAddr};

/// Binds an ephemeral port and immediately drops the listener, so the
/// address is (bar an astronomically unlikely race with another process on
/// this exact port) guaranteed to refuse connections for the rest of the
/// test process's lifetime. Same construction as `http-ng-rt-smol`'s
/// `adversarial_smol_connect.rs::closed_port` — a closed loopback port
/// refuses for real regardless of whatever proxies external egress.
///
/// Only safe to use for a SINGLE candidate address (its IP and port must
/// be used together) — see `dead_and_live` below for the two-address
/// fallback case, which needs a different construction entirely.
pub fn closed_port() -> SocketAddr {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    drop(l);
    addr
}

/// Returns `(dead, live)` such that connecting to `dead` at `live.port()`
/// is genuinely refused, and connecting to `live` itself succeeds (a
/// background thread accepts exactly one connection). Structurally cannot
/// return a live address for `dead`: `dead` is a hardcoded, different
/// loopback address (`127.0.0.2`) than the one `live` binds to
/// (`127.0.0.1`) — there is no listener whose own IP or port `dead` is
/// derived from, so there is nothing for a caller to accidentally reuse
/// the way the bug this function replaced did (see the module doc).
pub fn dead_and_live() -> (IpAddr, SocketAddr) {
    let live = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let live_addr = live.local_addr().unwrap();
    std::thread::spawn(move || {
        let _ = live.accept();
    });
    let dead: IpAddr = "127.0.0.2".parse().unwrap();
    (dead, live_addr)
}
