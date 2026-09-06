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

use hclient::cookie::{BuiltinList, Capacity, CookieJar, Limits, MemoryStore, NoList, Rejected};
use http::{HeaderValue, Uri};
use std::assert_matches;

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
async fn store(jar: &CookieJar, at: &str, set_cookie: &str) -> Result<(), Rejected> {
    jar.store(&uri(at), &header(set_cookie), now()).await
}

/// The `Cookie` header the jar would send, as a `String`, or `""`.
async fn sent(jar: &CookieJar, to: &str) -> String {
    jar.cookie_header(&uri(to), now())
        .await
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
    futures_executor::block_on(async {
        let jar = CookieJar::new();
        // `notexample.com` ends with `example.com` as a *string*. Without the
        // label-boundary check in `domain_matches`, this is accepted and every
        // cookie `example.com` sets is then also sent to it.
        assert_matches!(
            store(
                &jar,
                "https://notexample.com/",
                "sid=leak; Domain=example.com"
            )
            .await,
            Err(Rejected::DomainMismatch { .. })
        );
        // And the same shape one level up: a host that is a *prefix* of the
        // domain rather than a suffix of it.
        assert_matches!(
            store(
                &jar,
                "https://example.com/",
                "sid=leak; Domain=www.example.com"
            )
            .await,
            Err(Rejected::DomainMismatch { .. })
        );
        // An unrelated domain entirely, which is the case everybody remembers.
        assert_matches!(
            store(&jar, "https://example.com/", "sid=leak; Domain=evil.test").await,
            Err(Rejected::DomainMismatch { .. })
        );
        assert!(jar.is_empty().await);
    })
}

/// Needs the compiled-in list, so it does not run in a
/// `--no-default-features` build — where the same header is refused for a
/// different and equally correct reason. `tests/without_the_list.rs` is
/// that build's half of the story.
#[cfg(feature = "public-suffix")]
#[test]
fn a_cookie_for_a_public_suffix_is_refused() {
    futures_executor::block_on(async {
        let jar = CookieJar::new();
        // The case the public suffix list exists for: `www.bbc.co.uk`
        // domain-matches `co.uk` perfectly well, so the boundary check above
        // does not catch this one and nothing but a list can.
        assert_matches!(
            store(&jar, "https://www.bbc.co.uk/", "sid=leak; Domain=co.uk").await,
            Err(Rejected::DomainIsPublicSuffix { .. })
        );
        assert_matches!(
            store(&jar, "https://example.com/", "sid=leak; Domain=com").await,
            Err(Rejected::DomainIsPublicSuffix { .. })
        );
        // The private section of the list counts too — one tenant of a shared
        // hosting domain must not set a cookie for the others.
        assert_matches!(
            store(
                &jar,
                "https://mine.github.io/",
                "sid=leak; Domain=github.io"
            )
            .await,
            Err(Rejected::DomainIsPublicSuffix { .. })
        );
        assert!(jar.is_empty().await);

        // …while the registrable name one label down is accepted, so this is a
        // rule about the registry and not a blanket refusal.
        store(&jar, "https://www.bbc.co.uk/", "sid=ok; Domain=bbc.co.uk")
            .await
            .expect("registrable");
        assert_eq!(sent(&jar, "https://news.bbc.co.uk/").await, "sid=ok");
    })
}

#[test]
fn a_domain_identical_to_the_host_survives_the_public_suffix_check_as_host_only() {
    futures_executor::block_on(async {
        // RFC 6265bis §5.7's one rescue. `localhost` is a public suffix by the
        // list's prevailing `*` rule, so without this branch a development
        // server on `http://localhost` could not set a cookie at all.
        let jar = CookieJar::new();
        store(&jar, "http://localhost/", "sid=dev; Domain=localhost")
            .await
            .expect("rescued");
        let stored = jar.cookies().await.into_iter().next().expect("one cookie");
        assert!(stored.host_only(), "the rescue must downgrade to host-only");
        assert_eq!(sent(&jar, "http://localhost/").await, "sid=dev");
        // Host-only means host-only: not to a subdomain, even though the
        // cookie's own domain string would domain-match it.
        assert_eq!(sent(&jar, "http://sub.localhost/").await, "");
    })
}

