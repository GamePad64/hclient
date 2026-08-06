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
    let Some(wasmtime) = find_wasmtime() else {
        // Громко, а не молча — тот же принцип, что у `::notice::`-пропусков
        // в `.github/workflows/ci.yml`: немой зелёный тест опаснее красного.
        eprintln!(
            "NOTICE: `wasmtime` не найден — живой прогон wasi:http пропущен. \
             Эта среда не может подтвердить Body::is_end_stream() против \
             настоящего хоста (crates/http-ng-wasi/src/body.rs)."
        );
        return;
    };

    // 1. Нативный мок-сервер: голый HTTP/1.1 от руки, в отдельном ОС-потоке.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("local_addr").port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
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
        // `chunked` + `Trailer:` — не `Content-Length`: см. doc-комментарий
        // модуля про то, почему только с трейлером у теста вообще есть шанс
        // поймать хардкод-`false` мутацию `is_end_stream()`.
        let mut out =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nTrailer: X-Checksum\r\n\r\n"
                .to_vec();
        out.extend_from_slice(format!("{:x}\r\n", RESPONSE_BODY.len()).as_bytes());
        out.extend_from_slice(RESPONSE_BODY);
        out.extend_from_slice(b"\r\n0\r\nX-Checksum: deadbeef\r\n\r\n");
        stream.write_all(&out).expect("write");
        let _ = stream.flush();
    });

    // 2. Собрать гостя. `--message-format=json` — чтобы взять реальный путь
    //    артефакта у cargo, а не гадать его по относительному пути (тот
    //    гадает неверно при нестандартном `CARGO_TARGET_DIR`).
    let artifact = build_guest();

    // 3. Прогнать под тем же раннером, что настроен в `.cargo/config.toml`
    //    (`-S http`), направив гостя на мок-сервер через argv.
    let output = Command::new(&wasmtime)
        .args([
            "run",
            "-S",
            "http",
            "--",
            artifact.to_str().expect("utf8 path"),
            &port.to_string(),
        ])
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn wasmtime");

    server.join().expect("mock server thread panicked");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() || !stdout.contains("ROUNDTRIP_OK") {
        panic!(
            "live wasi:http round-trip failed (exit {:?})\n--- guest stdout ---\n{stdout}\n--- guest stderr ---\n{stderr}",
            output.status.code(),
        );
    }
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
