//! Does the category of a transport error survive to reach the consumer.
//!
//! `Client::execute` did
//! `Error::new(ErrorKind::Other, e)` for ANY transport error. Forty lines
//! of `hclient-wasi::convert::wasi_err`, sorting 39 `ErrorCode` variants
//! into eight `ErrorKind`s, were thrown away one layer up: every `is_*`
//! predicate on the facade returned `false` for any error coming from the
//! transport, and `kind()` was `Other` identically for a DNS failure, a
//! TLS failure, a connect timeout, and a host rejection. 165 tests never
//! saw it, because the mock could only fail `execute` by exhausting its
//! queue — and that one's category is `Other` anyway, correctly.

// `hclient::mock` lives behind the `test-util` feature (see `mock.rs`).
#![cfg(feature = "test-util")]

use hclient::error::Phase;
use hclient::mock::MockTransport;
use hclient::{Client, Error, ErrorKind};
use std::error::Error as StdError;
use std::fmt::Display;

#[derive(Debug)]
struct Backend(&'static str);
impl Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl StdError for Backend {}

fn client_failing_with(kind: ErrorKind, msg: &'static str) -> Client {
    let m = MockTransport::new();
    m.push_transport_error(Error::new(kind, Backend(msg)));
    Client::builder(m).build().unwrap()
}

/// The core of the finding: `err.kind()` must be whatever the backend
/// named it. `Timeout(Connect)` is chosen because it's exactly what
/// `wasi_err` produces for `ErrorCode::ConnectionTimeout`, and exactly why
/// `Phase` exists.
#[test]
fn transport_error_kind_reaches_the_caller_instead_of_being_flattened() {
    let c = client_failing_with(ErrorKind::Timeout(Phase::Connect), "connect timed out");
    let err = futures_executor::block_on(c.get("https://a/x").send()).unwrap_err();

    assert_eq!(
        *err.kind(),
        ErrorKind::Timeout(Phase::Connect),
        "the category the transport named must survive to reach the caller, not \
         flatten into Other: {err}"
    );
    assert!(
        err.is_timeout(),
        "and the facade's predicates must agree with it"
    );
}

/// The same defect from the other side of the taxonomy: `Unsupported` is a
/// category `hclient-wasi::convert` states outright the caller must be
/// able to tell apart from other failures via `is_unsupported()` — "the
/// backend just can't do this". Through the facade, it couldn't.
#[test]
fn unsupported_from_the_transport_is_still_unsupported_at_the_facade() {
    let c = client_failing_with(
        ErrorKind::Unsupported,
        "wasi:http host rejected setting 'scheme'",
    );
    let err = futures_executor::block_on(c.get("https://a/x").send()).unwrap_err();

    assert!(err.is_unsupported(), "{err}");
    assert!(!err.is_timeout());
    assert!(!err.is_connect());
}

/// `Display` duplicating the source's text is a symptom of the same
/// nesting: `Other: Unsupported: wasi:http host rejected …` puts a
/// category the error doesn't have in front of the one it does.
#[test]
fn display_does_not_nest_a_second_kind_in_front_of_the_real_one() {
    let c = client_failing_with(ErrorKind::Unsupported, "host rejected setting 'scheme'");
    let err = futures_executor::block_on(c.get("https://a/x").send()).unwrap_err();

    let msg = err.to_string();
    assert_eq!(
        msg, "Unsupported: host rejected setting 'scheme'",
        "the category is printed once, and it's the real one"
    );
    assert!(!msg.contains("Other"), "{msg}");
}

/// The flip side: an error whose category REALLY IS `Other` stays that
/// way — identity in `MockTransport::to_error` doesn't mean "everything
/// became not-`Other`". Queue exhaustion is the mock's only failure of its
/// own, and it's honestly `Other`.
#[test]
fn a_genuinely_other_transport_error_stays_other() {
    let m = MockTransport::new();
    let c = Client::builder(m).build().unwrap();
    let err = futures_executor::block_on(c.get("https://a/x").send()).unwrap_err();

    assert_eq!(*err.kind(), ErrorKind::Other);
    let src = StdError::source(&err).expect("Error::new always sets a source");
    assert!(
        src.downcast_ref::<hclient::mock::QueueEmpty>().is_some(),
        "and the source is still QueueEmpty itself, not another wrapper around it"
    );
}
