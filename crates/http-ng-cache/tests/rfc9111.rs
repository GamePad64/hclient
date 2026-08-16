//! RFC 9111 against this cache: what it stores, what it serves, and what
//! it insists on asking about first.
//!
//! Every test here drives the crate directly, with an explicit `now` —
//! there is no clock and no socket to be uncertain about, which is the
//! whole point of the sans-io shape. The wiring into `http_ng::Client`
//! (when the cache is consulted, whose URI it is asked about, and the
//! `owns_cache` refusal) is tested from outside a real client, over a real
//! loopback server, in `crates/http-ng/tests/cache.rs`. Nothing is tested
//! in both places.

use std::time::{Duration, SystemTime};

use assert_matches::assert_matches;
use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, StatusCode, Uri};
use http_ng_cache::{CacheStore, HttpCache, Limits, Lookup, NotStored, StoredResponse};

/// A wall-clock instant. Far enough from the epoch that `Date` headers in
/// the corpus below are ordinary-looking dates rather than 1970.
fn t(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + secs)
}

/// `Sun, 14 Nov 2023 22:13:20 GMT` — `t(0)` written the way a server would
/// write it, checked against the parser rather than trusted.
const T0_AS_DATE: &str = "Tue, 14 Nov 2023 22:13:20 GMT";

fn uri() -> Uri {
    "https://example.test/thing".parse().unwrap()
}

fn req(fields: &[(&str, &str)]) -> HeaderMap {
    let mut m = HeaderMap::new();
    for (k, v) in fields {
        m.append(
            http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
            HeaderValue::from_str(v).unwrap(),
        );
    }
    m
}

fn parts(status: u16, fields: &[(&str, &str)]) -> http::response::Parts {
    let mut b = http::Response::builder().status(status);
    for (k, v) in fields {
        b = b.header(*k, *v);
    }
    b.body(()).unwrap().into_parts().0
}

/// Store one `GET` response and hand back the cache holding it.
fn cache_with(resp_fields: &[(&str, &str)], body: &'static [u8]) -> HttpCache {
    let mut c = HttpCache::new();
    put(&mut c, &req(&[]), &parts(200, resp_fields), body, t(0));
    c
}

fn put(
    c: &mut HttpCache,
    request: &HeaderMap,
    p: &http::response::Parts,
    body: &'static [u8],
    at: SystemTime,
) {
    let s = c
        .storing(&Method::GET, &uri(), request, p, at, at)
        .expect("storable");
    c.store(s, Bytes::from_static(body)).expect("stored");
}

fn look(c: &mut HttpCache, request: &HeaderMap, at: SystemTime) -> Lookup {
    c.lookup(&Method::GET, &uri(), request, at)
}

fn age_of(e: &StoredResponse) -> u64 {
    e.headers()
        .get(http::header::AGE)
        .expect("RFC 9111 §5.1 requires an Age on a reused response")
        .to_str()
        .unwrap()
        .parse()
        .unwrap()
}

// ── freshness ───────────────────────────────────────────────────────────

#[test]
fn a_fresh_entry_is_served_without_a_request() {
    let mut c = cache_with(&[("cache-control", "max-age=60")], b"hello");
    let hit = assert_matches!(look(&mut c, &req(&[]), t(30)), Lookup::Hit(e) => e);
    assert_eq!(hit.body().as_ref(), b"hello");
    assert_eq!(hit.status(), StatusCode::OK);
}

#[test]
fn the_served_response_carries_the_age_it_has_actually_reached() {
    let mut c = cache_with(&[("cache-control", "max-age=60")], b"x");
    let hit = assert_matches!(look(&mut c, &req(&[]), t(30)), Lookup::Hit(e) => e);
    assert_eq!(age_of(&hit), 30);
}

/// §4.2.3's `corrected_age_value`: an `Age` the origin's own upstream cache
/// put on the response counts against the lifetime, so an entry that
/// arrived already 55 seconds old under `max-age=60` has five seconds
/// left, not sixty.
#[test]
fn an_age_the_response_arrived_with_counts_against_its_lifetime() {
    let mut c = cache_with(&[("cache-control", "max-age=60"), ("age", "55")], b"x");
    assert_matches!(look(&mut c, &req(&[]), t(4)), Lookup::Hit(_));
    assert_matches!(look(&mut c, &req(&[]), t(6)), Lookup::Miss);
}

