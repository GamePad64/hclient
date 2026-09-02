//! `supports` and `lookup` — a capability and an answer, and why they are
//! two methods rather than one.
//!
//! An empty SVCB stream on its own is ambiguous: it can mean "this resolver
//! cannot do SVCB at all" or "the resolver asked and got zero records".
//! `Resolve` keeps the two apart by carrying the distinction in
//! `supports` instead of in the stream — which is only useful if a caller
//! can actually tell the two situations apart without inspecting the
//! stream. That is what this file checks.
//!
//! The two methods take the same type number, so the pair is now checkable
//! for every type rather than for SVCB alone: a resolver that answers
//! `supports(rtype) == false` and a resolver that has nothing to say about
//! `rtype` produce the same stream, and only the capability separates
//! them.

use futures_core::Stream;
use futures_util::StreamExt;
use hclient_core::Error;
use hclient_dns::{RData, Record, Resolve, SvcbEndpoint, rtype};
use std::assert_matches;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

/// The HTTPS records a stream carried, unwrapped.
///
/// The seam hands back `Record`s now, so this pulls the `SvcbEndpoint`
/// out of each — and asserts on the way that the answer is of the type
/// that was asked for, which the three-method shape guaranteed by
/// construction and one method has to check.
fn drain(stream: impl Stream<Item = Result<Record, Error>>) -> Vec<SvcbEndpoint> {
    futures_executor::block_on(stream.collect::<Vec<_>>())
        .into_iter()
        .map(|r| match r.expect("no test resolver here fails").rdata {
            RData::Https(e) => e,
            other => panic!("an HTTPS query answered {other:?}"),
        })
        .collect()
}

fn endpoint(target: &str) -> SvcbEndpoint {
    SvcbEndpoint::new(1, target.to_owned())
        .alpn(vec![b"h3".to_vec()])
        .port(Some(443))
}

fn no_addresses() -> impl Stream<Item = Result<Record, Error>> {
    futures_util::stream::empty()
}

/// Overrides neither SVCB method: a `getaddrinfo` wrapper, `wasi:http`, an
/// embedded resolver. It cannot ask.
#[derive(Debug)]
struct CannotAsk;
impl Resolve for CannotAsk {
    type Records<'a>
        = std::pin::Pin<Box<dyn futures_core::Stream<Item = Result<Record, Error>> + Send + 'a>>
    where
        Self: 'a;

    fn supports(&self, rtype: u16) -> bool {
        matches!(rtype, rtype::A | rtype::AAAA)
    }

    fn lookup<'a>(&'a self, name: &str, rtype: u16) -> Self::Records<'a> {
        let _ = name;
        match rtype {
            rtype::A => Box::pin(no_addresses()),
            rtype::AAAA => Box::pin(no_addresses()),
            _ => Box::pin(futures_util::stream::empty()),
        }
    }
}

/// Overrides both, and the real query came back with nothing. Its stream is
/// byte-for-byte the same shape as `CannotAsk`'s; only `supports`
/// separates them.
#[derive(Debug)]
struct AskedFoundNothing;
impl Resolve for AskedFoundNothing {
    type Records<'a>
        = std::pin::Pin<Box<dyn futures_core::Stream<Item = Result<Record, Error>> + Send + 'a>>
    where
        Self: 'a;

    fn supports(&self, rtype: u16) -> bool {
        matches!(rtype, rtype::A | rtype::AAAA | rtype::HTTPS)
    }

    fn lookup<'a>(&'a self, name: &str, rtype: u16) -> Self::Records<'a> {
        let _ = name;
        match rtype {
            rtype::A => Box::pin(no_addresses()),
            rtype::AAAA => Box::pin(no_addresses()),
            rtype::HTTPS => Box::pin(futures_util::stream::empty()),
            _ => Box::pin(futures_util::stream::empty()),
        }
    }
}

/// Overrides both, and found records.
#[derive(Debug)]
struct AskedFoundRecords;
impl Resolve for AskedFoundRecords {
    type Records<'a>
        = std::pin::Pin<Box<dyn futures_core::Stream<Item = Result<Record, Error>> + Send + 'a>>
    where
        Self: 'a;

    fn supports(&self, rtype: u16) -> bool {
        matches!(rtype, rtype::A | rtype::AAAA | rtype::HTTPS)
    }

