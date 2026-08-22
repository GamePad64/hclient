//! What a browser client with no hook pays for the hook's existence,
//! measured rather than asserted.
//!
//! # The measurement, and where the counting clock lives
//!
//! "Zero cost" here means one specific thing: with `NoHooks`, the
//! transport does not read a clock and does not clone the request's `Uri`,
//! and has no branch left that a monomorphised build cannot delete.
//!
//! `hclient-native` and `hclient-h3` count the reads through a `Timer`
//! they were handed — a runtime seam, and a counting implementation of it.
//! **This crate has no runtime seam**: in a browser the runtime *is* the
//! browser, which is the same fact that put `BrowserClock` in this crate
//! rather than in an `hclient-rt-browser` that does not exist. So the
//! counting clock is installed where the clock actually lives — this file
//! replaces `Performance.prototype.now` with a wrapper that tallies its
//! calls and delegates to the original. That is a stronger position than
//! the native one rather than a weaker one: nothing had to be injected
//! into the transport for the measurement to be possible, so the code
//! under test is exactly the code that ships.
//!
//! # It is exact, and deliberately so
//!
//! A `<=` would pass for a build that read the clock twice when it should
//! read it none. The numbers below are equalities, and if a legitimate
//! clock read is ever added to this path this file fails and the number
//! has to be changed **on purpose**.
//!
//! # The `Uri` clone is covered because it shares the clock's `Option`
//!
//! The other thing the feature costs is one allocation — `Head::uri`
//! promises the URI as the transport received it, and
//! `convert::to_web_request` consumes the request before the response
//! exists. There is no allocator to count in a browser, so that
//! allocation cannot be measured directly.
//!
//! A second `H::WATCHING` gate here **survives a mutation**: removing it
//! makes a `NoHooks` build clone a
//! `Uri` and call `NoHooks::on` for every request, and the first test
//! below still reads 0 — because the clock is only read from the `Some`
//! arm of `hooks::since`. All 10 behaviour tests and all 4 tests here
//! were green with it gone.
//!
//! `Fetch::execute` carries both in one `Option` now
//! (`mark::<H>().map(|at| (at, uri.clone()))`), so there is one
//! `H::WATCHING` in the function and a mutation that ignores it takes the
//! clock with it. The count below is what fails.
#![cfg(target_arch = "wasm32")]
use wasm_bindgen_test::*;
wasm_bindgen_test_configure!(run_in_browser);

use hclient_core::RequestBody;
use hclient_core::unversioned::{Event, Hooks, NoHooks, Transport};
use hclient_fetch::Fetch;
use std::cell::Cell;
use std::rc::Rc;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};

/// The browser's own clock, with a note kept of every read.
///
/// Installed on `Performance.prototype` rather than on the `performance`
/// instance, because that is where `now` lives and where wasm-bindgen's
/// generated shim (`arg0.now()`, a property lookup on every call) will
/// find it.
///
/// The wrapper is **leaked** (`Closure::into_js_value`) rather than held
/// and dropped. A dropped `Closure` whose function is still reachable from
/// JS traps when called, and these tests share one wasm instance with
/// every other test in the crate — so a panic between install and restore
/// would take the rest of the suite with it. Leaking one closure per test
/// run is the cheaper failure mode by a wide margin.
struct CountingClock {
    perf: JsValue,
    proto: js_sys::Object,
    original: js_sys::Function,
    count: Rc<Cell<usize>>,
}

impl CountingClock {
    fn install() -> Self {
        let perf = js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("performance"))
            .expect("global scope exposes performance");
        let proto = js_sys::Object::get_prototype_of(&perf);
        let original: js_sys::Function = js_sys::Reflect::get(&proto, &JsValue::from_str("now"))
            .expect("Performance.prototype has a `now`")
            .dyn_into()
            .expect("and it is a function");

        let count = Rc::new(Cell::new(0usize));
        let tally = Rc::clone(&count);
        let delegate = original.clone();
        let on = perf.clone();
        let patched = Closure::<dyn FnMut() -> f64>::new(move || {
            tally.set(tally.get() + 1);
            delegate
                .call0(&on)
                .expect("Performance.prototype.now does not throw")
                .as_f64()
                .expect("and it returns a number")
        })
        .into_js_value();
        js_sys::Reflect::set(&proto, &JsValue::from_str("now"), &patched)
            .expect("Performance.prototype is an ordinary, extensible object");

