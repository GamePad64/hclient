//! Один и тот же код, два рантайма, ноль cfg. Если этот файл потребует
//! `#[cfg]`, рантайм-шов декоративен и вертикаль провалена.
//!
//! `fetch_once` ниже — единственное обобщённое тело: оно упоминает `R`
//! (рантайм) через границы `http_ng_rt::{TcpConnect, Timer, Blocking}` +
//! `Clone`, собирает `Native<R, Rustls, SystemDns<R>>` и гоняет через него
//! реальный HTTP/1.1-запрос к настоящему TCP-серверу на loopback. Ниже два
//! теста инстанцируют его — один раз `http_ng_rt_tokio::Tokio` под
//! `tokio::runtime::Runtime`, один раз `http_ng_rt_smol::Smol` под голым
//! `futures_executor::block_on` (без spawn, без реактора smol целиком —
//! только способности `Smol` сама реализует поверх `async-io`) — и это
//! единственное различие между двумя прогонами. Критерий приёмки вертикали:
//! в этом файле нет ни одного `#[cfg]`.
//!
//! Свойство теста доказано мутацией (см. отчёт задачи 14): добавление
//! `+ Send` к границе `R` в `fetch_once` не ломает ни одну из инстанциаций
//! (обе способности `Send`) — ожидаемо, `Send` не тот шов, который здесь
//! проверяется (единственная известная в этой вертикали Send-асимметрия —
//! `Blocking::run`, а не сам рантайм-тип). Настоящая асимметрия и,
//! соответственно, настоящая проверка чувствительности — добавление
//! `R: PartialEq<std::time::Instant>`-подобной границы через
//! `http_ng_rt::Timer::Instant` ломает `Tokio` (`Instant =
//! tokio::time::Instant`, обёртка) и не ломает `Smol` (`Instant =
//! std::time::Instant`), см. `http-ng-rt-pair-check`'s
//! `pair_property.rs`, откуда и заимствован сам приём мутации.
use http_ng::{Client, Timeouts};
use http_ng_dns_system::SystemDns;
use http_ng_native::Native;
use http_ng_tls_rustls::Rustls;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

fn spawn_server() -> std::net::SocketAddr {
    use std::io::{Read, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let mut b = [0u8; 2048];
            let _ = s.read(&mut b);
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nsame");
        }
    });
    addr
}

/// Обобщённая функция: её тело — тот самый «один код на все рантаймы».
async fn fetch_once<R>(rt: R, addr: std::net::SocketAddr) -> String
where
    R: http_ng_rt::TcpConnect + http_ng_rt::Timer + http_ng_rt::Blocking + Clone,
    R::Stream: 'static,
{
    let t = Native::new(rt.clone(), Rustls::with_webpki_roots(), SystemDns::new(rt));
    let c = Client::builder(t)
        .timeouts(Timeouts {
            connect: Some(Duration::from_secs(5)),
            ..Default::default()
        })
        .build()
        .unwrap();
    c.get(&format!("http://{addr}/"))
        .send()
        .await
        .unwrap()
        .collect()
        .await
        .unwrap()
        .text()
        .unwrap()
}

/// Ограничивает произвольное блокирующее `run` (`tokio::Runtime::block_on`
/// или `futures_executor::block_on`) сторожевым потоком — тот же приём, что
/// `http_ng_native::connect::tests::bounded_block_on` и `tests/h1.rs`,
/// `tests/dual_runtime.rs` этого же workspace: регресс, из-за которого
/// рантайм-шов перестаёт продвигать `fetch_once` (например, `Native`
/// начинает молча ждать реактор, которого на голом `futures`-executor'е
/// нет), обязан дать `FAILED` с диагнозом, а не повесить CI-раннер немым.
/// Обёрнутая работа передаётся замыканием, а не `F: std::future::Future`
/// напрямую — граница переносится через `Arc<AtomicBool>`, сам `fetch_once`
/// внутри замыкания никакого `Send`-бонда не получает.
fn with_watchdog<T>(run: impl FnOnce() -> T) -> T {
    const BOUND: Duration = Duration::from_secs(30);
    let done = Arc::new(AtomicBool::new(false));
    let watchdog_done = done.clone();
    std::thread::spawn(move || {
        std::thread::sleep(BOUND);
        if !watchdog_done.load(Ordering::SeqCst) {
            eprintln!(
                "two_runtimes: не завершилось за {BOUND:?} - похоже, рантайм-шов сломан \
                 (fetch_once перестал продвигаться); падаем вместо того, чтобы повесить CI \
                 без имени теста и диагноза"
            );
            std::process::exit(101);
        }
    });
    let result = run();
    done.store(true, Ordering::SeqCst);
    result
}

#[test]
fn identical_code_on_tokio() {
    let addr = spawn_server();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let text = with_watchdog(|| rt.block_on(fetch_once(http_ng_rt_tokio::Tokio, addr)));
    assert_eq!(text, "same");
}

#[test]
fn identical_code_on_smol() {
    let addr = spawn_server();
    let text =
        with_watchdog(|| futures_executor::block_on(fetch_once(http_ng_rt_smol::Smol, addr)));
    assert_eq!(text, "same");
}
