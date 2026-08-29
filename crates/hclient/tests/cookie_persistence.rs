//! Saving a jar and loading it back: what survives the round trip, what a
//! reload must not do, and the refusals only the load side can meet.
//!
//! **The observer is the `Cookie` header**, not the jar's view of itself,
//! wherever a claim can be made that way — a jar that reloads perfectly
//! into the wrong §5.4 order passes every assertion about its own contents
//! and fails the first one here. Where the claim is about a refusal there
//! is nothing on the wire to look at, and the error is the observation.
//!
//! The first two tests are the *measurement* rather than a property of the
//! new code: they pin what the round trip that already existed —
//! `iter` out, a synthesised `Set-Cookie` back through `store` — does and
//! does not preserve. Both were written before [`CookieJar::restore`]
//! existed, because a feature whose motivating claim is wrong is worse
//! than no feature.
//!
//! **Six tests need the public suffix list and say so.** Without the
//! `public-suffix` feature every `Domain` attribute is refused (§5.7 has
//! no list to check it against), so a jar in that build holds host-only
//! cookies and nothing else — and this file's last test is the claim
//! `cookie_without_the_list.rs` makes for `store`, asked of `restore`:
//! **a build with no list is narrower, never wider.**
#![cfg(feature = "cookies")]

use hclient::cookie::{Cookie, CookieJar, CookieRecord, Limits, Rejected};
// Only the round-trip test reads it back, and that one needs the list.
#[cfg(feature = "public-suffix")]
use hclient::cookie::SameSite;
use http::{HeaderValue, Uri};
use std::time::{Duration, SystemTime};

fn t(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + secs)
}

fn uri(s: &str) -> Uri {
    s.parse().expect("uri")
}

fn set(jar: &mut CookieJar, u: &str, header: &str, now: SystemTime) {
    jar.store(
        &uri(u),
        &HeaderValue::from_str(header).expect("header"),
        now,
    )
    .expect("should store");
}

/// The one saved cookie in a jar holding exactly one.
fn only(jar: &CookieJar) -> CookieRecord {
    let mut it = jar.records();
    let one = it.next().expect("a record");
    assert!(it.next().is_none(), "expected exactly one record");
    one
}

/// A record for a plain host-only cookie on `example.com`, all four times
/// supplied by the caller.
fn record(name: &str, path: &str, creation: SystemTime, last_access: SystemTime) -> CookieRecord {
    CookieRecord {
        name: name.to_owned(),
        value: "v".to_owned(),
        domain: "example.com".to_owned(),
        path: path.to_owned(),
        expires: t(1_000_000),
        creation,
        last_access,
        host_only: true,
        secure: false,
        http_only: false,
        same_site: None,
    }
}

// ---------------------------------------------------------------------
// What the pre-existing round trip already did
// ---------------------------------------------------------------------

/// The claim this work was proposed on — *"a restored jar sends cookies in
/// a different order"* — is **false while the clock only moves forwards**,
/// and this is the test that says so.
///
/// §5.4 sorts by path length and then by creation time, and a replay gives
/// every cookie the same creation time, so the order looks lost. It is not:
/// `Cookie`'s insertion sequence breaks that tie, `iter` yields in
/// insertion order, and a replay in that order rebuilds the same sequence.
#[test]
fn replaying_set_cookie_headers_keeps_the_order_while_the_clock_moves_forwards() {
    let u = "https://www.example.com/app/x";
    let mut jar = CookieJar::new();
    set(&mut jar, u, "a=1; Path=/app; Max-Age=100000", t(0));
    set(&mut jar, u, "b=2; Path=/app; Max-Age=100000", t(10));
    let before = jar.cookie_header(&uri(u), t(20)).expect("a header");

    let replay: Vec<String> = jar
        .iter()
        .map(|c| {
            format!(
                "{}={}; Path={}; Max-Age=100000",
                c.name(),
                c.value(),
                c.path()
            )
        })
        .collect();
    let mut restored = CookieJar::new();
    for header in &replay {
        set(&mut restored, u, header, t(1000));
    }

    assert_eq!(before, "a=1; b=2");
    assert_eq!(restored.cookie_header(&uri(u), t(1001)), Some(before));
}

