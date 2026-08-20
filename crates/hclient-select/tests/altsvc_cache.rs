//! The slow tier's memory, and the clock is a parameter.
//!
//! Nothing here sleeps. `AltSvcCache` never reads a clock — `now` arrives
//! as an argument, exactly as it does on `hclient-native`'s negative cache
//! — so a lifetime measured in days is tested by handing it a `Duration`
//! rather than by waiting for one. That is what makes `ma` testable at
//! all: RFC 7838's default is twenty-four hours.
//!
//! The end of this file is where the two decisions that are *not* about
//! time live: what a present field replaces (RFC 7838 §3) and what
//! survives a network change (§2.2, and the part this crate cannot see for
//! itself).
#![cfg(not(target_family = "wasm"))]

use hclient_select::altsvc::{AltSvcCache, FieldValue, Origin, parse};
use std::time::Duration;

const ORIGIN: &str = "example.com";

fn origin() -> Origin {
    Origin::new(ORIGIN, 443)
}

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

/// Feed the cache a field value as it would arrive from a response.
fn note(cache: &AltSvcCache, field: &str, now: Duration) {
    cache.note(&origin(), &parse(field.as_bytes()), now);
}

// --- the lifetime is the origin's own -----------------------------------

/// `ma` is read, and it is what the entry lives for.
#[test]
fn an_entry_lives_exactly_as_long_as_its_ma_says() {
    let c = AltSvcCache::default();
    note(&c, r#"h3=":443"; ma=60"#, secs(0));

    assert!(c.advertises_h3(&origin(), secs(0)));
    assert!(c.advertises_h3(&origin(), secs(59)));
    assert!(
        !c.advertises_h3(&origin(), secs(60)),
        "the window is half-open: an entry whose ma has exactly run out is stale"
    );
}

/// The lifetime is measured from when the advertisement was heard, not
/// from the cache's own beginning — the same entry noted later expires
/// later.
#[test]
fn the_lifetime_starts_when_the_advertisement_was_heard() {
    let c = AltSvcCache::default();
    note(&c, r#"h3=":443"; ma=60"#, secs(1_000));

    assert!(c.advertises_h3(&origin(), secs(1_059)));
    assert!(!c.advertises_h3(&origin(), secs(1_060)));
}

/// RFC 7838 §3.1's default, exercised at the timescale it actually names:
/// still fresh at 23 hours 59 minutes, stale at 24 hours. A test that
/// waited for this would be a test nobody runs.
#[test]
fn a_field_with_no_ma_lives_for_the_rfcs_twenty_four_hours() {
    let c = AltSvcCache::default();
    note(&c, r#"h3=":443""#, secs(0));

    assert!(c.advertises_h3(&origin(), secs(86_399)));
    assert!(!c.advertises_h3(&origin(), secs(86_400)));
}

/// `ma=0` is a removal, and the half-open window is what makes it one
/// without a special case: an entry that expires at the instant it was
/// stored is stale at that instant.
#[test]
fn ma_zero_is_a_removal() {
    let c = AltSvcCache::default();
    note(&c, r#"h3=":443"; ma=86400"#, secs(0));
    assert!(c.advertises_h3(&origin(), secs(10)));

    note(&c, r#"h3=":443"; ma=0"#, secs(10));
    assert!(
        !c.advertises_h3(&origin(), secs(10)),
        "an origin that says `ma=0` has withdrawn the alternative"
    );
}

/// RFC 9110 §5.6.7's saturation, carried all the way through: a `ma`
/// larger than a `u64` becomes the largest representable one, and adding
/// it to any `now` must not overflow.
#[test]
fn an_enormous_ma_saturates_rather_than_overflowing() {
    let c = AltSvcCache::default();
    note(
        &c,
        r#"h3=":443"; ma=99999999999999999999999999999999"#,
        Duration::MAX - secs(1),
    );
    assert!(c.advertises_h3(&origin(), Duration::MAX - secs(1)));
}

/// A stale entry is forgotten by the lookup that found it stale, rather
/// than by a sweep — `hclient-native`'s model, and the reason is that this
/// is the only place that asks.
///
/// Observable because the *later* question is asked first: if the expiry
/// were only a comparison, an earlier `now` would find the entry fresh
/// again, and a cache that answered differently depending on the order it
/// was asked would be a cache with a memory of the future.
#[test]
fn a_stale_entry_is_forgotten_and_does_not_come_back() {
    let c = AltSvcCache::default();
    note(&c, r#"h3=":443"; ma=60"#, secs(0));

    assert!(!c.advertises_h3(&origin(), secs(100)), "stale, and removed");
    assert!(
        !c.advertises_h3(&origin(), secs(10)),
        "it was removed, so an earlier question does not revive it"
    );
}

// --- what is actionable -------------------------------------------------

/// Only `h3`, and only at the origin's own authority.
#[test]
fn only_h3_at_this_origin_is_remembered() {
    for field in [
        r#"h2=":443""#,              // another protocol
        r#"h3=":8443""#,             // another port
        r#"h3="other.example:443""#, // another host
        r#"h3-29=":443""#,           // a draft version is not h3
        r#"H3=":443""#,              // an ALPN name is an octet string
    ] {
        let c = AltSvcCache::default();
        note(&c, field, secs(0));
        assert!(!c.advertises_h3(&origin(), secs(0)), "{field}");
    }
}

/// An advertisement naming this origin's host explicitly is the same as
/// one that omits it, case and all.
#[test]
fn naming_this_origin_is_the_same_as_omitting_it() {
    for field in [
        r#"h3=":443""#,
        r#"h3="example.com:443""#,
        r#"h3="EXAMPLE.COM:443""#,
    ] {
        let c = AltSvcCache::default();
        note(&c, field, secs(0));
        assert!(c.advertises_h3(&origin(), secs(0)), "{field}");
    }
}

/// The origin key is a host *and* a port, and the host is not
/// case-sensitive.
#[test]
fn the_key_is_the_whole_origin() {
    let c = AltSvcCache::default();
    note(&c, r#"h3=":443""#, secs(0));

    assert!(c.advertises_h3(&Origin::new("EXAMPLE.com", 443), secs(0)));
    assert!(!c.advertises_h3(&Origin::new("example.com", 8443), secs(0)));
    assert!(!c.advertises_h3(&Origin::new("other.example", 443), secs(0)));
}

/// The first `h3` at this origin wins. RFC 7838 gives list order no
/// meaning beyond the order the origin wrote them in, and choosing by `ma`
/// would be this crate preferring whichever entry keeps itself alive
/// longest.
#[test]
fn the_first_actionable_h3_in_the_list_is_the_one_taken() {
    let c = AltSvcCache::default();
    note(
        &c,
        r#"h3="elsewhere:443"; ma=99999, h3=":443"; ma=60, h3=":443"; ma=99999"#,
        secs(0),
    );
    assert!(c.advertises_h3(&origin(), secs(59)));
    assert!(
        !c.advertises_h3(&origin(), secs(60)),
        "the second member is the first actionable one, and its ma is 60"
    );
}

// --- what a present field replaces --------------------------------------

/// RFC 7838 §3: *"When an Alt-Svc response header field is received from
/// an origin, its value invalidates and replaces all cached alternative
/// services for that origin."* So a field that no longer mentions `h3`
/// takes the entry away, without needing to say `clear`.
#[test]
fn a_field_that_no_longer_offers_h3_removes_what_was_stored() {
    let c = AltSvcCache::default();
    note(&c, r#"h3=":443"; ma=86400"#, secs(0));
    assert!(c.advertises_h3(&origin(), secs(1)));

    note(&c, r#"h2=":443""#, secs(1));
    assert!(!c.advertises_h3(&origin(), secs(1)));
}

/// …including a field nobody could parse. The direction is deliberate
/// twice over: it is what "invalidates and replaces" says, and forgetting
/// means going back to TCP, so the worst a garbled or hostile field can do
/// is cost a request the faster protocol.
#[test]
fn a_field_that_could_not_be_parsed_at_all_also_removes() {
    let c = AltSvcCache::default();
    note(&c, r#"h3=":443"; ma=86400"#, secs(0));

    note(&c, "!!! not a field value !!!", secs(1));
    assert!(!c.advertises_h3(&origin(), secs(1)));
}

/// `clear` removes, which is the same outcome by a different instruction —
/// and it is a separate test because the two travel through different
/// arms.
#[test]
fn clear_removes() {
    let c = AltSvcCache::default();
    note(&c, r#"h3=":443"; ma=86400"#, secs(0));

    note(&c, "clear", secs(1));
    assert!(!c.advertises_h3(&origin(), secs(1)));
}

/// A later field replaces an earlier one's lifetime rather than extending
/// or shortening it by halves: the newest statement is the whole answer.
#[test]
fn a_later_field_replaces_the_earlier_ones_lifetime() {
    let c = AltSvcCache::default();
    note(&c, r#"h3=":443"; ma=86400"#, secs(0));
    note(&c, r#"h3=":443"; ma=10"#, secs(0));
    assert!(!c.advertises_h3(&origin(), secs(10)), "shortened");

    note(&c, r#"h3=":443"; ma=10"#, secs(0));
    note(&c, r#"h3=":443"; ma=86400"#, secs(0));
    assert!(c.advertises_h3(&origin(), secs(1000)), "lengthened");
}

/// One origin's field says nothing about another's.
#[test]
fn a_field_replaces_only_its_own_origins_entry() {
    let c = AltSvcCache::default();
    let other = Origin::new("other.example", 443);
    c.note(&origin(), &parse(br#"h3=":443""#), secs(0));
    c.note(&other, &parse(br#"h3=":443""#), secs(0));

    c.note(&origin(), &FieldValue::Clear, secs(1));
    assert!(!c.advertises_h3(&origin(), secs(1)));
    assert!(
        c.advertises_h3(&other, secs(1)),
        "a neighbour's entry stands"
    );
}

// --- scope: the network change this crate cannot see --------------------

/// RFC 7838 §2.2: *"clients SHOULD remove from cache all alternative
/// services that lack the 'persist' flag with the value '1' when they
/// detect such a change"*. `persist` has exactly one reader, and this is
/// it.
#[test]
fn a_network_change_forgets_what_did_not_ask_to_persist() {
    let c = AltSvcCache::default();
    let persistent = Origin::new("persistent.example", 443);
    c.note(&origin(), &parse(br#"h3=":443"; ma=86400"#), secs(0));
    c.note(
        &persistent,
        &parse(br#"h3=":443"; ma=86400; persist=1"#),
        secs(0),
    );

    c.network_changed();

    assert!(
        !c.advertises_h3(&origin(), secs(1)),
        "an ordinary entry was reachable on the network we have left"
    );
    assert!(
        c.advertises_h3(&persistent, secs(1)),
        "`persist=1` is the origin's hint that this one is not network-specific"
    );
}

/// A `persist` the RFC makes us ignore does not keep an entry alive across
/// the change — ignoring the parameter means it was never there.
#[test]
fn a_persist_value_the_rfc_ignores_does_not_survive_a_network_change() {
    for field in [
        r#"h3=":443"; persist=0"#,
        r#"h3=":443"; persist=2"#,
        r#"h3=":443"; persist=true"#,
    ] {
        let c = AltSvcCache::default();
        note(&c, field, secs(0));
        assert!(c.advertises_h3(&origin(), secs(0)), "{field}");
        c.network_changed();
        assert!(!c.advertises_h3(&origin(), secs(0)), "{field}");
    }
}

/// A network change is not an expiry: a persistent entry still runs out of
/// `ma` on its own schedule.
#[test]
fn persisting_across_a_network_change_is_not_living_for_ever() {
    let c = AltSvcCache::default();
    note(&c, r#"h3=":443"; ma=60; persist=1"#, secs(0));
    c.network_changed();

    assert!(c.advertises_h3(&origin(), secs(59)));
    assert!(!c.advertises_h3(&origin(), secs(60)));
}

/// Cheap to clone, and every clone is the same cache — a memory that
/// lasted one request would be no memory at all.
#[test]
fn a_clone_is_the_same_cache() {
    let c = AltSvcCache::default();
    let d = c.clone();
    note(&c, r#"h3=":443""#, secs(0));
    assert!(d.advertises_h3(&origin(), secs(0)));

    d.network_changed();
    assert!(!c.advertises_h3(&origin(), secs(0)));
}
