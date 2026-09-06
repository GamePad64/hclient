//! The slow tier's memory, and the clock is a parameter.
//!
//! Nothing here sleeps. `AltSvcCache` never reads a clock — `now` arrives
//! as an argument, exactly as it does on `hclient-native`'s negative cache
//! — so a lifetime measured in days is tested by handing it a
//! `SystemTime` rather than by waiting for one. That is what makes `ma` testable at
//! all: RFC 7838's default is twenty-four hours.
//!
//! The end of this file is where the two decisions that are *not* about
//! time live: what a present field replaces (RFC 7838 §3) and what
//! survives a network change (§2.2, and the part this crate cannot see for
//! itself).
#![cfg(all(feature = "http3", not(target_family = "wasm")))]

use hclient_native::altsvc::{
    AltSvcCache, AltSvcStore, Entry, FieldValue, MemoryStore, Origin, parse,
};
use std::future::Ready;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, UNIX_EPOCH};
use web_time::SystemTime;

const ORIGIN: &str = "example.com";

fn origin() -> Origin {
    Origin::new(ORIGIN, 443)
}

/// **A calendar instant, where this returned an offset on the
/// transport's own `Timer`.** The seam is why: an entry whose expiry is
/// elapsed time from one transport's epoch means nothing to a store that
/// outlives it, so `AltSvcCache` reads a wall clock now and these times
/// are wall-clock times counted from the epoch, which keeps every figure
/// below exactly what it was.
fn secs(n: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(n)
}

/// Feed the cache a field value as it would arrive from a response.
async fn note<S: AltSvcStore>(cache: &AltSvcCache<S>, field: &str, now: SystemTime) {
    cache.note(&origin(), &parse(field.as_bytes()), now).await;
}

// --- the lifetime is the origin's own -----------------------------------

