//! Проверяем, что коннектор действительно гоняет Happy Eyeballs: сначала
//! пробуется мёртвый адрес, затем живой, и соединение получается; а когда
//! всё мертво, отказ репортится как `ErrorKind::Connect`.
//!
//! **Почему "мёртвый адрес" здесь — не TEST-NET-2.** Брифовская версия
//! этого файла использовала `198.51.100.1` (TEST-NET-2, RFC 5737) на
//! основании "гарантированно не отвечает". Проверено перед тем, как это
//! было принято на веру, не после: в этом контейнере есть интерфейс
//! `tun0` (виден в `ip route`), который прозрачно проксирует весь
//! исходящий трафик — попытка достучаться до `198.51.100.1` реально
//! УСПЕВАЕТ подключиться примерно за 40мс, подтверждено и через
//! `cargo test`, и отдельно сырым `socket.connect()` из Python в том же
//! контейнере. Хранить тест, который красный именно здесь и только здесь,
//! бессмысленно: он перестаёт быть сигналом для всех, кто тут работает, в
//! первый же день (тот же вывод, что review Task 4 сделал для похожей
//! ситуации с TEST-NET-1). Свойство, которое нас на самом деле волнует —
//! "отказавшая попытка ведёт к следующему адресу" / "все отказавшие
//! попытки репортятся как `ErrorKind::Connect`" — не зависит от ТОГО,
//! ПОЧЕМУ адрес мёртв; фикстуры из `net_fixtures` (см. `mod` ниже)
//! отказывают по-настоящему в любом окружении. Не реинстанцировать
//! TEST-NET-версию этого файла — она красная именно тут.
//!
//! Оба теста ниже обёрнуты в `tokio::time::timeout`, а не оставлены голым
//! `.await`: "мёртвый" адрес — по конструкции то самое место, где мутация в
//! цикле `drive` (например, потерянный `mark_v6_done`/`mark_v4_done` или
//! сломанное условие `Exhausted`) превращает тест не в красный, а в
//! зависший навсегда — Task 3 уже находила ровно такой тест, и это была не
//! гипотетическая, а реальная находка (см. Global Constraints этой
//! вертикали). Граница щедрая (30с), но конечная; на закрытом локальном
//! порту реальное время много меньше — `ECONNREFUSED` приходит от ядра, а
//! не от таймаута.
mod net_fixtures;

use http_ng_native::testing::connect_for_test;
use http_ng_rt_tokio::Tokio;
use std::time::Duration;

const BOUND: Duration = Duration::from_secs(30);

#[tokio::test]
async fn falls_over_from_a_dead_address_to_a_live_one() {
    let (dead, live) = net_fixtures::dead_and_live();
    let conn = tokio::time::timeout(
        BOUND,
        connect_for_test(&Tokio, &[dead, live.ip()], live.port()),
    )
    .await
    .expect("connect_for_test не должен зависать");
    assert!(conn.is_ok(), "должны дойти до живого адреса");
}

#[tokio::test]
async fn reports_connect_kind_when_everything_is_dead() {
    let dead = net_fixtures::closed_port();
    let err = tokio::time::timeout(BOUND, connect_for_test(&Tokio, &[dead.ip()], dead.port()))
        .await
        .expect("connect_for_test не должен зависать")
        .expect_err("closed port must refuse");
    assert!(
        matches!(err.kind(), http_ng_core::ErrorKind::Connect),
        "{err}"
    );
}