/// And here is where it does break, which is what the record type is for.
///
/// A wall clock is not monotone — an NTP correction between two responses
/// is ordinary — so a cookie stored *later* can carry an *earlier*
/// creation time. §5.4 then orders on creation and disagrees with insertion
/// order; the replay flattens every creation time onto the restore instant,
/// so insertion order is all that is left and the disagreement is resolved
/// the wrong way.
#[test]
fn replaying_set_cookie_headers_loses_the_order_when_the_clock_steps_back() {
    let u = "https://www.example.com/app/x";
    let mut jar = CookieJar::new();
    set(&mut jar, u, "a=1; Path=/app; Max-Age=100000", t(100));
    set(&mut jar, u, "b=2; Path=/app; Max-Age=100000", t(10));
    let before = jar.cookie_header(&uri(u), t(200)).expect("a header");
    assert_eq!(
        before, "b=2; a=1",
        "b was created first, so §5.4 sends it first"
    );

    let replay: Vec<String> = jar
        .iter()
        .map(|c| {
            format!(
                "{}={}; Path={}; Max-Age=100000",
                c.name(),
                c.value(),
                c.path()
            )
        })
        .collect();
    let mut replayed = CookieJar::new();
    for header in &replay {
        set(&mut replayed, u, header, t(1000));
    }
    assert_eq!(
        replayed.cookie_header(&uri(u), t(1001)).expect("a header"),
        "a=1; b=2",
        "the replay reordered them"
    );

    // The same jar through the records, which carry the creation times.
    let saved: Vec<CookieRecord> = jar.records().collect();
    let mut restored = CookieJar::new();
    for r in saved {
        restored.restore(r, t(1000)).expect("should restore");
    }
    assert_eq!(restored.cookie_header(&uri(u), t(1001)), Some(before));
}

// ---------------------------------------------------------------------
// What a record carries
// ---------------------------------------------------------------------

#[cfg(feature = "public-suffix")]
#[test]
fn every_fact_a_cookie_has_survives_the_round_trip() {
    let u = "https://www.example.com/app/x";
    let mut jar = CookieJar::new();
    set(
        &mut jar,
        u,
        "sid=abc; Domain=example.com; Path=/app; Max-Age=100000; Secure; HttpOnly; SameSite=Lax",
        t(5),
    );
    // A retrieval moves the last-access time away from the creation time,
    // so the two cannot pass by being accidentally equal.
    jar.cookie_header(&uri(u), t(9));

    let saved = only(&jar);
    let mut restored = CookieJar::new();
    restored
        .restore(saved.clone(), t(1000))
        .expect("should restore");
    let back = only(&restored);

    assert_eq!(back, saved);
    assert_eq!(back.name, "sid");
    assert_eq!(back.value, "abc");
    assert_eq!(back.domain, "example.com");
    assert_eq!(back.path, "/app");
    assert_eq!(back.creation, t(5));
    assert_eq!(back.last_access, t(9));
    assert!(!back.host_only, "it carried a Domain");
    assert!(back.secure);
    assert!(back.http_only);
    assert_eq!(back.same_site, Some(SameSite::Lax));
}

/// The rule everybody knows about cookies, made a property of the type
/// rather than of a filter the save side has to remember.
#[test]
fn a_session_cookie_has_no_record_at_all() {
    let u = "https://www.example.com/app";
    let mut jar = CookieJar::new();
    set(&mut jar, u, "session=1", t(0));
    set(&mut jar, u, "kept=2; Max-Age=100000", t(0));

    assert_eq!(jar.len(), 2);
    let names: Vec<String> = jar.records().map(|r| r.name).collect();
    assert_eq!(names, ["kept"], "the session cookie is not saveable");

    let session = jar
        .iter()
        .find(|c| c.name() == "session")
        .expect("still held");
    assert!(!session.persistent());
    assert_eq!(Cookie::to_record(session), None);
}

// ---------------------------------------------------------------------
// What a reload must not do
// ---------------------------------------------------------------------