        Self {
            perf,
            proto,
            original,
            count,
        }
    }

    fn reset(&self) {
        self.count.set(0);
    }

    fn reads(&self) -> usize {
        self.count.get()
    }

    /// Put the browser's clock back. Called before the assertions, never
    /// after: an assertion that fails must not leave the page patched.
    fn restore(self) {
        js_sys::Reflect::set(
            &self.proto,
            &JsValue::from_str("now"),
            self.original.as_ref(),
        )
        .expect("the same property that was just set");
        let _ = self.perf;
    }
}

/// A hook that does nothing but exist — the *presence* of a watcher is
/// what this file measures, not what a watcher does with the events.
#[derive(Clone, Copy, Default)]
struct Watching;
impl Hooks for Watching {
    fn on(&self, _event: Event<'_>) {}
}

fn page_url() -> String {
    web_sys::window()
        .expect("run_in_browser gives a window")
        .location()
        .href()
        .expect("the currently loaded page always has an href")
}

fn get(uri: &str) -> http::Request<RequestBody> {
    http::Request::builder()
        .uri(uri)
        .body(RequestBody::Empty)
        .expect("a well-formed GET")
}

/// The whole claim, in one number a caller can check.
#[wasm_bindgen_test]
async fn a_client_with_no_hook_reads_no_clock_at_all() {
    let quiet = Fetch::new();
    let url = page_url();

    let clock = CountingClock::install();
    clock.reset();
    let outcome = quiet.execute(get(&url)).await;
    let reads = clock.reads();
    clock.restore();

    assert!(outcome.is_ok(), "the harness page must answer");
    assert_eq!(
        reads, 0,
        "a request under `NoHooks` must read no clock at all: `WATCHING` is \
         a `const`, so the read is not a branch that was not taken, it is \
         code that is not there"
    );
}

/// The other side of the same measurement, and the reason the first is not
/// vacuous: with a hook, the same request does read the clock, and the
/// count says exactly which reads the hook is responsible for.
///
/// Without this pair the first test would pass against a transport whose
/// timing code was simply broken — never reading a clock for anybody.
#[wasm_bindgen_test]
async fn the_same_request_with_a_hook_reads_it_exactly_twice() {
    let watched = Fetch::new().hooks(Watching);
    let url = page_url();

    let clock = CountingClock::install();
    clock.reset();
    let outcome = watched.execute(get(&url)).await;
    let reads = clock.reads();
    clock.restore();

    assert!(outcome.is_ok(), "the harness page must answer");
    assert_eq!(
        reads, 2,
        "one mark at the top of `execute` and one read to close the \
         interval — `Head::elapsed` is the only duration this backend can \
         measure, because it is the only one it can observe. There is no \
         third read because there is no `Connected` to time: see \
         `src/hooks.rs`"
    );
}

/// A request that fails still pays for its mark and nothing more — the
/// second read never happens, because no `Head` is reported for a head
/// that never arrived.
///
/// This is the one place the two counts differ, and it pins the *shape* of
/// the emission rather than its content: a transport that reported a
/// `Head` on the error path would read the clock twice here.
#[wasm_bindgen_test]
async fn a_failed_request_reads_the_clock_once_because_it_reports_no_head() {
    let watched = Fetch::new().hooks(Watching);

    let clock = CountingClock::install();
    clock.reset();
    let outcome = watched.execute(get("http://127.0.0.1:59999/")).await;
    let reads = clock.reads();
    clock.restore();

    assert!(outcome.is_err(), "nothing is listening there");
    assert_eq!(
        reads, 1,
        "the mark is taken before anything can fail; the interval is never \
         closed because there is no head to report it with"
    );
}

/// `NoHooks` costs nothing to *carry* either: it is zero-sized, so a
/// transport that stores one is the same size as one with no field at all.
#[wasm_bindgen_test]
fn the_no_op_hook_takes_up_no_room_in_the_transport() {
    assert_eq!(std::mem::size_of::<NoHooks>(), 0);
    assert_eq!(
        std::mem::size_of::<Fetch<NoHooks>>(),
        std::mem::size_of::<Fetch>(),
        "the default type parameter must be the same type, not a second one"
    );
    const { assert!(!NoHooks::WATCHING) };
}
