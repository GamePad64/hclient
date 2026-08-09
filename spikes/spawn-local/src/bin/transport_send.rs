//! Negative control 2: **the price**. `Native<TokioLocal, _, _>` is not
//! `Send`, so the property `h1.rs`'s module doc records
//! (`Native<Tokio, Rustls, SystemDns<Tokio>>` is `Send + Sync`) does not
//! survive the swap. Must NOT compile.
//!
//! `cargo build --bin transport_send --features must-fail`

use http_ng_native::Native;
use http_ng_tls::NoTls;
use spawn_local_spike::TokioLocal;

fn assert_send<T: Send>() {}

fn main() {
    // The control: the shipped combination IS Send. This line compiles.
    assert_send::<Native<http_ng_rt_tokio::Tokio, NoTls, ()>>();
    // The cost: with the local runtime it is not.
    assert_send::<Native<TokioLocal, NoTls, ()>>();
}
