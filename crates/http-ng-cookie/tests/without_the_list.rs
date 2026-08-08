//! The half of the behaviour `--all-features` can never reach.
//!
//! `cargo nextest run --workspace --all-features` turns the
//! `public-suffix` feature **on**, so nothing in the ordinary suite ever
//! executes the no-list branch of `BuiltinList`. This file runs only when
//! the feature is off — `just test-no-default`, which is where the same
//! problem for `http-ng-proto`'s `idn` feature is already handled.
//!
//! What it has to establish is one claim, and it is a security claim rather
//! than a coverage one: **a build with no list is narrower than a build
//! with one, never wider.** A jar that quietly accepted `Domain=example.com`
//! when it could not check it would be worse than one that refuses the
//! feature outright.

#![cfg(not(feature = "public-suffix"))]

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use assert_matches::assert_matches;
use http::{HeaderValue, Uri};
use http_ng_cookie::{BuiltinList, Cookie, CookieJar, PublicSuffixList, Rejected};

fn now() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_700_000_000)
}

fn uri(s: &str) -> Uri {
    s.parse().expect("test URI")
}

fn header(s: &str) -> HeaderValue {
    HeaderValue::from_str(s).expect("test header")
}

#[test]
fn the_builtin_list_admits_it_has_no_list() {
    assert!(!BuiltinList.has_list());
    // …and answers the conservative way for everything, which is what makes
    // the refusal below happen at all.
    assert!(BuiltinList.is_public_suffix("example.com"));
    assert!(BuiltinList.is_public_suffix("co.uk"));
}

#[test]
fn every_domain_attribute_is_refused_and_the_error_names_the_build() {
    let mut jar = CookieJar::new();
    let at = uri("https://www.example.com/");

    // The one that a list would have refused anyway.
    assert_matches!(
        jar.store(&at, &header("a=1; Domain=co.uk"), now()),
        Err(Rejected::NoPublicSuffixList { .. })
    );
    // …and the one a list would have accepted. Refusing it is the cost of
    // the feature being off, and it is the *narrow* direction.
    assert_matches!(
        jar.store(&at, &header("b=2; Domain=example.com"), now()),
        Err(Rejected::NoPublicSuffixList { .. })
    );
    assert!(jar.is_empty());
}

#[test]
fn a_no_list_jar_is_still_a_working_host_only_jar() {
    let mut jar = CookieJar::new();
    let at = uri("https://www.example.com/app/page");

    jar.store(&at, &header("a=1"), now()).expect("host-only");
    // The `Domain == host` rescue is the one path that still stores a
    // cookie carrying a Domain attribute, and it downgrades to host-only.
    jar.store(&at, &header("b=2; Domain=www.example.com"), now())
        .expect("identical to the host");
    assert_eq!(jar.len(), 2);
    assert!(jar.iter().all(Cookie::host_only));

    // Everything that does not depend on the list still works: the default
    // path, the ordering, the send.
    assert_eq!(jar.iter().next().expect("one").path(), "/app");
    assert_eq!(
        jar.cookie_header(&uri("https://www.example.com/app/other"), now())
            .expect("sent"),
        "a=1; b=2"
    );
    // And no subdomain or sibling sees any of it.
    assert!(
        jar.cookie_header(&uri("https://deep.www.example.com/app/x"), now())
            .is_none()
    );
    assert!(
        jar.cookie_header(&uri("https://example.com/app/x"), now())
            .is_none()
    );
}

#[test]
fn the_rules_that_do_not_need_a_list_are_unaffected() {
    let mut jar = CookieJar::new();
    assert_matches!(
        jar.store(&uri("http://example.com/"), &header("a=1; Secure"), now()),
        Err(Rejected::SecureOverInsecure)
    );
    jar.store(
        &uri("https://example.com/"),
        &header("p=1; Path=/foo"),
        now(),
    )
    .expect("stored");
    assert!(
        jar.cookie_header(&uri("https://example.com/foobar"), now())
            .is_none(),
        "a string prefix is not a path prefix, list or no list"
    );
}
