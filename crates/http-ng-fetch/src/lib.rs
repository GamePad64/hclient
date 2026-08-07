//! http-ng transport over the browser's `fetch`.
//!
//! Depends **only** on Tier A: neither hyper, tokio, nor `http-ng-rt` in the
//! graph (`cargo tree -p http-ng-fetch -e normal` is the check).
//!
//! `deny`, not `forbid` (see `Cargo.toml`'s `[lints.rust]`): this crate
//! carries the project's one `unsafe impl`, in `promise.rs`, and `forbid`
//! cannot be locally relaxed for it. Every other crate in the workspace
//! keeps `forbid`; see `docs/superpowers/specs/2026-08-05-http-ng-design.md`
//! amendment C7.
#![deny(unsafe_code)]

mod promise;

// `body`, `caps` and `convert` land in later tasks of this vertical; this
// task is scoped to the `Send`-compatible promise adapter alone (see
// `.superpowers/sdd/2026-08-05-v01-fetch-and-acceptance/task-1-brief.md`).

#[doc(hidden)]
pub mod testing {
    pub use crate::promise::SendJsFuture as SendJsFutureAlias;

    pub fn send_js_future(
        p: js_sys::Promise,
    ) -> impl std::future::Future<Output = Result<wasm_bindgen::JsValue, wasm_bindgen::JsValue>>
    {
        crate::promise::SendJsFuture::new(p)
    }
}
