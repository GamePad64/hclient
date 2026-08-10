//! The bootstrap decision, as the constructors actually enforce it.
//!
//! `docs/v03-design.md` §W3 says the bootstrap is the design problem, and
//! that "the wrong choice is expensive to walk back because it shows in
//! the constructor". These tests are what makes the constructor a
//! statement rather than a name: `pinned` refuses a name, `bootstrapped`
//! refuses a literal, and neither accepts an endpoint whose bytes would
//! cross a network in the clear.

mod support;

use assert_matches::assert_matches;
use http_ng_dns::IpLiteralOnly;
use http_ng_dns_doh::{Doh, EndpointError};
use http_ng_native::Native;
use http_ng_rt_tokio::Tokio;
use http_ng_tls::NoTls;
use rstest::rstest;

fn transport() -> Native<Tokio, NoTls, IpLiteralOnly> {
    Native::new(Tokio, NoTls, IpLiteralOnly)
}

fn uri(s: &str) -> http::Uri {
    s.parse().expect("a valid uri")
}

/// Shape 1 of §W3: no bootstrap at all, because there is no name.
#[rstest]
#[case::v4("https://1.1.1.1/dns-query")]
#[case::v6("https://[2606:4700:4700::1111]/dns-query")]
#[case::with_port("https://9.9.9.9:8443/dns-query")]
fn pinned_accepts_an_ip_literal(#[case] endpoint: &str) {
    let doh = Doh::pinned(transport(), uri(endpoint)).expect("an IP-literal endpoint");
    assert_eq!(doh.endpoint(), &uri(endpoint), "the URI was not kept whole");
}

/// The partition, in the direction that matters most: `pinned` is the
/// no-bootstrap constructor, so a name in it is a caller who thinks they
/// have no bootstrap and does.
#[test]
fn pinned_refuses_a_name_and_names_the_other_constructor() {
    let e = Doh::pinned(transport(), uri("https://dns.example/dns-query"))
        .expect_err("a name is not an IP literal");
    assert_matches!(&e, EndpointError::NotAnIpLiteral { host } if host == "dns.example");
    assert!(
        e.to_string().contains("Doh::bootstrapped"),
        "the error does not say what to do instead: {e}"
    );
}

/// Shapes 2 and 3 of §W3, which this crate cannot tell apart and does not
/// need to: both are "the inner transport's resolver knows how".
#[test]
fn bootstrapped_accepts_a_name() {
    Doh::bootstrapped(transport(), uri("https://dns.example/dns-query"))
        .expect("a name is what this constructor is for");
}

/// The other direction. A `bootstrapped` that bootstraps nothing would
/// work perfectly and would be a lie in the source.
#[test]
fn bootstrapped_refuses_an_ip_literal() {
    let e = Doh::bootstrapped(transport(), uri("https://1.1.1.1/dns-query"))
        .expect_err("a literal needs no bootstrap");
    assert_matches!(&e, EndpointError::IsAnIpLiteral { host } if host == "1.1.1.1");
    assert!(e.to_string().contains("Doh::pinned"), "{e}");
}

/// A URI with a path and no authority. Both constructors refuse it, and
/// neither can do anything else: there is no host to pin and none to
/// bootstrap.
///
/// The obvious other spelling, `https:///dns-query`, never reaches this
/// crate — `http::Uri` refuses to parse it at all (`InvalidUri
/// (InvalidFormat)`, measured while writing this test). So `NoHost` is
/// reachable exactly for authority-less URIs, and this is the case that
/// reaches it.
#[test]
fn an_endpoint_with_no_host_is_refused_by_both_constructors() {
    assert_matches!(
        Doh::pinned(transport(), uri("/dns-query")),
        Err(EndpointError::NoHost { .. })
    );
    assert_matches!(
        Doh::bootstrapped(transport(), uri("/dns-query")),
        Err(EndpointError::NoHost { .. })
    );
}

/// Cleartext DNS to a host that is not this machine is what RFC 8484
/// exists to prevent, and a resolver that accepted it would give a caller
/// the confidentiality guarantee of plain DNS under the name DoH.
#[rstest]
#[case::literal("http://1.1.1.1/dns-query")]
#[case::name("http://dns.example/dns-query")]
fn a_cleartext_endpoint_off_this_machine_is_refused(#[case] endpoint: &str) {
    let e = Doh::pinned(transport(), uri(endpoint))
        .err()
        .or_else(|| Doh::bootstrapped(transport(), uri(endpoint)).err())
        .expect("one of the two constructors applies, and both must refuse this");
    assert_matches!(
        e,
        EndpointError::NotConfidential { .. } | EndpointError::NotAnIpLiteral { .. }
    );
    // Whichever constructor applies must refuse it for the confidentiality
    // reason, so check the applicable one directly too.
    let applicable = if endpoint.contains("1.1.1.1") {
        Doh::pinned(transport(), uri(endpoint)).err()
    } else {
        Doh::bootstrapped(transport(), uri(endpoint)).err()
    };
    assert_matches!(applicable, Some(EndpointError::NotConfidential { .. }));
}

/// The loopback exception, which exists for a local DoH proxy and not for
/// this test suite — though this test suite is what it lets be cheap.
#[rstest]
#[case::v4("http://127.0.0.1:5353/dns-query")]
#[case::v4_other("http://127.9.9.9/dns-query")]
#[case::v6("http://[::1]:5353/dns-query")]
fn a_cleartext_endpoint_on_loopback_is_accepted(#[case] endpoint: &str) {
    Doh::pinned(transport(), uri(endpoint)).expect("loopback never leaves the machine");
}
