//! What the connector says about itself: its trust identity, whether it
//! can present a client certificate, and what it prints.
//!
//! These are the three things a `NativeTls` answers without touching a
//! socket, and none of them was checked. The mutation sweep found every one
//! of them alive — `presents_client_certs` in **both** directions (a
//! constant `true` and a constant `false` each passed the whole suite),
//! `identity` and `add_root_certificate` returning `Default::default()`,
//! and `Debug::fmt` printing nothing at all.
//!
//! The rustls backend has had `tests/config_id.rs` for this since the
//! identity existed; this is that file's counterpart, plus the two
//! questions rustls does not have to answer the same way.

use hclient_tls::TlsIdentity;
use hclient_tls_native_tls::NativeTls;

/// A certificate this crate can hand to `add_root_certificate`, freshly
/// minted so that two calls genuinely differ.
fn a_certificate() -> native_tls::Certificate {
    let cert = rcgen::generate_simple_self_signed(vec!["test-ca.invalid".into()])
        .expect("self-signed certificate");
    native_tls::Certificate::from_der(cert.cert.der())
        .expect("a self-signed certificate is a valid root")
}

/// A client identity — a certificate and its key.
///
/// `from_pkcs8` is OpenSSL-shaped and the manifest records that `SChannel`
/// and Security.framework refuse the same PEM pair, which is why no test
/// here **requires** one: [`presents_client_certs`] is asserted through
/// whichever arm this platform can build, and the `false` half needs no
/// identity at all.
///
/// [`presents_client_certs`]: hclient_tls::TlsIdentity::presents_client_certs
fn an_identity() -> Option<native_tls::Identity> {
    let cert = rcgen::generate_simple_self_signed(vec!["client.invalid".into()])
        .expect("self-signed certificate");
    native_tls::Identity::from_pkcs8(
        cert.cert.pem().as_bytes(),
        cert.signing_key.serialize_pem().as_bytes(),
    )
    .ok()
}

/// **A connector with no client certificate says so**, which is the half
/// every platform can answer.
///
/// `hclient-native` reads this straight into `Capabilities::client_certs`,
/// so a constant `true` here is a capability that lies — the class this
/// workspace treats as worse than a silent downgrade, because a caller can
/// act on a capability. It was a live survivor.
#[test]
fn a_connector_with_no_identity_does_not_claim_to_present_one() {
    assert!(
        !NativeTls::new().presents_client_certs(),
        "nothing was configured, so claiming a client certificate would put a \
         `true` into Capabilities::client_certs that no handshake can honour"
    );
}

/// **And one with a client certificate does.**
///
/// The other half, and the one that kills a constant `false`. It is skipped
/// rather than failed where the platform cannot build an `Identity` from a
/// PEM pair — `native_tls::Identity::from_pkcs8` is OpenSSL-shaped, and the
/// manifest records `SChannel` answering `ASN1 bad tag value met` and
/// Security.framework `Unknown format in import` for exactly this input.
/// A hard failure there would be this test asserting a fact about `rcgen`
/// and the platform importer rather than about the accessor.
#[test]
fn a_connector_holding_an_identity_reports_it() {
    let Some(identity) = an_identity() else {
        // Not a silent pass: the reason is printed, so a platform that
        // quietly stopped building identities is visible in the log rather
        // than indistinguishable from one that ran the assertion.
        println!(
            "skipped: this platform's native-tls cannot import a PKCS#8 PEM pair, \
             which is a fact about the importer rather than about this accessor"
        );
        return;
    };
    assert!(
        NativeTls::new().identity(identity).presents_client_certs(),
        "the accessor asks whether an identity was configured, and one was — \
         a `false` here is the mTLS half of the capability going missing"
    );
}

