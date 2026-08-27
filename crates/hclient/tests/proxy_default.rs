//! The default is to honour the machine, and the way out of it.
//!
//! `Client::new()` reads the machine's proxy settings — see its doc for
//! why a convenience constructor should. What this file pins is the
//! **other** half: that a caller who must not proxy has a way to say so,
//! and that it is one line rather than three crates.

#![cfg(all(feature = "default-transport", not(target_family = "wasm")))]

use hclient_core::unversioned::Transport;

/// `default_transport()` reads nothing, and a client built over it
/// therefore proxies nothing — whatever the machine says.
///
/// This is the documented opt-out, so it is a test: a future edit that
/// made the seam read the settings "for consistency" would take the
/// escape hatch away, and nothing else in the suite would notice.
#[test]
fn a_client_built_over_the_seam_ignores_the_machine() {
    let inert = hclient::default_transport().expect("a transport");
    let client = hclient::Client::builder(inert).build().expect("caps ok");
    let t = client
        .transport_as::<hclient::DefaultTransport>()
        .expect("the default backend");

    assert!(
        !t.capabilities().proxy,
        "the seam read the machine's settings, and it is the one thing that must not"
    );
}

/// The control, and the reason the assertion above is not vacuous on a
/// machine with no proxy: the two constructors must be able to disagree.
///
/// It cannot assert that `Client::new()` *does* proxy — that depends on
/// the machine this runs on, and CI's has no proxy — so what it pins is
/// the weaker true thing: the seam never proxies, so if the constructor
/// ever does, they differ.
#[test]
fn the_constructor_is_the_one_that_reads() {
    let via_new = hclient::Client::new().expect("a client");
    let via_seam = hclient::Client::builder(hclient::default_transport().expect("a transport"))
        .build()
        .expect("caps ok");

    let proxied = |c: &hclient::Client| {
        c.transport_as::<hclient::DefaultTransport>()
            .expect("the default backend")
            .capabilities()
            .proxy
    };
    // On a machine with a proxy the first is true and the second false;
    // on one without, both are false. What is never true is the reverse.
    assert!(
        proxied(&via_new) || !proxied(&via_seam),
        "the seam proxied where the constructor did not"
    );
    assert!(!proxied(&via_seam));
}
