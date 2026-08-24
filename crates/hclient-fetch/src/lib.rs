//! hclient transport over the browser's `fetch`.
//!
//! Depends **only** on Tier A: neither hyper, tokio, nor `hclient-rt` in the
//! graph (`cargo tree -p hclient-fetch -e normal` is the check).
//!
//! `deny`, not `forbid` (see `Cargo.toml`'s `[lints.rust]`): this crate
//! carries the project's one `unsafe impl`, in `promise.rs`, and `forbid`
//! cannot be locally relaxed for it. Every other crate in the workspace
//! keeps `forbid`; see `docs/exceptions.md`, amendment C7.
#![deny(unsafe_code)]

mod body;
mod caps;
mod convert;
mod hooks;
pub mod opts;
mod promise;
mod timer;
mod websocket;

// `body` is the RESPONSE body bridge over `ReadableStream`
// (`web_sys::Response::body()` in, a `http_body::Body` out), the mirror
// image of `convert::resolve_body`'s REQUEST-body direction. It is what
// brings `wasm-streams` into the crate, pinned to the 0.5 line for the
// MSRV reason recorded in `Cargo.toml`.
//
// `impl Transport for Fetch`, below, composes the modules:
// `convert::to_web_request` turns the request into a `web_sys::Request`,
// the browser's own `fetch` (found through `js_sys::global()`, not
// `web_sys::window()`, so this also works from a Worker — see `execute`'s
// own comment) sends it, `Body::from_response` wraps the response, and
// `promise::SendJsFuture` is what makes awaiting that whole exchange a
// `Send` future in the first place.
//
// **One deliberate deviation from the brief's own Step 3 reference code.**
// That code destructures `to_web_request`'s result as
// `let (request, _abort) = ..;` and never touches the `AbortController`
// again — but `to_web_request`'s own doc comment
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
pub use timer::BrowserClock;
// The WebSocket seam, over the browser's own `WebSocket` global. Not
// behind a feature — see `websocket.rs`'s `impl
// WebSocketConnect` for the measurement that decided it. `Fetch` is the
// connector (`hclient_core::unversioned::WebSocketConnect`), so this is the
// only type the module has to export.
pub use websocket::FetchWebSocket;

use hclient_core::unversioned::{ConnectionId, Event, Head, Hooks, NoHooks, Transport};
use hclient_core::{Capabilities, Error, ErrorKind, RequestBody};
use wasm_bindgen::JsCast;

/// The browser `fetch` transport.
///
/// Holds one `Capabilities` snapshot, computed once at construction (see
/// `caps::probe`'s doc comment for why that's safe: capability probing
/// never depends on anything that changes over the process's lifetime —
/// the running browser is the running browser).
///
/// # `H`, the observability hook
///
/// [`NoHooks`] by default — a zero-sized type whose `Hooks::WATCHING` is
/// `false` — so `Fetch` still names the transport it always named, and a
/// build that asks for nothing reads no clock and clones no `Uri`.
/// [`Fetch::hooks`] is how a caller asks; what comes back is a *different
/// type*, because the hook is a type parameter rather than a
/// `Box<dyn Hooks>`, which is the whole of the zero-cost claim.
///
/// **This backend emits exactly one of the four events**, and which one is
/// the finding rather than an omission: see `crate::hooks`'s module doc
/// for why a transport that owns no connection has nothing to put in
/// `Connected`, `Reused` or `Closed`, and which two fields of `Head`
/// itself it cannot fill either.
#[derive(Debug)]
pub struct Fetch<H = NoHooks> {
    caps: Capabilities,
    /// The `RequestInit` members this transport sets beyond the five
    /// `to_web_request` always writes — see [`crate::opts`], including why
    /// `redirect` is not among them.
    opts: opts::FetchOpts,
    /// Where the events go. `NoHooks` is a ZST, so this field costs a
    /// build that wants nothing exactly nothing.
    hooks: H,
}

impl Fetch {
    pub fn new() -> Self {
        Self {
            caps: caps::probe(),
            opts: opts::FetchOpts::default(),
            hooks: NoHooks,
        }
    }
}

