//! What the jar refuses, and where it refuses to send.
//!
//! **These are the tests that are worth anything here.** A jar that stores
//! whatever it is handed and returns it to whoever asks passes every
//! happy-path test in the file below and fails almost every one in it; that
//! asymmetry is deliberate. Each test names the rule it stands for, so that
//! deleting the rule produces a failure that says which rule went.
//!
//! The happy-path cases that *are* here exist for one reason: a test that
//! only ever asserts `is_err()` would also pass against a jar that refuses
//! everything, which is the other way to have no matching rules at all.

#![cfg(feature = "cookies")]

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use assert_matches::assert_matches;
use hclient::cookie::{CookieJar, Limits, NoList, Rejected};
use http::{HeaderValue, Uri};

fn now() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_700_000_000)
}

fn uri(s: &str) -> Uri {
    s.parse().expect("test URI")
}

fn header(s: &str) -> HeaderValue {
    HeaderValue::from_str(s).expect("test header")
}

/// `store`, with the boilerplate gone.
fn store(jar: &mut CookieJar, at: &str, set_cookie: &str) -> Result<(), Rejected> {
    jar.store(&uri(at), &header(set_cookie), now())
}

/// The `Cookie` header the jar would send, as a `String`, or `""`.
fn sent(jar: &mut CookieJar, to: &str) -> String {
    jar.cookie_header(&uri(to), now())
        .map(|v| v.to_str().expect("ascii").to_owned())
        .unwrap_or_default()
}

// ── the domain rules ────────────────────────────────────────────────────

/// Needs the compiled-in list, so it does not run in a
/// `--no-default-features` build — where the same header is refused for a
/// different and equally correct reason. `tests/without_the_list.rs` is
/// that build's half of the story.
#[cfg(feature = "public-suffix")]
#[test]
fn a_cookie_for_a_sibling_domain_is_refused() {
    let mut jar = CookieJar::new();
    // `notexample.com` ends with `example.com` as a *string*. Without the
    // label-boundary check in `domain_matches`, this is accepted and every
    // cookie `example.com` sets is then also sent to it.
    assert_matches!(
        store(
            &mut jar,
            "https://notexample.com/",
            "sid=leak; Domain=example.com"
        ),
        Err(Rejected::DomainMismatch { .. })
    );
    // And the same shape one level up: a host that is a *prefix* of the
    // domain rather than a suffix of it.
    assert_matches!(
        store(
            &mut jar,
            "https://example.com/",
            "sid=leak; Domain=www.example.com"
        ),
        Err(Rejected::DomainMismatch { .. })
    );
    // An unrelated domain entirely, which is the case everybody remembers.
    assert_matches!(
        store(
            &mut jar,
            "https://example.com/",
            "sid=leak; Domain=evil.test"
        ),
        Err(Rejected::DomainMismatch { .. })
    );
    assert!(jar.is_empty());
}

/// Needs the compiled-in list, so it does not run in a
/// `--no-default-features` build — where the same header is refused for a
/// different and equally correct reason. `tests/without_the_list.rs` is
/// that build's half of the story.
#[cfg(feature = "public-suffix")]
#[test]
fn a_cookie_for_a_public_suffix_is_refused() {
    let mut jar = CookieJar::new();
    // The case the public suffix list exists for: `www.bbc.co.uk`
    // domain-matches `co.uk` perfectly well, so the boundary check above
    // does not catch this one and nothing but a list can.
    assert_matches!(
        store(&mut jar, "https://www.bbc.co.uk/", "sid=leak; Domain=co.uk"),
        Err(Rejected::DomainIsPublicSuffix { .. })
    );
    assert_matches!(
        store(&mut jar, "https://example.com/", "sid=leak; Domain=com"),
        Err(Rejected::DomainIsPublicSuffix { .. })
    );
    // The private section of the list counts too — one tenant of a shared
    // hosting domain must not set a cookie for the others.
    assert_matches!(
        store(
            &mut jar,
            "https://mine.github.io/",
            "sid=leak; Domain=github.io"
        ),
        Err(Rejected::DomainIsPublicSuffix { .. })
    );
    assert!(jar.is_empty());

    // …while the registrable name one label down is accepted, so this is a
    // rule about the registry and not a blanket refusal.
    store(
        &mut jar,
        "https://www.bbc.co.uk/",
        "sid=ok; Domain=bbc.co.uk",
    )
    .expect("registrable");
    assert_eq!(sent(&mut jar, "https://news.bbc.co.uk/"), "sid=ok");
}