#[test]
fn expires_minus_date_is_the_lifetime_when_there_is_no_max_age() {
    // Date t(0), Expires t(0)+60.
    let mut c = cache_with(
        &[
            ("date", T0_AS_DATE),
            ("expires", "Tue, 14 Nov 2023 22:14:20 GMT"),
        ],
        b"x",
    );
    assert_matches!(look(&mut c, &req(&[]), t(59)), Lookup::Hit(_));
    assert_matches!(look(&mut c, &req(&[]), t(61)), Lookup::Miss);
}

/// §5.3: `Expires: 0` is what every server sends to mean *already
/// expired*, and it is not an HTTP-date. Reading it as "no lifetime given"
/// would be indistinguishable here — both are stale — so the entry is
/// given a validator, which separates them: an unparsable `Expires` still
/// leaves a stored entry to revalidate.
#[test]
fn an_unparsable_expires_is_already_stale_rather_than_ignored() {
    let mut c = cache_with(&[("expires", "0"), ("etag", "\"v1\"")], b"x");
    assert_matches!(look(&mut c, &req(&[]), t(0)), Lookup::Revalidate { .. });
}

#[test]
fn max_age_beats_expires() {
    // `Expires` in the past, `max-age` generous: §4.2.1 takes max-age.
    let mut c = cache_with(
        &[
            ("date", T0_AS_DATE),
            ("expires", "Tue, 14 Nov 2023 20:00:00 GMT"),
            ("cache-control", "max-age=600"),
        ],
        b"x",
    );
    assert_matches!(look(&mut c, &req(&[]), t(300)), Lookup::Hit(_));
}

// ── storability ─────────────────────────────────────────────────────────

/// The rule that stands in for heuristic freshness: a validator alone is
/// enough to store on, and what that buys is a conditional request rather
/// than a hit.
#[test]
fn a_validator_alone_is_stored_and_is_stale_from_the_start() {
    let mut c = cache_with(&[("etag", "\"v1\"")], b"x");
    assert_matches!(look(&mut c, &req(&[]), t(0)), Lookup::Revalidate { .. });
}

/// The shape a `text/event-stream` takes, and the reason no heuristic
/// lifetime is assigned: with one, `http-ng` would record this body into
/// memory for as long as the caller kept reading it.
#[test]
fn a_response_with_neither_a_lifetime_nor_a_validator_is_not_stored() {
    let c = HttpCache::new();
    let e = c
        .storing(
            &Method::GET,
            &uri(),
            &req(&[]),
            &parts(200, &[("content-type", "text/event-stream")]),
            t(0),
            t(0),
        )
        .unwrap_err();
    assert_eq!(e, NotStored::NothingToReuseItWith);
}

#[test]
fn no_store_is_refused_from_either_side() {
    let c = HttpCache::new();
    assert_eq!(
        c.storing(
            &Method::GET,
            &uri(),
            &req(&[]),
            &parts(200, &[("cache-control", "max-age=60, no-store")]),
            t(0),
            t(0)
        )
        .unwrap_err(),
        NotStored::ResponseNoStore
    );
    assert_eq!(
        c.storing(
            &Method::GET,
            &uri(),
            &req(&[("cache-control", "no-store")]),
            &parts(200, &[("cache-control", "max-age=60")]),
            t(0),
            t(0)
        )
        .unwrap_err(),
        NotStored::RequestNoStore
    );
    // And a request that said `no-store` does not read the cache either.
    let mut held = cache_with(&[("cache-control", "max-age=60")], b"x");
    assert_matches!(
        look(&mut held, &req(&[("cache-control", "no-store")]), t(0)),
        Lookup::Miss
    );
}

/// The first of the three decisions the *private* premise settles. A
/// shared cache would refuse this response outright (§5.2.2.7).
#[test]
fn private_is_stored_because_this_is_a_user_agent_cache() {
    let mut c = cache_with(&[("cache-control", "private, max-age=60")], b"secret");
    assert_matches!(look(&mut c, &req(&[]), t(10)), Lookup::Hit(_));
}

