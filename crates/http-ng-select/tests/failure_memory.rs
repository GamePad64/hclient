//! The negative half's memory, and the clock is a parameter.
//!
//! Nothing here sleeps. `H3Failures` never reads a clock — `now` arrives as
//! an argument, exactly as it does on `AltSvcCache` next door and on
//! `http-ng-native`'s negative cache — so a five-minute window is tested by
//! handing it a `Duration` rather than by waiting for one.
//!
//! `tests/h3_failure.rs` is the other half: two real servers, and what a
//! request does about all of this. This file is the rules.
#![cfg(not(target_family = "wasm"))]

use http_ng_select::altsvc::Origin;
use http_ng_select::{H3_FAILURE_TTL, H3Failures};
use std::time::Duration;

fn origin() -> Origin {
    Origin::new("example.com", 443)
}

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

// --- the window ---------------------------------------------------------

/// Nothing is suppressed until something failed. The first assertion in the
/// file, because every other one below is a delta against it.
#[test]
fn an_origin_nothing_failed_at_is_not_suppressed() {
    let f = H3Failures::default();
    assert!(!f.suppressed(&origin(), secs(0)));
    assert!(!f.suppressed(&origin(), secs(1_000_000)));
}

/// A failure holds the origin off for exactly [`H3_FAILURE_TTL`], and the
/// window is half-open — an entry whose window has exactly closed is gone,
/// which is the same rule `AltSvcCache` applies to `ma`. Two memories
/// consulted for one request should not disagree about what "expired"
/// means.
#[test]
fn a_failure_lives_exactly_as_long_as_the_ttl_says() {
    let f = H3Failures::default();
    f.note(&origin(), secs(0));

    assert!(f.suppressed(&origin(), secs(0)));
    assert!(f.suppressed(&origin(), H3_FAILURE_TTL - Duration::from_nanos(1)));
    assert!(
        !f.suppressed(&origin(), H3_FAILURE_TTL),
        "the window is half-open"
    );
}

/// The window is measured from when the failure happened, not from the
/// memory's own beginning.
#[test]
fn the_window_starts_when_the_connect_failed() {
    let f = H3Failures::default();
    f.note(&origin(), secs(1_000));

    assert!(f.suppressed(&origin(), secs(1_000) + H3_FAILURE_TTL - secs(1)));
    assert!(!f.suppressed(&origin(), secs(1_000) + H3_FAILURE_TTL));
}

/// A second failure restarts the window rather than extending the first
/// one's deadline — so an origin that fails once and is never tried again
/// is forgotten one TTL later and not longer.
#[test]
fn a_second_failure_restarts_the_window() {
    let f = H3Failures::default();
    f.note(&origin(), secs(0));
    f.note(&origin(), secs(100));

    assert!(f.suppressed(&origin(), secs(100) + H3_FAILURE_TTL - secs(1)));
    assert!(!f.suppressed(&origin(), secs(100) + H3_FAILURE_TTL));
}

/// A lapsed entry is removed by the lookup that found it lapsed, so an
/// earlier question does not revive it.
#[test]
fn a_lapsed_entry_is_forgotten_and_does_not_come_back() {
    let f = H3Failures::default();
    f.note(&origin(), secs(0));

    assert!(
        !f.suppressed(&origin(), secs(100_000)),
        "lapsed, and removed"
    );
    assert!(
        !f.suppressed(&origin(), secs(0)),
        "it was removed, so an earlier question does not revive it"
    );
    assert!(format!("{f:?}").contains("suppressed: 0"));
}

// --- the key is the whole origin ----------------------------------------

/// A failure at one origin says nothing about another. The mutation this
/// exists to fail is a memory keyed on nothing, or on the host alone.
#[test]
fn a_failure_at_one_origin_suppresses_no_other() {
    let f = H3Failures::default();
    f.note(&origin(), secs(0));

    assert!(f.suppressed(&origin(), secs(0)));
    assert!(!f.suppressed(&Origin::new("other.example", 443), secs(0)));
    assert!(
        !f.suppressed(&Origin::new("example.com", 8443), secs(0)),
        "the same host at another port is another origin — another server, \
         quite possibly another network path"
    );
}

/// `Example.COM` and `example.com` name one origin, which is `Origin`'s own
/// rule and is asserted here because this memory is the second reader of
/// it: a key that told them apart would remember a failure under a name the
/// next request does not use.
#[test]
fn the_host_is_one_origin_however_it_is_spelled() {
    let f = H3Failures::default();
    f.note(&Origin::new("Example.COM", 443), secs(0));
    assert!(f.suppressed(&origin(), secs(0)));
}

// --- scope --------------------------------------------------------------

/// **The decision this type differs from `AltSvcCache` on.** A network
/// change forgets every failure, with no exception for anything.
///
/// The cache next door keeps `persist=1` entries, because that flag is the
/// *origin's* claim that its advertisement is a property of the origin
/// rather than of the path. Nothing says that about a failure: *"UDP/443
/// did not get through"* is a fact about the network, no peer ever asked us
/// to carry it, and it is exactly the entry a network change makes
/// certainly wrong.
#[test]
fn a_network_change_forgets_every_failure() {
    let f = H3Failures::default();
    f.note(&Origin::new("a.example", 443), secs(0));
    f.note(&Origin::new("b.example", 443), secs(0));
    f.note(&Origin::new("b.example", 8443), secs(0));

    f.network_changed();

    for o in [
        Origin::new("a.example", 443),
        Origin::new("b.example", 443),
        Origin::new("b.example", 8443),
    ] {
        assert!(
            !f.suppressed(&o, secs(0)),
            "{o:?} survived a network change"
        );
    }
    assert!(format!("{f:?}").contains("suppressed: 0"));
}

/// A network change clears what is there and does not stop the memory
/// working afterwards — the mutation this fails is one that clears by
/// replacing the map with something nothing can write to again.
#[test]
fn a_network_change_is_not_the_end_of_the_memory() {
    let f = H3Failures::default();
    f.note(&origin(), secs(0));
    f.network_changed();
    f.note(&origin(), secs(10));

    assert!(f.suppressed(&origin(), secs(10)));
}

/// A clone is the same memory, which is what makes it shareable by every
/// request one transport makes: a memory that lasted one request would be
/// no memory at all.
#[test]
fn a_clone_is_the_same_memory() {
    let f = H3Failures::default();
    let g = f.clone();
    f.note(&origin(), secs(0));

    assert!(g.suppressed(&origin(), secs(0)));
    g.network_changed();
    assert!(!f.suppressed(&origin(), secs(0)));
}
