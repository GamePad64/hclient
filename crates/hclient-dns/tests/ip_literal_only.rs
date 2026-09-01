//! `IpLiteralOnly` — the resolver that never asks anyone anything.
//!
//! The type is honest in two directions at once, and the two are easy to
//! swap by accident:
//!
//! * a name that is **not** a literal is an **error**, never an empty
//!   stream — an empty stream means "asked, found nothing", a claim this
//!   resolver is not entitled to make;
//! * a literal of the **wrong family** is an **empty stream**, never an
//!   error — A and AAAA are queried in parallel per RFC 8305, and "there is
//!   no AAAA for this v4 literal" is a true answer, not a failure.
//!
//! Both directions live in one case table below, so the two are read
//! together rather than in separate tests that can drift apart.

use std::assert_matches;
use futures_core::Stream;
use futures_util::StreamExt;
use hclient_core::{Error, ErrorKind};
use hclient_dns::{IpLiteralOnly, Resolve, ResolvedAddr};
use rstest::rstest;
use std::net::IpAddr;

type Answer = Result<ResolvedAddr, Error>;

fn drain(stream: impl Stream<Item = Answer>) -> Vec<Answer> {
    futures_executor::block_on(stream.collect())
}

/// What one family's lookup must produce for one input.
#[derive(Debug, Clone, Copy)]
enum Expect {
    /// Exactly this address, and no TTL.
    Addr(&'static str),
    /// Zero items: the other family's literal, which genuinely has no
    /// record of this type.
    NoRecords,
    /// One item, an `ErrorKind::Resolve` naming the input.
    NotALiteral,
}

fn check(got: &[Answer], expect: Expect, input: &str) {
    match expect {
        Expect::Addr(want) => {
            let [Ok(a)] = got else {
                panic!("`{input}` must resolve to exactly one address; got {got:?}")
            };
            assert_eq!(
                a.addr,
                want.parse::<IpAddr>().expect("test data is a valid address"),
                "`{input}` must resolve to itself"
            );
            assert_eq!(
                a.ttl, None,
                "a literal is not a DNS answer and must not carry a TTL that would let \
                 a cache expire it"
            );
        }
        Expect::NoRecords => assert!(
            got.is_empty(),
            "a literal of the other family has no record of this type, and that is an \
             answer rather than a failure — erroring (or inventing an address) here would \
             make every literal connection report something it did not observe; got {got:?}"
        ),
        Expect::NotALiteral => {
            let [Err(e)] = got else {
                panic!("`{input}` is not a literal and must yield one error; got {got:?}")
            };
            assert_eq!(
                *e.kind(), ErrorKind::Resolve,
                "the caller must classify this without substring-matching on Display: {e}"
            );
            assert!(
                e.to_string().contains(input),
                "the error must name what could not be resolved, or the caller cannot tell \
                 which of several names failed: {e}"
            );
        }
    }
}

/// One row per input: what A must answer, and what AAAA must answer.
///
/// The bracketed forms are not a curiosity: `http::Uri::host()` returns an
/// IPv6 literal WITH its brackets, so `[::1]` is the string this trait
/// actually receives in production, and the bare form is the one a caller
/// that already stripped them passes. Both must work, and they sit next to
/// each other here so a change that fixes one and breaks the other cannot
/// pass.
#[rstest]
#[case::v4_literal("192.0.2.1", Expect::Addr("192.0.2.1"), Expect::NoRecords)]
#[case::v4_loopback("127.0.0.1", Expect::Addr("127.0.0.1"), Expect::NoRecords)]
#[case::v6_bare("2001:db8::1", Expect::NoRecords, Expect::Addr("2001:db8::1"))]
#[case::v6_bracketed("[2001:db8::1]", Expect::NoRecords, Expect::Addr("2001:db8::1"))]
#[case::v6_loopback_bare("::1", Expect::NoRecords, Expect::Addr("::1"))]
#[case::v6_loopback_bracketed("[::1]", Expect::NoRecords, Expect::Addr("::1"))]
// An IPv4-mapped IPv6 literal is a v6 literal and nothing else: it is
// answered by AAAA alone, and A stays empty rather than unwrapping the
// embedded 192.0.2.1.
#[case::v4_mapped_into_v6(
    "::ffff:192.0.2.1",
    Expect::NoRecords,
    Expect::Addr("::ffff:192.0.2.1")
)]
#[case::hostname("example.com", Expect::NotALiteral, Expect::NotALiteral)]
#[case::bare_label("localhost", Expect::NotALiteral, Expect::NotALiteral)]
#[case::empty("", Expect::NotALiteral, Expect::NotALiteral)]
// Half a bracket pair is not a literal. Stripping `[` without requiring the
// matching `]` would quietly accept these.
#[case::unclosed_bracket("[::1", Expect::NotALiteral, Expect::NotALiteral)]
#[case::unopened_bracket("::1]", Expect::NotALiteral, Expect::NotALiteral)]
#[case::out_of_range_octet("999.0.0.1", Expect::NotALiteral, Expect::NotALiteral)]
// `Uri::host()` never includes the port, so a string that still carries one
// reached us by another route and is not something to guess at.
#[case::v4_with_port("192.0.2.1:8080", Expect::NotALiteral, Expect::NotALiteral)]
fn each_family_answers_a_literal_or_refuses_a_name(
    #[case] input: &str,
    #[case] expect_v4: Expect,
    #[case] expect_v6: Expect,
) {
    let resolver = IpLiteralOnly;
    check(&drain(resolver.lookup_ipv4(input)), expect_v4, input);
    check(&drain(resolver.lookup_ipv6(input)), expect_v6, input);
}