/// The second. `s-maxage` addresses a shared cache and is not read at all,
/// so `max-age=0` is the whole instruction and this entry is stale at
/// once — where a shared cache would have served it for ten minutes.
#[test]
fn s_maxage_does_not_extend_a_zero_max_age() {
    let mut c = cache_with(
        &[
            ("cache-control", "max-age=0, s-maxage=600"),
            ("etag", "\"v1\""),
        ],
        b"x",
    );
    assert_matches!(look(&mut c, &req(&[]), t(1)), Lookup::Revalidate { .. });
}

/// The third, and the narrowing that replaces §3.5's shared-cache
/// restriction: the response IS stored, and the credential is part of the
/// key whether or not the origin sent `Vary: Authorization`.
#[test]
fn a_response_to_an_authenticated_request_is_stored_and_keyed_on_the_credential() {
    let alice = req(&[("authorization", "Bearer alice")]);
    let bob = req(&[("authorization", "Bearer bob")]);
    let mut c = HttpCache::new();
    put(
        &mut c,
        &alice,
        &parts(200, &[("cache-control", "max-age=60")]),
        b"alice",
        t(0),
    );

    let hit = assert_matches!(look(&mut c, &alice, t(10)), Lookup::Hit(e) => e);
    assert_eq!(hit.body().as_ref(), b"alice");
    assert_matches!(
        look(&mut c, &bob, t(10)),
        Lookup::Miss,
        "one principal's response must not answer another's request"
    );
    assert_matches!(look(&mut c, &req(&[]), t(10)), Lookup::Miss);
}

#[test]
fn a_status_this_cache_cannot_reuse_is_not_stored() {
    let c = HttpCache::new();
    for status in [206u16, 500, 302] {
        let r = c.storing(
            &Method::GET,
            &uri(),
            &req(&[]),
            &parts(status, &[("cache-control", "max-age=60")]),
            t(0),
            t(0),
        );
        if status == 302 {
            assert!(r.is_ok(), "a 302 with explicit freshness is cacheable");
        } else {
            assert_eq!(
                r.unwrap_err(),
                NotStored::Status(StatusCode::from_u16(status).unwrap())
            );
        }
    }
}

#[test]
fn a_body_over_the_limit_is_not_stored_by_either_check() {
    let mut c = HttpCache::new().with_limits(Limits { max_body_bytes: 4 });
    // Declared too large: refused before a byte is recorded.
    assert_eq!(
        c.storing(
            &Method::GET,
            &uri(),
            &req(&[]),
            &parts(
                200,
                &[("cache-control", "max-age=60"), ("content-length", "9")]
            ),
            t(0),
            t(0)
        )
        .unwrap_err(),
        NotStored::TooLarge { bytes: 9, limit: 4 }
    );
    // Not declared at all — the ordinary HTTP/2 case — and refused on the
    // bytes that actually arrived.
    let s = c
        .storing(
            &Method::GET,
            &uri(),
            &req(&[]),
            &parts(200, &[("cache-control", "max-age=60")]),
            t(0),
            t(0),
        )
        .expect("storable until the body says otherwise");
    assert_eq!(
        c.store(s, Bytes::from_static(b"far too long")).unwrap_err(),
        NotStored::TooLarge {
            bytes: 12,
            limit: 4
        }
    );
    assert_eq!(c.store_ref().len(), 0);
}

#[test]
fn a_body_that_disagrees_with_content_length_is_not_stored() {
    let mut c = HttpCache::new();
    let s = c
        .storing(
            &Method::GET,
            &uri(),
            &req(&[]),
            &parts(
                200,
                &[("cache-control", "max-age=60"), ("content-length", "5")],
            ),
            t(0),
            t(0),
        )
        .unwrap();
    assert_eq!(
        c.store(s, Bytes::from_static(b"hi")).unwrap_err(),
        NotStored::LengthMismatch {
            bytes: 2,
            declared: 5
        }
    );
}

// ── Vary ────────────────────────────────────────────────────────────────

#[test]
fn vary_keys_on_the_named_field_and_nothing_else() {
    let gzip = req(&[("accept-encoding", "gzip"), ("accept", "text/html")]);
    let br = req(&[("accept-encoding", "br"), ("accept", "text/html")]);
    let gzip_other_accept = req(&[("accept-encoding", "gzip"), ("accept", "*/*")]);
    let mut c = HttpCache::new();
    put(
        &mut c,
        &gzip,
        &parts(
            200,
            &[("cache-control", "max-age=60"), ("vary", "Accept-Encoding")],
        ),
        b"gz",
        t(0),
    );
    assert_matches!(look(&mut c, &gzip, t(1)), Lookup::Hit(_));
    assert_matches!(look(&mut c, &br, t(1)), Lookup::Miss);
    assert_matches!(
        look(&mut c, &gzip_other_accept, t(1)),
        Lookup::Hit(_),
        "a field Vary did not name must not affect the match"
    );
}

