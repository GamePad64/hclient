//! The cycle §W3 asked about, and the finite composition it does not
//! forbid.
//!
//! The claim, until this file:
//!
//! > The interesting claim is that **the type system already refuses it**:
//! > a `Doh<C>` parameterised by the client it uses makes
//! > `Native<R, T, Doh<Client<Native<R, T, Doh<…>>>>>` an infinitely-sized
//! > type, which is a compile error rather than a stack overflow at run
//! > time. This is *unverified* — nobody has written the type — and the
//! > check is to write it and read the error, which must be a recursion or
//! > size error rather than an accepted definition.
//!
//! It has been written and the errors read. **The four transcripts below
//! are measured on this tree**, by putting each definition in a file under
//! `tests/` and running `cargo build --tests -p hclient-dns-doh`; they are
//! not predictions. They stay in a comment because a test file has to
//! compile, and the point of each is that it does not.
//!
//! **1. The type alias — `E0391`.** The claim, in the spelling anyone
//! would reach for first:
//!
//! ```text
//! type Cycle = Native<Tokio, NoTls, Doh<Cycle>>;
//!
//! error[E0391]: cycle detected when expanding type alias `Cycle`
//!   = note: ...which immediately requires expanding type alias `Cycle` again
//!   = note: type aliases cannot be recursive
//!   = help: consider using a struct, enum, or union instead to break the cycle
//! ```
//!
//! **2. The struct rustc suggests — `E0072`.** The other way to write the
//! type down, and the same answer:
//!
//! ```text
//! struct Cycle(Native<Tokio, NoTls, Doh<Cycle>>);
//!
//! error[E0072]: recursive type `Cycle` has infinite size
//!   6 | struct Cycle(Native<Tokio, NoTls, Doh<Cycle>>);
//!     | ^^^^^^^^^^^^                          ----- recursive without indirection
//!   help: insert some indirection (e.g., a `Box`, `Rc`, or `&`) to break the cycle
//!   6 | struct Cycle(Native<Tokio, NoTls, Doh<Box<Cycle>>>);
//! ```
//!
//! So the claim holds in both spellings, at compile time rather than as a
//! stack overflow at run time.
//!
//! # The escape hatch, measured rather than assumed
//!
//! §W3 names one: *"any `Arc<dyn Resolve>` erasure reopens the cycle, so
//! the guard is a property of not erasing."* That is right in principle
//! and — today — unreachable in practice, and both halves were checked
//! rather than argued.
//!
//! **3. Taking rustc's own suggestion does not reopen it — `E0277`.** The
//! `help` in transcript 2 is literally the escape hatch, and following it
//! produces a type that exists and is not a resolver:
//!
//! ```text
//! struct Cycle(Native<Tokio, NoTls, Doh<Box<Cycle>>>);
//! fn takes_resolver<R: Resolve>(_: R) {}
//! fn probe(c: Doh<Box<Cycle>>) { takes_resolver(c); }
//!
//! error[E0277]: the trait bound `Box<Cycle>: Transport` is not satisfied
//!   = note: required for `Doh<Box<Cycle>>` to implement `Resolve`
//! ```
//!
//! `Doh<C, F>` carries no bound on `C` at the struct, so the recursive
//! *type* is now definable — and `impl Resolve for Doh<C, F> where C:
//! Transport` does not apply to it, because `Box<Cycle>` is not a
//! `Transport`. The cycle is a type nobody can resolve names with.
//!
//! **4. The erasure §W3 named cannot be written at all — `E0038`.**
//! Neither trait is dyn compatible, and for the same reason in both cases:
//!
//! ```text
//! fn probe(_: Arc<dyn Resolve>) {}
//! error[E0038]: the trait `hclient_dns::Resolve` is not dyn compatible
//!   --> crates/hclient-dns/src/lib.rs:132:42
//!   = the trait is not dyn compatible because method `lookup_ipv4`
//!     references an `impl Trait` type in its return type
//!
//! fn probe2(_: Arc<dyn Transport<Body = …, Error = …>>) {}
//! error[E0038]: the trait `Transport` is not dyn compatible
//!   --> crates/hclient-core/src/unversioned/transport.rs:83:10
//!   = ... because method `execute` references an `impl Trait` type in its
//!     return type
//! ```
//!
//! **That is an accident, and it is the one to watch.** Returning `impl
//! Stream` from `Resolve` was chosen for RFC 8305, not to shut this door,
//! and no promise anywhere says the trait will stay dyn-incompatible. A
//! future `BoxResolve` convenience, or an object-safe `Resolve` with boxed
//! streams, would make transcript 4 compile — and then transcript 3 is the
//! only thing left, and it holds only because `Box<C>` is not a
//! `Transport`. A blanket `impl<T: Transport> Transport for Box<T>`, which
//! is an ordinary convenience nobody would think twice about, would remove
//! that too. **Neither is a change to this crate**, which is exactly why
//! it is written down here where someone might read it.