/// §5.3's identity: name, domain, path — and the host-only flag, which is
/// the part 6265 left out and 6265bis added.
#[cfg(feature = "public-suffix")]
#[test]
fn restoring_the_same_jar_twice_does_not_duplicate_a_cookie() {
    let u = "https://www.example.com/app";
    let mut jar = CookieJar::new();
    set(&mut jar, u, "a=1; Max-Age=100000", t(0));
    set(&mut jar, u, "a=2; Domain=example.com; Max-Age=100000", t(0));
    assert_eq!(jar.len(), 2, "host-only and domain-scoped are two cookies");

    let saved: Vec<CookieRecord> = jar.records().collect();
    let mut restored = CookieJar::new();
    for _ in 0..2 {
        for r in saved.clone() {
            restored.restore(r, t(1000)).expect("should restore");
        }
    }
    assert_eq!(restored.len(), 2);
}

#[test]
fn a_record_that_expired_while_the_process_was_down_is_dropped_rather_than_resurrected() {
    let mut jar = CookieJar::new();
    let mut r = record("a", "/", t(0), t(0));
    r.expires = t(100);
    assert_eq!(jar.restore(r, t(200)), Ok(()), "not an error, just gone");
    assert!(jar.is_empty());
}

/// §5.5's 400-day cap, re-applied against the restore instant. A saved
/// expiry was capped when it was stored; a hand-written one was not.
#[test]
fn a_far_future_expiry_in_a_record_is_capped_on_the_way_back_in() {
    let mut jar = CookieJar::new();
    let mut r = record("a", "/", t(0), t(0));
    r.expires = SystemTime::UNIX_EPOCH + Duration::from_secs(253_402_300_799);
    jar.restore(r, t(0)).expect("should restore");
    let cap = t(0) + Duration::from_secs(400 * 24 * 60 * 60);
    assert_eq!(only(&jar).expires, cap);
}

/// `last_access` is [`Limits`]' eviction key, so losing it replaces
/// least-recently-used with insertion order.
#[test]
fn eviction_after_a_reload_follows_use_rather_than_insertion_order() {
    let limits = Limits {
        max_per_domain: 2,
        ..Limits::default()
    };
    let mut jar = CookieJar::new().with_limits(limits);
    // `first` is restored first and used most recently; `second` is
    // restored second and used least recently. Insertion order and
    // recency disagree, which is the whole point.
    jar.restore(record("first", "/", t(0), t(90)), t(100))
        .expect("restore");
    jar.restore(record("second", "/", t(0), t(10)), t(100))
        .expect("restore");

    set(
        &mut jar,
        "https://example.com/",
        "third=3; Max-Age=100000",
        t(101),
    );

    let held: Vec<&str> = jar.iter().map(Cookie::name).collect();
    assert!(!held.contains(&"second"), "the least recently used went");
    assert!(held.contains(&"first"));
    assert!(held.contains(&"third"));
}

/// §5.4's *last* tiebreak, once path length and creation time have both
/// tied: the jar's own insertion order.
///
/// A restore that lands on a cookie the jar already holds keeps that
/// cookie's place in the queue, which is the same answer §5.7 gives
/// [`CookieJar::store`] about the creation time — the sequence is a fact
/// about the jar, not about the cookie, so refreshing a value must not
/// move it. It is reachable only when two cookies share a path length
/// *and* a creation time, which one response's `Set-Cookie` headers
/// routinely do, so this is a test rather than a mutation control: with
/// a fresh sequence assigned on replacement, `x` sorts behind `y`.
#[test]
fn a_restore_over_a_held_cookie_keeps_its_place_in_the_queue() {
    let u = "https://example.com/app/z";
    let mut jar = CookieJar::new();
    set(&mut jar, u, "x=1; Path=/app; Max-Age=100000", t(0));
    set(&mut jar, u, "y=2; Path=/app; Max-Age=100000", t(0));
    assert_eq!(
        jar.cookie_header(&uri(u), t(1)).expect("header"),
        "x=1; y=2"
    );

    let mut refreshed = record("x", "/app", t(0), t(0));
    refreshed.value = "9".to_owned();
    jar.restore(refreshed, t(2)).expect("should restore");

    assert_eq!(jar.len(), 2);
    assert_eq!(
        jar.cookie_header(&uri(u), t(3)).expect("header"),
        "x=9; y=2"
    );
}

// ---------------------------------------------------------------------
// The refusals
// ---------------------------------------------------------------------

