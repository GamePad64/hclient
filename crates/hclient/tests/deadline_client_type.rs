//! Switching a total timeout on must not change `Client`'s type.
//!
//! This is a compile-shape test, and it is here because the property is
//! invisible to every behavioural test in the suite: a `Client` that grew
//! a second parameter the moment a timeout was configured would pass all
//! of `tests/deadline.rs` and still break every consumer that stores one
//! in a struct.
//!
//! **Why that matters enough to pin.** `Client` carries `Arc` semantics
//! and a defaulted type parameter for one stated reason — so that
//! `struct App { http: Client }` compiles and a client can be handed
//! around like `reqwest`'s. The v0.2 design document turns down `tower`
//! layers for compression on exactly this ground (§W5): a layer changes
//! the client's type, and the field declaration stops compiling. A total
//! timeout that did the same thing would be the identical defect entered
//! from the other side.
//!
//! The mechanism that prevents it is `DefaultClock` (see `src/lib.rs`):
//! the second parameter's default is a REAL clock wherever
//! `DefaultTransport` exists — `Tokio` on native, which is already inside
//! `DefaultTransport` itself, and a `setTimeout` clock in the browser — so
//! `Client::new()?.total_timeout(d)` needs no clock argument and returns
//! the same type it was given.
//!
//! The native gate matches the one on `facade.rs`'s `DefaultTransport`
//! tests; the browser half of the same property is in `wasm_default.rs`,
//! which the `browser` CI job runs under `wasm-pack`.
#![cfg(all(feature = "default-transport", not(target_family = "wasm")))]

use hclient::Client;
use std::time::Duration;

/// The declaration the whole file exists for: a bare `Client`, no
/// parameters, in a struct field — the shape a consumer actually writes.
struct App {
    http: Client,
}

#[test]
fn a_client_with_a_total_timeout_still_fits_a_bare_client_field() {
    // Every one of these annotations is load-bearing. `Client::new()`
    // returning `Client` is already pinned by `facade.rs`; what is new
    // here is that `total_timeout` does not widen it.
    let plain: Client = Client::new().expect("default transport supports the default config");
    let bounded: Client = plain.total_timeout(Duration::from_secs(5));

    let app = App { http: bounded };
    assert_eq!(
        app.http.config().total,
        Some(Duration::from_secs(5)),
        "the bound must actually be stored, or this file would pass while \
         `total_timeout` did nothing at all"
    );

    // And cloning it is still the same bare type — `Client::clone` shares
    // the transport and copies the configuration, so both handles keep
    // their own bound.
    let clone: Client = app.http.clone();
    assert_eq!(clone.config().total, Some(Duration::from_secs(5)));
}

/// `total_timeout` returns a handle over the SAME transport, so an
/// unbounded client and a bounded one can coexist without a second
/// connection pool. Pinned because it is the reason the configuration
/// lives outside the `Arc` at all.
#[test]
fn a_bounded_handle_shares_the_transport_with_the_unbounded_one() {
    let plain: Client = Client::new().expect("supported");
    let also_plain = plain.clone();
    let bounded = plain.total_timeout(Duration::from_millis(250));

    assert_eq!(also_plain.config().total, None, "the original is untouched");
    assert_eq!(bounded.config().total, Some(Duration::from_millis(250)));
    // Asked through the downcast, which is the only transport accessor:
    // both handles must hand back the same address, and `Some` on both is
    // itself part of the claim — a handle that had built a second transport
    // would still answer `Some`, at a different address.
    let (a, b) = (
        also_plain
            .transport_as::<hclient::DefaultTransport>()
            .expect("the default backend"),
        bounded
            .transport_as::<hclient::DefaultTransport>()
            .expect("the default backend"),
    );
    assert!(
        std::ptr::eq(a, b),
        "the two handles must share one transport, not hold two"
    );
}

/// The same claim one type along: a response from this client fits a bare
/// `hclient::Response` field.
///
/// `Response<B>` keeps its parameter — `SseStream::new` takes a response
/// over any body — but it **defaults** to `ClientBody`, so the common case
/// names one type instead of two paths. Without the default this reads
/// `Response<hclient::body::ClientBody>`, which is the tax `Client` was
/// erased to remove, reappearing on the value it hands back.
#[test]
fn a_response_from_this_client_fits_a_bare_response_field() {
    #[allow(dead_code)]
    struct Page {
        resp: hclient::Response,
    }
    fn takes_one(_: &hclient::Response) {}

    // The control: the parameter is still there and still means something,
    // so the default is a default rather than a removal.
    fn takes_any<B>(_: &hclient::Response<B>) {}

    let _ = (
        takes_one as fn(&hclient::Response),
        takes_any::<()> as fn(&_),
    );
}