#[test]
fn vary_asterisk_is_not_stored_at_all() {
    let c = HttpCache::new();
    assert_eq!(
        c.storing(
            &Method::GET,
            &uri(),
            &req(&[]),
            &parts(200, &[("cache-control", "max-age=60"), ("vary", "*")]),
            t(0),
            t(0)
        )
        .unwrap_err(),
        NotStored::VaryAsterisk
    );
}

#[test]
fn the_most_recently_received_matching_variant_is_the_one_served() {
    let mut c = HttpCache::new();
    let a = req(&[("accept", "a")]);
    let b = req(&[("accept", "b")]);
    let fields = &[("cache-control", "max-age=600"), ("vary", "Accept")];
    put(&mut c, &a, &parts(200, fields), b"first", t(0));
    put(&mut c, &b, &parts(200, fields), b"second", t(10));
    assert_eq!(c.store_ref().len(), 2);
    let hit = assert_matches!(look(&mut c, &a, t(20)), Lookup::Hit(e) => e);
    assert_eq!(hit.body().as_ref(), b"first");
}

// ── the directives that force a question ────────────────────────────────

#[test]
fn no_cache_on_the_response_forces_revalidation_while_still_fresh() {
    let mut c = cache_with(
        &[
            ("cache-control", "max-age=600, no-cache"),
            ("etag", "\"v1\""),
        ],
        b"x",
    );
    assert_matches!(look(&mut c, &req(&[]), t(1)), Lookup::Revalidate { .. });
}

#[test]
fn no_cache_on_the_request_forces_revalidation_while_still_fresh() {
    let mut c = cache_with(
        &[("cache-control", "max-age=600"), ("etag", "\"v1\"")],
        b"x",
    );
    assert_matches!(look(&mut c, &req(&[]), t(1)), Lookup::Hit(_));
    assert_matches!(
        look(&mut c, &req(&[("cache-control", "no-cache")]), t(1)),
        Lookup::Revalidate { .. }
    );
}

#[test]
fn request_max_age_and_min_fresh_each_narrow_a_fresh_entry() {
    let mut c = cache_with(
        &[("cache-control", "max-age=600"), ("etag", "\"v1\"")],
        b"x",
    );
    assert_matches!(
        look(&mut c, &req(&[("cache-control", "max-age=10")]), t(30)),
        Lookup::Revalidate { .. }
    );
    assert_matches!(
        look(&mut c, &req(&[("cache-control", "min-fresh=1000")]), t(30)),
        Lookup::Revalidate { .. }
    );
    assert_matches!(
        look(&mut c, &req(&[("cache-control", "min-fresh=10")]), t(30)),
        Lookup::Hit(_)
    );
}

/// `max-stale` is the only thing that ever serves a stale entry here, and
/// it is therefore the only reader `must-revalidate` has — which is why
/// both are implemented or neither could be.
#[test]
fn max_stale_serves_a_stale_entry_and_must_revalidate_overrides_it() {
    let mut c = cache_with(&[("cache-control", "max-age=10"), ("etag", "\"v1\"")], b"x");
    let any = req(&[("cache-control", "max-stale")]);
    let bounded = req(&[("cache-control", "max-stale=5")]);
    assert_matches!(look(&mut c, &any, t(3600)), Lookup::Hit(_));
    assert_matches!(look(&mut c, &bounded, t(12)), Lookup::Hit(_));
    assert_matches!(look(&mut c, &bounded, t(30)), Lookup::Revalidate { .. });

    let mut strict = cache_with(
        &[
            ("cache-control", "max-age=10, must-revalidate"),
            ("etag", "\"v1\""),
        ],
        b"x",
    );
    assert_matches!(
        look(&mut strict, &any, t(3600)),
        Lookup::Revalidate { .. },
        "must-revalidate is what stops max-stale from reaching a stale entry"
    );
}

