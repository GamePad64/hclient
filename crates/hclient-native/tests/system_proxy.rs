//! The machine's proxy settings, as this **transport** installs them.
//!
//! The translation itself — which pattern means what, which
//! configurations are refused — is `hclient-proxy`'s and is tested there,
//! against no transport at all. What is left here is the half that is
//! this crate's: that an installed list is the one a request is routed
//! through, and that the transport's own claim about itself agrees with
//! what it installed.

#![cfg(feature = "system-proxy")]

use hclient_core::unversioned::Transport;
use hclient_dns::IpLiteralOnly;
use hclient_native::proxy::system::testing::system_proxies;
use hclient_native::testing::chosen_proxy;
use hclient_native::{HttpConnect, Native};
use hclient_rt_tokio::Tokio;
use hclient_tls::NoTls;

type Installed =
    Native<Tokio, NoTls, IpLiteralOnly, hclient_core::unversioned::NoHooks, HttpConnect>;

/// Where a request would go, asked through the chooser a request uses
/// rather than by reading fields back.
fn route(t: &Installed, use_tls: bool, host: &str, port: u16) -> Option<String> {
    chosen_proxy(t, use_tls, host, port).map(|p| format!("{}:{}", p.host(), p.port()))
}

fn transport() -> Native<Tokio, NoTls, IpLiteralOnly> {
    Native::new(Tokio, NoTls, IpLiteralOnly)
}

#[test]
fn the_installed_list_is_the_one_a_request_is_routed_through() {
    // The wiring, end to end: settings in, a routing decision out. The
    // ordering that makes the second assertion right is
    // `hclient-proxy`'s, and is asserted there.
    let sys = system_proxies(
        &[("http", "plain.corp:8080"), ("https", "secure.corp:8443")],
        &["internal.example.com"],
        false,
    );
    let t = transport().system_proxies_from(&sys).expect("installs");

    assert_eq!(
        route(&t, false, "example.com", 80).as_deref(),
        Some("plain.corp:8080")
    );
    assert_eq!(
        route(&t, true, "example.com", 443).as_deref(),
        Some("secure.corp:8443")
    );
    assert_eq!(route(&t, true, "internal.example.com", 443), None);
}

#[test]
fn a_machine_with_no_proxy_installs_none_and_does_not_claim_one() {
    // Not an error: most machines are this one. What matters is that the
    // capability does not claim a proxy that is not there — a transport
    // reporting `proxy` while proxying nothing is a capability that lies,
    // which is the defect `Native::hooks` was found to have one field
    // over.
    let t = transport()
        .system_proxies_from(&system_proxies(&[], &[], false))
        .expect("nothing configured is an ordinary answer");

    assert_eq!(route(&t, true, "example.com", 443), None);
    assert!(!t.capabilities().proxy);
}

#[test]
fn a_configured_machine_reports_the_capability() {
    // The control for the assertion above.
    let t = transport()
        .system_proxies_from(&system_proxies(&[("*", "proxy.corp:8080")], &[], false))
        .expect("installs");

    assert!(t.capabilities().proxy);
}

#[test]
fn a_star_bypass_installs_nothing_and_claims_nothing() {
    // `NO_PROXY=*` is *use no proxy*. The empty list is `hclient-proxy`'s
    // answer; that the capability follows it is this crate's.
    let sys = system_proxies(&[("*", "proxy.corp:8080")], &["*"], false);
    let t = transport().system_proxies_from(&sys).expect("installs");

    assert_eq!(route(&t, true, "example.com", 443), None);
    assert!(!t.capabilities().proxy);
}

#[test]
fn a_configuration_this_transport_cannot_hold_is_an_unsupported_error() {
    // The refusal is `hclient-proxy`'s decision and its message; what
    // this asserts is that it arrives as an error of the right *kind*,
    // because `ErrorKind` is what a caller matches on.
    let sys = system_proxies(&[("all", "socks5://socks.corp:1080")], &[], false);
    let err = transport()
        .system_proxies_from(&sys)
        .expect_err("a SOCKS proxy is not installable here");

    assert_eq!(*err.kind(), hclient_core::ErrorKind::Unsupported);
    assert!(err.to_string().contains("socks.corp"), "{err}");
}
