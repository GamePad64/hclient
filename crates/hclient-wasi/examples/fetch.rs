//! End-to-end example: the same application code that will work on native
//! in vertical 2, and in the browser in vertical 3. Only the transport
//! type changes between verticals (`WasiHttp` here, something like
//! `NativeHttp`/`TokioHttp` in vertical 2, `FetchHttp` in vertical 3) —
//! the `Client::builder(transport).build()` call and everything after it,
//! no.
//!
//! # Why this isn't `fn main()`
//!
//! Established, not assumed — Task 16 reproduced this on a live run
//! under wasmtime, and the same conclusion was independently reached
//! while preparing this example. An ordinary `fn main()` calling
//! `futures::executor::block_on(fut)` compiles, on `wasm32-wasip2`, to a
//! SYNCHRONOUS `wasi:cli/run@0.2.0` export — the one the rustc target
//! gives you out of the box. A synchronous (not async-lifted) root task
//! in the Component Model can't genuinely WAIT (`task.wait`) on its
//! subtasks, and `wasip3::http::client::send(..).await` inside
//! `WasiHttp::execute` requires exactly that, since `wasi:http` 0.3 is an
//! asynchronous protocol. The moment execution reaches a point with
//! nothing left to poll non-blockingly and a genuine wait on a subtask is
//! needed, wasmtime traps: `cannot block a synchronous task before
//! returning`. Only an ASYNCHRONOUS root export, `wasi:cli/run@0.3.0`,
//! can wait on subtasks, and `wasip3::cli::command::export!` is what
//! gives you that — hence the shape below, not `fn main()`. More detail
//! and sources (`wasip3::http_compat`) — the doc comment on
//! `examples/live_roundtrip_guest.rs`, where this same collision was
//! found first.
//!
//! Build: `cargo build -p hclient-wasi --example fetch --target
//! wasm32-wasip2`. Run (needs outbound network access): `wasmtime run -S
//! http -- target/wasm32-wasip2/debug/examples/fetch.wasm` (the `-S http`
//! flag wires `wasi:http` 0.3 up to the host — without it the
//! `wasi:http/outgoing-handler` import won't link, see
//! `.cargo/config.toml`, where the same flag is wired up for `cargo
//! run`/`cargo test`).
//!
//! `#![cfg(target_arch = "wasm32")]`: same reason as
//! `live_roundtrip_guest.rs` — `wasip3::cli::command::export!` generates
//! a Component Model export name the native linker rejects, and `cargo
//! test --workspace` (without `--target`, i.e. every non-wasip2 CI job)
//! still builds every `[[example]]` in the workspace for the host
//! regardless. Without this gate, the mere existence of this file would
//! break `cargo test --workspace` on every platform; with it, an empty,
//! harmless native `cdylib` instead.
#![cfg(target_arch = "wasm32")]

use hclient::Client;
use hclient_wasi::WasiHttp;

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
