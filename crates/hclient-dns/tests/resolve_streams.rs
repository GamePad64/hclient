//! What `Resolve` promises by returning a `Stream` per family rather than
//! one `Future<Output = Vec<SocketAddr>>`.
//!
//! Both promises are made in the trait's own doc comments and are invisible
//! to a test that only calls `.collect()`: collecting hides whether the
//! first address was available before the last one existed, and it hides
//! whether one family's stream had to be driven for the other to finish.
//! The tests below refuse to collect for exactly that reason.

use futures_core::Stream;
use futures_util::StreamExt;
use hclient_core::{Error, ErrorKind};
use hclient_dns::{RData, Record, Resolve, rtype};
use std::assert_matches;
use std::cell::Cell;
use std::error::Error as StdError;
use std::net::{IpAddr, Ipv4Addr};
use std::pin::{Pin, pin};
use std::rc::Rc;
use std::task::{Context, Poll};

#[derive(Debug, thiserror::Error)]
#[error("upstream {upstream} did not answer")]
struct UpstreamFailed {
    upstream: &'static str,
}

fn v4(last_octet: u8) -> Record {
    Record::new(RData::A(Ipv4Addr::new(10, 0, 0, last_octet)))
}

/// Produces its two addresses only when polled, and counts how many it has
/// handed out — so a test can tell "the caller has address one" from "the
/// resolver has finished".
#[derive(Debug)]
struct AddressesOnDemand {
    produced: Rc<Cell<usize>>,
}

impl Stream for AddressesOnDemand {
    type Item = Result<Record, Error>;

    fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let so_far = self.produced.get();
        if so_far == 2 {
            return Poll::Ready(None);
        }
        self.produced.set(so_far + 1);
        Poll::Ready(Some(Ok(v4(so_far as u8 + 1))))
    }
}

/// Panics if anything polls it. Stands in for the family whose answer has
/// not come back yet — or never will.
#[derive(Debug)]
struct MustNotBePolled;

impl Stream for MustNotBePolled {
    type Item = Result<Record, Error>;

    fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        panic!(
            "one family's stream was driven while the caller was reading the other: RFC 8305 \
             requires connecting over AAAA without waiting for the A answer, which is the \
             entire reason these are two streams"
        )
    }
}

#[derive(Debug)]
struct Lazy {
    produced: Rc<Cell<usize>>,
}

impl Resolve for Lazy {
    type Records<'a>
        = std::pin::Pin<Box<dyn futures_core::Stream<Item = Result<Record, Error>> + 'a>>
    where
        Self: 'a;

    fn supports(&self, rtype: u16) -> bool {
        matches!(rtype, rtype::A | rtype::AAAA)
    }

    fn lookup<'a>(&'a self, name: &str, rtype: u16) -> Self::Records<'a> {
        let _ = name;
        match rtype {
            rtype::A => Box::pin({
                AddressesOnDemand {
                    produced: Rc::clone(&self.produced),
                }
            }),
            rtype::AAAA => Box::pin(MustNotBePolled),
            _ => Box::pin(futures_util::stream::empty()),
        }
    }
}

/// One upstream of several failed; the others answered. The failure is an
/// item in the stream, not the end of it.
#[derive(Debug)]
struct PartiallyFailing;

impl Resolve for PartiallyFailing {
    type Records<'a>
        = std::pin::Pin<Box<dyn futures_core::Stream<Item = Result<Record, Error>> + 'a>>
    where
        Self: 'a;

    fn supports(&self, rtype: u16) -> bool {
        matches!(rtype, rtype::A | rtype::AAAA)
    }

    fn lookup<'a>(&'a self, name: &str, rtype: u16) -> Self::Records<'a> {
        let _ = name;
        match rtype {
            rtype::A => Box::pin({
                futures_util::stream::iter(vec![
                    Err(Error::new(
                        ErrorKind::Resolve,
                        UpstreamFailed {
                            upstream: "192.0.2.53",
                        },
                    )),
                    Ok(v4(7)),
                    Err(Error::new(
                        ErrorKind::Resolve,
                        UpstreamFailed {
                            upstream: "192.0.2.54",
                        },
                    )),
                ])
            }),
            rtype::AAAA => Box::pin(MustNotBePolled),
            _ => Box::pin(futures_util::stream::empty()),
        }
    }
}

/// The reason the trait returns a `Stream` and not a `Vec`: the caller can
/// take the first address and start connecting while the resolver has not
/// produced the second one. `.collect()` cannot show this — it waits for
/// the end either way — so the counter is what makes the claim checkable.
#[test]
fn the_first_address_reaches_the_caller_before_the_second_one_is_produced() {
    let produced = Rc::new(Cell::new(0));
    let resolver = Lazy {
        produced: Rc::clone(&produced),
    };
    let mut stream = pin!(resolver.lookup("example.com", rtype::A));

    let first = futures_executor::block_on(stream.next())
        .expect("the stream must yield a first item")
        .expect("and it must be an address");
    assert_eq!(
        first.rdata.addr().unwrap(),
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))
    );
    assert_eq!(
        produced.get(),
        1,
        "the caller is holding the first address while the resolver has produced exactly \
         one: a Vec-returning API would have required all of them first"
    );

    let second = futures_executor::block_on(stream.next())
        .expect("the second item is still there, untouched by taking the first")
        .expect("and it must be an address too");
    assert_eq!(
        second.rdata.addr().unwrap(),
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2))
    );
    assert_eq!(produced.get(), 2);
}

