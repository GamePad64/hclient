//! `supports_svcb()` and `lookup_svcb()` — a capability and an answer, and
//! why they are two methods.
//!
//! An empty SVCB stream on its own is ambiguous: it can mean "this resolver
//! cannot do SVCB at all" or "the resolver asked and got zero records".
//! `Resolve` keeps the two apart by carrying the distinction in
//! `supports_svcb()` instead of in the stream — which is only true if the
//! two defaults agree, and only useful if a caller can actually tell the
//! two situations apart without inspecting the stream. Both of those are
//! what this file checks.

use assert_matches::assert_matches;
use futures_core::Stream;
use futures_util::StreamExt;
use hclient_core::Error;
use hclient_dns::{Resolve, ResolvedAddr, SvcbEndpoint};
use std::pin::pin;
use std::task::{Context, Poll, Waker};

fn drain(stream: impl Stream<Item = Result<SvcbEndpoint, Error>>) -> Vec<SvcbEndpoint> {
    futures_executor::block_on(stream.collect::<Vec<_>>())
        .into_iter()
        .collect::<Result<_, _>>()
        .expect("no test resolver here fails")
}

fn endpoint(target: &str) -> SvcbEndpoint {
    SvcbEndpoint {
        priority: 1,
        target: target.to_owned(),
        alpn: vec![b"h3".to_vec()],
        port: Some(443),
        ipv4hint: vec![],
        ipv6hint: vec![],
        ech_config_list: None,
    }
}

fn no_addresses() -> impl Stream<Item = Result<ResolvedAddr, Error>> {
    futures_util::stream::empty()
}

/// Overrides neither SVCB method: a `getaddrinfo` wrapper, `wasi:http`, an
/// embedded resolver. It cannot ask.
#[derive(Debug)]
struct CannotAsk;
impl Resolve for CannotAsk {
    type Svcb<'a>
        = hclient_dns::NoSvcb
    where
        Self: 'a;

    fn lookup_svcb<'a>(&'a self, _name: &str) -> Self::Svcb<'a> {
        hclient_dns::NoSvcb::new()
    }

    type Ipv4<'a>
        =
        std::pin::Pin<Box<dyn futures_core::Stream<Item = Result<ResolvedAddr, Error>> + Send + 'a>>
    where
        Self: 'a;

    fn lookup_ipv4<'a>(&'a self, _: &str) -> Self::Ipv4<'a> {
        Box::pin(no_addresses())
    }
    type Ipv6<'a>
        =
        std::pin::Pin<Box<dyn futures_core::Stream<Item = Result<ResolvedAddr, Error>> + Send + 'a>>
    where
        Self: 'a;

    fn lookup_ipv6<'a>(&'a self, _: &str) -> Self::Ipv6<'a> {
        Box::pin(no_addresses())
    }
}

/// Overrides both, and the real query came back with nothing. Its stream is
/// byte-for-byte the same shape as `CannotAsk`'s; only `supports_svcb()`
/// separates them.
#[derive(Debug)]
struct AskedFoundNothing;
impl Resolve for AskedFoundNothing {
    type Ipv4<'a>
        =
        std::pin::Pin<Box<dyn futures_core::Stream<Item = Result<ResolvedAddr, Error>> + Send + 'a>>
    where
        Self: 'a;

    fn lookup_ipv4<'a>(&'a self, _: &str) -> Self::Ipv4<'a> {
        Box::pin(no_addresses())
    }
    type Ipv6<'a>
        =
        std::pin::Pin<Box<dyn futures_core::Stream<Item = Result<ResolvedAddr, Error>> + Send + 'a>>
    where
        Self: 'a;

    fn lookup_ipv6<'a>(&'a self, _: &str) -> Self::Ipv6<'a> {
        Box::pin(no_addresses())
    }
    fn supports_svcb(&self) -> bool {
        true
    }
    type Svcb<'a>
        =
        std::pin::Pin<Box<dyn futures_core::Stream<Item = Result<SvcbEndpoint, Error>> + Send + 'a>>
    where
        Self: 'a;

    fn lookup_svcb<'a>(&'a self, _: &str) -> Self::Svcb<'a> {
        Box::pin(futures_util::stream::empty())
    }
}

