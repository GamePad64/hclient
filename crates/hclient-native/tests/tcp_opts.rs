//! What socket options this transport asks its runtime for, and what
//! happens when it cannot have them.
//!
//! Two halves. `Native::tcp_opts` refuses, at construction, an option the
//! runtime cannot apply — and names it. And `Native::new` asks for exactly
//! one option of its own, `nodelay`, **only where the runtime's
//! `TcpConnect::APPLIES` says it applies it**: Nagle's algorithm costs the
//! head of a TLS exchange 41 ms (`tests/nagle_cost.rs`), and asking
//! unconditionally would turn the refusal above into every connect's fate
//! on a backend that left `APPLIES` at its `NONE` default.
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
//! to observe on a network, because **no connection is ever made** in this
//! file. For the refusal, the observable is the `Result` of the builder
//! call and the typed source inside its error, which is exactly the thing
//! under test — the same shape as
//! `unsupported_capability_is_rejected_at_build_time`. For what `new` asks
//! for, the observable is [`Recording`], a runtime that keeps the
//! `TcpOpts` it was handed and then fails the connect: `Native` hands its
//! whole option set to `TcpConnect::connect` and to nothing else, so that
//! argument *is* the answer, where reading a private field would be a
//! restatement of the code. What the option does once it reaches a socket
//! is the runtime crates' own tests' business (`hclient-rt-tokio` reads
//! `nodelay` back off the connected socket), and what it costs when it is
//! absent is `tests/nagle_cost.rs`'s.
#![cfg(not(target_family = "wasm"))]

use hclient_core::unversioned::Transport;
use hclient_core::{ErrorKind, RequestBody};
use hclient_dns::IpLiteralOnly;
use hclient_dns_system::SystemDns;
use hclient_native::Native;
use hclient_rt::{TcpConnect, TcpOpts, TcpOptsSupport, Timer, UnsupportedTcpOpts};
use hclient_rt_tokio::Tokio;
use hclient_tls::NoTls;
use hclient_tls_rustls::Rustls;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// `TcpOptsSupport::ALL` with exactly one field turned off, indexed in
/// `TcpOpts`' own field order — the same order, and the same construction,
/// as `hclient-rt`'s own `all_but`, so the two files cannot disagree about
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
    type Stream = hclient_rt_tokio::TokioIo;
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
fn every_field_set() -> TcpOpts {
    TcpOpts {
        nodelay: true,
        keepalive: Some(Duration::from_secs(30)),
        keepalive_interval: Some(Duration::from_secs(5)),
        keepalive_retries: Some(3),
        bind_device: Some("lo".to_owned()),
        user_timeout: Some(Duration::from_secs(20)),
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
/// `local_address`. `hclient_core::Error`'s source is the
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
        keepalive: every_field_set().keepalive,
        ..TcpOpts::default()
    };
    assert_eq!(refused_options::<1>(opts), ["keepalive"]);
}

#[test]
fn local_address_is_refused_by_name() {
    let opts = TcpOpts {
        local_address: every_field_set().local_address,
        ..TcpOpts::default()
    };
    assert_eq!(refused_options::<2>(opts), ["local_address"]);
}

#[test]
fn send_buffer_size_is_refused_by_name() {
    let opts = TcpOpts {
        send_buffer_size: every_field_set().send_buffer_size,
        ..TcpOpts::default()
    };
    assert_eq!(refused_options::<3>(opts), ["send_buffer_size"]);
}

