//! What a guest that owns no socket can tell a [`Hooks`], which is one
//! event out of four — and the reason is in the WIT rather than in this
//! crate.
//!
//! The vocabulary is `http_ng_core::unversioned::{Hooks, Event}` and it is
//! **unchanged** here. `http-ng-native` derived the event set from a
//! transport that dials its own sockets; `http-ng-h3` tested it against
//! connections that are *shared*; `http-ng-fetch` and this crate test it
//! against transports that have **no connection at all**. Both reached the
//! same answer independently, from different evidence, which is the part
//! worth knowing.
//!
//! # `wasi:http@0.3.0` has no connection in it, and that is checkable
//!
//! The whole client interface is one function:
//!
//! ```wit
//! interface client {
//!   send: async func(request: request) -> result<response, error-code>;
//! }
//! ```
//!
//! and the `response` resource it resolves to has exactly three accessors
//! — `get-status-code`, `get-headers`, `consume-body`. There is no
//! connection resource anywhere in the package, no handle a `Connected`
//! could name, no notification of one being opened or closed, and
//! `request-options` carries three timeouts the *guest* sets and nothing
//! the host reports back.
//!
//! So [`Connected`](http_ng_core::unversioned::Connected),
//! [`Reused`](http_ng_core::unversioned::Reused) and
//! [`Closed`](http_ng_core::unversioned::Closed) have no emitter here.
//! This is not a limitation of the host either:
//! `Capabilities::connection_reuse` is already `ReuseSupport::None` for a
//! measured reason (`WasiHttp::new`), so even the fact a `Reused` would
//! carry does not exist — and if a future host did pool, the guest would
//! still not be told.
//!
//! The one thing that *sounds* like a connection event is `error-code`,
//! which has `connection-refused`, `connection-terminated`,
//! `connection-timeout` and eight more of that family. They are not
//! events: they are how `send` fails, they already reach the caller as a
//! classified `http_ng_core::Error` through `convert::wasi_err`, and
//! reporting one as `CloseReason::Failed` would announce the end of a
//! connection whose beginning was never announced, under
//! [`ConnectionId::UNWATCHED`](http_ng_core::unversioned::ConnectionId::UNWATCHED),
//! to a caller who is about to be handed the same error anyway.
//!
//! # [`Head`] is the one, and two of its five fields have no source
//!
//! `uri`, `status` and `elapsed` are this transport's own facts. The other
//! two are not, and they are the *same two* `http-ng-fetch` cannot fill:
//!
//! - **`id` is `ConnectionId::UNWATCHED`**, whose own doc ties that value
//!   to `Hooks::WATCHING == false`. Here somebody is watching and there is
//!   no connection to name. It is the only value
//!   `ConnectionId::next` never returns, so it cannot collide with a real
//!   one — but the seam has no value meaning *there is no connection
//!   here*, and this is it being borrowed.
//! - **`version` is not observed**, and here that is stronger than a
//!   browser's silence: `wasi:http` has **no HTTP version concept at
//!   all**. Neither `request` nor `response` has a version accessor, by
//!   design — the host decides what to speak and the guest is
//!   version-agnostic. The value reported is `resp.version()`, read off
//!   the very response this transport is about to hand back, so the event
//!   and the response cannot disagree; that value is
//!   `http`'s default, `HTTP/1.1`, because that is what
//!   `wasip3::http_compat::http_from_wasi_response` builds.
//!
//! Both are defects in the seam's wording rather than in this backend,
//! recorded in `docs/v04-w2-hooks-ambient.md` rather than fixed by editing
//! `http-ng-core` for one backend's benefit — the treatment `http-ng-h3`
//! gave `ConnectTiming::tls`.
//!
//! # Zero cost when nobody is watching, and what this crate cannot prove
//!
//! Every clock read whose only purpose is an event goes through [`mark`]
//! and [`since`], and `H::WATCHING` is a `const`, so on
//! [`NoHooks`](http_ng_core::unversioned::NoHooks) the `then` is a
//! compile-time `false` and there is nothing left to remove.
//!
//! `http-ng-native` and `http-ng-h3` prove that with a counting `Timer`
//! handed to the transport, and `http-ng-fetch` proves it by replacing
//! `Performance.prototype.now` in the page. **Neither is available here**,
//! and the reason is the same fact this crate exists to state: a
//! `wasi:http` guest needs no runtime, so `WasiHttp` has no runtime seam
//! to inject a clock through — and the clock it does read,
//! `std::time::Instant::now()`, is a host call the guest cannot observe
//! itself making.
//!
//! Two ways of counting it from outside were tried and **measured to not
//! work**, rather than assumed:
//!
//! - **Diffing the component's imports.** If the hookless build read no
//!   clock, the linker would drop the import and
//!   `wasm-tools component wit` would show the difference. It does not: a
//!   component whose entire body is `WasiHttp::new()` already imports
//!   `wasi:clocks/monotonic-clock@0.2.9`, because wasi-libc does.
//! - **Taking the clock away.** `wasmtime run -S cli=n` would make a
//!   clock read a trap, so a hookless guest completing would be the
//!   proof. It refuses before the guest runs at all — measured on
//!   wasmtime 47.0.3: *"component imports instance `wasi:io/poll@0.2.9`,
//!   but a matching implementation was not found in the linker"* — so it
//!   cannot separate "did not call the clock" from "could not start".
//!
//! What is proved instead is exact, in two halves that together cover the
//! same ground:
//!
//! 1. [`mark`] returns `None` under `NoHooks` — the clock read is inside
//!    a closure that is not called — checked on the real target by the
//!    tests at the bottom of this file.
//! 2. [`mark`] and [`since`] are the **only** clock reads in this crate,
//!    checked mechanically rather than by reading:
//!    `tests/hooks.rs`'s `the_clock_is_read_in_exactly_one_place` walks
//!    `src/` and fails if `Instant::now()` appears anywhere else.
//!
//! A counting clock would collapse those two into one measurement. What
//! would make one possible is a clock seam on `WasiHttp`, and that is
//! declined rather than deferred: a transport whose whole premise is that
//! the host owns everything and it needs no runtime would gain a runtime
//! parameter in order to be tested.

