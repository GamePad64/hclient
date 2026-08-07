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

mod caps;
mod convert;
mod promise;

// `body` (the `ReadableStream` bridge for `RequestBody::Streaming`, needing
// `wasm-streams`) lands in a later task of this vertical. This task (Task 3)
// adds request conversion (`convert`) on top of Task 1's `Send`-compatible
// promise adapter and Task 2's runtime capability probing (`caps`); see
// `.superpowers/sdd/2026-08-05-v01-fetch-and-acceptance/task-3-brief.md`.

pub use caps::FORBIDDEN_HEADERS;

use http_ng_core::Capabilities;

/// The browser `fetch` transport.
///
/// Holds one `Capabilities` snapshot, computed once at construction (see
/// `caps::probe`'s doc comment for why that's safe: capability probing
/// never depends on anything that changes over the process's lifetime —
/// the running browser is the running browser).
#[derive(Debug)]
pub struct Fetch {
    caps: Capabilities,
}

impl Fetch {
    pub fn new() -> Self {
        Self {
            caps: caps::probe(),
        }
    }

    /// Test-only accessor for the probed `Capabilities`. `#[doc(hidden)]`:
    /// not part of the crate's advertised API, only reachable so
    /// `tests/caps.rs` (compiled as a separate, external crate) can assert
    /// on the probe's result. `Transport::capabilities` — the real,
    /// non-test-only accessor — lands with the `Transport` impl in a later
    /// task of this vertical.
    ///
    /// Returns an owned `Capabilities`, not `&Capabilities` as the task
    /// brief's illustrative skeleton has it: `Fetch::new().capabilities_for_test()`
    /// — the brief's own test file, step 1, used verbatim in
    /// `tests/caps.rs` — borrows from the `Fetch::new()` temporary, which
    /// is dropped at the end of that statement, so a reference-returning
    /// signature makes every one of the brief's four tests fail to compile
    /// with E0716 regardless of how the method body is written (confirmed
    /// by compiling against the literal `&Capabilities` signature first).
    /// `Capabilities: Clone` (see `http-ng-core/src/caps.rs`), and this is
    /// a `#[doc(hidden)]` test seam, not a hot path, so the clone's cost is
    /// irrelevant.
    #[doc(hidden)]
    pub fn capabilities_for_test(&self) -> Capabilities {
        self.caps.clone()
    }
}

impl Default for Fetch {
    fn default() -> Self {
        Self::new()
    }
}

#[doc(hidden)]
pub mod testing {
    pub use crate::promise::SendJsFuture as SendJsFutureAlias;

    pub fn send_js_future(
        p: js_sys::Promise,
    ) -> impl std::future::Future<Output = Result<wasm_bindgen::JsValue, wasm_bindgen::JsValue>>
    {
        crate::promise::SendJsFuture::new(p)
    }

    /// Fix round 1, finding 2: proves — deterministically, via
    /// `Weak::upgrade`, with no dependence on real `Promise`/browser-event
    /// timing — that dropping `SendJsFuture` while its promise is still
    /// pending does not drop the callbacks still registered against it.
    /// See `promise::SendJsFuture::downgrade_state`'s doc comment for the
    /// full argument.
    pub fn callbacks_survive_dropping_the_future_while_pending(p: js_sys::Promise) -> bool {
        let fut = crate::promise::SendJsFuture::new(p);
        let weak = fut.downgrade_state();
        drop(fut);
        weak.upgrade().is_some()
    }

    /// Converts through this `Fetch`'s own probed `Capabilities` — what
    /// every real call site actually gets. Covers the ordinary path; a test
    /// that needs a `Capabilities` this browser doesn't naturally have
    /// (e.g. `streaming_request_body = true` on a browser lacking `duplex`,
    /// or the reverse) uses [`to_web_request_with_caps`] instead, since a
    /// real `Fetch::new()` can't be made to lie about what it probed.
    pub fn to_web_request(
        f: &crate::Fetch,
        req: http::Request<http_ng_core::RequestBody>,
    ) -> Result<(web_sys::Request, Option<web_sys::AbortController>), http_ng_core::Error> {
        crate::convert::to_web_request(req, &f.caps)
    }

    /// Same conversion, against a caller-supplied `Capabilities` rather than
    /// a real `Fetch`'s probed one — what `tests/convert.rs` needs to
    /// exercise `streaming_request_body = true` deterministically even in a
    /// browser (this one, Chrome) where that's the only value the real
    /// probe could ever produce here, and `= false` even where it's the
    /// only value a real probe couldn't produce here.
    pub fn to_web_request_with_caps(
        req: http::Request<http_ng_core::RequestBody>,
        caps: &http_ng_core::Capabilities,
    ) -> Result<(web_sys::Request, Option<web_sys::AbortController>), http_ng_core::Error> {
        crate::convert::to_web_request(req, caps)
    }

    pub fn check_headers(
        h: &http::HeaderMap,
        caps: &http_ng_core::Capabilities,
    ) -> Result<(), http_ng_core::Error> {
        crate::convert::check_headers(h, caps)
    }
}