/// The distinction the whole type rests on, stated once as a rule rather
/// than only as rows of a table: for a name, BOTH families must produce an
/// error. Producing nothing would be the resolver claiming it asked.
#[rstest]
#[case("example.com")]
#[case("no-such-host.invalid")]
#[case("192.0.2.1.")]
fn a_name_is_refused_by_both_families_and_never_answered_with_silence(#[case] name: &str) {
    let resolver = IpLiteralOnly;
    for (family, got) in [
        ("A", drain(resolver.lookup_ipv4(name))),
        ("AAAA", drain(resolver.lookup_ipv6(name))),
    ] {
        let [Err(e)] = got.as_slice() else {
            panic!(
                "{family} for `{name}` must yield an error, not an empty stream: an \
                 empty stream means \"asked, found nothing\" and this resolver never \
                 asked; got {got:?}"
            )
        };
        assert_eq!(*e.kind(), ErrorKind::Resolve, "{family} for `{name}`: {e}");
    }
}

/// A literal is answered by exactly one family, and the other one is empty
/// rather than failing — so Happy Eyeballs gets a definite "no" from one
/// side and an address from the other, and never a spurious failure.
#[rstest]
#[case("192.0.2.1")]
#[case("[2001:db8::1]")]
#[case("::1")]
fn a_literal_is_answered_by_one_family_and_the_other_stays_empty(#[case] literal: &str) {
    let resolver = IpLiteralOnly;
    let v4 = drain(resolver.lookup_ipv4(literal));
    let v6 = drain(resolver.lookup_ipv6(literal));

    let answered: Vec<_> = [&v4, &v6].into_iter().filter(|s| !s.is_empty()).collect();
    assert_eq!(
        answered.len(),
        1,
        "exactly one family answers `{literal}`; got A={v4:?} AAAA={v6:?}"
    );
    assert_matches!(
        answered[0].as_slice(),
        [Ok(_)],
        "the family that answers must answer with an address, not an error"
    );
}

/// The doc comment's closing claim, both halves of it. Half alone would be
/// the failure mode this crate keeps legislating against: `supports_svcb()`
/// left at `false` while `lookup_svcb` returns something (records nobody
/// looks at), or the capability announced by a resolver that cannot query
/// DNS at all.
#[test]
fn svcb_reads_as_unavailable_because_this_resolver_cannot_query_dns_at_all() {
    assert!(
        !IpLiteralOnly.supports_svcb(),
        "a resolver that never asks anyone anything must not claim the SVCB capability, \
         or ECH and h3 discovery would read as available and silently find nothing"
    );
    let got: Vec<_> = futures_executor::block_on(IpLiteralOnly.lookup_svcb("192.0.2.1").collect());
    assert!(
        got.is_empty(),
        "and it must inherit the empty default rather than inventing records: {got:?}"
    );
}
