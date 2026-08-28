//! This runtime keeps the seam and does not get the facade, and both
//! halves are the assertion.
//!
//! # Why this file exists
//!
//! `hclient::Client` requires `SendTransport`, and this runtime cannot
//! promise it: `embassy_net::Stack` is `&'d RefCell<Inner>` and its
//! executor is single-threaded — measured, embassy-net carries no
//! `unsafe impl Send`/`Sync` anywhere, by design.
//!
//! That was the shape the whole `Send` design was chosen to avoid paying
//! twice. A `+ Send` written into `TcpConnect` would have taken this
//! runtime out of the **seam**, which is where it lives and where its
//! nine live TAP scenarios run. An associated future type lets it answer
//! for itself: it boxes its `Connecting` plain, stays a `TcpConnect`, and
//! a `Native` over it stays a `Transport`.
//!
//! So the first test is the property that had to survive, and the second
//! is the one that must not quietly appear — a `Native` here reporting
//! `SendTransport` would mean the bound had been satisfied by something
//! that cannot honour it.
#![cfg(all(feature = "proto-ipv4", feature = "medium-ethernet"))]

use hclient_core::unversioned::Transport;

type Embedded = hclient_native::Native<
    hclient_rt_embassy::Embassy<4, 1024, 1024>,
    hclient_tls::NoTls,
    hclient_dns::IpLiteralOnly,
>;

#[test]
fn the_embedded_transport_is_still_a_transport() {
    fn is_transport<T: Transport>() {}
    is_transport::<Embedded>();
}

/// A real negative, not `fn assert_not<T>() {}` — which accepts anything
/// and would pass for the exact defect it is here to catch. Inherent
/// methods win over trait ones, so the answer is `true` only where the
/// bound holds.
#[test]
fn and_deliberately_not_a_send_transport() {
    struct Probe<T>(std::marker::PhantomData<T>);
    trait Fallback {
        fn is() -> bool {
            false
        }
    }
    impl<T> Fallback for Probe<T> {}
    impl<T: hclient_core::unversioned::SendTransport> Probe<T> {
        fn is() -> bool {
            true
        }
    }

    assert!(
        !Probe::<Embedded>::is(),
        "embassy cannot promise Send, and a bound satisfied by something that cannot honour it \
         is worse than one nothing satisfies",
    );
    // The probe discriminates: a transport that *can* promise it answers
    // the other way, so a `Probe` that always said `false` would not pass
    // this pair.
    // The probe discriminates rather than always answering `false`, which
    // is what would make the assertion above vacuous: a transport that
    // *can* promise `Send` answers the other way.
    assert!(Probe::<hclient_mock::MockTransport>::is());
}