/// The direction `suffix.rs` names as the harmful one: the compiled-in
/// list can only ever be missing entries that were **added** since, and
/// those entries are overwhelmingly shared-hosting domains added so that
/// one tenant cannot set a cookie for another. A jar saved before the
/// list grew must not bring that cookie back.
#[cfg(feature = "public-suffix")]
#[test]
fn a_domain_that_has_since_become_a_public_suffix_is_refused_on_restore() {
    let mut jar = CookieJar::new();
    let mut r = record("a", "/", t(0), t(0));
    r.domain = "github.io".to_owned();
    r.host_only = false;
    assert_eq!(
        jar.restore(r, t(0)),
        Err(Rejected::DomainIsPublicSuffix {
            domain: "github.io".to_owned()
        })
    );

    // The control, and it is `store`'s own rescue: a **host-only** cookie
    // for that same name is what `http://localhost` depends on.
    let mut r = record("a", "/", t(0), t(0));
    r.domain = "github.io".to_owned();
    assert_eq!(jar.restore(r, t(0)), Ok(()));
    assert_eq!(jar.len(), 1);
}

/// §5.7 makes an IP-literal host host-only unconditionally, so `store`
/// cannot produce this pair and §5.1.3's domain-match is not written to
/// survive it: `evil.1.2.3.4` is an ordinary name, so it domain-matches
/// `1.2.3.4`.
#[test]
fn a_record_scoped_to_an_ip_literal_must_be_host_only() {
    let mut jar = CookieJar::new();
    let mut r = record("a", "/", t(0), t(0));
    r.domain = "1.2.3.4".to_owned();
    r.host_only = false;
    assert_eq!(
        jar.restore(r, t(0)),
        Err(Rejected::IpDomainNotHostOnly {
            domain: "1.2.3.4".to_owned()
        })
    );

    // The control: host-only is what `store` would have produced, and it
    // reaches the literal and nothing else.
    let mut r = record("a", "/", t(0), t(0));
    r.domain = "1.2.3.4".to_owned();
    jar.restore(r, t(0)).expect("should restore");
    assert!(jar.cookie_header(&uri("https://1.2.3.4/"), t(1)).is_some());
    assert!(
        jar.cookie_header(&uri("https://evil.1.2.3.4/"), t(1))
            .is_none()
    );
}

/// §4.1.3's durable half. There is no request to be secure over, so
/// `secure` is the whole of what `__Secure-` can still be checked against
/// — and it is exactly what `store` would have required.
#[cfg(feature = "public-suffix")]
#[test]
fn the_name_prefixes_are_rechecked_against_the_records_own_facts() {
    let mut jar = CookieJar::new();

    let mut r = record("__Secure-a", "/", t(0), t(0));
    assert_eq!(jar.restore(r.clone(), t(0)), Err(Rejected::SecurePrefix));
    r.secure = true;
    assert_eq!(jar.restore(r, t(0)), Ok(()));

    for (path, host_only, secure) in [("/app", true, true), ("/", false, true), ("/", true, false)]
    {
        let mut r = record("__Host-b", path, t(0), t(0));
        r.host_only = host_only;
        r.secure = secure;
        assert_eq!(
            jar.restore(r, t(0)),
            Err(Rejected::HostPrefix),
            "path={path} host_only={host_only} secure={secure}"
        );
    }
    let mut r = record("__Host-b", "/", t(0), t(0));
    r.secure = true;
    assert_eq!(jar.restore(r, t(0)), Ok(()));
}

/// §5.2.3's leading dot and case are how a `Domain` is *written*, not a
/// different scope, so they are normalised rather than refused — the same
/// answer `parse.rs` gives the attribute.
#[cfg(feature = "public-suffix")]
#[test]
fn a_records_domain_is_normalised_the_way_the_attribute_is() {
    let mut jar = CookieJar::new();
    let mut r = record("a", "/", t(0), t(0));
    r.domain = ".EXAMPLE.com".to_owned();
    r.host_only = false;
    jar.restore(r, t(0)).expect("should restore");
    assert_eq!(only(&jar).domain, "example.com");
    assert!(
        jar.cookie_header(&uri("https://www.example.com/"), t(1))
            .is_some()
    );
}