/// Overrides both, and found records.
#[derive(Debug)]
struct AskedFoundRecords;
impl Resolve for AskedFoundRecords {
    type Ipv4<'a>
        =
        std::pin::Pin<Box<dyn futures_core::Stream<Item = Result<ResolvedAddr, Error>> + Send + 'a>>
    where
        Self: 'a;

    fn lookup_ipv4<'a>(&'a self, _: &str) -> Self::Ipv4<'a> {
        Box::pin(no_addresses())
    }
    type Ipv6<'a>
        =
        std::pin::Pin<Box<dyn futures_core::Stream<Item = Result<ResolvedAddr, Error>> + Send + 'a>>
    where
        Self: 'a;

    fn lookup_ipv6<'a>(&'a self, _: &str) -> Self::Ipv6<'a> {
        Box::pin(no_addresses())
    }
    fn supports_svcb(&self) -> bool {
        true
    }
    type Svcb<'a>
        =
        std::pin::Pin<Box<dyn futures_core::Stream<Item = Result<SvcbEndpoint, Error>> + Send + 'a>>
    where
        Self: 'a;

    fn lookup_svcb<'a>(&'a self, _: &str) -> Self::Svcb<'a> {
        Box::pin(futures_util::stream::iter(vec![Ok(endpoint(
            "svc.example",
        ))]))
    }
}

/// The defect `supports_svcb`'s doc comment warns about, written down so
/// its consequence is visible: real records behind a capability that still
/// reports itself absent.
#[derive(Debug)]
struct OverrodeOnlyTheLookup;
impl Resolve for OverrodeOnlyTheLookup {
    type Ipv4<'a>
        =
        std::pin::Pin<Box<dyn futures_core::Stream<Item = Result<ResolvedAddr, Error>> + Send + 'a>>
    where
        Self: 'a;

    fn lookup_ipv4<'a>(&'a self, _: &str) -> Self::Ipv4<'a> {
        Box::pin(no_addresses())
    }
    type Ipv6<'a>
        =
        std::pin::Pin<Box<dyn futures_core::Stream<Item = Result<ResolvedAddr, Error>> + Send + 'a>>
    where
        Self: 'a;

    fn lookup_ipv6<'a>(&'a self, _: &str) -> Self::Ipv6<'a> {
        Box::pin(no_addresses())
    }
    // `supports_svcb` deliberately left at the default.
    type Svcb<'a>
        = std::pin::Pin<Box<dyn futures_core::Stream<Item = Result<SvcbEndpoint, Error>> + 'a>>
    where
        Self: 'a;

    fn lookup_svcb<'a>(&'a self, _: &str) -> Self::Svcb<'a> {
        Box::pin(futures_util::stream::iter(vec![Ok(endpoint(
            "unreachable.example",
        ))]))
    }
}

/// What a caller — ECH configuration, h3 discovery — actually needs to end
/// up with. Three outcomes, not two.
#[derive(Debug, PartialEq, Eq)]
enum SvcbAnswer {
    /// The resolver cannot ask. Not a negative answer about the name: no
    /// answer at all, and a reason to fall back rather than to conclude
    /// the origin offers no ECH.
    CapabilityAbsent,
    /// It asked. The name has no HTTPS/SVCB records.
    NoRecords,
    Records(Vec<SvcbEndpoint>),
}

/// The caller the trait is written for: it asks the capability question
/// FIRST, and only then reads the stream. Nothing here inspects the
/// stream's emptiness to decide whether the resolver was able to ask —
/// that is exactly the inference `supports_svcb()` exists to make
/// unnecessary.
fn ask<R: Resolve>(resolver: &R, name: &str) -> SvcbAnswer {
    if !resolver.supports_svcb() {
        return SvcbAnswer::CapabilityAbsent;
    }
    let records = drain(resolver.lookup_svcb(name));
    if records.is_empty() {
        SvcbAnswer::NoRecords
    } else {
        SvcbAnswer::Records(records)
    }
}

