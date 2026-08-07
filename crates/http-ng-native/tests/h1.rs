//! Сервер — голый `std::net::TcpListener`, говорящий HTTP/1.1 руками.
//! Никаких серверных фреймворков: тест проверяет наш клиент, а не чужой
//! сервер.
//!
//! IO — `http_ng_native::testing::blocking_io`, неблокирующий `TcpStream`
//! с busy-spin вместо реактора (см. его doc-комментарий за подробным
//! разбором того, почему буквально блокирующий `poll_read` вешает обмен
//! ещё до отправки запроса — `hyper::proto::h1::dispatch::Dispatcher::
//! poll_loop` пробует читать раньше, чем писать, на каждой итерации).
//!
//! # Почему `block_on` здесь обёрнут в `bounded_block_on`, а не голый
//!
//! Это ровно то место, где мутация "инлайн-драйв `Connection` убран"
//! обязана дать красный тест, а не зависший процесс — `body_keeps_driving_
//! the_connection_after_headers` устроен так, что второй чанк ответа придёт
//! только если кто-то продолжает поллить `Connection` ПОСЛЕ заголовков; без
//! этого `NativeBody::poll_frame` вернул бы `Pending` навсегда (канал
//! `Incoming` пуст, а наполнять его больше некому, включая busy-spin
//! `blocking_io`'s собственного `wake_by_ref` — он будит только на
//! готовность СОКЕТА, а сокет тут ни при чём: никто больше не читает из
//! него). Task 3 уже находила ровно такой тест (см. Global Constraints
//! вертикали): тот, что виснет под мутацией вместо падения, глушит CI без
//! имени теста и без диагноза. Тот же приём, что `http-ng-native::connect::
//! tests::bounded_block_on` и `tests/dual_runtime.rs`'s watchdog для
//! `smol`: отдельный поток-сторож + `process::exit(101)`, никакого
//! `Send`-бонда на сам `fut`.
use std::io::{Read, Write};
use std::time::Duration;

const BOUND: Duration = Duration::from_secs(30);

fn spawn_h1_server(response: &'static str) -> std::net::SocketAddr {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in l.incoming() {
            let Ok(mut s) = stream else { continue };
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf);
            let _ = s.write_all(response.as_bytes());
            let _ = s.flush();
        }
    });
    addr
}

/// `futures_executor::block_on`, но с потолком в `BOUND`. Не `F: Send` —
/// только `Arc<AtomicBool>` пересекает границу потока, сам `fut` гоняется
/// на текущем потоке как обычно, так что это не покушается на "рантайм-шов
/// без `Send`", который и доказывают тесты этого файла.
fn bounded_block_on<F: std::future::Future>(fut: F) -> F::Output {
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watchdog_done = done.clone();
    std::thread::spawn(move || {
        std::thread::sleep(BOUND);
        if !watchdog_done.load(std::sync::atomic::Ordering::SeqCst) {
            eprintln!(
                "bounded_block_on: не завершилось за {BOUND:?} - похоже, соединение \
                 перестало поллиться (регресс инлайн-драйва); падаем вместо того, чтобы \
                 повесить CI без имени теста и диагноза"
            );
            std::process::exit(101);
        }
    });
    let result = futures_executor::block_on(fut);
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    result
}

#[test]
fn works_on_a_bare_futures_executor_with_no_spawn() {
    // Ключевой тест вертикали: ни tokio, ни smol — только futures::block_on.
    let addr = spawn_h1_server("HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello");

    bounded_block_on(async move {
        let std_tcp = std::net::TcpStream::connect(addr).unwrap();
        // `blocking_io` реконфигурирует сокет сама (см. её doc-комментарий);
        // здесь проверяется, что hyper не требует ни spawn, ни таймера — не
        // то, в каком режиме сокет.
        let io = http_ng_native::testing::blocking_io(std_tcp);
        let req = http::Request::builder()
            .uri("/")
            .body(http_ng_native::testing::empty_body())
            .unwrap();
        let resp = http_ng_native::testing::exchange_for_test(io, req)
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = http_ng_native::testing::collect(resp.into_body())
            .await
            .unwrap();
        assert_eq!(&body[..], b"hello");
    });
}

#[test]
fn body_keeps_driving_the_connection_after_headers() {
    // Тело приходит отдельным чанком после заголовков: если бы соединение
    // перестали поллить, чтение зависло бы.
    let addr = spawn_h1_server(
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n",
    );
    bounded_block_on(async move {
        let std_tcp = std::net::TcpStream::connect(addr).unwrap();
        let io = http_ng_native::testing::blocking_io(std_tcp);
        let req = http::Request::builder()
            .uri("/")
            .body(http_ng_native::testing::empty_body())
            .unwrap();
        let resp = http_ng_native::testing::exchange_for_test(io, req)
            .await
            .unwrap();
        let body = http_ng_native::testing::collect(resp.into_body())
            .await
            .unwrap();
        assert_eq!(&body[..], b"hello");
    });
}
