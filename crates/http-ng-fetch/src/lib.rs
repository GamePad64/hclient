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

mod body;
mod caps;
mod convert;
mod promise;

// Task 4 adds `body` — the RESPONSE body bridge over `ReadableStream`
// (`web_sys::Response::body()` in, a `http_body::Body` out). It is the
// mirror image of `convert::resolve_body`'s REQUEST-body direction: that
// function still cannot forward a streaming REQUEST body (no
// `wasm-streams` dependency, `ErrorKind::Unsupported` today — unchanged by
// this task), while this module is what actually adds `wasm-streams` to
// the crate, pinned to the 0.5 line for the MSRV reason recorded in
// `Cargo.toml`. See
// `.superpowers/sdd/2026-08-05-v01-fetch-and-acceptance/task-4-brief.md`.
//
// Task 5 (this file's `impl Transport for Fetch`, below) composes Tasks
// 1-4 into the third backend in the project, after `wasi:http` and native:
// `convert::to_web_request` (Task 3) turns the request into a
// `web_sys::Request`, the browser's own `fetch` (found through `js_sys::
// global()`, not `web_sys::window()`, so this also works from a Worker —
// see `execute`'s own comment) sends it, `Body::from_response` (Task 4)
// wraps the response, and `promise::SendJsFuture` (Task 1) is what makes
// awaiting that whole exchange a `Send` future in the first place. See
// `.superpowers/sdd/2026-08-05-v01-fetch-and-acceptance/task-5-brief.md`.
//
// **One deliberate deviation from the brief's own Step 3 reference code.**
// That code destructures `to_web_request`'s result as
// `let (request, _abort) = ..;` and never touches the `AbortController`
// again — but `to_web_request`'s own doc comment (Task 3, `convert.rs`)
// says explicitly that a later task, "the `Transport` impl, driving
// deadlines and cancellation," needs to HOLD ONTO it after the function
// returns. An immediately-discarded local binding doesn't hold onto
// anything: if a caller races `execute`'s future against something else
// (there's no `tokio::time::timeout` on wasm, but `futures::select!` or an
// equivalent works the same way) and drops it while the fetch promise is
// still pending, the brief's own code leaves the browser's `fetch()`
// running to completion, unseen, for nothing — the in-flight-cancellation
// capability `AbortController` exists to provide is built, handed over,
// and then thrown away. [`AbortOnDrop`] below closes exactly that gap:
// armed across the one `.await` in `execute` that can actually be
// interrupted, `defuse`d the instant the promise settles (success or
// failure) — never later, because aborting AFTER a successful fetch would
// also abort the response body stream the caller hasn't started reading
// yet (the Fetch Standard propagates a signal's abort to an already-
// obtained `Response`'s body). See `tests/transport.rs`'s
// `dropping_the_guard_before_defuse_aborts_the_signal`/
// `defusing_the_guard_prevents_the_abort` for the deterministic, network-
// free proof, and this task's report for why this isn't presented as a
// silent fix.

pub use body::Body;
pub use caps::FORBIDDEN_HEADERS;

use http_ng_core::unversioned::Transport;
use http_ng_core::{Capabilities, Error, ErrorKind, RequestBody};
use wasm_bindgen::JsCast;

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

/// Cancels the `AbortController`'s signal when dropped, unless [`defuse`]
/// was called first — see the module doc comment's section on why the
/// brief's own `_abort` discard doesn't honor what `convert::to_web_request`
/// asked of a "later task."
///
/// [`defuse`]: AbortOnDrop::defuse
struct AbortOnDrop(Option<web_sys::AbortController>);

impl AbortOnDrop {
    /// Consumes the guard without aborting. Called exactly once in
    /// `execute`, immediately after the fetch promise settles (`Ok` or
    /// `Err`): at that point there is nothing left to cancel, and calling
    /// `abort()` anyway would, for a SUCCESSFUL fetch, also abort the
    /// response body's `ReadableStream` before the caller has read a single
    /// byte of it — the Fetch Standard propagates a signal's abort to an
    /// already-obtained `Response`, not just to the initial request.
    fn defuse(mut self) {
        self.0 = None;
    }
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if let Some(c) = self.0.take() {
            c.abort();
        }
    }
}

