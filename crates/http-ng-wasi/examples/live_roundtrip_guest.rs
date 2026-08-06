//! Гость для `tests/live_roundtrip.rs` (Task 16) — единственное место в
//! проекте, где `Transport::execute` реально исполняется против настоящего
//! хоста `wasi:http`, а не только компилируется.
//!
//! Не `fn main()`. Обычный `fn main()` на `wasm32-wasip2` компилируется в
//! СИНХРОННЫЙ экспорт `wasi:cli/run@0.2.0` — тот, что даёт rustc-таргет из
//! коробки. Синхронная (не async-lifted) корневая задача Component Model не
//! может по-настоящему асинхронно ЖДАТЬ (`task.wait`) свои сабтаски: первая
//! версия этого гостя была именно `fn main()` со своими `std::net`-сокетами
//! внутри самого гостя, и как только `wasip3::http::client::send(..).await`
//! доходил до точки, где ему нечего было опрашивать неблокирующе и
//! требовалось по-настоящему подождать, wasmtime трапал:
//! `cannot block a synchronous task before returning`. `wasip3::cli::command::export!`
//! экспортирует АСИНХРОННЫЙ `wasi:cli/run@0.3.0`, которому ждать сабтаски
//! можно — именно этого и требует настоящий вызов `WasiHttp::execute`, раз
//! `wasi:http` 0.3 — асинхронный протокол. Мок-сервер поэтому живёт не
//! здесь, а нативно на стороне `tests/live_roundtrip.rs`, вне WASI вообще —
//! никакого смешения синхронных `wasi:sockets`-вызовов с этой асинхронной
//! задачей.
//!
//! # Почему ответ — chunked с трейлерами, а не просто `Content-Length`
//!
//! Эмпирически проверено (см. `wasip3::http_compat::IncomingBody::poll_frame`
//! /`is_end_stream`, `wasip3-0.7.0+wasi-0.3.0/src/http_compat/mod.rs:216-283`):
//! для тела без трейлеров `i.is_end_stream()` становится `true` РОВНО в тот
//! же вызов `poll_frame`, что возвращает `Ready(None)` — то есть ровно тогда
//! же, когда наш `Body::poll_frame` сам переводит `self.inner` в
//! `Inner::Done`. В этом случае у хардкод-`false`-мутации нет внешне
//! наблюдаемого отличия от честной реализации: обе ветки недостижимы порознь
//! (проверено вручную обоими способами при подготовке этого теста).
//! Но когда есть трейлеры, `IncomingBody` выставляет свой внутренний
//! `IncomingState::Done` РАНЬШЕ — в момент, когда он ещё возвращает
//! `Ready(Some(Ok(trailers_frame)))`, а не `Ready(None)`. Наш `Body::poll_frame`
//! на ветке `Ready(Some(Ok(f)))` состояние `self.inner` не меняет — значит в
//! этот момент оно ещё `Inner::Incoming`, а `i.is_end_stream()` уже `true`.
//! Вот это окно и проверяется ниже: единственное место, где
//! `Inner::Incoming(i) => i.is_end_stream()` реально отличим от
//! `Inner::Incoming(_) => false`.
//!
//! `#![cfg(target_arch = "wasm32")]`: `wasip3::cli::command::export!`
//! generates a component-model export name
//! (`[async-lift]wasi:cli/run@0.3.0#run`) that the native linker rejects
//! outright — `cargo test --workspace` (no `--target`, i.e. every non-wasip2
//! CI job) still visits every `[[example]]` in the workspace to build it for
//! the host, so without this gate the mere existence of this file would
//! break `cargo test --workspace` on every platform. Gated out, it compiles
//! to an empty, harmless native `cdylib` there instead.
#![cfg(target_arch = "wasm32")]

use http_body::Body as _;
use http_ng_core::RequestBody;
use http_ng_core::unversioned::Transport;
use std::future::poll_fn;
use std::pin::Pin;

wasip3::cli::command::export!(Guest);

struct Guest;

/// Должно совпадать с данными чанка, который пишет мок-сервер в
/// `tests/live_roundtrip.rs`.
const EXPECTED_BODY: &[u8] = b"hello from a real wasi:http host";

impl wasip3::exports::cli::run::Guest for Guest {
    async fn run() -> Result<(), ()> {
        let args = wasip3::cli::environment::get_arguments();
        let port: u16 = args
            .get(1)
            .unwrap_or_else(|| {
                eprintln!("usage: live_roundtrip_guest <port>");
                std::process::abort()
            })
            .parse()
            .unwrap_or_else(|e| {
                eprintln!("port must be numeric: {e}");
                std::process::abort()
            });

        let uri: http::Uri = format!("http://127.0.0.1:{port}/probe")
            .parse()
            .expect("uri");
        let req = http::Request::builder()
            .method(http::Method::GET)
            .uri(uri)
            .body(RequestBody::Empty)
            .expect("request");

        let transport = http_ng_wasi::WasiHttp::new();
        let resp = transport.execute(req).await.map_err(|e| {
            eprintln!("execute failed: {e}");
        })?;
        if resp.status() != http::StatusCode::OK {
            eprintln!("unexpected status: {}", resp.status());
            return Err(());
        }

        let mut body = resp.into_body();
        let mut collected = Vec::new();
        let mut end_flagged_at_trailers = false;
        let mut saw_trailers = false;
        loop {
            match poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await {
                Some(Ok(f)) => {
                    if f.is_trailers() {
                        // Ключевая проверка задачи, и ключевая по срокам:
                        // опрашиваем `is_end_stream()` СРАЗУ после кадра
                        // трейлеров, ДО следующего `poll_frame` — то есть
                        // пока `Body::inner` ещё `Inner::Incoming`, а не
                        // `Inner::Done` (тот наступит только на следующем
                        // `Ready(None)`). Именно в этом окне честная ветка
                        // `Inner::Incoming(i) => i.is_end_stream()` реально
                        // отличима от хардкод-`false` мутации — см.
                        // doc-комментарий модуля про то, почему без
                        // трейлеров такого окна нет вовсе.
                        saw_trailers = true;
                        end_flagged_at_trailers = body.is_end_stream();
                    } else if let Ok(data) = f.into_data() {
                        collected.extend_from_slice(&data);
                    }
                }
                Some(Err(e)) => {
                    eprintln!("body error: {e}");
                    return Err(());
                }
                None => break,
            }
        }

        if collected != EXPECTED_BODY {
            eprintln!("body mismatch: {:?}", String::from_utf8_lossy(&collected));
            return Err(());
        }
        if !saw_trailers {
            eprintln!("expected a trailers frame from the mock server, got none");
            return Err(());
        }
        if !end_flagged_at_trailers {
            eprintln!(
                "is_end_stream() must already report true right after the trailers frame \
                 arrives, while Body::inner is still Inner::Incoming — this is the exact gap \
                 Task 16 exists to close, see the doc-comment on Body::is_end_stream"
            );
            return Err(());
        }

        println!("ROUNDTRIP_OK");
        Ok(())
    }
}
