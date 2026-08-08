//! `ClientBuilder::base_url()` — resolving a request's relative URIs.
//!
//! Before this round, the value was stored in `Config` and read by NOBODY:
//! `client.get("/v1/things")` reached the transport as the URI
//! `/v1/things`, with no scheme and no authority. The third "saved and
//! ignored" case in the project, the second in this same struct, and one
//! that survived the whole branch's full review, which caught its twin
//! (B1, client timeouts).
//!
//! **The semantics are RFC 3986 §5, the same ones `redirect::decide` uses
//! to resolve `Location:`.** One client shouldn't resolve relative
//! references by two different rules depending on whether they came from
//! the caller or from a response header; the shared implementation is
//! `http_ng_proto::uri::resolve_reference`, which the redirect stage also
//! calls.

// `http_ng::mock` lives behind the `test-util` feature (see `mock.rs`).
#![cfg(feature = "test-util")]

use http_ng::mock::MockTransport;
use http_ng::{Client, ErrorKind, InvalidBaseUrl, RequestBody};
// Only the feature-off test names this type; importing it
// unconditionally would be an unused import in a default build.
#[cfg(not(feature = "idn"))]
use http_ng::UriError;

fn client_with_base(base: &str) -> Client<MockTransport> {
    let m = MockTransport::new();
    m.push_response(http::Response::builder().status(200).body("").unwrap());
    Client::builder(m)
        .base_url(base.parse().unwrap())
        .build()
        .unwrap()
}

fn sent_uri(c: &Client<MockTransport>) -> String {
    c.transport().requests()[0].uri.to_string()
}

/// The case this setting exists for, and exactly the one that used to be
/// a silent no-op: a relative reference + a base.
#[test]
fn a_relative_request_uri_is_resolved_against_the_base() {
    let c = client_with_base("https://example.test/api/");
    futures_executor::block_on(c.get("v1/things").send()).unwrap();
    assert_eq!(sent_uri(&c), "https://example.test/api/v1/things");
}

/// An absolute request URI ignores the base — RFC 3986 §5.2.2: if a
/// reference has a scheme, resolution returns it unchanged.
#[test]
fn an_absolute_request_uri_ignores_the_base() {
    let c = client_with_base("https://example.test/api/");
    futures_executor::block_on(c.get("https://other.test/direct").send()).unwrap();
    assert_eq!(sent_uri(&c), "https://other.test/direct");
}

/// Without a base, the URI goes out as-is — including a relative one.
/// Nothing is invented for the caller; a transport that needs
/// absolute-form will reject it itself, with its own type
/// (`WasiHttp::execute` → `scheme_of`).
#[test]
fn without_a_base_the_uri_reaches_the_transport_unchanged() {
    let m = MockTransport::new();
    m.push_response(http::Response::builder().status(200).body("").unwrap());
    let c = Client::builder(m).build().unwrap();
    futures_executor::block_on(c.get("/v1/things").send()).unwrap();
    assert_eq!(sent_uri(&c), "/v1/things");
}

/// The sharp corner of RFC 3986 and the one non-obvious part of the rule:
/// a reference starting with `/` replaces the base's ENTIRE path, rather
/// than appending to it. Pinning both variants in one test so "fixing" one
/// doesn't slip by unnoticed.
#[test]
fn an_absolute_path_replaces_the_bases_path_while_a_relative_one_extends_it() {
    let c = client_with_base("https://example.test/api/");
    futures_executor::block_on(c.get("/v1/things").send()).unwrap();
    assert_eq!(
        sent_uri(&c),
        "https://example.test/v1/things",
        "a leading / replaces the base's path (RFC 3986 §5.2.2), rather than appending to it"
    );

    let c2 = client_with_base("https://example.test/api/");
    futures_executor::block_on(c2.get("v1/things").send()).unwrap();
    assert_eq!(sent_uri(&c2), "https://example.test/api/v1/things");
}

/// The second half of the same sharp corner: a base WITHOUT a trailing
/// slash loses its last segment when resolving a relative reference — this
/// is the merge algorithm from RFC 3986 §5.3, not our own invention.
/// Documented on the setter; the test keeps the documentation honest.
#[test]
fn a_base_without_a_trailing_slash_drops_its_last_segment() {
    let c = client_with_base("https://example.test/api");
    futures_executor::block_on(c.get("v1/things").send()).unwrap();
    assert_eq!(
        sent_uri(&c),
        "https://example.test/v1/things",
        "`/api` with no slash is not a directory: RFC 3986 §5.3 drops the last segment"
    );
}