#[test]
fn a_domain_identical_to_the_host_survives_the_public_suffix_check_as_host_only() {
    // RFC 6265bis §5.7's one rescue. `localhost` is a public suffix by the
    // list's prevailing `*` rule, so without this branch a development
    // server on `http://localhost` could not set a cookie at all.
    let mut jar = CookieJar::new();
    store(&mut jar, "http://localhost/", "sid=dev; Domain=localhost").expect("rescued");
    let stored = jar.iter().next().expect("one cookie");
    assert!(stored.host_only(), "the rescue must downgrade to host-only");
    assert_eq!(sent(&mut jar, "http://localhost/"), "sid=dev");
    // Host-only means host-only: not to a subdomain, even though the
    // cookie's own domain string would domain-match it.
    assert_eq!(sent(&mut jar, "http://sub.localhost/"), "");
}

#[test]
fn without_a_list_every_domain_attribute_is_refused_rather_than_guessed() {
    // The no-list build, reachable here without a second cargo invocation.
    // It is narrower than the list build, never wider — which is the whole
    // argument for the feature being safe to turn off.
    let mut jar = CookieJar::with_public_suffix_list(NoList);
    let at = uri("https://www.example.com/");
    assert_matches!(
        jar.store(&at, &header("sid=x; Domain=example.com"), now()),
        Err(Rejected::NoPublicSuffixList { .. })
    );
    // Host-only cookies still work, and the `Domain == host` rescue still
    // fires, so a no-list jar is usable rather than inert.
    jar.store(&at, &header("a=1"), now()).expect("host-only");
    jar.store(&at, &header("b=2; Domain=www.example.com"), now())
        .expect("domain identical to host");
    assert_eq!(jar.len(), 2);
    assert!(jar.iter().all(hclient::cookie::Cookie::host_only));
}

/// Needs the compiled-in list, so it does not run in a
/// `--no-default-features` build — where the same header is refused for a
/// different and equally correct reason. `tests/without_the_list.rs` is
/// that build's half of the story.
#[cfg(feature = "public-suffix")]
#[test]
fn a_host_only_cookie_does_not_reach_a_subdomain_and_a_domain_cookie_does() {
    let mut jar = CookieJar::new();
    store(&mut jar, "https://example.com/", "host=1").expect("host-only");
    store(
        &mut jar,
        "https://example.com/",
        "dom=2; Domain=example.com",
    )
    .expect("domain");

    assert_eq!(sent(&mut jar, "https://example.com/"), "host=1; dom=2");
    // The subdomain sees the `Domain` cookie and only that one. If the
    // host-only flag were dropped, both would arrive.
    assert_eq!(sent(&mut jar, "https://www.example.com/"), "dom=2");
    // And a sibling of the *parent* sees neither.
    assert_eq!(sent(&mut jar, "https://notexample.com/"), "");
}

#[test]
fn an_ip_host_takes_no_domain_but_itself() {
    let mut jar = CookieJar::new();
    assert_matches!(
        store(&mut jar, "http://127.0.0.1:8080/", "sid=x; Domain=0.0.1"),
        Err(Rejected::DomainMismatch { .. })
    );
    store(
        &mut jar,
        "http://127.0.0.1:8080/",
        "sid=x; Domain=127.0.0.1",
    )
    .expect("identical");
    assert!(jar.iter().next().expect("one").host_only());
}