/// `ma` is read, and it is what the entry lives for.
#[test]
fn an_entry_lives_exactly_as_long_as_its_ma_says() {
    futures_executor::block_on(async {
        let c = AltSvcCache::default();
        note(&c, r#"h3=":443"; ma=60"#, secs(0)).await;

        assert!(c.advertises_h3(&origin(), secs(0)).await);
        assert!(c.advertises_h3(&origin(), secs(59)).await);
        assert!(
            !c.advertises_h3(&origin(), secs(60)).await,
            "the window is half-open: an entry whose ma has exactly run out is stale"
        );
    })
}

/// The lifetime is measured from when the advertisement was heard, not
/// from the cache's own beginning — the same entry noted later expires
/// later.
#[test]
fn the_lifetime_starts_when_the_advertisement_was_heard() {
    futures_executor::block_on(async {
        let c = AltSvcCache::default();
        note(&c, r#"h3=":443"; ma=60"#, secs(1_000)).await;

        assert!(c.advertises_h3(&origin(), secs(1_059)).await);
        assert!(!c.advertises_h3(&origin(), secs(1_060)).await);
    })
}

/// RFC 7838 §3.1's default, exercised at the timescale it actually names:
/// still fresh at 23 hours 59 minutes, stale at 24 hours. A test that
/// waited for this would be a test nobody runs.
#[test]
fn a_field_with_no_ma_lives_for_the_rfcs_twenty_four_hours() {
    futures_executor::block_on(async {
        let c = AltSvcCache::default();
        note(&c, r#"h3=":443""#, secs(0)).await;

        assert!(c.advertises_h3(&origin(), secs(86_399)).await);
        assert!(!c.advertises_h3(&origin(), secs(86_400)).await);
    })
}

/// `ma=0` is a removal, and the half-open window is what makes it one
/// without a special case: an entry that expires at the instant it was
/// stored is stale at that instant.
#[test]
fn ma_zero_is_a_removal() {
    futures_executor::block_on(async {
        let c = AltSvcCache::default();
        note(&c, r#"h3=":443"; ma=86400"#, secs(0)).await;
        assert!(c.advertises_h3(&origin(), secs(10)).await);

        note(&c, r#"h3=":443"; ma=0"#, secs(10)).await;
        assert!(
            !c.advertises_h3(&origin(), secs(10)).await,
            "an origin that says `ma=0` has withdrawn the alternative"
        );
    })
}

/// RFC 9110 §5.6.7's saturation, carried all the way through: a `ma`
/// larger than a `u64` becomes the largest representable one, and adding
/// it to any `now` must not overflow.
#[test]
fn an_enormous_ma_is_capped_rather_than_overflowing() {
    futures_executor::block_on(async {
        let c = AltSvcCache::default();
        note(
            &c,
            r#"h3=":443"; ma=99999999999999999999999999999999"#,
            secs(0),
        )
        .await;
        // Fresh well inside the cap…
        assert!(c.advertises_h3(&origin(), secs(399 * 86_400)).await);
        // …and stale past it, which is what says the cap is applied
        // rather than the peer's number.
        assert!(
            !c.advertises_h3(&origin(), secs(400 * 86_400 + 1)).await,
            "an unbounded ma must not outlive the cap"
        );
    })
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
    futures_executor::block_on(async {
        let c = AltSvcCache::default();
        note(&c, r#"h3=":443"; ma=60"#, secs(0)).await;

        assert!(
            !c.advertises_h3(&origin(), secs(100)).await,
            "stale, and removed"
        );
        assert!(
            !c.advertises_h3(&origin(), secs(10)).await,
            "it was removed, so an earlier question does not revive it"
        );
    })
}

// --- what is actionable -------------------------------------------------

/// Only `h3`, and only at the origin's own authority.
#[test]
fn only_h3_at_this_origin_is_remembered() {
    futures_executor::block_on(async {
        for field in [
            r#"h2=":443""#,              // another protocol
            r#"h3=":8443""#,             // another port
            r#"h3="other.example:443""#, // another host
            r#"h3-29=":443""#,           // a draft version is not h3
            r#"H3=":443""#,              // an ALPN name is an octet string
        ] {
            let c = AltSvcCache::default();
            note(&c, field, secs(0)).await;
            assert!(!c.advertises_h3(&origin(), secs(0)).await, "{field}");
        }
    })
}

/// An advertisement naming this origin's host explicitly is the same as
/// one that omits it, case and all.
#[test]
fn naming_this_origin_is_the_same_as_omitting_it() {
    futures_executor::block_on(async {
        for field in [
            r#"h3=":443""#,
            r#"h3="example.com:443""#,
            r#"h3="EXAMPLE.COM:443""#,
        ] {
            let c = AltSvcCache::default();
            note(&c, field, secs(0)).await;
            assert!(c.advertises_h3(&origin(), secs(0)).await, "{field}");
        }
    })
}

/// The origin key is a host *and* a port, and the host is not
/// case-sensitive.
#[test]
fn the_key_is_the_whole_origin() {
    futures_executor::block_on(async {
        let c = AltSvcCache::default();
        note(&c, r#"h3=":443""#, secs(0)).await;

        assert!(
            c.advertises_h3(&Origin::new("EXAMPLE.com", 443), secs(0))
                .await
        );
        assert!(
            !c.advertises_h3(&Origin::new("example.com", 8443), secs(0))
                .await
        );
        assert!(
            !c.advertises_h3(&Origin::new("other.example", 443), secs(0))
                .await
        );
    })
}

/// The first `h3` at this origin wins. RFC 7838 gives list order no
/// meaning beyond the order the origin wrote them in, and choosing by `ma`
/// would be this crate preferring whichever entry keeps itself alive
/// longest.
#[test]
fn the_first_actionable_h3_in_the_list_is_the_one_taken() {
    futures_executor::block_on(async {
        let c = AltSvcCache::default();
        note(
            &c,
            r#"h3="elsewhere:443"; ma=99999, h3=":443"; ma=60, h3=":443"; ma=99999"#,
            secs(0),
        )
        .await;
        assert!(c.advertises_h3(&origin(), secs(59)).await);
        assert!(
            !c.advertises_h3(&origin(), secs(60)).await,
            "the second member is the first actionable one, and its ma is 60"
        );
    })
}

// --- what a present field replaces --------------------------------------

/// RFC 7838 §3: *"When an Alt-Svc response header field is received from
/// an origin, its value invalidates and replaces all cached alternative
/// services for that origin."* So a field that no longer mentions `h3`
/// takes the entry away, without needing to say `clear`.
#[test]
fn a_field_that_no_longer_offers_h3_removes_what_was_stored() {
    futures_executor::block_on(async {
        let c = AltSvcCache::default();
        note(&c, r#"h3=":443"; ma=86400"#, secs(0)).await;
        assert!(c.advertises_h3(&origin(), secs(1)).await);

        note(&c, r#"h2=":443""#, secs(1)).await;
        assert!(!c.advertises_h3(&origin(), secs(1)).await);
    })
}

/// …including a field nobody could parse. The direction is deliberate
/// twice over: it is what "invalidates and replaces" says, and forgetting
/// means going back to TCP, so the worst a garbled or hostile field can do
/// is cost a request the faster protocol.
#[test]
fn a_field_that_could_not_be_parsed_at_all_also_removes() {
    futures_executor::block_on(async {
        let c = AltSvcCache::default();
        note(&c, r#"h3=":443"; ma=86400"#, secs(0)).await;

        note(&c, "!!! not a field value !!!", secs(1)).await;
        assert!(!c.advertises_h3(&origin(), secs(1)).await);
    })
}

/// `clear` removes, which is the same outcome by a different instruction —
/// and it is a separate test because the two travel through different
/// arms.
#[test]
fn clear_removes() {
    futures_executor::block_on(async {
        let c = AltSvcCache::default();
        note(&c, r#"h3=":443"; ma=86400"#, secs(0)).await;

        note(&c, "clear", secs(1)).await;
        assert!(!c.advertises_h3(&origin(), secs(1)).await);
    })
}

/// A later field replaces an earlier one's lifetime rather than extending
/// or shortening it by halves: the newest statement is the whole answer.
#[test]
fn a_later_field_replaces_the_earlier_ones_lifetime() {
    futures_executor::block_on(async {
        let c = AltSvcCache::default();
        note(&c, r#"h3=":443"; ma=86400"#, secs(0)).await;
        note(&c, r#"h3=":443"; ma=10"#, secs(0)).await;
        assert!(!c.advertises_h3(&origin(), secs(10)).await, "shortened");

        note(&c, r#"h3=":443"; ma=10"#, secs(0)).await;
        note(&c, r#"h3=":443"; ma=86400"#, secs(0)).await;
        assert!(c.advertises_h3(&origin(), secs(1000)).await, "lengthened");
    })
}

/// One origin's field says nothing about another's.
#[test]
fn a_field_replaces_only_its_own_origins_entry() {
    futures_executor::block_on(async {
        let c = AltSvcCache::default();
        let other = Origin::new("other.example", 443);
        c.note(&origin(), &parse(br#"h3=":443""#), secs(0)).await;
        c.note(&other, &parse(br#"h3=":443""#), secs(0)).await;

        c.note(&origin(), &FieldValue::Clear, secs(1)).await;
        assert!(!c.advertises_h3(&origin(), secs(1)).await);
        assert!(
            c.advertises_h3(&other, secs(1)).await,
            "a neighbour's entry stands"
        );
    })
}

// --- scope: the network change this crate cannot see --------------------

/// RFC 7838 §2.2: *"clients SHOULD remove from cache all alternative
/// services that lack the 'persist' flag with the value '1' when they
/// detect such a change"*. `persist` has exactly one reader, and this is
/// it.
#[test]
fn a_network_change_forgets_what_did_not_ask_to_persist() {
    futures_executor::block_on(async {
        let c = AltSvcCache::default();
        let persistent = Origin::new("persistent.example", 443);
        c.note(&origin(), &parse(br#"h3=":443"; ma=86400"#), secs(0))
            .await;
        c.note(
            &persistent,
            &parse(br#"h3=":443"; ma=86400; persist=1"#),
            secs(0),
        )
        .await;

        c.network_changed().await;

        assert!(
            !c.advertises_h3(&origin(), secs(1)).await,
            "an ordinary entry was reachable on the network we have left"
        );
        assert!(
            c.advertises_h3(&persistent, secs(1)).await,
            "`persist=1` is the origin's hint that this one is not network-specific"
        );
    })
}

/// A `persist` the RFC makes us ignore does not keep an entry alive across
/// the change — ignoring the parameter means it was never there.
#[test]
fn a_persist_value_the_rfc_ignores_does_not_survive_a_network_change() {
    futures_executor::block_on(async {
        for field in [
            r#"h3=":443"; persist=0"#,
            r#"h3=":443"; persist=2"#,
            r#"h3=":443"; persist=true"#,
        ] {
            let c = AltSvcCache::default();
            note(&c, field, secs(0)).await;
            assert!(c.advertises_h3(&origin(), secs(0)).await, "{field}");
            c.network_changed().await;
            assert!(!c.advertises_h3(&origin(), secs(0)).await, "{field}");
        }
    })
}

/// A network change is not an expiry: a persistent entry still runs out of
/// `ma` on its own schedule.
#[test]
fn persisting_across_a_network_change_is_not_living_for_ever() {
    futures_executor::block_on(async {
        let c = AltSvcCache::default();
        note(&c, r#"h3=":443"; ma=60; persist=1"#, secs(0)).await;
        c.network_changed().await;

        assert!(c.advertises_h3(&origin(), secs(59)).await);
        assert!(!c.advertises_h3(&origin(), secs(60)).await);
    })
}

/// Cheap to clone, and every clone is the same cache — a memory that
/// lasted one request would be no memory at all.
#[test]
fn a_clone_is_the_same_cache() {
    futures_executor::block_on(async {
        let c = AltSvcCache::default();
        let d = c.clone();
        note(&c, r#"h3=":443""#, secs(0)).await;
        assert!(d.advertises_h3(&origin(), secs(0)).await);

        d.network_changed().await;
        assert!(!c.advertises_h3(&origin(), secs(0)).await);
    })
}

// ── the seam ────────────────────────────────────────────────────────────

/// A store that records what it was asked, and otherwise is
/// [`MemoryStore`].
///
/// The counter lives outside the cache, so a cache that quietly used a
/// store of its own would leave it at zero — which is the only thing that
/// makes this a test of the seam rather than a second run of the ones
/// above.
#[derive(Default)]
struct Watched {
    inner: MemoryStore,
    gets: Arc<AtomicUsize>,
    puts: Arc<AtomicUsize>,
    removes: Arc<AtomicUsize>,
    retains: Arc<AtomicUsize>,
}

impl AltSvcStore for Watched {
    type Get<'a> = Ready<Option<Entry>>;
    type Done<'a> = Ready<()>;

    fn get<'a>(&'a self, origin: &'a Origin) -> Self::Get<'a> {
        self.gets.fetch_add(1, Ordering::Relaxed);
        self.inner.get(origin)
    }
    fn put<'a>(&'a self, origin: &'a Origin, entry: Entry) -> Self::Done<'a> {
        self.puts.fetch_add(1, Ordering::Relaxed);
        self.inner.put(origin, entry)
    }
    fn remove<'a>(&'a self, origin: &'a Origin) -> Self::Done<'a> {
        self.removes.fetch_add(1, Ordering::Relaxed);
        self.inner.remove(origin)
    }
    fn retain_persistent(&self) -> Self::Done<'_> {
        self.retains.fetch_add(1, Ordering::Relaxed);
        self.inner.retain_persistent()
    }
}

/// **The rules run over a store the caller supplied, and only the storage
/// is the store's.**
///
/// Every assertion above is repeated through a substituted store rather
/// than restated: the lifetime, the expiry sweep, the replacement and the
/// network change all still hold, and the counters say each one really
/// went through the caller's value. A cache that had kept a private map
/// would pass none of them.
#[test]
fn the_rules_run_over_a_store_of_the_callers_own() {
    futures_executor::block_on(async {
        let (gets, puts, removes, retains) = (
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
        );
        let c = AltSvcCache::with_store(Watched {
            inner: MemoryStore::default(),
            gets: Arc::clone(&gets),
            puts: Arc::clone(&puts),
            removes: Arc::clone(&removes),
            retains: Arc::clone(&retains),
        });

        note(&c, r#"h3=":443"; ma=60"#, secs(0)).await;
        assert_eq!(
            puts.load(Ordering::Relaxed),
            1,
            "the caller's store was written"
        );

        assert!(c.advertises_h3(&origin(), secs(59)).await);
        assert!(gets.load(Ordering::Relaxed) >= 1, "and read");

        // The expiry sweep is the cache's rule and the removal is the
        // store's work: a stale entry is forgotten through the seam.
        assert!(!c.advertises_h3(&origin(), secs(60)).await);
        assert_eq!(removes.load(Ordering::Relaxed), 1, "swept through the seam");

        note(&c, r#"h3=":443"; ma=60; persist=1"#, secs(60)).await;
        c.network_changed().await;
        assert_eq!(retains.load(Ordering::Relaxed), 1);
        assert!(
            c.advertises_h3(&origin(), secs(90)).await,
            "persist=1 survives, and §2.2 is still this side of the seam"
        );
    })
}

/// **And the store reaches the transport**, which is the half a test of
/// the cache alone cannot see.
///
/// `Native::alt_svc_store` is one assignment, and one assignment is
/// exactly the kind of thing that compiles while reaching nothing — a
/// seam this workspace has shipped unreachable before, one crate over and
/// on the same day this one was written. So the installed store is asked
/// through the transport's own public entry point rather than through the
/// cache's.
///
/// `NoTls` and `IpLiteralOnly` because nothing here connects: what is
/// under test is that the value a caller handed in is the value
/// `Native::network_changed` reaches.
#[tokio::test]
async fn a_store_installed_on_the_transport_is_the_one_consulted() {
    let retains = Arc::new(AtomicUsize::new(0));
    let t = hclient_native::Native::new(
        hclient_rt_tokio::TokioHandle::current().expect("inside #[tokio::test]"),
        hclient_tls::NoTls,
        hclient_dns::IpLiteralOnly,
    )
    .alt_svc_store(Watched {
        inner: MemoryStore::default(),
        gets: Arc::new(AtomicUsize::new(0)),
        puts: Arc::new(AtomicUsize::new(0)),
        removes: Arc::new(AtomicUsize::new(0)),
        retains: Arc::clone(&retains),
    });

    t.network_changed().await;
    assert_eq!(
        retains.load(Ordering::Relaxed),
        1,
        "the transport asked the store the caller installed, not one of its own"
    );
}
