//! `Client::new()` under `http3`, and what the feature actually changes.
//!
//! It is not the type any more: `DefaultTransport` is `Native` in both
//! builds, and the feature decides whether that `Native` was given a QUIC
//! arm. So the claim has to be read off something the arm moves, and there
//! is one that cannot be faked — **`Capabilities`**.
//!
//! `Native::http3` reduces the two paths to one honest report, and the
//! `Timeouts` fields are where they differ: this transport enforces
//! `first_byte` and `between_bytes` and `hclient-h3` does not, so a
//! transport that can serve a request either way must stop claiming them.
//! A `Client::new()` that stored the arm and went on reporting `true` for
//! both would be exactly the capability that lies.
//!
//! The control is the other arm: without the feature the same call reports
//! `true`, so the assertion is a difference rather than an observation.
#![cfg(all(feature = "default-transport", not(target_family = "wasm")))]

fn caps() -> hclient::caps::Capabilities {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime");
    let _guard = rt.enter();
    let c = hclient::Client::new().expect("the platform verifier is available");
    c.capabilities().clone()
}

/// With the feature, the default transport carries a QUIC arm — and the
/// two body-level timeout claims it cannot keep over HTTP/3 are withdrawn.
#[cfg(feature = "http3")]
#[test]
fn with_the_feature_the_default_transport_reports_the_floor_across_both_paths() {
    let caps = caps();
    assert!(
        !caps.timeouts.first_byte,
        "`hclient-h3` does not enforce `first_byte`, so a transport that may \
         serve over it must not claim to"
    );
    assert!(!caps.timeouts.between_bytes);
    assert!(
        caps.timeouts.connect,
        "and `connect` is kept, because both paths do enforce it — the floor \
         is not a blanket withdrawal"
    );
}

/// Without it, the same call keeps both: there is one path and it enforces
/// them.
#[cfg(not(feature = "http3"))]
#[test]
fn without_the_feature_the_default_transport_keeps_its_body_timeouts() {
    let caps = caps();
    assert!(caps.timeouts.first_byte);
    assert!(caps.timeouts.between_bytes);
}
