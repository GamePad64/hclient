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
// **`target_os = "linux"` is not about this file's subject, it is about
// where its dependencies exist.** `hclient-core`, `hclient-native`,
// `hclient-tls`, `hclient-dns` and `hclient-mock` are all declared under
// `[target.'cfg(target_os = "linux")'.dev-dependencies]`, and this crate's
// manifest already says why the gate and the `#![cfg]` only work as a
// pair — `tuntap.rs` carries the matching line. This file did not, so it
// compiled nowhere but Linux, and both `test (windows-latest)` and
// `test (macos-latest)` failed on it with six `E0433`s.
#![cfg(all(
    target_os = "linux",
    feature = "proto-ipv4",
    feature = "medium-ethernet"
))]

// **Keeps `embassy-executor`'s rlib on this binary's link line**, which
// nothing else here does: every other test file in this crate names the
// executor for its own reasons, and this one only names types. Under
// `--no-gc-sections` — `just embassy-strict-link` — the linker keeps
// `embassy-executor-timer-queue`'s reference to
// `__embassy_time_queue_item_from_waker`, whose only definition is in
// `embassy-executor`'s `raw` module, and an archive member nothing asks
// for is never pulled. Same `#[no_mangle]` contract, and the same repair,
// as the `use` in `src/lib.rs`, which its own comment explains at length.
//
// It was absent for as long as this file has existed, and the gate said
// so — on Linux only because the recipe passes the flag; a Windows link
// would have been `LNK2019`.
use embassy_executor as _;
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