// ── the path rules ──────────────────────────────────────────────────────

#[test]
fn a_string_prefix_is_not_a_path_prefix() {
    let mut jar = CookieJar::new();
    store(&mut jar, "https://example.com/", "p=1; Path=/foo").expect("stored");

    // The refusal this test exists for: `/foo` starts `/foobar`, and they
    // are different resources.
    assert_eq!(sent(&mut jar, "https://example.com/foobar"), "");
    assert_eq!(sent(&mut jar, "https://example.com/foo.html"), "");
    assert_eq!(sent(&mut jar, "https://example.com/fo"), "");
    assert_eq!(sent(&mut jar, "https://example.com/"), "");

    // …and the three shapes that must still match, so the rule is a
    // boundary and not a ban.
    assert_eq!(sent(&mut jar, "https://example.com/foo"), "p=1");
    assert_eq!(sent(&mut jar, "https://example.com/foo/"), "p=1");
    assert_eq!(sent(&mut jar, "https://example.com/foo/bar"), "p=1");
}

#[test]
fn the_default_path_is_the_directory_and_not_the_resource() {
    let mut jar = CookieJar::new();
    store(&mut jar, "https://example.com/a/b/c", "p=1").expect("stored");
    assert_eq!(jar.iter().next().expect("one").path(), "/a/b");
    assert_eq!(sent(&mut jar, "https://example.com/a/b/other"), "p=1");
    // A sibling directory whose name starts with the same bytes: the same
    // boundary rule, arrived at through the default path rather than an
    // explicit one.
    assert_eq!(sent(&mut jar, "https://example.com/a/bb/x"), "");
    assert_eq!(sent(&mut jar, "https://example.com/a/"), "");
}

// ── Secure, and the name prefixes ───────────────────────────────────────

#[test]
fn a_secure_cookie_is_refused_over_an_insecure_request() {
    let mut jar = CookieJar::new();
    assert_matches!(
        store(&mut jar, "http://example.com/", "sid=x; Secure"),
        Err(Rejected::SecureOverInsecure)
    );
    assert!(jar.is_empty());
    store(&mut jar, "https://example.com/", "sid=x; Secure").expect("over https");
}

#[test]
fn a_secure_cookie_is_never_sent_over_an_insecure_request() {
    // The storage refusal above is not enough on its own: a cookie set over
    // https must not leak back out over http to the same host.
    let mut jar = CookieJar::new();
    store(&mut jar, "https://example.com/", "s=1; Secure").expect("stored");
    store(&mut jar, "https://example.com/", "p=2").expect("stored");

    assert_eq!(sent(&mut jar, "https://example.com/"), "s=1; p=2");
    assert_eq!(sent(&mut jar, "http://example.com/"), "p=2");
}

#[test]
fn loopback_over_http_counts_as_secure() {
    // Deliberate, documented divergence from "scheme == https": without
    // it, every `Secure` cookie silently vanishes in local development.
    let mut jar = CookieJar::new();
    store(&mut jar, "http://localhost:3000/", "a=1; Secure").expect("localhost");
    store(&mut jar, "http://127.0.0.1:3000/", "b=2; Secure").expect("ipv4 loopback");
    store(&mut jar, "http://[::1]:3000/", "c=3; Secure").expect("ipv6 loopback");
    // …and no further: a private address is not a loopback one.
    assert_matches!(
        store(&mut jar, "http://10.0.0.1/", "d=4; Secure"),
        Err(Rejected::SecureOverInsecure)
    );
}

