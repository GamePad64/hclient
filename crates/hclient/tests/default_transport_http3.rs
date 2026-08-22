//! `Client::new()` under `http3`, and the claim is which transport it built.
//!
//! The feature's whole promise is that a caller who never names a transport
//! gets HTTP/3 by putting a word in a manifest. That promise is not
//! observable from a request — an origin that advertises no `h3` in its
//! HTTPS record is served over TCP either way, which is correct and says
//! nothing about what was constructed. So what is asserted here is the
//! type, through `Client::transport_as`, which is the one door erasure
//! leaves open.
//!
//! The control is the other arm: without the feature, the same call builds
//! a `Native`, and the downcast to `Selecting` fails. Both directions are
//! needed, because a `transport_as` that always answered `Some` would pass
//! the first assertion alone.
#![cfg(all(feature = "default-transport", not(target_family = "wasm")))]

type Tcp = hclient_native::Native<
    hclient_rt_tokio::Tokio,
    hclient_tls_rustls::Rustls,
    hclient_dns_system::SystemDns<hclient_rt_tokio::Tokio>,
>;

#[cfg(feature = "http3")]
type Both = hclient_select::Selecting<
    hclient_rt_tokio::Tokio,
    hclient_tls_rustls::Rustls,
    hclient_dns_system::SystemDns<hclient_rt_tokio::Tokio>,
>;

/// With `http3`, the default transport is the pair — and it is *not* the
/// TCP stack, which is the half that fails if the alias were left alone
/// and only the feature list moved.
#[cfg(feature = "http3")]
#[test]
fn with_the_feature_the_default_transport_is_the_selecting_pair() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime");
    let _guard = rt.enter();
    let c = hclient::Client::new().expect("the platform verifier is available");
    assert!(
        c.transport_as::<Both>().is_some(),
        "`Client::new()` under `http3` must build `Selecting`"
    );
    assert!(
        c.transport_as::<Tcp>().is_none(),
        "and it must not still be the bare TCP stack — that is what a moved \
         feature list with an unmoved alias would look like"
    );
}

/// Without it, the same call builds the TCP stack and nothing else.
#[cfg(not(feature = "http3"))]
#[test]
fn without_the_feature_the_default_transport_is_the_tcp_stack() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime");
    let _guard = rt.enter();
    let c = hclient::Client::new().expect("the platform verifier is available");
    assert!(c.transport_as::<Tcp>().is_some());
}