impl<H> Fetch<H> {
    /// Send this transport's events to `hooks` — see
    /// [`hclient_core::unversioned::Hooks`] for what it hears and what it
    /// costs, and `crate::hooks` for the three quarters of the
    /// vocabulary a browser cannot speak.
    ///
    /// **It returns a different type**, and that is the zero-cost
    /// mechanism rather than an inconvenience: the hook is a type
    /// parameter, so the `NoHooks` build monomorphises to code with no
    /// clock reads in it at all, where a `Box<dyn Hooks>` field would
    /// leave every no-hook build carrying a null check on the request
    /// path.
    ///
    /// The hook may be `!Send`, and here that costs something a caller can
    /// see: nothing on this path declares `Send`, so an `Rc` inside a hook
    /// makes this transport `!Send` — and with it the future
    /// [`Transport::execute`] returns, which `crate::promise::SendJsFuture`
    /// otherwise keeps `Send`. Both halves compile and both halves work
    /// (P13; `crates/hclient-core/tests/shape.rs`, and `tests/hooks.rs`
    /// here).
    pub fn hooks<H2>(self, hooks: H2) -> Fetch<H2> {
        Fetch {
            caps: self.caps,
            // Carried: this method changes `H` and nothing else, unlike
            // `hclient-native`'s `hooks`, which has an installer whose
            // type names `H` and must drop it.
            opts: self.opts,
            hooks,
        }
    }

