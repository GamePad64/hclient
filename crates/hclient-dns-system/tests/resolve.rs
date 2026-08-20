//! `SystemDns` through the door a consumer actually uses — the `Resolve`
//! trait, its three streams, and `hclient_core::Error`.
//!
//! **Why any of this is out here rather than in `src`.** The unit tests
//! inside the crate reach the seams directly: `svcb::endpoints_from_answer`
//! over byte vectors, `sys::classify_written` over lengths. What they
//! cannot see is whether those seams are still wired to the trait — a
//! `lookup_svcb` that classified every failure as an empty stream would
//! leave every one of them green. These start from `SystemDns::new` and
//! assert on what a caller receives.
//!
//! **Nothing here needs a name server.** Two of the cases hand
//! `getaddrinfo` an address literal, which `std::net::ToSocketAddrs`
//! answers without a query; the rest are rejected by this crate's own name
//! guard before any resolver is asked. That is deliberate: a test that
//! needs outbound DNS goes red for reasons unconnected to this code, and
//! the one test in this crate that genuinely needs it is `#[ignore]`d in
//! `src/lib.rs`.

use assert_matches::assert_matches;
use futures_util::StreamExt;
use hclient_core::{Error, ErrorKind};
use hclient_dns::{Resolve, ResolvedAddr};
use hclient_dns_system::SystemDns;
use hclient_rt::{Blocking, Cancelled};
use rstest::rstest;
use std::error::Error as _;
use std::net::IpAddr;

/// Runs the work where it stands. `SystemDns` needs a `Blocking`
/// capability and none of these tests are about the pool, so the smallest
/// honest one is used: it really runs `f`, and it never cancels.
struct Inline;

impl Blocking for Inline {
    async fn run<T: Send + 'static, F: FnOnce() -> T + Send + 'static>(
        &self,
        f: F,
    ) -> Result<T, Cancelled> {
        Ok(f())
    }
}

fn drain<S, T>(stream: S) -> Vec<Result<T, Error>>
where
    S: futures_core::Stream<Item = Result<T, Error>>,
{
    futures_executor::block_on(stream.collect())
}

fn addresses(items: Vec<Result<ResolvedAddr, Error>>) -> Vec<IpAddr> {
    items
        .into_iter()
        .map(|item| {
            item.expect("an address literal cannot fail to resolve")
                .addr
        })
        .collect()
}

/// The family filter, proved in both directions and without a resolver.
///
/// `("127.0.0.1", 0).to_socket_addrs()` returns the literal itself, so each
/// case knows the exact single address `getaddrinfo` produced — which is
/// what makes "the other stream is empty" an assertion rather than a
/// coincidence of the machine. The existing `localhost` test in `src`
/// cannot make it: on a host where `localhost` is v4-only, its v6 half
/// passes by having nothing to check.
#[rstest]
#[case::v4_literal("127.0.0.1")]
#[case::v6_literal("::1")]
fn an_address_literal_reaches_its_own_family_stream_and_no_other(#[case] literal: &str) {
    let resolver = SystemDns::new(Inline);
    let expected: IpAddr = literal.parse().expect("the case is a literal");

    let v4 = addresses(drain(resolver.lookup_ipv4(literal)));
    let v6 = addresses(drain(resolver.lookup_ipv6(literal)));

    if expected.is_ipv4() {
        assert_eq!(v4, vec![expected]);
        assert_eq!(
            v6,
            Vec::<IpAddr>::new(),
            "a v4 address in the v6 stream would be handed to a connector that opens an \
             AF_INET6 socket for it"
        );
    } else {
        assert_eq!(v6, vec![expected]);
        assert_eq!(v4, Vec::<IpAddr>::new());
    }
}

/// `getaddrinfo` returns `sockaddr`s and no TTLs — there is nowhere in its
/// result for one. `ResolvedAddr::ttl` is therefore `None` from this
/// resolver always, and a `Some` here would be an invented number a cache
/// would then honour.
#[test]
fn an_address_from_getaddrinfo_carries_no_ttl_because_there_is_none_to_carry() {
    let items = drain(SystemDns::new(Inline).lookup_ipv4("127.0.0.1"));
    let addr = items
        .into_iter()
        .next()
        .expect("the literal resolves")
        .expect("and does not fail");
    assert_eq!(addr.ttl, None);
}

