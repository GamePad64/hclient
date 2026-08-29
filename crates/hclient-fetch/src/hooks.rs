//! What a transport that owns no connection can tell a [`Hooks`].
//!
//! **Two of the five events, and the pair is the point.** [`Head`] is what
//! this backend can say about an exchange, and
//! [`Progress`](hclient_core::unversioned::Progress) is what it can say
//! about the octets — the second arrived later and did not disturb the
//! argument below, because that argument is about *connections* and octets
//! are not one. What is still absent is absent for the same reason it
//! always was.
//!
//! The vocabulary is `hclient_core::unversioned::{Hooks, Event}` and it is
//! **unchanged** here. `hclient-native` derived the event set from a
//! transport that dials its own sockets; `hclient-h3` was the test of
//! whether it survived connections that are *shared*. This crate is the
//! test of whether it survives a transport that has **no connection at
//! all**: the browser makes them, keeps them, reuses them and closes them,
//! and tells a page nothing about any of it.
//!
//! # Three of the five variants have no emitter here
//!
//! [`Connected`](hclient_core::unversioned::Connected),
//! [`Reused`](hclient_core::unversioned::Reused) and
//! [`Closed`](hclient_core::unversioned::Closed) are not emitted, and the
//! reason is not that it would be awkward — there is nothing to read.
//! `fetch()` takes a `Request` and resolves to a `Response`; the Fetch
//! Standard exposes no connection object, no connection identity, and no
//! event that fires when one is made or goes away. This is the mirror of
//! `hclient-native`'s refusal to add a *request queued* variant and of
//! `hclient-h3`'s report that `CloseReason::Ended` has no subject in QUIC:
//! **an event nothing can emit is a capability that lies**, and an event
//! set is a capability.
//!
//! ## The `Performance` surface looks like it disagrees, and it does not
//!
//! `PerformanceResourceTiming` carries `domainLookupStart`/`End`,
//! `connectStart`/`End`, `secureConnectionStart` and `nextHopProtocol` —
//! which is very nearly
//! [`ConnectTiming`](hclient_core::unversioned::ConnectTiming) plus
//! `Connected::version`. It was considered and rejected, on three grounds
//! — **the first two measured in `tests/hooks_timing.rs`, the third read
//! off the specification and said to be**:
//!
//! - **An entry cannot be attributed to a request.** The only handle is
//!   `name`, the URL. Two fetches of one URL produce two entries that are
//!   equal in every field a caller could match on, so a transport with two
//!   requests in flight to one origin cannot say which timing belongs to
//!   which — and a `ConnectionId` minted against the wrong one is worse
//!   than none.
//! - **The entry does not exist yet.** It is queued when the resource
//!   finishes, and `Transport::execute` returns when the *head* arrives,
//!   with the body still streaming. A `Connected` must precede the `Head`
//!   it explains; here the material for it arrives after the body.
//! - **Cross-origin it is all zeroes** — Resource Timing Level 2 §4.2:
//!   without `Timing-Allow-Origin` every phase timestamp is `0` and
//!   `nextHopProtocol` is the empty string. The general case for an HTTP
//!   client is exactly the case with no timing in it. **Not measured
//!   here**: a `wasm-pack` harness serves one origin and has no network,
//!   and the two ways of reaching the same socket under another host name
//!   both fail before a socket exists. `tests/hooks_timing.rs`'s last
//!   test records the attempts.
//!
//! Either of the first two is sufficient on its own, which is why the
//! third being unmeasured costs nothing.
//!
//! So there is no honest `Connected` to emit, and without one there is no
//! connection for a `Closed` to close or a `Reused` to name.
//!
//! # [`Head`] is the one, and two of its five fields had no source
//!
//! `uri`, `status` and `elapsed` are this transport's own facts and are
//! reported as such. The other two were recorded as debts owed by the seam
//! and have since been answered, one
//! each way — §9 of that document is the working:
//!
//! - **`id` is [`ConnectionId::UNWATCHED`], and that is not a borrowing.**
//!   The value means *this event names no connection*. Its other producer
//!   is a build with `Hooks::WATCHING == false`, whose events by that
//!   const's own definition nobody reads — so a hook can only ever meet
//!   this value in the sense used here, and a second value distinguishing
//!   the two would be one no caller decision turns on. What was wrong was
//!   the constant's doc comment, which named a producer as if it were the
//!   meaning; it says the meaning now, and nothing changed shape.
//! - **`version` is `None`, and the field is an `Option` because of
//!   this.** The Fetch Standard's `Response` has no protocol member at
//!   all; the browser knows whether it spoke HTTP/1.1, h2 or h3 and does
//!   not say. This crate used to report `resp.version()`, which is
//!   `http`'s builder default, `HTTP/1.1`, on every response it has ever
//!   produced — a value a caller cannot tell from an HTTP/1.1 exchange
//!   somebody watched happen. `Capabilities::version_reported` is `false`
//!   here and says the same thing, but a [`Hooks`] impl is handed an
//!   [`Event`](hclient_core::unversioned::Event) and no capabilities, so
//!   the honest value has to be in the event.
//!
//! `Connected::version` and `Reused::version` stayed plain, which is the
//! line that keeps this from being a change made for one backend: only a
//! transport that owns a connection emits either, and owning one means
//! having negotiated its protocol.
//!
//! # Zero cost when nobody is watching
//!
//! Every clock read whose only purpose is an event goes through [`mark`],
//! and `H::WATCHING` is a `const`, so on
//! [`NoHooks`](hclient_core::unversioned::NoHooks) the `then` is a
//! compile-time `false` and there is nothing left to remove — not a
//! branch, not a call. The same gate stands in front of the one allocation
//! the feature needs, the clone of the request's `Uri` (see
//! `Fetch::execute`), because the request is consumed by
//! `convert::to_web_request` before the response exists to report.
//!
//! `tests/hooks_cost.rs` counts the reads from outside, and here the
//! counting clock is the browser's own: the test replaces
//! `Performance.prototype.now` with a wrapper that tallies its calls. That
//! is the same measurement `hclient-native` and `hclient-h3` make through
//! a counting `Timer` — this crate has no runtime seam to inject one
//! through, because in a browser the runtime *is* the browser, so the
//! clock is replaced where it actually lives.
//!
//! # The clock is `performance.now()`, not this crate's [`Timer`]
//!
//! [`crate::BrowserClock`] is this crate's
//! [`Timer`](hclient_core::unversioned::Timer), and it reads
//! `Date.now()` — milliseconds since the epoch, which is what a *sleep*
//! wants and what SSE reconnect asks it for. `Head::elapsed` is a
//! duration, and a wall clock is the wrong instrument for one twice over:
//! it can step backwards while a request is in flight, and its resolution
//! is a whole millisecond, which is longer than most of the requests this
//! measurement exists to explain. `performance.now()` is monotonic by
//! specification and sub-millisecond, so it is what the event uses.
//!
//! Found through `js_sys::global()` rather than `web_sys::window()`, for
//! the reason `Fetch::execute` already gives for finding `fetch` that way
//! and `crate::timer` gives for `setTimeout`: this must also work from a
//! Worker, which has a `performance` and no `window`.

