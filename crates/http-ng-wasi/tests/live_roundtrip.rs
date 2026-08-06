//! Интеграционный прогон `WasiHttp::execute` против настоящего хоста
//! `wasi:http` (Task 16).
//!
//! Закрывает конкретную дыру, описанную doc-комментарием
//! `Body::is_end_stream` (`crates/http-ng-wasi/src/body.rs`): ветка
//! `Inner::Incoming(i) => i.is_end_stream()` не имеет конструктора
//! `IncomingResponseBody` без живого хоста, поэтому её нельзя проверить
//! юнит-тестом — мутационный прогон ревью подтвердил, что замена этой ветки
//! на жёсткий `false` (тот самый баг хостовой стороны `act`, ради которого
//! существует вся эта задача) не роняет ни один тест в `#[cfg(test)]`.
//!
//! Мок-сервер ниже намеренно отвечает `Transfer-Encoding: chunked` с
//! трейлером, а не просто `Content-Length`: без трейлеров у мутации нет
//! внешне наблюдаемого отличия от честной реализации (обе ветки становятся
//! `Inner::Done`/`true` строго в один и тот же вызов `poll_frame`, см.
//! doc-комментарий модуля `examples/live_roundtrip_guest.rs` — там же ссылка
//! на источник `wasip3::http_compat`, откуда это установлено). С трейлером
//! `is_end_stream()` у настоящего хоста становится `true` на кадр раньше,
//! чем наш `Body` сам переходит в `Inner::Done` — вот это окно и ловит
//! гость.
//!
//! # Почему этот файл нативный, а не `#[cfg(target_os = "wasi")]`
//!
//! Первая версия ставила мок-сервер (сырой `TcpListener`) и клиентский
//! вызов `WasiHttp::execute` в одну и ту же гостевую задачу под
//! `wasm32-wasip2`, склеенные через `futures::join!`. Это трапало wasmtime:
//! `cannot block a synchronous task before returning`, как только
//! `client::send` доходил до точки, где ему было по-настоящему нечего
//! неблокирующе опрашивать. Корень: обычный `fn main()`
//! компилируется в СИНХРОННЫЙ экспорт `wasi:cli/run@0.2.0`, а синхронная
//! корневая задача Component Model не может по-настоящему асинхронно ждать
//! (`task.wait`) свои сабтаски — независимо от того, что ещё она попутно
//! делает. Экспорт, которому ждать можно, — асинхронный
//! `wasi:cli/run@0.3.0` (`wasip3::cli::command::export!`), а он несовместим
//! с обычным `fn main()`/`#[test]`-таргетом (см. doc-комментарий
//! `wasip3::cli::command::export!`), только с `cdylib`.
//!
//! Поэтому раздел труда здесь такой:
//! - Мок-сервер — здесь, нативно, плайн `std::net` в отдельном ОС-потоке;
//!   никакого WASI, никакой синхронной/асинхронной коллизии.
//! - Клиентский вызов — `examples/live_roundtrip_guest.rs`, отдельный
//!   `cdylib`-компонент с асинхронным `run()`, запускается под `wasmtime`
//!   как подпроцесс. Единственная задача этого компонента — ждать
//!   `WasiHttp::execute`, ничего синхронного он не делает вовсе.
//!
//! `#![cfg(not(target_arch = "wasm32"))]`: этот файл сам никогда не
//! компилируется под `wasm32-wasip2` — он использует `std::process::Command`
//! для запуска wasmtime, что не имеет смысла (и, вероятно, не работает) из
//! гостя. `cargo test -p http-ng-wasi --target wasm32-wasip2` по-прежнему
//! гоняет 21 чистый юнит-тест из `src/`, этот файл в тот прогон просто не
//! попадает.
#![cfg(not(target_arch = "wasm32"))]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Должно совпадать с `EXPECTED_BODY` в `live_roundtrip_guest.rs`.
const RESPONSE_BODY: &[u8] = b"hello from a real wasi:http host";

#[test]
fn wasi_transport_round_trips_a_real_response_through_wasmtime() {
    let Some(wasmtime) =
        require_wasmtime("wasi_transport_round_trips_a_real_response_through_wasmtime")
    else {
        return;
    };

    let (stdout, stderr, status) = run_guest_against_mock_server(&wasmtime, None, drain_headers);
    if !status.success() || !stdout.contains("ROUNDTRIP_OK") {
        panic!(
            "live wasi:http round-trip failed (exit {:?})\n--- guest stdout ---\n{stdout}\n--- guest stderr ---\n{stderr}",
            status.code(),
        );
    }
}

