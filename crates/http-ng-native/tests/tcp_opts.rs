//! `Native::tcp_opts` refuses, at construction, an option the runtime
//! cannot apply — and names it.
//!
//! # One test per option, deliberately
//!
//! A single test that sets all six options against a runtime that applies
//! none of them would go green on an implementation that noticed only
//! `nodelay`, and would stay green for the other five for ever. Worse, it
//! is the shape that invites the eventual "why does this fail — let me
//! declare `TcpOptsSupport::ALL` and move on": one assertion covering a
//! set has no way of saying which member of the set it lost.
//!
//! So each of `TcpOpts`' six fields gets its own test, against a runtime
//! that applies **every option but that one** (`FakeRt<N>`, whose
//! `APPLIES` is `TcpOptsSupport::ALL` with field `N` turned off). Setting
//! the one option it cannot apply must fail; the error must name that
//! option and no other. A seventh test sets all six against a runtime
//! that applies all six and requires success, so the six above are
//! measuring the refusal rather than a method that refuses everything.
//!
//! # Where the observer is
//!
//! Not outside the client here, and that is not a lapse: there is nothing
//! to observe on a network, because the whole point is that **no
//! connection is ever made**. The observable is the `Result` of the
//! builder call and the typed source inside its error, which is exactly
//! the thing under test — the same shape as
//! `unsupported_capability_is_rejected_at_build_time`. What the option
//! does once it reaches a socket is the runtime crates' own tests'
//! business (`http-ng-rt-tokio` reads `nodelay` back off the connected
//! socket).
#![cfg(not(target_family = "wasm"))]

use http_ng_core::ErrorKind;
use http_ng_dns_system::SystemDns;
use http_ng_native::Native;
use http_ng_rt::{TcpConnect, TcpOpts, TcpOptsSupport, Timer, UnsupportedTcpOpts};
use http_ng_rt_tokio::Tokio;
use http_ng_tls_rustls::Rustls;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

/// `TcpOptsSupport::ALL` with exactly one field turned off, indexed in
/// `TcpOpts`' own field order — the same order, and the same construction,
/// as `http-ng-rt`'s own `all_but`, so the two files cannot disagree about
/// which index is which option.
const fn all_but(i: usize) -> TcpOptsSupport {
    let mut can = TcpOptsSupport::ALL;
    match i {
        0 => can.nodelay = false,
        1 => can.keepalive = false,
        2 => can.local_address = false,
        3 => can.send_buffer_size = false,
        4 => can.recv_buffer_size = false,
        5 => can.reuse_address = false,
        _ => panic!("TcpOpts has six fields"),
    }
    can
}

/// A runtime that applies every socket option except the `MISSING`th.
///
/// A const parameter rather than six hand-written structs: the only thing
/// that differs between them is one index, and six copies of the same
/// `impl` blocks would be six places for the `APPLIES` line to be edited
/// into agreement with a failing test.
///
/// It never connects — `connect` returns a future that stays `Pending`
/// for ever — because no test here gets as far as a connection. If one
/// ever did, hanging is the failure mode that says so loudest.
#[derive(Debug, Clone, Copy, Default)]
struct FakeRt<const MISSING: usize>;

impl<const MISSING: usize> TcpConnect for FakeRt<MISSING> {
    type Stream = http_ng_rt_tokio::TokioIo;
    const APPLIES: TcpOptsSupport = all_but(MISSING);

    async fn connect(&self, _addr: SocketAddr, _opts: &TcpOpts) -> std::io::Result<Self::Stream> {
        std::future::pending().await
    }
}

impl<const MISSING: usize> Timer for FakeRt<MISSING> {
    type Instant = std::time::Instant;
    type Sleep = std::future::Pending<()>;
    fn sleep(&self, _d: Duration) -> Self::Sleep {
        std::future::pending()
    }
    fn now(&self) -> Self::Instant {
        std::time::Instant::now()
    }
    fn elapsed_since(&self, earlier: Self::Instant) -> Duration {
        std::time::Instant::now().saturating_duration_since(earlier)
    }
}

fn native<const MISSING: usize>() -> Native<FakeRt<MISSING>, Rustls, SystemDns<Tokio>> {
    Native::new(
        FakeRt::<MISSING>,
        Rustls::with_webpki_roots(),
        SystemDns::new(Tokio),
    )
}

/// Every field of `TcpOpts` set to something a runtime would have to act
/// on. Each test below starts from this and keeps exactly one field,
/// which is what makes "the error names this option and no other"
/// checkable at all.
fn all_six_set() -> TcpOpts {
    TcpOpts {
        nodelay: true,
        keepalive: Some(Duration::from_secs(30)),
        local_address: Some(IpAddr::from([127, 0, 0, 1])),
        send_buffer_size: Some(4096),
        recv_buffer_size: Some(4096),
        reuse_address: true,
    }
}

/// The refusal, unwrapped down to the typed value that names the options.
///
/// Two downcasts rather than a substring of the `Display` text: the
/// message is a computed list, and a test matching on it would go green
/// for an error that named `nodelay` inside a sentence about
/// `local_address`. `http_ng_core::Error`'s source is the
/// `std::io::Error` that `TcpOpts::reject_unsupported` built, and that
/// error's own payload is the `UnsupportedTcpOpts`.
fn refused_options<const MISSING: usize>(opts: TcpOpts) -> Vec<&'static str> {
    let err = native::<MISSING>()
        .tcp_opts(opts)
        .expect_err("an option this runtime cannot apply must be refused at construction");
    assert_eq!(
        *err.kind(),
        ErrorKind::Unsupported,
        "a socket option the runtime cannot apply is exactly `Unsupported`: {err}"
    );
    let io = std::error::Error::source(&err)
        .and_then(|s| s.downcast_ref::<std::io::Error>())
        .expect("the source is the io::Error reject_unsupported built");
    assert_eq!(io.kind(), std::io::ErrorKind::Unsupported);
    let named = io
        .get_ref()
        .and_then(|s| s.downcast_ref::<UnsupportedTcpOpts>())
        .expect("and its payload names the options");
    named.names().collect()
}