use core::time::Duration;
use hclient_core::unversioned::Hooks;
use wasm_bindgen::{JsCast, JsValue};

/// `performance.now()`: milliseconds since the time origin, monotonic.
///
/// `expect` rather than a fallback to `Date.now()`, matching
/// `crate::timer`'s treatment of `setTimeout`: `performance` is exposed by
/// `Window` and by every `WorkerGlobalScope`, so a host without it has
/// lied about being a browser, and a fallback here would be a second
/// unreachable clock whose epoch disagrees with this one's.
fn now() -> f64 {
    js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("performance"))
        .expect(
            "global scope exposes performance — true of Window and every \
             WorkerGlobalScope",
        )
        .unchecked_into::<web_sys::Performance>()
        .now()
}

/// A stopwatch that does not exist when nobody is watching.
///
/// See the module doc: `H::WATCHING` is a `const`, so this is the whole of
/// the zero-cost claim and the only place this crate reads a clock for an
/// event.
pub(crate) fn mark<H: Hooks>() -> Option<f64> {
    hclient_core::unversioned::mark::<H, _>(now)
}

/// The other half of [`mark`]: the interval since one, or `ZERO` when
/// there was no mark to measure from.
///
/// The `ZERO` is never reported — a build that produced `None` above has
/// no hook to hand it to — which is why this is not an `Option<Duration>`
/// that the one call site would then have to unwrap into a lie.
///
/// It takes no `H`: the gate is [`mark`]'s, once, and a second `H` here
/// would be a second place that has to agree with it.
///
/// `max(0.0)` for the same reason [`crate::BrowserClock::elapsed_since`]
/// has one: `Duration::from_secs_f64` panics on a negative, and a clock
/// this code did not install is not this code's to trust absolutely.
pub(crate) fn since(at: Option<f64>) -> Duration {
    hclient_core::unversioned::since(at, |t| {
        Duration::from_secs_f64((now() - t).max(0.0) / 1000.0)
    })
}