    fn lookup<'a>(&'a self, name: &str, rtype: u16) -> Self::Records<'a> {
        let _ = name;
        match rtype {
            rtype::A => Box::pin(no_addresses()),
            rtype::AAAA => Box::pin(no_addresses()),
            rtype::HTTPS => Box::pin(futures_util::stream::iter(vec![Ok(Record::new(
                RData::Https(endpoint("svc.example")),
            ))])),
            _ => Box::pin(futures_util::stream::empty()),
        }
    }
}

/// The defect `supports`'s doc comment warns about, written down so
/// its consequence is visible: real records behind a capability that still
/// reports itself absent.
#[derive(Debug)]
struct OverrodeOnlyTheLookup;
impl Resolve for OverrodeOnlyTheLookup {
    type Records<'a>
        = std::pin::Pin<Box<dyn futures_core::Stream<Item = Result<Record, Error>> + Send + 'a>>
    where
        Self: 'a;

    fn supports(&self, rtype: u16) -> bool {
        matches!(rtype, rtype::A | rtype::AAAA)
    }

    fn lookup<'a>(&'a self, name: &str, rtype: u16) -> Self::Records<'a> {
        let _ = name;
        match rtype {
            rtype::A => Box::pin(no_addresses()),
            rtype::AAAA => Box::pin(no_addresses()),
            rtype::HTTPS => Box::pin(futures_util::stream::iter(vec![Ok(Record::new(
                RData::Https(endpoint("unreachable.example")),
            ))])),
            _ => Box::pin(futures_util::stream::empty()),
        }
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
/// that is exactly the inference `supports` exists to make
/// unnecessary.
fn ask<R: Resolve>(resolver: &R, name: &str) -> SvcbAnswer {
    if !resolver.supports(rtype::HTTPS) {
        return SvcbAnswer::CapabilityAbsent;
    }
    let records = drain(resolver.lookup(name, rtype::HTTPS));
    if records.is_empty() {
        SvcbAnswer::NoRecords
    } else {
        SvcbAnswer::Records(records)
    }
}

/// Both defaults, checked together: `supports` says the capability
/// is absent AND `lookup` yields nothing. Either one alone would let
/// the pair drift into the state the doc comment forbids — a `true`
/// default over an empty stream would be a resolver lying about a
/// capability it never implemented.
#[test]
fn the_two_defaults_agree_that_the_capability_is_absent() {
    assert!(
        !CannotAsk.supports(rtype::HTTPS),
        "the default must say \"cannot do this\" outright, not stay silent and leave the \
         caller to infer it from an empty stream"
    );
    assert_eq!(
        drain(CannotAsk.lookup("example.com", rtype::HTTPS)),
        Vec::<SvcbEndpoint>::new(),
        "and the default lookup must yield nothing, or getaddrinfo, wasi:http and \
         embedded resolvers could not implement the trait at all"
    );
}

/// The point of the whole split: the two situations are INDISTINGUISHABLE
/// in the stream — deliberately so — and a caller still tells them apart,
/// via `supports` alone.
#[test]
fn cannot_ask_and_asked_found_nothing_differ_only_through_supports() {
    let cannot = drain(CannotAsk.lookup("example.com", rtype::HTTPS));
    let nothing = drain(AskedFoundNothing.lookup("example.com", rtype::HTTPS));
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

/// Overriding only `lookup` is not caught by the compiler, and this is
/// what it costs: records that exist and are never read, because the
/// capability the caller checks still reports itself absent. The test is
/// here so the hazard is a recorded, reproducible consequence rather than
/// a warning in a doc comment.
#[test]
fn overriding_only_the_lookup_hides_real_records_from_a_capability_checking_caller() {
    let records = drain(OverrodeOnlyTheLookup.lookup("example.com", rtype::HTTPS));
    assert_eq!(
        records.len(),
        1,
        "precondition: this resolver does produce a record"
    );
    assert!(
        !OverrodeOnlyTheLookup.supports(rtype::HTTPS),
        "precondition: and it left the capability at the default"
    );
    assert_eq!(
        ask(&OverrodeOnlyTheLookup, "example.com"),
        SvcbAnswer::CapabilityAbsent,
        "the record above is unreachable through the documented calling convention — which \
         is why `supports` must name every type `lookup` really asks about"
    );
}

/// A resolver that can do SVCB reports the capability AND hands over the
/// records — both halves, since either alone is one of the two failure
/// modes above.
#[test]
fn a_real_svcb_resolver_reports_the_capability_and_the_records_together() {
    assert!(AskedFoundRecords.supports(rtype::HTTPS));
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
    let mut stream = pin!(CannotAsk.lookup("example.com", rtype::HTTPS));
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