impl Transport for Fetch {
    type Body = Body;
    type Error = Error;

    async fn execute(
        &self,
        req: http::Request<RequestBody>,
    ) -> Result<http::Response<Body>, Error> {
        let (request, abort) = convert::to_web_request(req, &self.caps)?;
        let guard = AbortOnDrop(abort);

        // `fetch` lives in both `Window` and `WorkerGlobalScope` — go
        // through `js_sys::global()`, not `web_sys::window()`, so this
        // works in both contexts, not just a page's main thread.
        let global = js_sys::global();
        let fetch_fn = js_sys::Reflect::get(&global, &wasm_bindgen::JsValue::from_str("fetch"))
            .map_err(convert::js_err)?
            .dyn_into::<js_sys::Function>()
            .map_err(convert::js_err)?;
        let promise = fetch_fn
            .call1(&global, &request)
            .map_err(convert::js_err)?
            .dyn_into::<js_sys::Promise>()
            .map_err(convert::js_err)?;

        let outcome = promise::SendJsFuture::new(promise).await;
        // The promise has settled — see `AbortOnDrop::defuse`'s doc
        // comment for why this must happen here, before anything else, on
        // BOTH the success and the failure path.
        guard.defuse();

        let value = outcome.map_err(|e| {
            // fetch distinguishes a network error only by `TypeError` —
            // that's all the browser gives you, and it's recorded in
            // `Capabilities` (no separate "resolve failed" concept exists
            // for this backend, unlike native/wasi).
            let base = convert::js_err(e);
            Error::new(ErrorKind::Connect, base)
        })?;
        let resp: web_sys::Response = value.dyn_into().map_err(convert::js_err)?;

        let mut builder = http::Response::builder().status(resp.status());
        let headers = resp.headers();
        let iter = js_sys::try_iter(&headers).map_err(convert::js_err)?;
        if let Some(iter) = iter {
            for entry in iter {
                let entry = entry.map_err(convert::js_err)?;
                let pair: js_sys::Array = entry.into();
                let (Some(k), Some(v)) = (pair.get(0).as_string(), pair.get(1).as_string()) else {
                    continue;
                };
                builder = builder.header(k, v);
            }
        }
        let body = Body::from_response(&resp)?;
        builder
            .body(body)
            .map_err(|e| Error::new(ErrorKind::Other, e))
    }

    /// Identity, not the default wrapping: `Self::Error` is already
    /// `http_ng_core::Error`, and every fallible step in `execute`
    /// (`convert::to_web_request`, the fetch call, response-building,
    /// `Body::from_response`) has already set its own category. Without
    /// this override `Client::execute` would still behave identically
    /// (the default recognizes an already-`Error` and passes it through
    /// unchanged) — the line is behaviorally redundant and semantically
    /// needed anyway: it names the intent where it's read, and it survives
    /// a future change to the default. Same reasoning, same wording, as
    /// `http-ng-native`'s and `http-ng-wasi`'s identical overrides.
    fn to_error(&self, e: Self::Error) -> Error {
        e
    }