#[test]
fn without_a_list_every_domain_attribute_is_refused_rather_than_guessed() {
    futures_executor::block_on(async {
        // The no-list build, reachable here without a second cargo invocation.
        // It is narrower than the list build, never wider — which is the whole
        // argument for the feature being safe to turn off.
        let jar = CookieJar::with_public_suffix_list(NoList);
        let at = uri("https://www.example.com/");
        assert_matches!(
            jar.store(&at, &header("sid=x; Domain=example.com"), now())
                .await,
            Err(Rejected::NoPublicSuffixList { .. })
        );
        // Host-only cookies still work, and the `Domain == host` rescue still
        // fires, so a no-list jar is usable rather than inert.
        jar.store(&at, &header("a=1"), now())
            .await
            .expect("host-only");
        jar.store(&at, &header("b=2; Domain=www.example.com"), now())
            .await
            .expect("domain identical to host");
        assert_eq!(jar.len().await, 2);
        assert!(jar.cookies().await.into_iter().all(|c| c.host_only()));
    })
}

/// Needs the compiled-in list, so it does not run in a
/// `--no-default-features` build — where the same header is refused for a
/// different and equally correct reason. `tests/without_the_list.rs` is
/// that build's half of the story.
#[cfg(feature = "public-suffix")]
#[test]
fn a_host_only_cookie_does_not_reach_a_subdomain_and_a_domain_cookie_does() {
    futures_executor::block_on(async {
        let jar = CookieJar::new();
        store(&jar, "https://example.com/", "host=1")
            .await
            .expect("host-only");
        store(&jar, "https://example.com/", "dom=2; Domain=example.com")
            .await
            .expect("domain");

        assert_eq!(sent(&jar, "https://example.com/").await, "host=1; dom=2");
        // The subdomain sees the `Domain` cookie and only that one. If the
        // host-only flag were dropped, both would arrive.
        assert_eq!(sent(&jar, "https://www.example.com/").await, "dom=2");
        // And a sibling of the *parent* sees neither.
        assert_eq!(sent(&jar, "https://notexample.com/").await, "");
    })
}

#[test]
fn an_ip_host_takes_no_domain_but_itself() {
    futures_executor::block_on(async {
        let jar = CookieJar::new();
        assert_matches!(
            store(&jar, "http://127.0.0.1:8080/", "sid=x; Domain=0.0.1").await,
            Err(Rejected::DomainMismatch { .. })
        );
        store(&jar, "http://127.0.0.1:8080/", "sid=x; Domain=127.0.0.1")
            .await
            .expect("identical");
        assert!(
            jar.cookies()
                .await
                .into_iter()
                .next()
                .expect("one")
                .host_only()
        );
    })
}

// ── the path rules ──────────────────────────────────────────────────────

#[test]
fn a_string_prefix_is_not_a_path_prefix() {
    futures_executor::block_on(async {
        let jar = CookieJar::new();
        store(&jar, "https://example.com/", "p=1; Path=/foo")
            .await
            .expect("stored");

        // The refusal this test exists for: `/foo` starts `/foobar`, and they
        // are different resources.
        assert_eq!(sent(&jar, "https://example.com/foobar").await, "");
        assert_eq!(sent(&jar, "https://example.com/foo.html").await, "");
        assert_eq!(sent(&jar, "https://example.com/fo").await, "");
        assert_eq!(sent(&jar, "https://example.com/").await, "");

        // …and the three shapes that must still match, so the rule is a
        // boundary and not a ban.
        assert_eq!(sent(&jar, "https://example.com/foo").await, "p=1");
        assert_eq!(sent(&jar, "https://example.com/foo/").await, "p=1");
        assert_eq!(sent(&jar, "https://example.com/foo/bar").await, "p=1");
    })
}

