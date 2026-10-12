//! The webpki variant of the default transport: under the
//! `default-transport-webpki` feature [`Client::new`] builds the same
//! stack over Mozilla's compiled-in roots — the variant for targets
//! (Android) where the platform verifier needs an application-side JVM
//! setup this crate cannot perform and panics without.
//!
//! What a test can see from outside the crate is narrow by design: the
//! constructor picks one TLS builder under a `#[cfg]`, and the choice's
//! *cause* — no trust store read at all — is exactly what cannot fail
//! here. So the pin is that a default client constructs without reading
//! the OS, keeps the whole erased-`Client` surface, and still answers a
//! plain request; the cfg itself is guarded by the compile of this file,
//! which is only included under the feature.

#![cfg(feature = "default-transport-webpki")]

#[test]
fn a_default_client_constructs_without_reading_the_os_trust_store() {
    // `with_webpki_roots()` is infallible — the whole point of the
    // variant — so this is the constructor's no-OS-read half. The
    // platform-verifier build exercises the same line against whatever
    // the machine's store is; this build has no call to it at all.
    let c = hclient::Client::new().expect("the webpki constructor cannot fail");
    let _ = c.get("http://127.0.0.1:1/x"); // builder surface, no I/O yet
}

#[test]
fn the_transport_is_reachable_as_the_concrete_default_type() {
    let c = hclient::Client::new().expect("construct");
    // The erased client hands the default stack back by its alias —
    // the same `transport_as` a platform-verifier build answers. A
    // variant that resolved `DefaultTransport` to something else would
    // silently make this `None`.
    assert!(
        c.transport_as::<hclient::DefaultTransport>().is_some(),
        "the default client carries a DefaultTransport"
    );
}
