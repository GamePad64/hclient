//! What `Fetch` tells a `Hooks`, in a real browser.
//!
//! The claim under test is small on purpose and the smallness is the
//! result: **one event out of four**, and two of that one's five fields
//! carrying values this transport did not observe. `src/hooks.rs`'s module
//! doc says why; this file is where each half of it is a line that fails
//! if it stops being true.
//!
//! Everything here runs against the test harness's own already-loaded page
//! (`location.href`), the same offline, deterministic URL
//! `tests/transport.rs` uses — a real `fetch()` over a real socket to a
//! real server, with no Internet.
#![cfg(target_arch = "wasm32")]
use wasm_bindgen_test::*;
wasm_bindgen_test_configure!(run_in_browser);

use hclient_core::RequestBody;
use hclient_core::unversioned::{ConnectionId, Event, Hooks, NoHooks, Transport};
use hclient_fetch::Fetch;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;
use wasm_bindgen::{JsCast, JsValue};

/// One event, flattened into something owned — `Event<'_>` borrows the
/// `Uri` it names, so a recorder has to copy out what it wants before
/// `on` returns.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Seen {
    Connected,
    Reused,
    Closed,
    /// **Never emitted by this backend**, and here so that the `match`
    /// below stays exhaustive rather than gaining a `_` arm.
    ///
    /// `Event` is deliberately not `#[non_exhaustive]`, which is what made
    /// a new variant a compile error here instead of a silently unhandled
    /// case — and this file is the evidence that the design works and the
    /// running of it did not: the variant landed on 2026-08-16 and this
    /// suite did not build again until it was noticed, because
    /// `just test-browsers` is its own CI job and nothing local runs it.
    ///
    /// A `_` arm would have absorbed the next variant too. Every test here
    /// asserts an exact event sequence, so one of these appearing fails a
    /// line rather than being counted.
    Informational,
    Head {
        id: u64,
        uri: String,
        status: u16,
        /// The `Option` `Head::version` actually is, not a `String` of
        /// it. A `String` would be the shape for an assertion about what
        /// the value *prints as*; the claim here is that there is no value,
        /// and `None` is a thing to compare against rather than to format.
        version: Option<http::Version>,
    },
}

/// A hook whose state lives behind an `Rc`, so it is **genuinely
/// `!Send`** — not a `PhantomData` gesture.
///
/// That is deliberate rather than convenient: P13 asks whether a hook can
/// be `!Send`, and a browser is where the answer is load bearing, because
/// `wasm32-unknown-unknown` has no threads to spare it. Every test in this
/// file uses this recorder, so the whole file is also the P13 probe for
/// this backend — including the two that go through `hclient::Client`,
/// which is the layer `crate::promise::SendJsFuture`'s `unsafe impl Send`
/// exists for.
#[derive(Clone, Default)]
struct Recorder {
    seen: Rc<RefCell<Vec<Seen>>>,
    elapsed: Rc<RefCell<Vec<Duration>>>,
}

impl Recorder {
    fn seen(&self) -> Vec<Seen> {
        self.seen.borrow().clone()
    }
    fn elapsed(&self) -> Vec<Duration> {
        self.elapsed.borrow().clone()
    }
    fn head(&self) -> Seen {
        let seen = self.seen();
        assert_eq!(seen.len(), 1, "expected exactly one event, got {seen:?}");
        seen.into_iter().next().expect("length checked above")
    }
}

impl Hooks for Recorder {
    fn on(&self, event: Event<'_>) {
        let flat = match event {
            Event::Connected(_) => Seen::Connected,
            Event::Reused(_) => Seen::Reused,
            Event::Closed(_) => Seen::Closed,
            Event::Informational(_) => Seen::Informational,
            Event::Head(h) => {
                self.elapsed.borrow_mut().push(h.elapsed);
                Seen::Head {
                    id: h.id.get(),
                    uri: h.uri.to_string(),
                    status: h.status.as_u16(),
                    version: h.version,
                }
            }
        };
        self.seen.borrow_mut().push(flat);
    }
}

/// The harness's own page, with a query added so that the URI under test
/// is one **this test** built rather than one the browser handed us — see
/// `the_uri_is_the_one_the_transport_was_given`.
///
/// Built from `location.origin()` rather than from `location.href()`,
/// which `tests/transport.rs` uses: the harness's `href` carries a
/// fragment (`#wbg_style=display-none`, measured), and `http::Uri` has no
/// fragment at all — parsing that string and printing it back gives
/// `http://127.0.0.1:PORT/`, so a test comparing the two would be
/// comparing `Uri`'s grammar with the browser's rather than the transport
/// with itself.
fn page_url() -> String {
    let origin = web_sys::window()
        .expect("run_in_browser gives a window")
        .location()
        .origin()
        .expect("a loaded page has an origin");
    format!("{origin}/?hooks=1")
}