#[test]
fn the_default_path_is_the_directory_and_not_the_resource() {
    futures_executor::block_on(async {
        let jar = CookieJar::new();
        store(&jar, "https://example.com/a/b/c", "p=1")
            .await
            .expect("stored");
        assert_eq!(
            jar.cookies().await.into_iter().next().expect("one").path(),
            "/a/b"
        );
        assert_eq!(sent(&jar, "https://example.com/a/b/other").await, "p=1");
        // A sibling directory whose name starts with the same bytes: the same
        // boundary rule, arrived at through the default path rather than an
        // explicit one.
        assert_eq!(sent(&jar, "https://example.com/a/bb/x").await, "");
        assert_eq!(sent(&jar, "https://example.com/a/").await, "");
    })
}

// ── Secure, and the name prefixes ───────────────────────────────────────

#[test]
fn a_secure_cookie_is_refused_over_an_insecure_request() {
    futures_executor::block_on(async {
        let jar = CookieJar::new();
        assert_matches!(
            store(&jar, "http://example.com/", "sid=x; Secure").await,
            Err(Rejected::SecureOverInsecure)
        );
        assert!(jar.is_empty().await);
        store(&jar, "https://example.com/", "sid=x; Secure")
            .await
            .expect("over https");
    })
}

#[test]
fn a_secure_cookie_is_never_sent_over_an_insecure_request() {
    futures_executor::block_on(async {
        // The storage refusal above is not enough on its own: a cookie set over
        // https must not leak back out over http to the same host.
        let jar = CookieJar::new();
        store(&jar, "https://example.com/", "s=1; Secure")
            .await
            .expect("stored");
        store(&jar, "https://example.com/", "p=2")
            .await
            .expect("stored");

        assert_eq!(sent(&jar, "https://example.com/").await, "s=1; p=2");
        assert_eq!(sent(&jar, "http://example.com/").await, "p=2");
    })
}

#[test]
fn loopback_over_http_counts_as_secure() {
    futures_executor::block_on(async {
        // Deliberate, documented divergence from "scheme == https": without
        // it, every `Secure` cookie silently vanishes in local development.
        let jar = CookieJar::new();
        store(&jar, "http://localhost:3000/", "a=1; Secure")
            .await
            .expect("localhost");
        store(&jar, "http://127.0.0.1:3000/", "b=2; Secure")
            .await
            .expect("ipv4 loopback");
        store(&jar, "http://[::1]:3000/", "c=3; Secure")
            .await
            .expect("ipv6 loopback");
        // …and no further: a private address is not a loopback one.
        assert_matches!(
            store(&jar, "http://10.0.0.1/", "d=4; Secure").await,
            Err(Rejected::SecureOverInsecure)
        );
    })
}

#[test]
fn the_name_prefixes_are_enforced() {
    futures_executor::block_on(async {
        let jar = CookieJar::new();

        // `__Secure-` needs the Secure attribute, over a secure request.
        assert_matches!(
            store(&jar, "https://example.com/", "__Secure-a=1").await,
            Err(Rejected::SecurePrefix)
        );
        store(&jar, "https://example.com/", "__Secure-a=1; Secure")
            .await
            .expect("with Secure");

        // `__Host-` needs Secure, no Domain, and Path=/ — one test per clause,
        // because a check that only looks at one of them still passes a test
        // that only violates one.
        assert_matches!(
            store(&jar, "https://example.com/", "__Host-a=1; Path=/").await,
            Err(Rejected::HostPrefix)
        );
        assert_matches!(
            store(
                &jar,
                "https://example.com/a/b",
                "__Host-a=1; Secure; Path=/a"
            )
            .await,
            Err(Rejected::HostPrefix)
        );
        // A `__Host-` cookie set from `/a/b` with no Path at all defaults to
        // `/a`, so it is refused too — the prefix is about the stored path, not
        // about what was written in the header.
        assert_matches!(
            store(&jar, "https://example.com/a/b", "__Host-a=1; Secure").await,
            Err(Rejected::HostPrefix)
        );
        store(&jar, "https://example.com/", "__Host-a=1; Secure; Path=/")
            .await
            .expect("all three");
    })
}