    /// The `RequestInit` members this transport sets beyond method,
    /// headers, body, signal and `duplex` — `mode`, `credentials`, `cache`
    /// and `referrerPolicy`.
    ///
    /// See [`crate::opts`] for what each buys, why they are here rather
    /// than on `Transport` or in a request extension, and why `redirect`
    /// is deliberately not among them.
    ///
    /// ```no_run
    /// # use hclient_fetch::{Fetch, opts::FetchOpts};
    /// # use web_sys::RequestCredentials;
    /// let t = Fetch::new().opts(FetchOpts {
    ///     // What a cross-origin authenticated request needs; the
    ///     // browser's default is `same-origin`.
    ///     credentials: Some(RequestCredentials::Include),
    ///     ..FetchOpts::default()
    /// });
    /// # let _ = t;
    /// ```
    ///
    /// **Nothing in [`Capabilities`] moves**, which is worth stating
    /// because one of these looks as though it should: `redirect` would
    /// have, which is exactly why it is absent — see the module doc.
    #[must_use]
    pub fn opts(mut self, opts: opts::FetchOpts) -> Self {
        self.opts = opts;
        self
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
    /// `Capabilities: Clone` (see `hclient-core/src/caps.rs`), and this is
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

impl<H: Hooks> Transport for Fetch<H> {
    type Body = Body;
    type Error = Error;

    async fn execute(
        &self,
        req: http::Request<RequestBody>,
    ) -> Result<http::Response<Body>, Error> {
        // One gate, once — the clock read and the one allocation this
        // feature costs come out of the same `Option`, so `H::WATCHING`
        // is read in exactly one place and a mutation that ignores it
        // cannot take only the half no test can see.
        //
        // A second `H::WATCHING` gate here **survives a mutation run**:
        // with the `Uri` gate removed, a `NoHooks` build
        // clones a `Uri` and calls `NoHooks::on` for every request, and
        // `tests/hooks_cost.rs` still reads 0 — because the clock is only
        // read from the `Some` arm of `hooks::since`, and there is no
        // allocator to count in a browser. One `Option` closes it: the
        // same mutation now also ungates the clock, and the cost test
        // dies.
        //
        // The `Uri` has to be taken now because `to_web_request` consumes
        // the request, and it cannot be taken back off the
        // `web_sys::Request` afterwards — its `url()` is the browser's
        // serialisation, not the caller's `http::Uri`, and `Head::uri`
        // promises the URI "as the transport received it".
        let watched = hooks::mark::<H>().map(|at| (at, req.uri().clone()));

        let convert::Converted {
            request,
            abort,
            streamed,
        } = convert::to_web_request(req, &self.caps, &self.opts)?;
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
            if streamed {
                // One cause the browser structurally cannot report, added
                // only where it can apply: a `ReadableStream` request body
                // needs an HTTP/2 origin, and over HTTP/1.1 Chrome produces
                // this exact, unlabelled `TypeError` in milliseconds with
                // nothing sent. `ErrorKind` stays `Connect` — that is still
                // what happened, and a request that failed because the
                // origin refused the connection reaches here identically.
                // See `convert::StreamingBodyFetchFailed`.
                Error::new(
                    ErrorKind::Connect,
                    convert::StreamingBodyFetchFailed(Error::new(ErrorKind::Connect, base)),
                )
            } else {
                Error::new(ErrorKind::Connect, base)
            }
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
        let out = builder
            .body(body)
            .map_err(|e| Error::new(ErrorKind::Other, e))?;

        // The one event this backend has. `if let Some(uri)` is the gate
        // rather than a second read of `H::WATCHING`: the `Some` above is
        // exactly `H::WATCHING`, and two places that have to agree is one
        // more than is safe.
        //
        // `status` comes off the response this function is about to
        // return, not from anything read separately — the same discipline
        // `hclient-native`'s `report_head` follows.
        //
        // `version` is `None`, and that is the whole of what this backend
        // has to say about the protocol: a browser will not tell a page
        // which one it spoke, `capabilities()` says so with
        // `version_reported: false`, and `out.version()` here is
        // `http::response::Builder`'s default rather than an observation.
        // Reporting that default would be indistinguishable, in a caller's
        // log, from an HTTP/1.1 exchange this transport had watched happen
        // — see `Head::version` in `hclient-core`, which changed shape for
        // exactly this.
        //
        // Nothing is reported on the error path: no head arrived, and a
        // `Head` for a request that got none would be the loudest
        // available lie.
        if let Some((began, uri)) = &watched {
            self.hooks.on(Event::Head(Head {
                id: ConnectionId::UNWATCHED,
                uri,
                status: out.status(),
                version: None,
                elapsed: hooks::since(Some(*began)),
            }));
        }
        Ok(out)
    }

    /// Identity, not the default wrapping: `Self::Error` is already
    /// `hclient_core::Error`, and every fallible step in `execute`
    /// (`convert::to_web_request`, the fetch call, response-building,
    /// `Body::from_response`) has already set its own category. Without
    /// this override `Client::execute` would still behave identically
    /// (the default recognizes an already-`Error` and passes it through
    /// unchanged) — the line is behaviorally redundant and semantically
    /// needed anyway: it names the intent where it's read, and it survives
    /// a future change to the default. Same reasoning, same wording, as
    /// `hclient-native`'s and `hclient-wasi`'s identical overrides.
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
    use std::future::poll_fn;
    use std::pin::Pin;

    /// The cheap `'duplex' in Request.prototype` check — an observation,
    /// not the probe anything decides by.
    ///
    /// `caps::supports_duplex` is `pub(crate)` and drives no field of
    /// `Capabilities` (v0.2 W6 gave that job to
    /// `caps::supports_streaming_request_body`, which is behavioural), so
    /// without this accessor `rustc` would flag it `dead_code`. It is kept
    /// because `tests/caps.rs` pins the two probes against each other: they
    /// agree in Chrome 151 and Firefox 153 today, and the day they diverge
    /// is the day the cheap check — the one most of the ecosystem uses —
    /// starts lying.
    pub fn supports_duplex_for_test() -> bool {
        crate::caps::supports_duplex()
    }

    /// The probe that actually decides
    /// `Capabilities::streaming_request_body` — whatwg/fetch#1470's
    /// behavioural detection. Exposed on the same terms as
    /// [`supports_duplex_for_test`]: `tests/caps.rs` mutation-tests it
    /// directly, by replacing the `Request` constructor with one that
    /// behaves like the other browser, in both directions.
    pub fn supports_streaming_request_body_for_test() -> bool {
        crate::caps::supports_streaming_request_body()
    }

    pub fn send_js_future(
        p: js_sys::Promise,
    ) -> impl std::future::Future<Output = Result<wasm_bindgen::JsValue, wasm_bindgen::JsValue>>
    {
        crate::promise::SendJsFuture::new(p)
    }

    /// Proves — deterministically, via `Weak::upgrade`, with no dependence
    /// on real `Promise`/browser-event timing — that dropping
    /// `SendJsFuture` while its promise is still
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
    /// that needs a `Capabilities` this browser's real probe does not
    /// produce (`streaming_request_body = true` in Firefox, `= false` in
    /// Chrome) uses [`to_web_request_with_caps`] instead, since a real
    /// `Fetch::new()` can't be made to lie about what it probed.
    ///
    /// Returns the tuple this function has always returned, dropping
    /// `Converted::streamed`; the flag exists for `execute`'s error
    /// labelling and no test reaches for it through here.
    pub fn to_web_request(
        f: &crate::Fetch,
        req: http::Request<hclient_core::RequestBody>,
    ) -> Result<(web_sys::Request, Option<web_sys::AbortController>), hclient_core::Error> {
        crate::convert::to_web_request(req, &f.caps, &f.opts).map(|c| (c.request, c.abort))
    }

    /// Same conversion, against a caller-supplied `Capabilities` rather than
    /// a real `Fetch`'s probed one.
    ///
    /// Since v0.2 W6 the value of `caps.streaming_request_body` genuinely
    /// changes what this does — it is the branch `convert::to_web_request`
    /// takes — so this is how `tests/convert.rs` exercises BOTH arms in a
    /// single browser, rather than testing only the one the local probe
    /// happens to produce. Passing `true` in a browser that would corrupt
    /// the body builds the stream anyway: that is the point of a test seam,
    /// and it is also why the seam is `#[doc(hidden)]` and why nothing on
    /// the real path can reach it — `Fetch::caps` comes from
    /// `caps::probe()` and from nowhere else.
    pub fn to_web_request_with_caps(
        req: http::Request<hclient_core::RequestBody>,
        caps: &hclient_core::Capabilities,
    ) -> Result<(web_sys::Request, Option<web_sys::AbortController>), hclient_core::Error> {
        crate::convert::to_web_request(req, caps, &crate::opts::FetchOpts::default())
            .map(|c| (c.request, c.abort))
    }

    /// The conversion with caller-chosen
    /// [`FetchOpts`](crate::opts::FetchOpts), so `tests/opts.rs` can read
    /// the four members back off a `web_sys::Request` without sending
    /// anything.
    ///
    /// A seam rather than a live request, for the reason the whole file
    /// gives: what is claimed is that a value the caller set reaches the
    /// `Request`, and a browser is not needed to see that — while
    /// `credentials: "include"` against a real cross-origin server is not
    /// something a headless test can arrange honestly.
    pub fn to_web_request_with_opts(
        req: http::Request<hclient_core::RequestBody>,
        caps: &hclient_core::Capabilities,
        opts: &crate::opts::FetchOpts,
    ) -> Result<web_sys::Request, hclient_core::Error> {
        crate::convert::to_web_request(req, caps, opts).map(|c| c.request)
    }

    pub fn check_headers(
        h: &http::HeaderMap,
        caps: &hclient_core::Capabilities,
    ) -> Result<(), hclient_core::Error> {
        crate::convert::check_headers(h, caps)
    }

    /// Builds a [`crate::Body`] from a `web_sys::Response` — the only path
    /// to `Body::from_response` outside this crate, `#[doc(hidden)]` like
    /// everything else in this module; the real, non-test-only path is the
    /// eventual `Transport` impl.
    pub fn body_from_response(
        resp: &web_sys::Response,
    ) -> Result<crate::Body, hclient_core::Error> {
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
    /// only `http`/`https` schemes, which would reject exactly the
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
    ) -> Result<bytes::Bytes, hclient_core::Error> {
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
    async fn collect(body: &mut crate::Body) -> Result<bytes::Bytes, hclient_core::Error> {
        use http_body::Body as _;
        let mut buf = bytes::BytesMut::new();
        loop {
            match poll_fn(|cx| Pin::new(&mut *body).poll_frame(cx)).await {
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

    /// Exercises `crate::AbortOnDrop` directly and deterministically — no
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
