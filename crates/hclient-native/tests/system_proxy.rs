//! The machine's proxy settings, as this **transport** installs them.
//!
//! The translation itself — which pattern means what, which
//! configurations are refused — is `hclient-proxy`'s and is tested there,
//! against no transport at all. What is left here is the half that is
//! this crate's: that an installed list is the one a request is routed
//! through, and that the transport's own claim about itself agrees with
//! what it installed.

#![cfg(feature = "system-proxy")]

use hclient::Client;
use hclient_core::unversioned::Transport;
use hclient_dns::IpLiteralOnly;
use hclient_native::proxy::system::testing::system_proxies;
use hclient_native::testing::chosen_proxy;
use hclient_native::{HttpConnect, Native};
use hclient_rt_tokio::Tokio;
use hclient_tls::NoTls;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc;
use std::time::Duration;

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

// --- settings to the wire ------------------------------------------------
//
// Everything above asks what the transport *holds*. These two ask what
// the bytes do, which is the join the other tests cannot make: the
// translation is `hclient-proxy`'s and is tested there, the wire is
// `tests/proxy.rs`'s and is tested there, and nothing until here has run
// one into the other.

const BOUND: Duration = Duration::from_secs(5);

fn read_head(s: &mut TcpStream) -> String {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match s.read(&mut byte) {
            Ok(0) | Err(_) => break,
            Ok(_) => head.push(byte[0]),
        }
    }
    String::from_utf8_lossy(&head).into_owned()
}

/// An HTTP proxy that answers `http://` itself and reports the request
/// line it was given.
fn http_proxy() -> (SocketAddr, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut s) = conn else { break };
            let _ = tx.send(read_head(&mut s));
            let _ =
                s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi");
            let _ = s.flush();
        }
    });
    (addr, rx)
}

#[tokio::test]
async fn a_request_really_goes_through_the_proxy_the_settings_named() {
    // The whole chain in one assertion: a `host:port` string of the shape
    // a registry key holds, translated, installed, and then *read off the
    // wire at the proxy* — absolute-form, which is what says the request
    // went through it rather than to it.
    let (proxy, seen) = http_proxy();
    let sys = system_proxies(&[("*", &proxy.to_string())], &[], false);
    let transport = transport().system_proxies_from(&sys).expect("installs");
    let client = Client::builder(transport).build().expect("caps ok");

    let body = client
        .get("http://example.com/thing")
        .send()
        .await
        .expect("the proxy answers")
        .collect()
        .await
        .expect("a body")
        .text()
        .expect("utf-8");

    assert_eq!(body, "hi");
    let head = seen.recv_timeout(BOUND).expect("the proxy saw a request");
    assert!(
        head.starts_with("GET http://example.com/thing HTTP/1.1\r\n"),
        "{head}"
    );
}

#[tokio::test]
async fn a_bypassed_host_from_the_settings_never_reaches_the_proxy() {
    // The control for the test above, and the half that matters more: a
    // bypass that was translated but not *applied* would be invisible
    // here except that the proxy sees traffic it should not.
    let (proxy, seen) = http_proxy();
    let sys = system_proxies(&[("*", &proxy.to_string())], &["example.com"], false);
    let transport = transport().system_proxies_from(&sys).expect("installs");
    let client = Client::builder(transport).build().expect("caps ok");

    // `IpLiteralOnly` resolves no names, so a request that goes direct
    // fails to resolve — which is the observation. What must not happen
    // is the proxy receiving it.
    let _ = client.get("http://example.com/thing").send().await;
    assert!(
        seen.recv_timeout(Duration::from_millis(300)).is_err(),
        "a bypassed host reached the proxy"
    );
}

/// The machine's own settings, read for real.
///
/// **The only test that executes the platform readers at all**, and it is
/// shape-only on purpose: what the machine says is not this suite's to
/// decide, and a test asserting *no proxy* would fail on a developer's
/// machine behind one. What it does catch is the class of defect the pure
/// rules cannot — a registry value read as the wrong type, a dictionary
/// key that is not there, a panic — and it catches it on the Windows and
/// macOS runners, where those four lines are the only ones this workspace
/// cannot exercise from Linux.
#[test]
fn reading_the_real_machine_answers_something_self_consistent() {
    use hclient_native::proxy::system::SystemProxies;

    let sys = SystemProxies::detect();
    for entry in sys.entries() {
        assert!(!entry.host().is_empty(), "a nameless proxy: {entry:?}");
        assert_ne!(entry.port(), 0, "a portless proxy: {entry:?}");
    }
    // Ordering is imposed rather than the platform's, so two readings of
    // one unchanged machine must agree — the property a `HashMap` in the
    // reader would silently break.
    assert_eq!(sys.entries(), SystemProxies::detect().entries());

    // And whatever it said, the transport either installs it or refuses
    // by name. A third outcome — a panic, or a proxy with no host — is
    // what this is here to fail on.
    match transport().system_proxies_from(&sys) {
        Ok(t) => assert_eq!(t.capabilities().proxy, !sys.is_empty()),
        Err(e) => assert!(!e.to_string().is_empty()),
    }
}