/// The `__Host-` clause that needs a `Domain` attribute to reach the
/// prefix check at all, so it needs the list too.
#[cfg(feature = "public-suffix")]
#[test]
fn the_host_prefix_refuses_a_domain_attribute() {
    futures_executor::block_on(async {
        let jar = CookieJar::new();
        assert_matches!(
            store(
                &jar,
                "https://www.example.com/",
                "__Host-a=1; Secure; Path=/; Domain=example.com"
            )
            .await,
            Err(Rejected::HostPrefix)
        );
        // The prefix comparison is case-insensitive, per 6265bis §4.1.3: this
        // one would be stored, with a subdomain scope, if the check only ever
        // looked for the exact spelling.
        assert_matches!(
            store(
                &jar,
                "https://www.example.com/",
                "__HOST-b=1; Secure; Path=/; Domain=example.com"
            )
            .await,
            Err(Rejected::HostPrefix)
        );
        assert!(jar.is_empty().await);
    })
}

// ── expiry ──────────────────────────────────────────────────────────────

#[test]
fn max_age_zero_deletes_and_a_past_expires_deletes() {
    futures_executor::block_on(async {
        let jar = CookieJar::new();
        store(&jar, "https://example.com/", "a=1")
            .await
            .expect("stored");
        store(&jar, "https://example.com/", "b=2")
            .await
            .expect("stored");
        assert_eq!(sent(&jar, "https://example.com/").await, "a=1; b=2");

        store(&jar, "https://example.com/", "a=; Max-Age=0")
            .await
            .expect("deletion");
        assert_eq!(sent(&jar, "https://example.com/").await, "b=2");

        // The form that actually appears on the wire, and the one an HTTP-date
        // parser would silently ignore. See `tests/dates.rs`.
        store(
            &jar,
            "https://example.com/",
            "b=; Expires=Thu, 01-Jan-1970 00:00:01 GMT",
        )
        .await
        .expect("deletion by Expires");
        assert_eq!(sent(&jar, "https://example.com/").await, "");
        assert!(jar.is_empty().await);
    })
}

#[test]
fn max_age_wins_over_expires() {
    futures_executor::block_on(async {
        // §5.7 gives Max-Age precedence, and the two here point in opposite
        // directions: an Expires far in the past against a Max-Age in the
        // future. If precedence were the other way round the cookie would be
        // gone.
        let jar = CookieJar::new();
        store(
            &jar,
            "https://example.com/",
            "a=1; Expires=Thu, 01 Jan 1970 00:00:01 GMT; Max-Age=3600",
        )
        .await
        .expect("stored");
        assert_eq!(sent(&jar, "https://example.com/").await, "a=1");
        assert_eq!(
            jar.cookies()
                .await
                .into_iter()
                .next()
                .expect("one")
                .expires(),
            Some(now() + Duration::from_secs(3600))
        );
    })
}

#[test]
fn an_expiry_beyond_four_hundred_days_is_capped() {
    futures_executor::block_on(async {
        let jar = CookieJar::new();
        store(&jar, "https://example.com/", "a=1; Max-Age=999999999")
            .await
            .expect("stored");
        let cap = now() + Duration::from_secs(400 * 24 * 60 * 60);
        assert_eq!(
            jar.cookies()
                .await
                .into_iter()
                .next()
                .expect("one")
                .expires(),
            Some(cap)
        );

        // The same cap through `Expires`, and the same arithmetic hazard shut
        // off: i64::MAX seconds cannot be added to a SystemTime.
        store(
            &jar,
            "https://example.com/",
            "b=2; Max-Age=9223372036854775807",
        )
        .await
        .expect("no overflow");
        assert_eq!(
            jar.cookies()
                .await
                .into_iter()
                .find(|c| c.name() == "b")
                .expect("b")
                .expires(),
            Some(cap)
        );
    })
}

#[test]
fn an_expired_cookie_is_not_returned() {
    futures_executor::block_on(async {
        let jar = CookieJar::new();
        store(&jar, "https://example.com/", "a=1; Max-Age=10")
            .await
            .expect("stored");
        assert_eq!(sent(&jar, "https://example.com/").await, "a=1");
        let later = now() + Duration::from_secs(11);
        assert!(
            jar.cookie_header(&uri("https://example.com/"), later)
                .await
                .is_none()
        );
    })
}