#[test]
fn the_name_prefixes_are_enforced() {
    let mut jar = CookieJar::new();

    // `__Secure-` needs the Secure attribute, over a secure request.
    assert_matches!(
        store(&mut jar, "https://example.com/", "__Secure-a=1"),
        Err(Rejected::SecurePrefix)
    );
    store(&mut jar, "https://example.com/", "__Secure-a=1; Secure").expect("with Secure");

    // `__Host-` needs Secure, no Domain, and Path=/ — one test per clause,
    // because a check that only looks at one of them still passes a test
    // that only violates one.
    assert_matches!(
        store(&mut jar, "https://example.com/", "__Host-a=1; Path=/"),
        Err(Rejected::HostPrefix)
    );
    assert_matches!(
        store(
            &mut jar,
            "https://example.com/a/b",
            "__Host-a=1; Secure; Path=/a"
        ),
        Err(Rejected::HostPrefix)
    );
    // A `__Host-` cookie set from `/a/b` with no Path at all defaults to
    // `/a`, so it is refused too — the prefix is about the stored path, not
    // about what was written in the header.
    assert_matches!(
        store(&mut jar, "https://example.com/a/b", "__Host-a=1; Secure"),
        Err(Rejected::HostPrefix)
    );
    store(
        &mut jar,
        "https://example.com/",
        "__Host-a=1; Secure; Path=/",
    )
    .expect("all three");
}

/// The `__Host-` clause that needs a `Domain` attribute to reach the
/// prefix check at all, so it needs the list too.
#[cfg(feature = "public-suffix")]
#[test]
fn the_host_prefix_refuses_a_domain_attribute() {
    let mut jar = CookieJar::new();
    assert_matches!(
        store(
            &mut jar,
            "https://www.example.com/",
            "__Host-a=1; Secure; Path=/; Domain=example.com"
        ),
        Err(Rejected::HostPrefix)
    );
    // The prefix comparison is case-insensitive, per 6265bis §4.1.3: this
    // one would be stored, with a subdomain scope, if the check only ever
    // looked for the exact spelling.
    assert_matches!(
        store(
            &mut jar,
            "https://www.example.com/",
            "__HOST-b=1; Secure; Path=/; Domain=example.com"
        ),
        Err(Rejected::HostPrefix)
    );
    assert!(jar.is_empty());
}

// ── expiry ──────────────────────────────────────────────────────────────

#[test]
fn max_age_zero_deletes_and_a_past_expires_deletes() {
    let mut jar = CookieJar::new();
    store(&mut jar, "https://example.com/", "a=1").expect("stored");
    store(&mut jar, "https://example.com/", "b=2").expect("stored");
    assert_eq!(sent(&mut jar, "https://example.com/"), "a=1; b=2");

    store(&mut jar, "https://example.com/", "a=; Max-Age=0").expect("deletion");
    assert_eq!(sent(&mut jar, "https://example.com/"), "b=2");

    // The form that actually appears on the wire, and the one an HTTP-date
    // parser would silently ignore. See `tests/dates.rs`.
    store(
        &mut jar,
        "https://example.com/",
        "b=; Expires=Thu, 01-Jan-1970 00:00:01 GMT",
    )
    .expect("deletion by Expires");
    assert_eq!(sent(&mut jar, "https://example.com/"), "");
    assert!(jar.is_empty());
}

#[test]
fn max_age_wins_over_expires() {
    // §5.7 gives Max-Age precedence, and the two here point in opposite
    // directions: an Expires far in the past against a Max-Age in the
    // future. If precedence were the other way round the cookie would be
    // gone.
    let mut jar = CookieJar::new();
    store(
        &mut jar,
        "https://example.com/",
        "a=1; Expires=Thu, 01 Jan 1970 00:00:01 GMT; Max-Age=3600",
    )
    .expect("stored");
    assert_eq!(sent(&mut jar, "https://example.com/"), "a=1");
    assert_eq!(
        jar.iter().next().expect("one").expires(),
        Some(now() + Duration::from_secs(3600))
    );
}