mod support;

use futures_util::StreamExt;
use hclient_dns::{IpLiteralOnly, Resolve};
use hclient_dns_doh::Doh;
use hclient_native::Native;
use hclient_rt_tokio::Tokio;
use hclient_tls::NoTls;
use support::{Rr, Server, TYPE_A, noerror};

/// What a caller actually wants, and what the cycle is not: **two** levels,
/// finite, each a different type.
///
/// The inner transport resolves by IP literal only — it never needs a name,
/// because `Doh::pinned` refuses an endpoint that is not one. The outer
/// transport resolves through DoH. That this composition has a name and
/// the infinite one does not is the whole of the guard.
type Bootstrap = Native<Tokio, NoTls, IpLiteralOnly>;
type Resolver = Doh<Bootstrap>;
type Composed = Native<Tokio, NoTls, Resolver>;

#[tokio::test]
async fn a_doh_resolver_composes_into_a_transport_and_that_transport_resolves() {
    let server = Server::answering(noerror(
        "example.com",
        TYPE_A,
        &[Rr::a("example.com", 60, [192, 0, 2, 1])],
    ));

    // The composition the whole crate is for: a `Native` whose `D` is a
    // `Doh`. That this line type-checks is half the claim.
    let _composed: Composed = Native::new(
        Tokio,
        NoTls,
        Doh::pinned(Native::new(Tokio, NoTls, IpLiteralOnly), server.endpoint())
            .expect("a loopback literal endpoint"),
    );

    // The other half: the resolver in it actually resolves, over the
    // bootstrap transport underneath it.
    let resolver: Resolver =
        Doh::pinned(Native::new(Tokio, NoTls, IpLiteralOnly), server.endpoint()).expect("endpoint");
    let addrs: Vec<_> = resolver
        .lookup_ipv4("example.com")
        .map(|r| r.expect("an address").addr)
        .collect()
        .await;
    assert_eq!(
        addrs,
        vec![
            "192.0.2.1"
                .parse::<std::net::IpAddr>()
                .expect("a v4 literal")
        ]
    );
    assert_eq!(server.requests().len(), 1);
}

/// Three levels also compose, and that is not a curiosity: it is the shape
/// of "DoH, bootstrapped through a second DoH endpoint that is pinned to a
/// literal", which is a real deployment — and it is exactly what a runtime
/// cycle check would have had to tell apart from the infinite one. It does
/// not have to be told apart, because the infinite one has no name.
#[test]
fn three_levels_of_doh_compose_because_each_one_is_a_different_type() {
    type Two = Doh<Native<Tokio, NoTls, Doh<Bootstrap>>>;
    fn accepts(_: Two) {}
    // Never called: the claim is that the type exists and satisfies the
    // bound, which is settled by this file compiling.
    let _ = accepts;
    fn _is_a_resolver<R: Resolve>() {}
    _is_a_resolver::<Two>();
}
