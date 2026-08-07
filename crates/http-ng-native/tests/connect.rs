//! Проверяем, что коннектор действительно гоняет Happy Eyeballs: сначала
//! пробуется мёртвый адрес, затем живой, и соединение получается; а когда
//! всё мертво, отказ репортится как `ErrorKind::Connect`.
//!
//! **Почему "мёртвый адрес" здесь — закрытый локальный порт, а не
//! TEST-NET-2.** Брифовская версия этого файла использовала
//! `198.51.100.1` (TEST-NET-2, RFC 5737) на основании "гарантированно не
//! отвечает". Проверено перед тем, как это было принято на веру, не после:
//! в этом контейнере есть интерфейс `tun0` (виден в `ip route`), который
//! прозрачно проксирует весь исходящий трафик — попытка достучаться до
//! `198.51.100.1` реально УСПЕВАЕТ подключиться примерно за 40мс,
//! подтверждено и через `cargo test` (панике здесь предшествовал
//! `TokioIo { .. peer: 198.51.100.1:81 .. }` — то есть реальный TCP-коннект
//! состоялся), и отдельно сырым `socket.connect()` из Python в том же
//! контейнере. Хранить тест, который красный именно здесь и только здесь,
//! бессмысленно: он перестаёт быть сигналом для всех, кто тут работает,
//! в первый же день (тот же вывод, что review Task 4 сделал для похожей
//! ситуации с TEST-NET-1 — тест был просто убран, не оставлен "падать по
//! уважительной причине"). Свойство, которое нас на самом деле волнует —
//! "отказавшая попытка ведёт к следующему адресу" / "все отказавшие попытки
//! репортятся как `ErrorKind::Connect`" — не зависит от ТОГО, ПОЧЕМУ адрес
//! мёртв; закрытый локальный порт (см. `closed_port` ниже, та же
//! конструкция, что `http-ng-rt-smol`'s `adversarial_smol_connect.rs`)
//! отказывает по-настоящему в любом окружении, включая это. Не
//! реинстанцировать TEST-NET-версию этого файла — она красная именно тут.
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
use http_ng_native::testing::connect_for_test;
use http_ng_rt_tokio::Tokio;
use std::net::SocketAddr;
use std::time::Duration;

const BOUND: Duration = Duration::from_secs(30);

/// Binds an ephemeral port and immediately drops the listener, so the
/// address is (bar an astronomically unlikely race with another process on
/// this exact port) guaranteed to refuse connections for the rest of the
/// test process's lifetime. Same construction as `http-ng-rt-smol`'s
/// `adversarial_smol_connect.rs::closed_port` — a closed loopback port
/// refuses for real regardless of whatever proxies external egress.
fn closed_port() -> SocketAddr {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    drop(l);
    addr
}

#[tokio::test]
async fn falls_over_from_a_dead_address_to_a_live_one() {
    let live = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = live.local_addr().unwrap();
    std::thread::spawn(move || {
        let _ = live.accept();
    });

    // `closed_port()` doesn't work here: Happy Eyeballs tries the SAME port
    // on every candidate address (`connect_for_test` takes one shared
    // `port`, not one per address), so a dead address needs its own IP, not
    // its own port. `127.0.0.2` — a distinct loopback address; `live` above
    // is bound strictly to `127.0.0.1`, not `0.0.0.0`, so nothing listens on
    // `127.0.0.2` at `addr.port()` — same class of refusal as
    // `closed_port()`, just addressed differently because the constraint
    // here is "same port, different IP".
    let dead: std::net::IpAddr = "127.0.0.2".parse().unwrap();
    let conn = tokio::time::timeout(
        BOUND,
        connect_for_test(&Tokio, &[dead, addr.ip()], addr.port()),
    )
    .await
    .expect("connect_for_test не должен зависать");
    assert!(conn.is_ok(), "должны дойти до живого адреса");
}

#[tokio::test]
async fn reports_connect_kind_when_everything_is_dead() {
    let dead = closed_port();
    let err = tokio::time::timeout(BOUND, connect_for_test(&Tokio, &[dead.ip()], dead.port()))
        .await
        .expect("connect_for_test не должен зависать")
        .expect_err("closed port must refuse");
    assert!(
        matches!(err.kind(), http_ng_core::ErrorKind::Connect),
        "{err}"
    );
}