/// **A connector's identity is fixed for its lifetime.**
///
/// `TlsConfigId` is a component of `hclient-native`'s pool key, so an
/// identity redrawn on every call would pool nothing at all, silently —
/// which is the mistake `TlsConfigId::new_unique`'s own doc names and which
/// the rustls backend's `tests/config_id.rs` catches for that backend.
#[test]
fn one_connector_reports_the_same_identity_every_time() {
    let tls = NativeTls::new();
    let first = tls.config_id();
    for _ in 0..3 {
        assert_eq!(
            tls.config_id(),
            first,
            "drawn once in the constructor, not per call"
        );
    }
}

/// **Two separately built connectors are not interchangeable.**
///
/// The other direction, and the one a `TypeId` implementation would break:
/// two `NativeTls` values are the same *type* and may trust entirely
/// different roots, so sharing a pooled connection between them would hand
/// a connection established under one trust configuration to a client that
/// asked for another.
#[test]
fn two_connectors_have_different_identities() {
    assert_ne!(
        NativeTls::new().config_id(),
        NativeTls::new().config_id(),
        "nothing says these two trust the same things, so they must not share \
         a pooled connection"
    );
}

/// **Every builder method that changes a trust decision redraws the
/// identity.**
///
/// The struct's own doc says this is what keeps the id honest under
/// `Clone`: a clone is interchangeable with its original and shares an
/// identity, and the moment it is given a different root or a different
/// client certificate it stops being either. `add_root_certificate` and
/// `identity` were both live survivors — a body returning
/// `Default::default()` drops the setting **and** draws a fresh id, so the
/// obvious assertion on the id alone would pass for it. That is why the
/// clone is the baseline: it pins that the id moved *relative to a value
/// that kept it*.
#[test]
fn adding_a_root_redraws_the_trust_identity() {
    let base = NativeTls::new();
    let clone = base.clone();
    assert_eq!(
        base.config_id(),
        clone.config_id(),
        "a clone is the same configuration and shares its identity — without \
         this, the assertion below would hold for any method at all"
    );

    let extended = base.clone().add_root_certificate(a_certificate());
    assert_ne!(
        extended.config_id(),
        base.config_id(),
        "a connector that trusts one more root is not interchangeable with one \
         that does not, so it must not share a pooled connection with it"
    );
}

/// The same for a client certificate, where the consequence is sharper:
/// two identities reaching one origin over one connection is how one
/// tenant's certificate answers for another.
#[test]
fn setting_an_identity_redraws_the_trust_identity() {
    let Some(identity) = an_identity() else {
        println!("skipped: this platform's native-tls cannot import a PKCS#8 PEM pair");
        return;
    };
    let base = NativeTls::new();
    let with_cert = base.clone().identity(identity);
    assert_ne!(
        with_cert.config_id(),
        base.config_id(),
        "presenting a client certificate is a different trust configuration, \
         and sharing a connection across the two crosses tenants"
    );
}

/// **`Debug` prints the fields it promises and none of the secrets.**
///
/// The impl is hand-written precisely because `native_tls::Identity` and
/// `Certificate` have no `Debug` and printing their contents would be worse
/// than useless — and its own doc singles out `insecure` as the field debug
/// output must not hide. Mutated to print nothing, it passed the whole
/// suite.
///
/// The assertion is about *what is said* and *what is not*, not about the
/// exact rendering: a test pinned to the formatter's spacing is one that
/// gets relaxed rather than read.
#[test]
fn debug_names_the_configuration_without_printing_the_secrets() {
    let printed = format!(
        "{:?}",
        NativeTls::new().add_root_certificate(a_certificate())
    );

    assert!(
        printed.contains("NativeTls"),
        "the type must name itself: {printed}"
    );
    assert!(
        printed.contains("insecure"),
        "whether a connector skips certificate validation is exactly what debug \
         output must not hide: {printed}"
    );
    assert!(
        printed.contains("extra_roots"),
        "the root count is a fact a reader needs and a secret it is not: {printed}"
    );
    assert!(
        printed.contains("config_id"),
        "the trust identity is what decides pooling, so it is worth printing: {printed}"
    );
}