fn get(uri: &str) -> http::Request<RequestBody> {
    http::Request::builder()
        .uri(uri)
        .body(RequestBody::Empty)
        .expect("a well-formed GET")
}

// ---------------------------------------------------------------------
// The event set: one of four, and which one.
// ---------------------------------------------------------------------

/// The whole finding, as one assertion: a successful request through this
/// transport produces **exactly one** event, and it is a `Head`.
///
/// The `Connected`/`Reused`/`Closed` arms of `Recorder::on` exist only so
/// that this test can fail loudly rather than silently if one of them ever
/// starts being emitted — a browser gives a page nothing to put in any of
/// them (`src/hooks.rs`), so an implementation that produced one would be
/// inventing it.
#[wasm_bindgen_test]
async fn a_successful_request_reports_one_head_and_no_connection_event_at_all() {
    let rec = Recorder::default();
    let t = Fetch::new().hooks(rec.clone());

    let resp = t.execute(get(&page_url())).await.expect("the harness page");
    assert_eq!(resp.status(), 200);

    let seen = rec.seen();
    assert_eq!(
        seen.len(),
        1,
        "one request, one event — a browser owns the connection and reports \
         nothing about it, so there is nothing else to say: {seen:?}"
    );
    assert!(
        matches!(seen[0], Seen::Head { .. }),
        "and the one event is the head: {seen:?}"
    );
}

/// The counterpart, and the reason the test above is not vacuous: a
/// request that never got a head reports **nothing**.
///
/// A `Head` here would be the loudest lie available — a caller counting
/// heads against requests would see every failure as a success. The
/// failure is the one this backend can produce offline and instantly: a
/// loopback port with nothing bound to it (`tests/transport.rs` uses the
/// same one, for the same reason).
#[wasm_bindgen_test]
async fn a_request_that_never_got_a_head_reports_nothing() {
    let rec = Recorder::default();
    let t = Fetch::new().hooks(rec.clone());

    let err = t
        .execute(get("http://127.0.0.1:59999/"))
        .await
        .expect_err("nothing is listening there");
    let _ = err;

    assert_eq!(
        rec.seen(),
        Vec::new(),
        "no head arrived, so no `Head` may be reported"
    );
}

// ---------------------------------------------------------------------
// The five fields of the one event: three observed, two not.
// ---------------------------------------------------------------------

/// `uri` is the URI **the transport was given**, not the browser's
/// serialisation of it.
///
/// The distinction is real and this is what pins it: `web_sys::Response`
/// carries a `url()` of its own, it is the obvious thing to reach for, and
/// it is the *final* URL after any internal redirect with the fragment
/// stripped. `Head::uri`'s own doc says "as the transport received it —
/// absolute, before any protocol rewrote it", which is the caller's
/// `http::Uri` and nothing else.
#[wasm_bindgen_test]
async fn the_uri_is_the_one_the_transport_was_given() {
    let rec = Recorder::default();
    let t = Fetch::new().hooks(rec.clone());
    let url = page_url();

    t.execute(get(&url)).await.expect("the harness page");

    let Seen::Head { uri, .. } = rec.head() else {
        panic!("the one event is a head");
    };
    assert_eq!(
        uri, url,
        "the query this test appended must survive into the event — a \
         `Head` built from `web_sys::Response::url()` would be a different \
         string on any request the browser redirected"
    );
}

/// `status` is the server's.
#[wasm_bindgen_test]
async fn the_status_is_the_one_the_server_sent() {
    let rec = Recorder::default();
    let t = Fetch::new().hooks(rec.clone());

    // A path the harness's server does not serve: a real response, with a
    // status that is not the 200 every other test in this file sees, so a
    // hard-coded 200 in the emitter cannot pass.
    let base = web_sys::window()
        .expect("run_in_browser gives a window")
        .location()
        .origin()
        .expect("a loaded page has an origin");
    let missing = format!("{base}/hclient-hooks-no-such-path");
    let resp = t.execute(get(&missing)).await.expect("a 404 is a response");
    let sent = resp.status().as_u16();
    assert_ne!(sent, 200, "the harness must not serve this path: {missing}");

    let Seen::Head { status, .. } = rec.head() else {
        panic!("the one event is a head");
    };
    assert_eq!(status, sent, "the event's status is the response's");
}