#[test]
fn nodelay_is_refused_by_name() {
    let opts = TcpOpts {
        nodelay: true,
        ..TcpOpts::default()
    };
    assert_eq!(refused_options::<0>(opts), ["nodelay"]);
}

#[test]
fn keepalive_is_refused_by_name() {
    let opts = TcpOpts {
        keepalive: all_six_set().keepalive,
        ..TcpOpts::default()
    };
    assert_eq!(refused_options::<1>(opts), ["keepalive"]);
}

#[test]
fn local_address_is_refused_by_name() {
    let opts = TcpOpts {
        local_address: all_six_set().local_address,
        ..TcpOpts::default()
    };
    assert_eq!(refused_options::<2>(opts), ["local_address"]);
}

#[test]
fn send_buffer_size_is_refused_by_name() {
    let opts = TcpOpts {
        send_buffer_size: all_six_set().send_buffer_size,
        ..TcpOpts::default()
    };
    assert_eq!(refused_options::<3>(opts), ["send_buffer_size"]);
}

#[test]
fn recv_buffer_size_is_refused_by_name() {
    let opts = TcpOpts {
        recv_buffer_size: all_six_set().recv_buffer_size,
        ..TcpOpts::default()
    };
    assert_eq!(refused_options::<4>(opts), ["recv_buffer_size"]);
}

#[test]
fn reuse_address_is_refused_by_name() {
    let opts = TcpOpts {
        reuse_address: true,
        ..TcpOpts::default()
    };
    assert_eq!(refused_options::<5>(opts), ["reuse_address"]);
}

/// The control the six above need: a runtime that applies everything
/// takes everything. Without it, `tcp_opts` returning `Err` for every
/// input whatsoever would pass all six.
#[test]
fn a_runtime_that_applies_everything_refuses_nothing() {
    let t = Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio));
    assert_eq!(
        <Tokio as TcpConnect>::APPLIES,
        TcpOptsSupport::ALL,
        "the shipped tokio runtime is the control precisely because it applies all six"
    );
    t.tcp_opts(all_six_set())
        .expect("a runtime with APPLIES = ALL has nothing to refuse");
}

/// And the other half of that control: the six refusals are about the
/// options the caller **set**, not about the runtime's `APPLIES` alone.
/// `TcpOpts::default()` is all-off, so even a runtime that applies
/// nothing serves a caller who asked for nothing — a transport built
/// without ever calling `tcp_opts` must keep working on such a runtime,
/// which is the case W7's embassy backend actually is.
#[test]
fn a_runtime_that_applies_nothing_still_takes_the_default_options() {
    native::<0>()
        .tcp_opts(TcpOpts::default())
        .expect("nothing was asked for, so nothing can be unsupported");
}

/// Two unappliable options are both named, in one error.
///
/// Not a replacement for the six — it cannot say which option a
/// regression lost — but it pins the property the six cannot see between
/// them: `names()` reports the whole set, so a caller who fixes the first
/// one the message mentions does not walk into a second identical
/// failure.
#[test]
fn two_unappliable_options_are_both_named() {
    // `FakeRt<0>` applies everything but `nodelay`, so the second offender
    // has to be manufactured differently: ask for every option, and the
    // one it cannot apply is `nodelay` alone. Use the runtime that applies
    // nothing at all instead — index 6 is out of range for `all_but`, so
    // this is a separate, deliberately minimal type.
    #[derive(Debug, Clone, Copy)]
    struct AppliesNothing;
    impl TcpConnect for AppliesNothing {
        type Stream = http_ng_rt_tokio::TokioIo;
        // No `APPLIES` line: the trait's default is `NONE`, and this type
        // exists to use it.
        async fn connect(
            &self,
            _addr: SocketAddr,
            _opts: &TcpOpts,
        ) -> std::io::Result<Self::Stream> {
            std::future::pending().await
        }
    }
    impl Timer for AppliesNothing {
        type Instant = std::time::Instant;
        type Sleep = std::future::Pending<()>;
        fn sleep(&self, _d: Duration) -> Self::Sleep {
            std::future::pending()
        }
        fn now(&self) -> Self::Instant {
            std::time::Instant::now()
        }
        fn elapsed_since(&self, earlier: Self::Instant) -> Duration {
            std::time::Instant::now().saturating_duration_since(earlier)
        }
    }

    let err = Native::new(
        AppliesNothing,
        Rustls::with_webpki_roots(),
        SystemDns::new(Tokio),
    )
    .tcp_opts(TcpOpts {
        nodelay: true,
        reuse_address: true,
        ..TcpOpts::default()
    })
    .expect_err("a runtime that applies nothing must refuse both");
    let io = std::error::Error::source(&err)
        .and_then(|s| s.downcast_ref::<std::io::Error>())
        .expect("io::Error");
    let named = io
        .get_ref()
        .and_then(|s| s.downcast_ref::<UnsupportedTcpOpts>())
        .expect("UnsupportedTcpOpts");
    assert_eq!(
        named.names().collect::<Vec<_>>(),
        ["nodelay", "reuse_address"]
    );
}