use core::time::Duration;
use http_ng_core::unversioned::Hooks;
use std::time::Instant;

/// A stopwatch that does not exist when nobody is watching.
///
/// See the module doc: `H::WATCHING` is a `const`, so on `NoHooks` this is
/// a compile-time `false` and the closure is not merely skipped, it is not
/// there.
pub(crate) fn mark<H: Hooks>() -> Option<Instant> {
    H::WATCHING.then(Instant::now)
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
/// `Instant::now().saturating_duration_since(t)` rather than
/// `t.elapsed()`, which is the same thing: it puts the crate's second and
/// last clock read in the same spelling as its first, so
/// `tests/hooks.rs`'s source check has one pattern to look for rather than
/// two.
pub(crate) fn since(at: Option<Instant>) -> Duration {
    match at {
        Some(t) => Instant::now().saturating_duration_since(t),
        None => Duration::ZERO,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_ng_core::unversioned::{Event, NoHooks};

    /// A hook that does nothing but exist.
    struct Watching;
    impl Hooks for Watching {
        fn on(&self, _event: Event<'_>) {}
    }

    /// The zero-cost claim, as an equality on the one gate.
    ///
    /// `None` is not "the clock read a value we threw away" — the closure
    /// `bool::then` takes is never called, so `Instant::now()` does not
    /// run. Combined with `tests/hooks.rs`'s check that this is the only
    /// clock read in the crate, that is the whole of what the counting
    /// clocks in `http-ng-native` and `http-ng-h3` measure.
    ///
    /// This runs on `wasm32-wasip2` under a real wasmtime (`just
    /// test-wasi`) as well as on the host, so the target where the claim
    /// matters is the target where it is checked.
    #[test]
    fn a_hookless_build_takes_no_mark_and_a_watched_one_does() {
        assert!(
            mark::<NoHooks>().is_none(),
            "`NoHooks::WATCHING` is `false`, so the clock is not read"
        );
        assert!(
            mark::<Watching>().is_some(),
            "and a real hook must get a real mark, or the first assertion \
             would pass against a transport whose timing was simply broken"
        );
    }

    /// `since` never invents an interval, and never panics on one.
    #[test]
    fn an_unmarked_interval_is_zero_and_a_marked_one_is_not_negative() {
        assert_eq!(
            since(None),
            Duration::ZERO,
            "no mark, no interval — and nothing to report it to"
        );
        let t = Instant::now();
        assert!(since(Some(t)) < Duration::from_secs(1));
    }

    /// The `const` the whole claim rests on, asserted at compile time
    /// because that is where it holds.
    #[test]
    fn the_gate_is_a_compile_time_constant() {
        const { assert!(!NoHooks::WATCHING) };
        const { assert!(Watching::WATCHING) };
        assert_eq!(std::mem::size_of::<NoHooks>(), 0);
    }
}