#[test]
fn an_expiry_beyond_four_hundred_days_is_capped() {
    let mut jar = CookieJar::new();
    store(&mut jar, "https://example.com/", "a=1; Max-Age=999999999").expect("stored");
    let cap = now() + Duration::from_secs(400 * 24 * 60 * 60);
    assert_eq!(jar.iter().next().expect("one").expires(), Some(cap));

    // The same cap through `Expires`, and the same arithmetic hazard shut
    // off: i64::MAX seconds cannot be added to a SystemTime.
    store(
        &mut jar,
        "https://example.com/",
        "b=2; Max-Age=9223372036854775807",
    )
    .expect("no overflow");
    assert_eq!(
        jar.iter().find(|c| c.name() == "b").expect("b").expires(),
        Some(cap)
    );
}

#[test]
fn an_expired_cookie_is_not_returned() {
    let mut jar = CookieJar::new();
    store(&mut jar, "https://example.com/", "a=1; Max-Age=10").expect("stored");
    assert_eq!(sent(&mut jar, "https://example.com/"), "a=1");
    let later = now() + Duration::from_secs(11);
    assert!(
        jar.cookie_header(&uri("https://example.com/"), later)
            .is_none()
    );
}

#[test]
fn a_session_cookie_has_no_expiry_and_never_ages_out() {
    let mut jar = CookieJar::new();
    store(&mut jar, "https://example.com/", "a=1").expect("stored");
    let cookie = jar.iter().next().expect("one");
    assert_eq!(cookie.expires(), None);
    assert!(!cookie.persistent());
    let much_later = now() + Duration::from_secs(10 * 365 * 24 * 3600);
    assert!(
        jar.cookie_header(&uri("https://example.com/"), much_later)
            .is_some()
    );
}

// ── the storage model ───────────────────────────────────────────────────

#[test]
fn same_name_domain_and_path_replaces_and_keeps_the_original_creation_time() {
    let mut jar = CookieJar::new();
    store(&mut jar, "https://example.com/", "a=first").expect("stored");
    let created = jar.iter().next().expect("one").creation();

    let later = now() + Duration::from_secs(60);
    jar.store(&uri("https://example.com/"), &header("a=second"), later)
        .expect("replaced");

    assert_eq!(jar.len(), 1, "same name/domain/path replaces");
    let cookie = jar.iter().next().expect("one");
    assert_eq!(cookie.value(), "second");
    assert_eq!(
        cookie.creation(),
        created,
        "§5.7: a replacement inherits the old creation time"
    );
}

/// Needs the compiled-in list, so it does not run in a
/// `--no-default-features` build — where the same header is refused for a
/// different and equally correct reason. `tests/without_the_list.rs` is
/// that build's half of the story.
#[cfg(feature = "public-suffix")]
#[test]
fn a_different_path_or_domain_is_a_different_cookie() {
    let mut jar = CookieJar::new();
    store(&mut jar, "https://example.com/", "a=root; Path=/").expect("stored");
    store(&mut jar, "https://example.com/", "a=deep; Path=/x").expect("stored");
    store(
        &mut jar,
        "https://example.com/",
        "a=dom; Domain=example.com; Path=/",
    )
    .expect("stored");
    assert_eq!(jar.len(), 3);
}

#[test]
fn retrieval_puts_longer_paths_first_then_earlier_cookies() {
    let mut jar = CookieJar::new();
    // Stored shortest-path-first and in creation order, so a jar that
    // simply returns insertion order gets a different answer.
    store(&mut jar, "https://example.com/", "a=1; Path=/").expect("stored");
    store(&mut jar, "https://example.com/", "b=2; Path=/x").expect("stored");
    store(&mut jar, "https://example.com/", "c=3; Path=/x/y").expect("stored");
    store(&mut jar, "https://example.com/", "d=4; Path=/x").expect("stored");

    assert_eq!(
        sent(&mut jar, "https://example.com/x/y/z"),
        "c=3; b=2; d=4; a=1"
    );
}