#[test]
fn a_session_cookie_has_no_expiry_and_never_ages_out() {
    futures_executor::block_on(async {
        let jar = CookieJar::new();
        store(&jar, "https://example.com/", "a=1")
            .await
            .expect("stored");
        let cookie = jar.cookies().await.into_iter().next().expect("one");
        assert_eq!(cookie.expires(), None);
        assert!(!cookie.persistent());
        let much_later = now() + Duration::from_secs(10 * 365 * 24 * 3600);
        assert!(
            jar.cookie_header(&uri("https://example.com/"), much_later)
                .await
                .is_some()
        );
    })
}

// ── the storage model ───────────────────────────────────────────────────

#[test]
fn same_name_domain_and_path_replaces_and_keeps_the_original_creation_time() {
    futures_executor::block_on(async {
        let jar = CookieJar::new();
        store(&jar, "https://example.com/", "a=first")
            .await
            .expect("stored");
        let created = jar
            .cookies()
            .await
            .into_iter()
            .next()
            .expect("one")
            .creation();

        let later = now() + Duration::from_secs(60);
        jar.store(&uri("https://example.com/"), &header("a=second"), later)
            .await
            .expect("replaced");

        assert_eq!(jar.len().await, 1, "same name/domain/path replaces");
        let cookie = jar.cookies().await.into_iter().next().expect("one");
        assert_eq!(cookie.value(), "second");
        assert_eq!(
            cookie.creation(),
            created,
            "§5.7: a replacement inherits the old creation time"
        );
    })
}

/// Needs the compiled-in list, so it does not run in a
/// `--no-default-features` build — where the same header is refused for a
/// different and equally correct reason. `tests/without_the_list.rs` is
/// that build's half of the story.
#[cfg(feature = "public-suffix")]
#[test]
fn a_different_path_or_domain_is_a_different_cookie() {
    futures_executor::block_on(async {
        let jar = CookieJar::new();
        store(&jar, "https://example.com/", "a=root; Path=/")
            .await
            .expect("stored");
        store(&jar, "https://example.com/", "a=deep; Path=/x")
            .await
            .expect("stored");
        store(
            &jar,
            "https://example.com/",
            "a=dom; Domain=example.com; Path=/",
        )
        .await
        .expect("stored");
        assert_eq!(jar.len().await, 3);
    })
}

#[test]
fn retrieval_puts_longer_paths_first_then_earlier_cookies() {
    futures_executor::block_on(async {
        let jar = CookieJar::new();
        // Stored shortest-path-first and in creation order, so a jar that
        // simply returns insertion order gets a different answer.
        store(&jar, "https://example.com/", "a=1; Path=/")
            .await
            .expect("stored");
        store(&jar, "https://example.com/", "b=2; Path=/x")
            .await
            .expect("stored");
        store(&jar, "https://example.com/", "c=3; Path=/x/y")
            .await
            .expect("stored");
        store(&jar, "https://example.com/", "d=4; Path=/x")
            .await
            .expect("stored");

        assert_eq!(
            sent(&jar, "https://example.com/x/y/z").await,
            "c=3; b=2; d=4; a=1"
        );
    })
}