#[test]
fn the_refusals_a_set_cookie_could_never_reach() {
    let mut jar = CookieJar::new();

    let mut r = record("a", "/", t(0), t(0));
    r.domain = ".".to_owned();
    assert_eq!(jar.restore(r, t(0)), Err(Rejected::EmptyDomain));

    let mut r = record("a", "app", t(0), t(0));
    assert_eq!(
        jar.restore(r.clone(), t(0)),
        Err(Rejected::RelativePath {
            path: "app".to_owned()
        })
    );
    r.path = String::new();
    assert_eq!(
        jar.restore(r, t(0)),
        Err(Rejected::RelativePath {
            path: String::new()
        })
    );
}

/// §5.2's own refusals, asked of a record because a file is at least as
/// untrustworthy as a server.
#[test]
fn a_hand_edited_record_meets_the_same_bounds_a_server_would() {
    let mut jar = CookieJar::new();

    let mut r = record("a", "/", t(0), t(0));
    r.name = String::new();
    assert!(matches!(jar.restore(r, t(0)), Err(Rejected::Malformed(_))));

    let mut r = record("a", "/", t(0), t(0));
    r.value = "one\ntwo".to_owned();
    assert!(matches!(jar.restore(r, t(0)), Err(Rejected::Malformed(_))));

    let mut r = record("a", "/", t(0), t(0));
    r.value = "x".repeat(5000);
    assert!(matches!(
        jar.restore(r, t(0)),
        Err(Rejected::TooLarge { .. })
    ));
}

// ---------------------------------------------------------------------
// End to end
// ---------------------------------------------------------------------

/// The whole point, in the shape a caller writes it: save every record,
/// throw the jar away, load them back, and ask the reloaded jar for the
/// header the old one would have produced.
#[cfg(feature = "public-suffix")]
#[test]
fn a_jar_saved_and_loaded_sends_the_same_cookie_header() {
    let u = "https://www.example.com/app/deep/x";
    let mut jar = CookieJar::new();
    set(&mut jar, u, "root=0; Path=/; Max-Age=100000", t(0));
    set(&mut jar, u, "deep=1; Path=/app/deep; Max-Age=100000", t(1));
    set(&mut jar, u, "mid=2; Path=/app; Max-Age=100000", t(2));
    set(
        &mut jar,
        u,
        "wide=3; Domain=example.com; Max-Age=100000",
        t(3),
    );
    set(&mut jar, u, "session=4", t(4));
    let before = jar.cookie_header(&uri(u), t(10)).expect("a header");

    let saved: Vec<CookieRecord> = jar.records().collect();
    drop(jar);

    let mut restored = CookieJar::new();
    for r in saved {
        restored.restore(r, t(1_000)).expect("should restore");
    }

    let after = restored.cookie_header(&uri(u), t(1_001)).expect("a header");
    assert_eq!(
        after.to_str().expect("ascii"),
        before
            .to_str()
            .expect("ascii")
            .replace("; session=4", "")
            .replace("session=4; ", ""),
        "same order, minus the session cookie a restart ends"
    );
    assert!(after.to_str().expect("ascii").starts_with("deep=1; "));
}

/// `cookie_without_the_list.rs`'s claim, asked of the load side: **a
/// build with no list is narrower than one with a list, never wider.**
///
/// `NoList::is_public_suffix` answers `true` for everything, so every
/// domain-scoped record is refused there — including ones this build
/// would have been right to keep. That is the safe direction and the
/// only one available: a jar that accepted `Domain=example.com` because
/// it could not check it is how one registrant's cookie reaches another.
/// Host-only cookies are unaffected, which is what leaves such a build
/// usable at all — the same rescue `store` relies on for
/// `http://localhost`.
#[cfg(not(feature = "public-suffix"))]
#[test]
fn a_build_with_no_list_refuses_every_domain_scoped_record() {
    let mut jar = CookieJar::new();
    let mut r = record("a", "/", t(0), t(0));
    r.host_only = false;
    assert_eq!(
        jar.restore(r, t(0)),
        Err(Rejected::NoPublicSuffixList {
            domain: "example.com".to_owned()
        })
    );

    assert_eq!(jar.restore(record("a", "/", t(0), t(0)), t(0)), Ok(()));
    assert!(
        jar.cookie_header(&uri("https://example.com/"), t(1))
            .is_some()
    );
    assert!(
        jar.cookie_header(&uri("https://www.example.com/"), t(1))
            .is_none(),
        "host-only, so no subdomain sees it"
    );
}