#[test]
fn the_bound_is_enforced_per_domain_and_evicts_the_least_recently_used() {
    let mut jar = CookieJar::new().with_limits(Limits {
        max_per_domain: 2,
        ..Limits::default()
    });
    store(&mut jar, "https://example.com/", "a=1").expect("stored");
    store(&mut jar, "https://example.com/", "b=2").expect("stored");
    // Touching `b` makes `a` the least recently used one.
    assert_eq!(sent(&mut jar, "https://example.com/"), "a=1; b=2");
    jar.cookie_header(&uri("https://example.com/"), now() + Duration::from_secs(1));

    store(&mut jar, "https://example.com/", "c=3").expect("stored");
    assert_eq!(jar.len(), 2, "the bound holds");
    let names: Vec<_> = jar.iter().map(hclient::cookie::Cookie::name).collect();
    assert!(
        !names.contains(&"a"),
        "the least recently used one went: {names:?}"
    );
    assert!(names.contains(&"c"));

    // A different domain has its own budget, so this is a per-domain bound
    // and not a global one wearing its name.
    store(&mut jar, "https://other.test/", "d=4").expect("stored");
    assert_eq!(jar.len(), 3);
}

#[test]
fn the_total_bound_is_enforced_across_domains() {
    let mut jar = CookieJar::new().with_limits(Limits {
        max_cookies: 2,
        ..Limits::default()
    });
    store(&mut jar, "https://a.test/", "x=1").expect("stored");
    store(&mut jar, "https://b.test/", "x=2").expect("stored");
    store(&mut jar, "https://c.test/", "x=3").expect("stored");
    assert_eq!(jar.len(), 2);
    let domains: Vec<_> = jar.iter().map(hclient::cookie::Cookie::domain).collect();
    assert!(!domains.contains(&"a.test"), "{domains:?}");
}

#[test]
fn an_oversized_cookie_is_refused_rather_than_truncated() {
    let mut jar = CookieJar::new().with_limits(Limits {
        max_name_value_bytes: 16,
        ..Limits::default()
    });
    assert_matches!(
        store(&mut jar, "https://example.com/", "name=0123456789012345"),
        Err(Rejected::TooLarge {
            bytes: 20,
            limit: 16
        })
    );
    assert!(jar.is_empty());
    store(&mut jar, "https://example.com/", "name=012345678901").expect("exactly at the limit");
}

#[test]
fn a_uri_with_no_host_stores_nothing_and_matches_nothing() {
    let mut jar = CookieJar::new();
    assert_matches!(store(&mut jar, "/relative", "a=1"), Err(Rejected::NoHost));
    store(&mut jar, "https://example.com/", "a=1").expect("stored");
    assert_eq!(sent(&mut jar, "/relative"), "");
}

#[test]
fn store_response_keeps_going_past_a_bad_header() {
    let mut jar = CookieJar::new();
    let mut headers = http::HeaderMap::new();
    headers.append(http::header::SET_COOKIE, header("a=1"));
    headers.append(http::header::SET_COOKIE, header("b=2; Domain=co.uk"));
    headers.append(http::header::SET_COOKIE, header("c=3"));
    assert_eq!(
        jar.store_response(&uri("https://example.com/"), &headers, now()),
        2
    );
    assert_eq!(sent(&mut jar, "https://example.com/"), "a=1; c=3");
}

/// Needs the compiled-in list, so it does not run in a
/// `--no-default-features` build — where the same header is refused for a
/// different and equally correct reason. `tests/without_the_list.rs` is
/// that build's half of the story.
#[cfg(feature = "public-suffix")]
#[test]
fn the_documented_example_holds() {
    // The same sequence as `CookieJar`'s doc comment. Doctests do not run
    // under nextest (see AGENTS.md, "Running the tests"), so the claim in
    // the documentation is pinned here as well rather than only there.
    let mut jar = CookieJar::new();
    jar.store(
        &uri("https://www.example.com/app"),
        &header("sid=abc; Domain=example.com"),
        now(),
    )
    .expect("stored");
    assert_eq!(
        jar.cookie_header(&uri("https://api.example.com/v1"), now())
            .expect("sent"),
        "sid=abc"
    );
}