#[test]
fn the_bound_is_enforced_per_domain_and_evicts_the_least_recently_used() {
    futures_executor::block_on(async {
        let jar = CookieJar::with_store(
            BuiltinList,
            MemoryStore::with_capacity(Capacity {
                max_per_domain: 2,
                ..Capacity::default()
            }),
        );
        store(&jar, "https://example.com/", "a=1")
            .await
            .expect("stored");
        store(&jar, "https://example.com/", "b=2")
            .await
            .expect("stored");
        // Touching `b` makes `a` the least recently used one.
        assert_eq!(sent(&jar, "https://example.com/").await, "a=1; b=2");
        jar.cookie_header(&uri("https://example.com/"), now() + Duration::from_secs(1))
            .await;

        store(&jar, "https://example.com/", "c=3")
            .await
            .expect("stored");
        assert_eq!(jar.len().await, 2, "the bound holds");
        let names: Vec<_> = jar
            .cookies()
            .await
            .into_iter()
            .map(|c| c.name().to_owned())
            .collect();
        assert!(
            !names.iter().any(|n| n == "a"),
            "the least recently used one went: {names:?}"
        );
        assert!(names.iter().any(|n| n == "c"));

        // A different domain has its own budget, so this is a per-domain bound
        // and not a global one wearing its name.
        store(&jar, "https://other.test/", "d=4")
            .await
            .expect("stored");
        assert_eq!(jar.len().await, 3);
    })
}

#[test]
fn the_total_bound_is_enforced_across_domains() {
    futures_executor::block_on(async {
        let jar = CookieJar::with_store(
            BuiltinList,
            MemoryStore::with_capacity(Capacity {
                max_cookies: 2,
                ..Capacity::default()
            }),
        );
        store(&jar, "https://a.test/", "x=1").await.expect("stored");
        store(&jar, "https://b.test/", "x=2").await.expect("stored");
        store(&jar, "https://c.test/", "x=3").await.expect("stored");
        assert_eq!(jar.len().await, 2);
        let domains: Vec<_> = jar
            .cookies()
            .await
            .into_iter()
            .map(|c| c.domain().to_owned())
            .collect();
        assert!(!domains.iter().any(|d| d == "a.test"), "{domains:?}");
    })
}

#[test]
fn an_oversized_cookie_is_refused_rather_than_truncated() {
    futures_executor::block_on(async {
        let jar = CookieJar::new().with_limits(Limits {
            max_name_value_bytes: 16,
        });
        assert_matches!(
            store(&jar, "https://example.com/", "name=0123456789012345").await,
            Err(Rejected::TooLarge {
                bytes: 20,
                limit: 16
            })
        );
        assert!(jar.is_empty().await);
        store(&jar, "https://example.com/", "name=012345678901")
            .await
            .expect("exactly at the limit");
    })
}

#[test]
fn a_uri_with_no_host_stores_nothing_and_matches_nothing() {
    futures_executor::block_on(async {
        let jar = CookieJar::new();
        assert_matches!(store(&jar, "/relative", "a=1").await, Err(Rejected::NoHost));
        store(&jar, "https://example.com/", "a=1")
            .await
            .expect("stored");
        assert_eq!(sent(&jar, "/relative").await, "");
    })
}

#[test]
fn store_response_keeps_going_past_a_bad_header() {
    futures_executor::block_on(async {
        let jar = CookieJar::new();
        let mut headers = http::HeaderMap::new();
        headers.append(http::header::SET_COOKIE, header("a=1"));
        headers.append(http::header::SET_COOKIE, header("b=2; Domain=co.uk"));
        headers.append(http::header::SET_COOKIE, header("c=3"));
        assert_eq!(
            jar.store_response(&uri("https://example.com/"), &headers, now())
                .await,
            2
        );
        assert_eq!(sent(&jar, "https://example.com/").await, "a=1; c=3");
    })
}

/// Needs the compiled-in list, so it does not run in a
/// `--no-default-features` build — where the same header is refused for a
/// different and equally correct reason. `tests/without_the_list.rs` is
/// that build's half of the story.
#[cfg(feature = "public-suffix")]
#[test]
fn the_documented_example_holds() {
    futures_executor::block_on(async {
        // The same sequence as `CookieJar`'s doc comment. Doctests do not run
        // under nextest (see AGENTS.md, "Running the tests"), so the claim in
        // the documentation is pinned here as well rather than only there.
        let jar = CookieJar::new();
        jar.store(
            &uri("https://www.example.com/app"),
            &header("sid=abc; Domain=example.com"),
            now(),
        )
        .await
        .expect("stored");
        assert_eq!(
            jar.cookie_header(&uri("https://api.example.com/v1"), now())
                .await
                .expect("sent"),
            "sid=abc"
        );
    })
}