/// A request with no path at all: the base should reach the transport
/// whole, not collapse into its root.
#[test]
fn an_empty_reference_resolves_to_the_base_itself() {
    let c = client_with_base("https://example.test/api/things");
    futures_executor::block_on(c.get("").send()).unwrap();
    assert_eq!(sent_uri(&c), "https://example.test/api/things");
}

/// A reference's query survives resolution.
#[test]
fn the_query_of_a_relative_reference_survives() {
    let c = client_with_base("https://example.test/api/");
    futures_executor::block_on(c.get("search?q=1&n=2").send()).unwrap();
    assert_eq!(sent_uri(&c), "https://example.test/api/search?q=1&n=2");
}

/// The base itself must be absolute: a relative one has nothing to
/// resolve against. This is a typed error with a nameable source, not a
/// silently ignored setting — the same contract as an unsupported timeout
/// (M3).
#[test]
fn a_relative_base_is_a_typed_error_not_a_silently_ignored_setting() {
    let c = client_with_base("/api/");
    let err = futures_executor::block_on(c.get("v1/things").send())
        .expect_err("a relative base is unfit for use — this must be an error");

    assert_eq!(*err.kind(), ErrorKind::Other, "{err}");
    let src = std::error::Error::source(&err).expect("Error::new always sets a source");
    let bad = src
        .downcast_ref::<InvalidBaseUrl>()
        .expect("the source must name the specific problem, not just be a string");
    assert_eq!(bad.base, "/api/".parse::<http::Uri>().unwrap());
    assert_eq!(bad.requested, "v1/things");
    assert!(
        c.transport().requests().is_empty(),
        "a request with an unfit base must not reach the transport"
    );
}

/// `Response::url()` must name the URL the request actually went out on,
/// not what the caller typed. Otherwise base resolution would be visible
/// to the transport and invisible to the consumer.
#[test]
fn response_url_reports_the_resolved_uri_not_the_relative_one() {
    let c = client_with_base("https://example.test/api/");
    let resp = futures_executor::block_on(c.get("v1/things").send()).unwrap();
    assert_eq!(
        resp.url(),
        &"https://example.test/api/v1/things"
            .parse::<http::Uri>()
            .unwrap()
    );
}

/// `Client::execute` is a public entry point that takes an already-built
/// `http::Request`; the base must apply here too, or the setting only
/// works through `RequestBuilder` and is partial all over again.
///
/// The reference here is `/v1/things`, not `v1/things`, and that's not a
/// choice made by the test: `http::Request::builder().uri("v1/things")`
/// doesn't even build — `http::Uri` can't represent a path-relative
/// reference. Only origin-form and absolute-form are expressible through
/// this entry point, so the base can give such a request a scheme and
/// authority, but not a path. RFC 3986 §5.2.2 would say the exact same
/// thing about any reference with a leading `/`.
#[test]
fn client_execute_resolves_the_base_too_not_only_request_builder() {
    let c = client_with_base("https://example.test/api/");
    let req = http::Request::builder()
        .uri("/v1/things")
        .body(RequestBody::Empty)
        .unwrap();
    futures_executor::block_on(c.execute(req)).unwrap();
    assert_eq!(sent_uri(&c), "https://example.test/v1/things");
}

/// The flip side of the previous test: `http::Uri` can't represent a
/// path-relative reference, so the only way to use one at all is
/// `RequestBuilder`, which resolves the original STRING before parsing.
/// The test pins down that this is exactly the limitation, not "`get`
/// can't do it either".
#[test]
fn a_path_relative_reference_is_expressible_through_the_builder_only() {
    assert!(
        "v1/things".parse::<http::Uri>().is_err(),
        "if http::Uri ever learns this form, resolution could be unified on Uri"
    );
    let c = client_with_base("https://example.test/api/");
    futures_executor::block_on(c.get("v1/things").send()).unwrap();
    assert_eq!(sent_uri(&c), "https://example.test/api/v1/things");
}