/// Both defaults, checked together: `supports_svcb()` says the capability
/// is absent AND `lookup_svcb` yields nothing. Either one alone would let
/// the pair drift into the state the doc comment forbids — a `true`
/// default over an empty stream would be a resolver lying about a
/// capability it never implemented.
#[test]
fn the_two_defaults_agree_that_the_capability_is_absent() {
    assert!(
        !CannotAsk.supports_svcb(),
        "the default must say \"cannot do this\" outright, not stay silent and leave the \
         caller to infer it from an empty stream"
    );
    assert_eq!(
        drain(CannotAsk.lookup_svcb("example.com")),
        Vec::<SvcbEndpoint>::new(),
        "and the default lookup must yield nothing, or getaddrinfo, wasi:http and \
         embedded resolvers could not implement the trait at all"
    );
}

/// The point of the whole split: the two situations are INDISTINGUISHABLE
/// in the stream — deliberately so — and a caller still tells them apart,
/// via `supports_svcb()` alone.
#[test]
fn cannot_ask_and_asked_found_nothing_differ_only_through_supports_svcb() {
    let cannot = drain(CannotAsk.lookup_svcb("example.com"));
    let nothing = drain(AskedFoundNothing.lookup_svcb("example.com"));
    assert_eq!(
        cannot, nothing,
        "the streams must be identical: the distinction is not carried here, and a caller \
         that tried to read it off the stream would be reading a coincidence"
    );

    assert_eq!(
        ask(&CannotAsk, "example.com"),
        SvcbAnswer::CapabilityAbsent,
        "a resolver that cannot query SVCB must not be reported as having answered"
    );
    assert_eq!(
        ask(&AskedFoundNothing, "example.com"),
        SvcbAnswer::NoRecords,
        "a resolver that asked and found nothing gave a real, negative answer — conflating \
         it with \"cannot ask\" would discard the one piece of information it produced"
    );
}

/// Overriding only `lookup_svcb` is not caught by the compiler, and this is
/// what it costs: records that exist and are never read, because the
/// capability the caller checks still reports itself absent. The test is
/// here so the hazard is a recorded, reproducible consequence rather than
/// a warning in a doc comment.
#[test]
fn overriding_only_the_lookup_hides_real_records_from_a_capability_checking_caller() {
    let records = drain(OverrodeOnlyTheLookup.lookup_svcb("example.com"));
    assert_eq!(
        records.len(),
        1,
        "precondition: this resolver does produce a record"
    );
    assert!(
        !OverrodeOnlyTheLookup.supports_svcb(),
        "precondition: and it left the capability at the default"
    );
    assert_eq!(
        ask(&OverrodeOnlyTheLookup, "example.com"),
        SvcbAnswer::CapabilityAbsent,
        "the record above is unreachable through the documented calling convention — which \
         is why `supports_svcb` and `lookup_svcb` must be overridden together"
    );
}

/// A resolver that can do SVCB reports the capability AND hands over the
/// records — both halves, since either alone is one of the two failure
/// modes above.
#[test]
fn a_real_svcb_resolver_reports_the_capability_and_the_records_together() {
    assert!(AskedFoundRecords.supports_svcb());
    assert_eq!(
        ask(&AskedFoundRecords, "example.com"),
        SvcbAnswer::Records(vec![endpoint("svc.example")]),
        "the records must arrive intact through the seam, field for field"
    );
}

/// The default stream must END, immediately, on the first poll — not park
/// on a waker it will never call. `block_on(collect())` cannot tell those
/// apart: a stream that never completes hangs the test rather than failing
/// it. Polling by hand can. `size_hint` is checked in the same place
/// because it is the machine-readable half of the same statement.
#[test]
fn the_default_svcb_stream_ends_on_the_first_poll_rather_than_pending() {
    let mut stream = pin!(CannotAsk.lookup_svcb("example.com"));
    assert_eq!(
        stream.size_hint(),
        (0, Some(0)),
        "the default must declare itself exactly empty, not \"length unknown\""
    );
    let mut cx = Context::from_waker(Waker::noop());
    assert_matches!(
        stream.as_mut().poll_next(&mut cx),
        Poll::Ready(None),
        "a resolver without SVCB must answer \"nothing\" at once; pending forever would \
         stall every caller that awaits the default"
    );
}