#[test]
fn recv_buffer_size_is_refused_by_name() {
    let opts = TcpOpts {
        recv_buffer_size: every_field_set().recv_buffer_size,
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
    t.tcp_opts(every_field_set())
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
        type Stream = hclient_rt_tokio::TokioIo;
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

// --- what `Native::new` asks for, and of whom ----------------------------

/// The `TcpOpts` a runtime was handed, kept where a test can read them.
///
/// `Native` passes its whole option set to `TcpConnect::connect` and to
/// nowhere else, so the argument this records is the transport's answer
/// rather than a description of it. Recording and then failing the connect
/// — instead of hanging like `FakeRt` above — is what lets the calling
/// test `await` a whole `execute` and get its verdict back.
#[derive(Debug, Clone, Default)]
struct Seen(Arc<Mutex<Vec<TcpOpts>>>);

impl Seen {
    fn only(&self) -> TcpOpts {
        let v = self
            .0
            .lock()
            .expect("no test here panics while holding this");
        assert_eq!(
            v.len(),
            1,
            "one address, one attempt — so exactly one connect, or this \
             test is reading someone else's options"
        );
        v[0].clone()
    }
}

/// A runtime that records what it is asked for and declares **all six**.
#[derive(Debug, Clone, Default)]
struct Declaring(Seen);

/// The same, and the whole point of it is the line that is missing:
/// **no `APPLIES`**, so it inherits `TcpConnect`'s `NONE` default. This is
/// the third-party backend that default was written to protect — the one
/// that would refuse every connect if this transport asked for an option
/// unconditionally.
#[derive(Debug, Clone, Default)]
struct Silent(Seen);

macro_rules! recording_runtime {
    ($ty:ty $(, $applies:expr)?) => {
        impl TcpConnect for $ty {
            type Stream = hclient_rt_tokio::TokioIo;
            $(const APPLIES: TcpOptsSupport = $applies;)?

            async fn connect(
                &self,
                _addr: SocketAddr,
                opts: &TcpOpts,
            ) -> std::io::Result<Self::Stream> {
                self.0
                    .0
                    .lock()
                    .expect("no test here panics while holding this")
                    .push(opts.clone());
                // Refusing rather than hanging: the observable is the
                // recorded argument, and an `execute` that returns lets
                // the test say so with an ordinary assertion.
                Err(std::io::Error::other(
                    "recorded, and this runtime has no socket",
                ))
            }
        }

        impl Timer for $ty {
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
    };
}

recording_runtime!(Declaring, TcpOptsSupport::ALL);
// No second argument, deliberately — that absence is the subject of
// `a_runtime_that_declares_nothing_is_asked_for_nothing`.
recording_runtime!(Silent);

/// One request through a transport built by `build`, returning what its
/// runtime was handed. Nothing reaches a network: the runtimes above fail
/// the connect the moment they have recorded.
async fn asked_of<R>(
    rt: R,
    seen: &Seen,
    build: impl FnOnce(Native<R, NoTls, IpLiteralOnly>) -> Native<R, NoTls, IpLiteralOnly>,
) -> TcpOpts
where
    R: TcpConnect<Stream = hclient_rt_tokio::TokioIo> + Timer<Instant = std::time::Instant> + Clone,
{
    let t = build(Native::new(rt, NoTls, IpLiteralOnly));
    let req = http::Request::builder()
        // A literal, so no resolver is consulted and exactly one address
        // is tried; port 9 is never dialled — the runtime fails before a
        // socket exists.
        .uri("http://127.0.0.1:9/")
        .body(RequestBody::Empty)
        .expect("a well-formed request");
    t.execute(req)
        .await
        .expect_err("this runtime records and refuses");
    seen.only()
}

#[tokio::test]
async fn a_runtime_that_applies_nodelay_is_asked_for_it() {
    // The fix, stated where it is observable: Nagle costs the head of a
    // TLS exchange 41 ms (`tests/nagle_cost.rs`), and `Native::new` — not
    // `TcpOpts::default()`, and not the caller — is what asks for it off.
    let rt = Declaring::default();
    let seen = rt.0.clone();
    let opts = asked_of(rt, &seen, |t| t).await;
    assert!(
        opts.nodelay,
        "a transport that speaks request/response over TLS asks for TCP_NODELAY"
    );
}

#[tokio::test]
async fn nodelay_is_the_only_option_this_transport_asks_for_by_itself() {
    // The other five stay at `TcpOpts::default()`. A `nodelay: true` that
    // arrived by replacing the whole struct with something opinionated
    // would pass the test above and fail this one.
    let rt = Declaring::default();
    let seen = rt.0.clone();
    let opts = asked_of(rt, &seen, |t| t).await;
    assert!(opts.keepalive.is_none());
    assert!(opts.local_address.is_none());
    assert!(opts.send_buffer_size.is_none());
    assert!(opts.recv_buffer_size.is_none());
    assert!(!opts.reuse_address);
}

#[tokio::test]
async fn a_runtime_that_declares_nothing_is_asked_for_nothing() {
    // **The compatibility half, and the reason the fix is not
    // `TcpOpts::default()` gaining `nodelay: true`.** A third-party
    // backend that forgot its `APPLIES` line gets exactly what it got
    // before this change: an all-off `TcpOpts`, a connect that proceeds,
    // and Nagle left on. Silence understates, and this transport believes
    // it.
    let rt = Silent::default();
    let seen = rt.0.clone();
    let opts = asked_of(rt, &seen, |t| t).await;
    assert!(
        !opts.nodelay,
        "an option a runtime has not declared must not be asked of it"
    );
    // Said as the runtime itself would say it, since that is the
    // consequence rather than the field: whatever `Native::new` put in
    // there, a `NONE` runtime that checks refuses none of it.
    opts.reject_unsupported(<Silent as TcpConnect>::APPLIES)
        .expect("a transport built with `new` alone never refuses a connect on any runtime");
}

#[tokio::test]
async fn tcp_opts_replaces_the_whole_set_including_the_nodelay_new_asked_for() {
    // Documented on the method, and pinned here because it is a decision
    // rather than an oversight: these are *the* options for every attempt
    // this transport makes, so a caller who supplies them supplies all
    // six. A `tcp_opts` that quietly OR-ed its own `nodelay` back in
    // would take the choice away from a caller with a reason to want
    // Nagle.
    let rt = Declaring::default();
    let seen = rt.0.clone();
    let opts = asked_of(rt, &seen, |t| {
        t.tcp_opts(TcpOpts {
            keepalive: Some(Duration::from_secs(30)),
            ..TcpOpts::default()
        })
        .expect("this runtime applies everything")
    })
    .await;
    assert!(!opts.nodelay, "the caller's set is the whole set");
    assert_eq!(opts.keepalive, Some(Duration::from_secs(30)));
}

/// The refusal path, on the backend this change is most likely to meet:
/// one that declares nothing, whose caller asks for `nodelay` anyway.
///
/// It fails at construction rather than at connect, and the message says
/// both what was refused and where the claim that it cannot be applied
/// comes from — which is the half a backend author needs, because their
/// `connect` may well apply it and their `APPLIES` line be the defect.
#[test]
fn a_caller_who_asks_a_silent_runtime_for_nodelay_is_still_refused_by_name() {
    let err = Native::new(Silent::default(), NoTls, IpLiteralOnly)
        .tcp_opts(TcpOpts {
            nodelay: true,
            ..TcpOpts::default()
        })
        .expect_err("a runtime that declares nothing must refuse what it was asked for");
    assert_eq!(*err.kind(), ErrorKind::Unsupported);
    let io = std::error::Error::source(&err)
        .and_then(|s| s.downcast_ref::<std::io::Error>())
        .expect("the source is the io::Error reject_unsupported built");
    let named = io
        .get_ref()
        .and_then(|s| s.downcast_ref::<UnsupportedTcpOpts>())
        .expect("and its payload names the options");
    assert_eq!(named.names().collect::<Vec<_>>(), ["nodelay"]);
    assert!(
        io.to_string().contains("TcpConnect::APPLIES"),
        "the message has to name the constant an implementor would change: {io}"
    );
}

/// Every runtime this workspace ships declares `nodelay`, so `new`'s
/// conditional is live rather than vacuous for all of them.
///
/// A tripwire rather than a claim about the fix: if one of them ever stops
/// declaring it, the 41 ms comes back silently on that runtime and nothing
/// else in this crate would notice. The three are named separately because
/// `TokioHandle` has been the odd one out once already — it applied every
/// option and declared none, found by measurement
/// (`docs/v04-w1-acceptance.md` §8, finding 5) rather than by reading.
///
/// `const` blocks, so this is checked when the file is **compiled** and a
/// regression is a build failure rather than a red test. It stays inside a
/// named `#[test]` anyway, because the name is the sentence and a bare
/// `const _: () = ..` at the bottom of a test file is not where anyone
/// looks for it.
#[test]
fn every_shipped_runtime_declares_the_option_this_transport_asks_for() {
    const { assert!(<Tokio as TcpConnect>::APPLIES.nodelay, "Tokio") };
    const {
        assert!(
            <hclient_rt_tokio::TokioHandle as TcpConnect>::APPLIES.nodelay,
            "TokioHandle"
        )
    };
    const {
        assert!(
            <hclient_rt_smol::Smol as TcpConnect>::APPLIES.nodelay,
            "Smol"
        )
    };
}