    fn capabilities(&self) -> &Capabilities {
        &self.caps
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

    /// Builds a [`crate::Body`] from a `web_sys::Response` — the only path
    /// to `Body::from_response` outside this crate, `#[doc(hidden)]` like
    /// everything else in this module; the real, non-test-only path is the
    /// eventual `Transport` impl.
    pub fn body_from_response(
        resp: &web_sys::Response,
    ) -> Result<crate::Body, http_ng_core::Error> {
        crate::body::Body::from_response(resp)
    }

    /// Fetches `url` with a plain `GET` and returns the collected response
    /// body — the smallest possible end-to-end exercise of
    /// `Body::from_response` against a REAL `fetch()` round trip (as
    /// opposed to `tests/body.rs`'s own hand-built `ReadableStream`s, which
    /// prove the edge cases a real network response can't be made to
    /// produce on demand: a mid-stream error, a non-byte chunk, cancel-on-
    /// drop).
    ///
    /// `_f: &crate::Fetch` is intentionally unused: a bare `GET` with no
    /// custom header, body, or timeout needs none of `Fetch`'s probed
    /// `Capabilities`, and this helper deliberately does NOT route through
    /// `convert::to_web_request` — that function's `checked_url` accepts
    /// only `http`/`https` schemes (Task 3), which would reject exactly the
    /// `data:` URL this crate's own brief specifies for a deterministic,
    /// no-network test. The parameter stays in the signature only because
    /// the brief's own test (`tests/body.rs`,
    /// `streams_a_response_body_in_chunks`) calls
    /// `testing::fetch_body(&f, url)` verbatim — dropping it would break
    /// that call, which this task was told to keep failing-then-passing,
    /// not rewrite.
    pub async fn fetch_body(
        _f: &crate::Fetch,
        url: &str,
    ) -> Result<bytes::Bytes, http_ng_core::Error> {
        use wasm_bindgen::JsCast;
        let window = web_sys::window().expect("fetch tests run inside a browser window");
        let resp_value = crate::promise::SendJsFuture::new(window.fetch_with_str(url))
            .await
            .map_err(crate::convert::js_err)?;
        let response: web_sys::Response = resp_value.unchecked_into();
        let mut body = crate::body::Body::from_response(&response)?;
        collect(&mut body).await
    }

    /// Drains a [`crate::Body`] into `Bytes` by hand-polling `poll_frame`
    /// via `std::future::poll_fn` — no `http-body-util` dependency added
    /// just for this one hidden test helper (see `Cargo.toml`'s dependency
    /// history for how deliberately each addition here is tracked).
    async fn collect(body: &mut crate::Body) -> Result<bytes::Bytes, http_ng_core::Error> {
        use http_body::Body as _;
        let mut buf = bytes::BytesMut::new();
        loop {
            match std::future::poll_fn(|cx| std::pin::Pin::new(&mut *body).poll_frame(cx)).await {
                Some(Ok(frame)) => {
                    if let Ok(data) = frame.into_data() {
                        buf.extend_from_slice(&data);
                    }
                }
                Some(Err(e)) => return Err(e),
                None => break,
            }
        }
        Ok(buf.freeze())
    }

    /// Exercises [`crate::AbortOnDrop`] directly and deterministically — no
    /// network involved, `AbortController`/`AbortSignal` are plain,
    /// synchronous browser objects, unlike a real `fetch()`'s timing (which
    /// this test environment has no way to bound or control on demand; see
    /// `tests/transport.rs`'s own comment on why this isn't tested by
    /// racing a real request against a timer instead).
    ///
    /// `defuse: true` proves the happy path leaves the signal untouched
    /// (`AbortOnDrop::defuse`'s doc comment: aborting after a successful
    /// fetch would also abort the response body the caller hasn't read
    /// yet). `defuse: false` proves the guard genuinely cancels when
    /// dropped without ever being defused — the case that matters, an
    /// `execute()` future abandoned mid-flight.
    pub fn abort_guard_behavior(defuse: bool) -> bool {
        let controller =
            web_sys::AbortController::new().expect("AbortController::new in a real browser");
        // Read the signal from the controller BEFORE moving the controller
        // into the guard — `AbortSignal` is a distinct object from
        // `AbortController`, `AbortController::signal` takes `&self`, and
        // this is the only handle this function keeps to check the
        // outcome against afterward.
        let signal = controller.signal();
        let guard = crate::AbortOnDrop(Some(controller));
        if defuse {
            guard.defuse();
        } else {
            // Explicit, not left to fall out of scope at the end of the
            // function: a function's tail expression is evaluated BEFORE
            // its still-live locals are dropped, so `signal.aborted()`
            // below would observe the guard's PRE-drop state (always
            // `false`) if `guard` were merely left to drop implicitly
            // here. Caught by running this exact function without the
            // `drop(guard)` call — see the task report.
            drop(guard);
        }
        signal.aborted()
    }
}