#[test]
fn only_if_cached_is_unsatisfiable_rather_than_a_request() {
    let mut empty = HttpCache::new();
    assert_matches!(
        look(
            &mut empty,
            &req(&[("cache-control", "only-if-cached")]),
            t(0)
        ),
        Lookup::Unsatisfiable
    );
    let mut held = cache_with(&[("cache-control", "max-age=60")], b"x");
    assert_matches!(
        look(
            &mut held,
            &req(&[("cache-control", "only-if-cached")]),
            t(1)
        ),
        Lookup::Hit(_)
    );
    // A stale entry that would have been revalidated is still a request,
    // and the caller forbade one.
    let mut stale = cache_with(&[("cache-control", "max-age=1"), ("etag", "\"v1\"")], b"x");
    assert_matches!(
        look(
            &mut stale,
            &req(&[("cache-control", "only-if-cached")]),
            t(60)
        ),
        Lookup::Revalidate { .. },
        "only-if-cached does not forbid a validation the caller can still refuse to send"
    );
}

// ── validation ──────────────────────────────────────────────────────────

#[test]
fn both_validators_are_offered_when_both_are_stored() {
    let mut c = cache_with(
        &[
            ("cache-control", "max-age=0"),
            ("etag", "\"v1\""),
            ("last-modified", T0_AS_DATE),
        ],
        b"x",
    );
    let conditions = assert_matches!(look(&mut c, &req(&[]), t(1)), Lookup::Revalidate { conditions, .. } => conditions);
    assert_eq!(
        conditions,
        vec![
            (
                http::header::IF_NONE_MATCH,
                HeaderValue::from_static("\"v1\"")
            ),
            (
                http::header::IF_MODIFIED_SINCE,
                HeaderValue::from_str(T0_AS_DATE).unwrap()
            ),
        ]
    );
}

#[test]
fn a_304_freshens_the_entry_and_updates_the_fields_it_carries() {
    let mut c = cache_with(
        &[
            ("cache-control", "max-age=10"),
            ("etag", "\"v1\""),
            ("x-note", "old"),
        ],
        b"body",
    );
    let (key, stale) = assert_matches!(
        look(&mut c, &req(&[]), t(60)),
        Lookup::Revalidate { key, stale, .. } => (key, stale)
    );
    let served = c.revalidated(
        &key,
        stale,
        &parts(304, &[("cache-control", "max-age=10"), ("x-note", "new")]),
        t(60),
        t(60),
    );
    assert_eq!(
        served.body().as_ref(),
        b"body",
        "the body came from the store"
    );
    assert_eq!(served.headers()["x-note"], "new");
    assert_eq!(
        age_of(&served),
        0,
        "a validated response is zero seconds old"
    );
    // And it is fresh again, from the store, with no second request.
    assert_matches!(look(&mut c, &req(&[]), t(65)), Lookup::Hit(_));
}

/// The stored bytes are the wire's, `Content-Encoding` and all — `http-ng`
/// decompresses **above** this cache. A `304` that relabelled them would
/// hand the decompressor a body that is not what the label says, which is
/// why `Content-Encoding` joins §3.2's own `Content-Length` exception.
#[test]
fn a_304_does_not_relabel_the_stored_bytes() {
    let mut c = cache_with(
        &[
            ("cache-control", "max-age=0"),
            ("etag", "\"v1\""),
            ("content-encoding", "gzip"),
            ("content-length", "4"),
        ],
        b"gzip",
    );
    let (key, stale) = assert_matches!(
        look(&mut c, &req(&[]), t(1)),
        Lookup::Revalidate { key, stale, .. } => (key, stale)
    );
    let served = c.revalidated(
        &key,
        stale,
        &parts(
            304,
            &[
                ("content-encoding", "identity"),
                ("content-length", "0"),
                ("connection", "x-hop"),
                ("x-hop", "gone"),
                ("etag", "\"v1\""),
            ],
        ),
        t(1),
        t(1),
    );
    assert_eq!(served.headers()["content-encoding"], "gzip");
    assert_eq!(served.headers()["content-length"], "4");
    assert!(
        !served.headers().contains_key("x-hop"),
        "a field the 304's own Connection nominated is that hop's, not the message's"
    );
}