/// Резолюция review, находка B-1, живой прогон: `Streaming`-тело запроса
/// реально эмитит трейлеры без `Trailer:` в заголовках — `WasiHttp::execute`
/// обязана вернуть ошибку, а не тихо потерять их (измерено: `wasi:http`'s
/// HTTP/1.1-кодировщик роняет необъявленные трейлеры на проводе). Мок-сервер
/// не проверяет сами байты трейлеров на проводе — это уже измерено при
/// подготовке фикс-раунда; тест проверяет, что наш гвард
/// (`convert::TrailerWatch` и `convert::undeclared_trailers`) реально
/// доходит до вызывающей стороны как типизированная ошибка.
#[test]
fn wasi_transport_rejects_streaming_request_trailers_without_a_trailer_header() {
    let Some(wasmtime) = require_wasmtime(
        "wasi_transport_rejects_streaming_request_trailers_without_a_trailer_header",
    ) else {
        return;
    };

    let (stdout, stderr, status) = run_guest_against_mock_server(
        &wasmtime,
        Some("request-trailers-undeclared"),
        drain_request_fully,
    );
    if !status.success() || !stdout.contains("TRAILERS_REJECTED_OK") {
        panic!(
            "expected WasiHttp::execute to reject undeclared streaming request trailers \
             (exit {:?})\n--- guest stdout ---\n{stdout}\n--- guest stderr ---\n{stderr}",
            status.code(),
        );
    }
}

/// Симметрия к тесту выше: тот же `Streaming`-поток с трейлерами, но
/// заголовок `Trailer:` объявлен корректно — гвард не должен ложно
/// срабатывать на легитимное использование трейлеров.
#[test]
fn wasi_transport_accepts_streaming_request_trailers_when_declared() {
    let Some(wasmtime) =
        require_wasmtime("wasi_transport_accepts_streaming_request_trailers_when_declared")
    else {
        return;
    };

    let (stdout, stderr, status) = run_guest_against_mock_server(
        &wasmtime,
        Some("request-trailers-declared"),
        drain_request_fully,
    );
    if !status.success() || !stdout.contains("TRAILERS_ACCEPTED_OK") {
        panic!(
            "expected WasiHttp::execute to accept declared streaming request trailers \
             (exit {:?})\n--- guest stdout ---\n{stdout}\n--- guest stderr ---\n{stderr}",
            status.code(),
        );
    }
}

/// Читает только до конца заголовков запроса — используется сценарием
/// `response-roundtrip`, где у запроса нет тела вовсе (`RequestBody::Empty`),
/// так что дальше и не придёт ничего.
fn drain_headers(stream: &mut std::net::TcpStream) {
    let mut buf = [0u8; 1024];
    let mut seen = Vec::new();
    loop {
        let n = stream.read(&mut buf).expect("read");
        if n == 0 {
            break;
        }
        seen.extend_from_slice(&buf[..n]);
        if seen.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
}

/// Читает, пока не наступит пауза без новых байт — используется сценариями
/// с телом запроса (`request-trailers-*`), где после заголовков ещё придут
/// chunked-кадры данных (и, может быть, трейлеров). Не читает "до EOF":
/// `wasi:http` не обязан закрывать TCP-соединение после тела запроса. Тела
/// в этих тестах — единицы байт, так что даже сильно урезанное окно тишины
/// комфортно перекрывает время, нужное гостю на запись.
fn drain_request_fully(stream: &mut std::net::TcpStream) {
    stream
        .set_read_timeout(Some(std::time::Duration::from_millis(1500)))
        .expect("set_read_timeout");
    let mut buf = [0u8; 4096];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(_) => continue,
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                break;
            }
            Err(e) => panic!("unexpected read error while draining request: {e}"),
        }
    }
}

