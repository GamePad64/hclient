//! Сквозной пример: тот же прикладной код, который в вертикали 2 заработает
//! на native, а в вертикали 3 — в браузере. Между вертикалями меняется
//! только тип транспорта (`WasiHttp` здесь, что-то вроде `NativeHttp`/
//! `TokioHttp` в вертикали 2, `FetchHttp` в вертикали 3) — вызов
//! `Client::builder(transport).build()` и всё, что после него, нет.
//!
//! # Почему это не `fn main()`
//!
//! Установлено, а не предположено — Task 16 воспроизвёл это на живом
//! прогоне под wasmtime, и тот же вывод независимо получен при подготовке
//! этого примера. Обычный `fn main()`, вызывающий
//! `futures::executor::block_on(fut)`, на `wasm32-wasip2` компилируется в
//! СИНХРОННЫЙ экспорт `wasi:cli/run@0.2.0` — тот, что даёт rustc-таргет из
//! коробки. Синхронная (не async-lifted) корневая задача Component Model не
//! умеет по-настоящему асинхронно ЖДАТЬ (`task.wait`) свои сабтаски, а
//! `wasip3::http::client::send(..).await` внутри `WasiHttp::execute` именно
//! этого и требует, раз `wasi:http` 0.3 — асинхронный протокол. Как только
//! исполнение доходит до точки, где неблокирующе опрашивать больше нечего и
//! нужно по-настоящему подождать сабтаску, wasmtime трапает: `cannot block a
//! synchronous task before returning`. Ждать сабтаски может только
//! АСИНХРОННЫЙ корневой экспорт `wasi:cli/run@0.3.0`, которому его даёт
//! `wasip3::cli::command::export!` — отсюда форма ниже, а не `fn main()`.
//! Подробнее и с источниками (`wasip3::http_compat`) — doc-комментарий
//! `examples/live_roundtrip_guest.rs`, где та же коллизия была найдена
//! первой.
//!
//! Собрать: `cargo build -p http-ng-wasi --example fetch --target
//! wasm32-wasip2`. Запустить (нужен исходящий доступ к сети):
//! `wasmtime run -S http -- target/wasm32-wasip2/debug/examples/fetch.wasm`
//! (флаг `-S http` подключает `wasi:http` 0.3 хосту — без него импорт
//! `wasi:http/outgoing-handler` не свяжется, см. `.cargo/config.toml`, где
//! тот же флаг подключён для `cargo run`/`cargo test`).
//!
//! `#![cfg(target_arch = "wasm32")]`: та же причина, что у
//! `live_roundtrip_guest.rs` — `wasip3::cli::command::export!` генерирует
//! имя экспорта Component Model, которое нативный линковщик отвергает, а
//! `cargo test --workspace` (без `--target`, то есть каждый non-wasip2 CI
//! job) всё равно собирает каждый `[[example]]` воркспейса под хост. Без
//! этого гейта одно только существование файла ломало бы
//! `cargo test --workspace` на всех платформах; с ним — пустой безобидный
//! нативный `cdylib`.
#![cfg(target_arch = "wasm32")]

use http_ng::Client;
use http_ng_wasi::WasiHttp;

wasip3::cli::command::export!(Guest);

struct Guest;

impl wasip3::exports::cli::run::Guest for Guest {
    async fn run() -> Result<(), ()> {
        let client = Client::builder(WasiHttp::new()).build().expect("caps ok");

        let resp = client
            .get("https://example.com/")
            .send()
            .await
            .map_err(|e| {
                eprintln!("request failed: {e}");
            })?;
        let collected = resp.collect().await.map_err(|e| {
            eprintln!("collecting body failed: {e}");
        })?;
        let text = collected.text().map_err(|e| {
            eprintln!("body is not valid UTF-8: {e}");
        })?;

        println!("{} {}", collected.status(), text);
        Ok(())
    }
}
