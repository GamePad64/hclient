//! `Native::http1`/`Native::http2` — which versions this transport may
//! speak, at runtime rather than at compile time.
//!
//! The `http2` feature decides whether the h2 code exists; these decide
//! whether it is used. What makes the pair worth having is not the switch
//! but its one irreversible consequence: **forbidding HTTP/1.1 moves the
//! `Capabilities` floor**, and nothing else in this crate can. `capabilities()`
//! returns a `&Capabilities` stored at construction, and `RequireVersion`
//! is per request and arrives after it has been answered.
#![cfg(not(target_family = "wasm"))]

use hclient_core::unversioned::Transport as _;
use hclient_native::Native;
use hclient_rt_tokio::Tokio;

fn transport() -> Native<Tokio, hclient_tls::NoTls, hclient_dns::IpLiteralOnly> {
    Native::new(Tokio, hclient_tls::NoTls, hclient_dns::IpLiteralOnly)
}

/// The floor as it stands by default: HTTP/1.1 might be negotiated, so
/// both fields report what holds on it.
#[test]
fn by_default_the_floor_is_http11() {
    let t = transport();
    assert!(!t.capabilities().full_duplex);
    assert!(!t.capabilities().response_trailers);
}

/// **The point of the setting.** With HTTP/1.1 forbidden the worst
/// protocol this transport can negotiate is HTTP/2, and the two fields
/// become honestly `true`.
#[cfg(feature = "http2")]
#[test]
fn forbidding_http1_raises_the_floor_to_http2() {
    let t = transport().http1(false).expect("h2 is compiled in and on");
    assert!(
        t.capabilities().full_duplex,
        "with HTTP/1.1 ruled out the floor is HTTP/2's, and h2 is full duplex"
    );
    assert!(t.capabilities().response_trailers);
}

/// And it goes back: the floor is a function of the policy, not a latch.
#[cfg(feature = "http2")]
#[test]
fn allowing_http1_again_lowers_the_floor_back() {
    let t = transport()
        .http1(false)
        .unwrap()
        .http1(true)
        .expect("turning it back on can never be the last one off");
    assert!(!t.capabilities().full_duplex);
}

/// Turning off both is refused by whichever call would do it, so the empty
/// state is unreachable rather than checked at `build()`.
#[cfg(feature = "http2")]
#[test]
fn the_last_version_cannot_be_turned_off_from_either_side() {
    let e = transport()
        .http2(false)
        .unwrap()
        .http1(false)
        .expect_err("h1 off with h2 already off leaves nothing");
    assert!(e.to_string().contains("cannot both be off"), "{e}");

    let e = transport()
        .http1(false)
        .unwrap()
        .http2(false)
        .expect_err("and the same from the other order");
    assert!(e.to_string().contains("cannot both be off"), "{e}");
}

/// Asking for a protocol this build did not compile is a named refusal,
/// not a silent `false` — the fix is a cargo feature, which a caller
/// cannot infer from a request that quietly went out over HTTP/1.1.
#[cfg(not(feature = "http2"))]
#[test]
fn asking_for_http2_without_the_feature_names_the_feature() {
    let e = transport()
        .http2(true)
        .expect_err("there is no h2 code in this build");
    assert!(e.to_string().contains("`http2` feature"), "{e}");
}

/// Without the feature, `http1(false)` has nothing to fall back to and is
/// refused for that reason rather than accepted into a transport that can
/// speak nothing.
#[cfg(not(feature = "http2"))]
#[test]
fn forbidding_http1_without_http2_compiled_in_is_refused() {
    let e = transport()
        .http1(false)
        .expect_err("no h2 in this build, so this leaves nothing");
    assert!(e.to_string().contains("cannot both be off"), "{e}");
}