#[test]
fn a_non_304_answer_to_a_revalidation_removes_the_stale_variant() {
    let mut c = cache_with(&[("cache-control", "max-age=10"), ("etag", "\"v1\"")], b"x");
    let (key, stale) = assert_matches!(
        look(&mut c, &req(&[]), t(60)),
        Lookup::Revalidate { key, stale, .. } => (key, stale)
    );
    c.superseded(&key, &stale);
    assert_eq!(c.store_ref().len(), 0);
    assert_matches!(
        look(&mut c, &req(&[("cache-control", "max-stale")]), t(60)),
        Lookup::Miss
    );
}

// ── invalidation, and the requests that stand aside ─────────────────────

#[test]
fn an_unsafe_method_invalidates_the_stored_get_and_a_failed_one_does_not() {
    let mut c = cache_with(&[("cache-control", "max-age=600")], b"x");
    c.invalidated_by(&Method::POST, &uri(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_matches!(look(&mut c, &req(&[]), t(1)), Lookup::Hit(_));
    c.invalidated_by(&Method::GET, &uri(), StatusCode::OK);
    assert_matches!(
        look(&mut c, &req(&[]), t(1)),
        Lookup::Hit(_),
        "a safe method invalidates nothing"
    );
    c.invalidated_by(&Method::POST, &uri(), StatusCode::CREATED);
    assert_matches!(look(&mut c, &req(&[]), t(1)), Lookup::Miss);
}

#[test]
fn a_method_nobody_here_has_heard_of_invalidates() {
    let mut c = cache_with(&[("cache-control", "max-age=600")], b"x");
    c.invalidated_by(
        &Method::from_bytes(b"PURGE").unwrap(),
        &uri(),
        StatusCode::OK,
    );
    assert_matches!(look(&mut c, &req(&[]), t(1)), Lookup::Miss);
}

#[test]
fn a_head_is_neither_stored_nor_looked_up() {
    let mut c = cache_with(&[("cache-control", "max-age=600")], b"x");
    assert_eq!(
        c.storing(
            &Method::HEAD,
            &uri(),
            &req(&[]),
            &parts(200, &[("cache-control", "max-age=60")]),
            t(0),
            t(0)
        )
        .unwrap_err(),
        NotStored::Method(Method::HEAD)
    );
    assert_matches!(
        c.lookup(&Method::HEAD, &uri(), &req(&[]), t(1)),
        Lookup::Miss,
        "the method is in the primary key, so a stored GET could not answer this anyway"
    );
}

#[test]
fn a_range_or_a_caller_supplied_precondition_makes_the_cache_stand_aside() {
    let mut c = cache_with(&[("cache-control", "max-age=600")], b"x");
    for field in [
        ("range", "bytes=0-3"),
        ("if-none-match", "\"whatever\""),
        ("if-modified-since", T0_AS_DATE),
    ] {
        assert_matches!(
            look(&mut c, &req(&[field]), t(1)),
            Lookup::Miss,
            "a hit would answer a question the caller asked the origin"
        );
        assert_eq!(
            c.storing(
                &Method::GET,
                &uri(),
                &req(&[field]),
                &parts(200, &[("cache-control", "max-age=60")]),
                t(0),
                t(0)
            )
            .unwrap_err(),
            NotStored::RequestStoodAside
        );
    }
}

#[test]
fn an_origin_form_uri_is_never_stored() {
    let c = HttpCache::new();
    assert_eq!(
        c.storing(
            &Method::GET,
            &"/thing".parse().unwrap(),
            &req(&[]),
            &parts(200, &[("cache-control", "max-age=60")]),
            t(0),
            t(0)
        )
        .unwrap_err(),
        NotStored::NoKey
    );
}

/// The corpus's own anchor: `T0_AS_DATE` is used as a `Date` above and the
/// arithmetic in `expires_minus_date_is_the_lifetime_when_there_is_no_max_age`
/// depends on it naming `t(0)`. Asserted rather than trusted, because a
/// wrong constant there would make that test pass for the wrong reason.
#[test]
fn the_corpus_date_constant_names_the_instant_the_tests_call_t0() {
    let mut c = HttpCache::new();
    put(
        &mut c,
        &req(&[]),
        &parts(
            200,
            &[("date", T0_AS_DATE), ("cache-control", "max-age=100")],
        ),
        b"x",
        t(0),
    );
    let hit = assert_matches!(look(&mut c, &req(&[]), t(40)), Lookup::Hit(e) => e);
    assert_eq!(
        age_of(&hit),
        40,
        "apparent_age is received_at - Date, so a Date naming t(0) gives exactly the elapsed time"
    );
}