/// **`id` is `ConnectionId::UNWATCHED`, which is the seam's value for
/// *this event names no connection*.**
///
/// There is no connection object in the Fetch Standard, so there is
/// nothing to take an id from and nothing a later event could match it
/// against. `UNWATCHED` is the only value `ConnectionId::next` never
/// returns, so it cannot collide with a real connection from another
/// transport in the same process, and a hook looking it up in a table of
/// live connections cannot hit one.
///
/// It is not a value being *borrowed* against its own doc: the other
/// thing that produces it is a build with
/// `Hooks::WATCHING == false`, whose events by that const's own definition
/// nobody reads, so the constant's doc comment carries the meaning rather
/// than one producer.
///
/// The assertion is written against `ConnectionId::UNWATCHED` rather than
/// against `0` so that it stays true if the constant moves, and it is
/// paired with the inequality below because "the id is never a real one"
/// is the claim, not "the id is zero".
#[wasm_bindgen_test]
async fn the_id_names_no_connection_because_there_is_none_to_name() {
    let rec = Recorder::default();
    let t = Fetch::new().hooks(rec.clone());

    t.execute(get(&page_url())).await.expect("the harness page");

    let Seen::Head { id, .. } = rec.head() else {
        panic!("the one event is a head");
    };
    assert_eq!(id, ConnectionId::UNWATCHED.get());
    assert_ne!(
        id,
        ConnectionId::next().get(),
        "and it must not be a number the counter could ever hand out"
    );
}

/// **`version` is `None`, and it is the response's `HTTP/1.1` that the
/// event refuses to repeat.**
///
/// The Fetch Standard's `Response` has no protocol member: the browser
/// knows whether it spoke HTTP/1.1, h2 or h3 to this origin and does not
/// tell a page. The value on the response is therefore
/// `http::response::Builder`'s default, and this transport used to report
/// it — an `HTTP/1.1` in a caller's log that nothing distinguishes from an
/// exchange somebody watched happen over HTTP/1.1.
///
/// Two assertions, because the pair is the decision and either alone
/// passes for the wrong reason. `version == None` alone would also pass on
/// a transport whose responses carried no version either; the second
/// assertion is what makes the first a *refusal* — there is a value right
/// there, it is `HTTP/1.1`, and the event does not take it.
#[wasm_bindgen_test]
async fn the_version_is_none_and_not_the_builder_default_on_the_response() {
    let rec = Recorder::default();
    let t = Fetch::new().hooks(rec.clone());

    let resp = t.execute(get(&page_url())).await.expect("the harness page");
    let on_the_response = resp.version();

    let Seen::Head { version, .. } = rec.head() else {
        panic!("the one event is a head");
    };
    assert_eq!(
        version, None,
        "a browser will not say which protocol it spoke, so the event says \
         nothing rather than something — see `Head::version` in \
         `hclient-core`"
    );
    assert_eq!(
        on_the_response,
        http::Version::HTTP_11,
        "and the value the event declined is right here on the response: \
         `http::response::Builder`'s default, not something this transport \
         read off the wire"
    );
}

/// **The event and the capability are one fact spelled twice, and they
/// have to agree.**
///
/// `Head::version`'s doc states the rule for all four backends: `Some`
/// exactly when `Capabilities::version_reported`. Both spellings exist
/// because they are reachable from different places — the capability from
/// whoever built the transport, the event from a hook that is handed an
/// `Event` and nothing else — and two spellings of one fact are worth
/// having only while something checks that they still say the same thing.
///
/// It sits here rather than in `tests/caps.rs`, where the `false` is also
/// asserted, because what is being pinned is the *pair*: a future
/// `hclient-fetch` that learned to read a real protocol from somewhere
/// must change both lines in this one test, and cannot change one of them
/// alone and stay green.
#[wasm_bindgen_test]
async fn the_event_says_no_version_exactly_where_the_capability_does() {
    let rec = Recorder::default();
    let t = Fetch::new().hooks(rec.clone());

    let reported = t.capabilities().version_reported;

    t.execute(get(&page_url())).await.expect("the harness page");
    let Seen::Head { version, .. } = rec.head() else {
        panic!("the one event is a head");
    };

    assert!(
        !reported,
        "this backend cannot observe the protocol, and the capability is \
         where it says so to whoever built it"
    );
    assert_eq!(
        version.is_some(),
        reported,
        "`Head::version` is `Some` exactly when `version_reported` — the \
         rule in `hclient-core`, checked on the one backend in this crate"
    );
}

/// `elapsed` is a real interval measured across the whole of `execute`.
///
/// Bracketed with the same clock the transport uses, so the two numbers
/// are comparable: the event's interval is contained in the caller's, and
/// it is not zero. The upper bound catches an `elapsed` computed from
/// something other than this request; the lower bound catches the two ways
/// the measurement can be absent — `Duration::ZERO` from a `since(None)`,
/// and a mark taken after the fetch instead of before it.
#[wasm_bindgen_test]
async fn elapsed_is_measured_across_the_whole_call() {
    let rec = Recorder::default();
    let t = Fetch::new().hooks(rec.clone());

    let before = now_ms();
    t.execute(get(&page_url())).await.expect("the harness page");
    let outer = Duration::from_secs_f64((now_ms() - before).max(0.0) / 1000.0);

    let elapsed = rec.elapsed();
    assert_eq!(elapsed.len(), 1);
    assert!(
        elapsed[0] > Duration::ZERO,
        "a real fetch over a real socket takes measurable time: {:?}",
        elapsed[0]
    );
    assert!(
        elapsed[0] <= outer,
        "the transport's interval must sit inside the caller's: {:?} > {outer:?}",
        elapsed[0]
    );
}