/// A name `getaddrinfo` cannot even be asked about, and what a caller can
/// still learn from the refusal.
///
/// An interior NUL is rejected by `std` before any resolver is involved, so
/// this is deterministic on every machine. Two things have to survive to
/// the caller: the category (`Resolve`, so it is distinguishable from
/// cancellation without a downcast) and the `io::Error` itself, which is
/// the only place the errno lives. A `#[source]` dropped from
/// `ResolveFailed` leaves the message identical and this chain empty.
#[test]
fn a_resolve_failure_keeps_the_name_in_its_message_and_the_io_error_in_its_source() {
    let items = drain(SystemDns::new(Inline).lookup_ipv4("bad\0name"));
    assert_eq!(
        items.len(),
        1,
        "one failure, and nothing that looks like an address alongside it"
    );
    let error = items
        .into_iter()
        .next()
        .expect("one item")
        .expect_err("a NUL is not a host name");

    assert_eq!(
        error.kind(),
        &ErrorKind::Resolve,
        "the name failed on its own merits — this is not cancellation and not an opaque \
         backend error"
    );

    let failed = error
        .source()
        .expect("hclient_core::Error chains its cause");
    assert!(
        failed.to_string().contains("bad\0name"),
        "the message must say which name failed: `{failed}`"
    );

    let cause = failed
        .source()
        .expect("`ResolveFailed` wraps the io::Error rather than only quoting it");
    let io = cause
        .downcast_ref::<std::io::Error>()
        .expect("a caller reaches the errno by downcast, not by parsing a message");
    assert_eq!(io.kind(), std::io::ErrorKind::InvalidInput);
}

/// The pair `Resolve::supports_svcb` exists to keep honest, checked against
/// behaviour rather than against a second copy of a `#[cfg]`.
///
/// `sys::unsupported::lookup` ignores its argument and returns an empty
/// result for every name, so it CANNOT produce this error; a real backend
/// rejects an over-long name before it reaches the FFI. So a build that
/// claims `supports_svcb()` and answers this name with silence is a build
/// whose capability is a lie, and a build that disclaims SVCB and answers
/// it with an error has a backend it is not admitting to. Each branch
/// asserts; neither is a `return` dressed up as a pass.
#[test]
fn supports_svcb_and_lookup_svcb_agree_about_whether_a_backend_exists() {
    // 256 octets: one past RFC 1035 §2.3.4's limit on the wire form of a
    // name, and short enough that nothing else objects first.
    let too_long = "a".repeat(256);
    let resolver = SystemDns::new(Inline);
    let items = drain(resolver.lookup_svcb(&too_long));

    if resolver.supports_svcb() {
        assert_eq!(items.len(), 1, "a backend that cannot ask must say so");
        let error = items
            .into_iter()
            .next()
            .expect("one item")
            .expect_err("256 octets cannot be a DNS name");
        assert_eq!(
            error.kind(),
            &ErrorKind::Resolve,
            "an unusable name is a failed lookup, not an absent capability"
        );
        assert!(
            error
                .source()
                .expect("chained")
                .to_string()
                .contains(&too_long),
            "the name that could not be asked about has to be in the message"
        );
    } else {
        assert!(
            items.is_empty(),
            "with no backend there is nothing to fail: an absent capability is an empty \
             stream, never an error, or every caller on this target learns its DNS is broken"
        );
    }
}

/// The other half of `lookup_svcb`'s contract, at the seam a caller sees:
/// the pool going away is neither an empty stream nor a DNS failure.
///
/// `src/lib.rs` asserts this too. It is repeated here because the two
/// tests can fail for different reasons: that one would still pass if
/// `lookup_svcb` stopped being reachable through `Resolve` at all, and
/// this one goes through the trait.
#[test]
fn a_cancelled_svcb_lookup_is_reported_as_cancelled_through_the_trait() {
    struct AlwaysCancelled;
    impl Blocking for AlwaysCancelled {
        async fn run<T: Send + 'static, F: FnOnce() -> T + Send + 'static>(
            &self,
            _f: F,
        ) -> Result<T, Cancelled> {
            Err(Cancelled)
        }
    }

    let items = drain(SystemDns::new(AlwaysCancelled).lookup_svcb("example.com"));
    assert_eq!(items.len(), 1, "the pool going away is not silence");
    let error = items
        .into_iter()
        .next()
        .expect("one item")
        .expect_err("must be an error");
    assert_matches!(error.kind(), ErrorKind::Cancelled);
    assert!(error.is_cancelled());
}