// ── internationalised hosts ──────────────────────────────────────────
//
// Not a `base_url` feature, but a `base_url` BUG until this round, which
// is why it is pinned in this file. Measured before the fix:
//
//   client.get("https://münchen.de/x")
//     with no base_url  ->  error: invalid uri character
//     with any base_url ->  ok, https://xn--mnchen-3ya.de/x
//
// Two different code paths, and the difference was never decided by
// anyone: without a base the string went to `http::Uri`, which rejects a
// non-ASCII authority; with one it went through `url::Url`, whose IDNA
// punycoded it. `Location:` on a redirect took the second path for the
// same reason. Both paths now go through `http_ng_proto::uri`, so the
// answer no longer depends on an unrelated setting — which is the whole
// point of the first two tests standing next to each other.

/// The `idn` feature is on by default, so this is what a plain build does.
#[cfg(feature = "idn")]
#[test]
fn an_internationalised_host_resolves_the_same_with_and_without_a_base() {
    let m = MockTransport::new();
    m.push_response(http::Response::builder().status(200).body("").unwrap());
    let no_base = Client::builder(m).build().unwrap();
    futures_executor::block_on(no_base.get("https://münchen.de/x").send()).unwrap();
    assert_eq!(
        sent_uri(&no_base),
        "https://xn--mnchen-3ya.de/x",
        "with no base configured, the host must still be punycoded"
    );

    let with_base = client_with_base("https://example.test/api/");
    futures_executor::block_on(with_base.get("https://münchen.de/x").send()).unwrap();
    assert_eq!(
        sent_uri(&with_base),
        sent_uri(&no_base),
        "the same absolute URL must not depend on whether a base URL is set"
    );
}

/// A relative reference against an internationalised base. The base itself
/// cannot be a U-label — `ClientBuilder::base_url` takes an `http::Uri`,
/// which will not hold one — so the A-label is the only way to express it,
/// and it must survive resolution untouched. That is the idempotence the
/// conversion is built on, seen from the facade.
#[test]
fn a_relative_request_against_an_a_label_base_keeps_the_host_untouched() {
    let c = client_with_base("https://xn--mnchen-3ya.de/api/");
    futures_executor::block_on(c.get("v1/things").send()).unwrap();
    assert_eq!(sent_uri(&c), "https://xn--mnchen-3ya.de/api/v1/things");
}

/// With the feature off there is no `idna` in the build at all, and the
/// answer must say so — with or without a base, and in words that name the
/// A-label as the way through. `http::Uri`'s own "invalid uri character"
/// is exactly the message this replaces.
#[cfg(not(feature = "idn"))]
#[test]
fn without_the_idn_feature_a_u_label_is_a_typed_error_that_names_the_way_out() {
    for (label, client) in [
        ("no base", {
            let m = MockTransport::new();
            m.push_response(http::Response::builder().status(200).body("").unwrap());
            Client::builder(m).build().unwrap()
        }),
        ("with a base", client_with_base("https://example.test/api/")),
    ] {
        let err = futures_executor::block_on(client.get("https://münchen.de/x").send())
            .expect_err("no `idn` feature, so this cannot be sent");
        assert_eq!(*err.kind(), ErrorKind::Other, "{label}: {err}");
        let src = std::error::Error::source(&err).expect("Error::new always sets a source");
        let named = src
            .downcast_ref::<UriError>()
            .unwrap_or_else(|| panic!("{label}: the source must be a `UriError`, got {src}"));
        assert!(
            matches!(named, UriError::NonAsciiHost { host } if host == "münchen.de"),
            "{label}: the error must name the host: {named:?}"
        );
        assert!(
            named.to_string().contains("xn--"),
            "{label}: the error must name the A-label form as the way through: {named}"
        );
        assert!(
            client.transport().requests().is_empty(),
            "{label}: nothing may be sent"
        );
    }
}

/// The A-label works whichever way `idn` is set — an A-label is ASCII, and
/// nothing about it should depend on the feature. This is what makes the
/// advice in the error above true rather than merely polite.
#[test]
fn an_a_label_host_is_sent_as_written_whichever_way_idn_is_set() {
    let m = MockTransport::new();
    m.push_response(http::Response::builder().status(200).body("").unwrap());
    let c = Client::builder(m).build().unwrap();
    futures_executor::block_on(c.get("https://xn--mnchen-3ya.de/x").send()).unwrap();
    assert_eq!(sent_uri(&c), "https://xn--mnchen-3ya.de/x");
}