/// The transport's own clock, read from outside it — `performance.now()`,
/// found the way `src/hooks.rs` finds it.
fn now_ms() -> f64 {
    js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("performance"))
        .expect("global scope exposes performance")
        .unchecked_into::<web_sys::Performance>()
        .now()
}

// ---------------------------------------------------------------------
// P13 for this backend, in both directions.
// ---------------------------------------------------------------------

/// The `!Send` half — **at the transport, because `Client` can no longer
/// hold it.**
///
/// `Recorder` holds an `Rc`, so `Fetch<Recorder>` is `!Send`, and that is
/// the whole point: the future `execute` returns is the one
/// `crate::promise::SendJsFuture`'s `unsafe impl Send` exists to keep
/// `Send` when a hook is not in the way. P13's answer is unchanged —
/// **a single-threaded runtime can watch** — but the layer it is answered
/// at has moved down one.
///
/// This used to go through `hclient::Client`, which bounded only
/// `T::Error: Send + Sync`. An erased `Client` names no transport type, so
/// it boxes one as `Send + Sync` and an `Rc`-holding hook is refused at
/// `Client::builder`. That refusal is a compile error at the line that
/// asked, not a silent downgrade, and the property itself is a fact about
/// the **hooks seam** rather than about `Client` — which is why the test
/// belongs here. What is genuinely lost is watching a `!Send`-hooked
/// browser transport *through the facade*: cookies, redirects and the
/// response cache are not available to a caller who wants that.
#[wasm_bindgen_test]
async fn a_non_send_hook_works_all_the_way_through_the_transport() {
    use hclient_core::unversioned::Transport as _;

    let rec = Recorder::default();
    let t = Fetch::new().hooks(rec.clone());

    let req = http::Request::builder()
        .method(http::Method::GET)
        .uri(page_url())
        .body(hclient_core::RequestBody::Empty)
        .expect("request");
    let resp = t.execute(req).await.expect("the harness page");
    assert_eq!(resp.status(), 200);

    assert_eq!(
        rec.seen().len(),
        1,
        "one hop, one head — `Client` adds no events of its own"
    );
}

/// The other half, and the half a `!Send` probe cannot see: a hook that
/// **is** `Send` must leave the transport's future `Send`.
///
/// Without this, the seam could satisfy the test above by being
/// unconditionally `!Send`-poisoning, and this crate would lose the one
/// property `promise.rs` carries an `unsafe impl` for. Auto traits pass
/// through in both directions or they are not auto traits.
#[wasm_bindgen_test]
fn a_send_hook_leaves_the_execute_future_send() {
    fn assert_send<T: Send>(_: T) {}

    struct Atomic(std::sync::atomic::AtomicUsize);
    impl Hooks for Atomic {
        fn on(&self, _event: Event<'_>) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    let t = Fetch::new().hooks(Atomic(std::sync::atomic::AtomicUsize::new(0)));
    assert_send(t.execute(get("http://example.invalid/")));

    // And the default is `Send` too, which is what every existing caller
    // of this crate already relies on.
    let quiet = Fetch::new();
    assert_send(quiet.execute(get("http://example.invalid/")));
    let _ = NoHooks;
}

/// The WebSocket seam survives a hook.
///
/// `impl WebSocketConnect for Fetch<H>` is generic for a reason stated in
/// `src/websocket.rs`: a caller who switched observability on must not
/// lose an unrelated capability. Pinned by *using* it — the handshake
/// fails (nothing is listening), which is fine; what is being checked is
/// that the call compiles against `Fetch<Recorder>` and that opening a
/// socket emits no event, since the vocabulary has no word for one.
#[wasm_bindgen_test]
async fn a_hooked_transport_can_still_open_a_websocket_and_reports_nothing_for_it() {
    use hclient_core::unversioned::WebSocketConnect;

    let rec = Recorder::default();
    let t = Fetch::new().hooks(rec.clone());
    let req = http::Request::builder()
        .uri("ws://127.0.0.1:59999/")
        .body(())
        .expect("a well-formed handshake request");

    let outcome = t.websocket(req).await;
    assert!(
        outcome.is_err(),
        "nothing is listening on that port, so the handshake must fail"
    );
    assert_eq!(
        rec.seen(),
        Vec::new(),
        "a WebSocket is not an HTTP request and the event set has no word \
         for one — see `src/websocket.rs`"
    );
}