/// The families are separate streams, and reading one must not drive the
/// other. `Lazy::lookup` panics on the first poll, so the A stream
/// below can only run to completion if nothing about it touches AAAA.
#[test]
fn one_family_runs_to_completion_without_the_other_being_polled_at_all() {
    let resolver = Lazy {
        produced: Rc::new(Cell::new(0)),
    };
    // Constructed and held, the way a caller starting both families would
    // hold it — creating the stream must not poll it.
    let aaaa = resolver.lookup("example.com", rtype::AAAA);

    let a: Vec<_> = futures_executor::block_on(resolver.lookup("example.com", rtype::A).collect());
    assert_eq!(a.len(), 2, "the A stream completes on its own");

    drop(aaaa);
}

/// `lookup`'s doc comment: "an error on one is not required to stop
/// the rest." A resolver with several upstreams reports a partial failure
/// and keeps going — and a caller that treated the first `Err` as the end
/// of the stream would throw away the address that came after it.
#[test]
fn an_error_item_does_not_end_the_stream_and_the_address_after_it_still_arrives() {
    let mut stream = pin!(PartiallyFailing.lookup("example.com", rtype::A));

    let first = futures_executor::block_on(stream.next()).expect("an item, not the end");
    assert_matches!(first, Err(ref e) if *e.kind() == ErrorKind::Resolve,
        "the first item is a partial failure of one upstream");

    let second = futures_executor::block_on(stream.next())
        .expect("the stream must continue past the failure, not end on it")
        .expect("and the next item is a usable address");
    assert_eq!(
        second.rdata.addr().unwrap(),
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, 7)),
        "this address is exactly what a caller that stopped at the first error would lose"
    );

    let third = futures_executor::block_on(stream.next()).expect("a third item");
    assert_matches!(
        third,
        Err(_),
        "and a later failure is likewise just an item"
    );
    assert!(
        futures_executor::block_on(stream.next()).is_none(),
        "then the stream ends"
    );
}

/// The seam carries the error's category, not a rendered message: a caller
/// classifies a resolution failure by `kind()` and can still reach the
/// original cause underneath by downcasting, without parsing `Display`.
#[test]
fn a_failed_item_keeps_both_its_kind_and_its_underlying_cause() {
    let items: Vec<_> =
        futures_executor::block_on(PartiallyFailing.lookup("x", rtype::A).collect());
    let failure = items[0]
        .as_ref()
        .expect_err("the first item is the failure");

    assert_eq!(*failure.kind(), ErrorKind::Resolve, "{failure}");
    let cause = StdError::source(failure).expect("the cause must survive the wrap");
    let upstream = cause
        .downcast_ref::<UpstreamFailed>()
        .expect("and it must still be the resolver's own error type, not a string");
    assert_eq!(
        upstream.upstream, "192.0.2.53",
        "which upstream failed is information the caller can act on — retry policy, \
         upstream health — and it must not be flattened into the message"
    );
}

/// A resolver may skip SVCB entirely and still be a complete `Resolve`
/// implementation: `Lazy` above answers `supports(HTTPS) == false` and an
/// empty stream, which is the pair the trait asks of a resolver that
/// cannot ask — and it writes no method it does not have.
#[test]
fn a_resolver_that_ignores_svcb_still_satisfies_the_trait() {
    let resolver = Lazy {
        produced: Rc::new(Cell::new(0)),
    };
    assert!(!resolver.supports(rtype::HTTPS));
    let records: Vec<Result<Record, Error>> =
        futures_executor::block_on(resolver.lookup("example.com", rtype::HTTPS).collect());
    assert!(records.is_empty(), "{records:?}");
}

/// Every answer names the type it was asked for.
///
/// **The property the three-method shape had by construction.** One
/// method returning one `Record` type means a resolver *can* answer an
/// `A` record to an `AAAA` question, and nothing in the type system says
/// otherwise — so the resolvers in this workspace are asked, and
/// `RData::rtype` is what a consumer outside it would ask with.
///
/// `IpLiteralOnly` is the one resolver here whose answers are decided by
/// this crate rather than by a network, so it is the one that can be
/// checked without a server.
#[test]
fn an_answer_names_the_type_it_was_asked_for() {
    use hclient_dns::IpLiteralOnly;

    for (name, want) in [
        ("192.0.2.1", rtype::A),
        ("[2001:db8::1]", rtype::AAAA),
        ("192.0.2.1", rtype::AAAA),
        ("[2001:db8::1]", rtype::A),
    ] {
        let got: Vec<Result<Record, Error>> =
            futures_executor::block_on(IpLiteralOnly.lookup(name, want).collect());
        for record in got {
            let record = record.expect("a literal cannot fail to resolve");
            assert_eq!(
                record.rdata.rtype(),
                want,
                "{name} answered a type nobody asked for"
            );
        }
    }
}