/// Общая обвязка: поднять мок-сервер (принять одно соединение, слить запрос
/// через `drain`, ответить заранее известным `chunked`+`Trailer:` ответом),
/// собрать гостя и прогнать его под `wasmtime` в заданном режиме, направив
/// на мок-сервер через argv. Возвращает `(stdout, stderr, ExitStatus)`
/// гостя — вызывающий тест сам решает, что считать успехом для своего
/// режима.
fn run_guest_against_mock_server(
    wasmtime: &Path,
    mode: Option<&str>,
    drain: fn(&mut std::net::TcpStream),
) -> (String, String, std::process::ExitStatus) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("local_addr").port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        drain(&mut stream);
        // `chunked` + `Trailer:` — не `Content-Length`: см. doc-комментарий
        // модуля про то, почему только с трейлером у теста вообще есть шанс
        // поймать хардкод-`false` мутацию `is_end_stream()`. Общий ответ для
        // всех режимов — режимы `request-trailers-*` проверяют поведение на
        // СТОРОНЕ ЗАПРОСА и не читают этот ответ содержательно.
        let mut out =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nTrailer: X-Checksum\r\n\r\n"
                .to_vec();
        out.extend_from_slice(format!("{:x}\r\n", RESPONSE_BODY.len()).as_bytes());
        out.extend_from_slice(RESPONSE_BODY);
        out.extend_from_slice(b"\r\n0\r\nX-Checksum: deadbeef\r\n\r\n");
        stream.write_all(&out).expect("write");
        let _ = stream.flush();
    });

    let artifact = build_guest();

    let mut args = vec![
        "run".to_string(),
        "-S".to_string(),
        "http".to_string(),
        "--".to_string(),
        artifact.to_str().expect("utf8 path").to_string(),
        port.to_string(),
    ];
    if let Some(mode) = mode {
        args.push(mode.to_string());
    }
    let output = Command::new(wasmtime)
        .args(&args)
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn wasmtime");

    server.join().expect("mock server thread panicked");

    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status,
    )
}

/// Резолюция review (Task 16, находка B-7): раньше отсутствие `wasmtime`
/// везде вело к одному и тому же — `NOTICE` в stderr и `return` из теста,
/// который сам же выглядит как `ok`. На ноутбуке без `wasmtime` это
/// разумный компромисс, но в CI — ровно тот класс дефекта, что правился во
/// всех остальных job'ах вертикали: зелёный `cargo test` перестаёт
/// означать, что что-то реально проверено. `CI` — переменная, которую
/// GitHub Actions выставляет сама для каждого job'а (и большинство других
/// CI-систем тоже) — здесь превращает "нет `wasmtime`" из тихого пропуска
/// в падение теста: раз мы в CI, `wasmtime` обязан быть установлен
/// (см. job `wasip2` в `.github/workflows/ci.yml`), и его отсутствие —
/// поломка окружения, а не повод молчать.
fn require_wasmtime(test_name: &str) -> Option<PathBuf> {
    if let Some(p) = find_wasmtime() {
        return Some(p);
    }
    if std::env::var_os("CI").is_some() {
        panic!(
            "`wasmtime` не найден в CI (`{test_name}`) — job `wasip2` обязан был установить \
             его перед этим тестом; окружение сломано, а не намеренно ограничено, как на \
             ноутбуке без wasmtime."
        );
    }
    eprintln!(
        "NOTICE: `wasmtime` не найден — живой прогон `{test_name}` пропущен. Эта среда не \
         может подтвердить его против настоящего хоста."
    );
    None
}

fn find_wasmtime() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("WASMTIME") {
        let p = PathBuf::from(path);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let candidate = Path::new(&home).join(".cargo/bin/wasmtime");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    for dir in std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
    {
        let candidate = dir.join("wasmtime");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Собирает `examples/live_roundtrip_guest.rs` под `wasm32-wasip2` и
/// возвращает путь к получившемуся `.wasm`, вычитанный из
/// `--message-format=json` — не собранный вручную по относительному пути
/// (ломается под нестандартным `CARGO_TARGET_DIR`).
fn build_guest() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let output = Command::new(env!("CARGO"))
        .args([
            "build",
            "--manifest-path",
            &format!("{manifest_dir}/Cargo.toml"),
            "--target",
            "wasm32-wasip2",
            "--example",
            "live_roundtrip_guest",
            "--message-format=json",
        ])
        .output()
        .expect("failed to spawn cargo build for the guest");

    if !output.status.success() {
        panic!(
            "failed to build live_roundtrip_guest for wasm32-wasip2 \
             (is the `wasm32-wasip2` rustup target installed?)\n--- stderr ---\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if msg.get("reason").and_then(|r| r.as_str()) != Some("compiler-artifact") {
            continue;
        }
        let is_our_target = msg
            .get("target")
            .and_then(|t| t.get("name"))
            .and_then(|n| n.as_str())
            == Some("live_roundtrip_guest");
        if !is_our_target {
            continue;
        }
        // `cdylib` без `fn main()` не считается "executable" у cargo
        // (это поле остаётся `null`) — путь к `.wasm` берём из `filenames`.
        let wasm = msg
            .get("filenames")
            .and_then(|f| f.as_array())
            .into_iter()
            .flatten()
            .filter_map(|f| f.as_str())
            .find(|f| f.ends_with(".wasm"));
        if let Some(path) = wasm {
            return PathBuf::from(path);
        }
    }
    panic!(
        "cargo build did not report a .wasm artifact for live_roundtrip_guest; raw output:\n{stdout}"
    );
}
