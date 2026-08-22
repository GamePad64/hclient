//! What one transport over two stacks may promise, and what it refuses to
//! be built from.
//!
//! Two halves, and they check different things. The first measures the
//! **actual** capability sets `hclient-native` and `hclient-h3` report in
//! this tree and pins the composite against them, so the answer moves when
//! a member moves rather than when this file is edited. The second drives
//! [`combine`] directly with hand-assembled `Capabilities`, because only
//! one of its refusals is reachable from a `Native` and an `H3` built here
//! today — a rule whose other arms could only be exercised by a member that
//! does not exist yet would otherwise ship unpinned.
#![cfg(all(feature = "http3", not(target_family = "wasm")))]

use hclient_core::unversioned::Transport;
use hclient_core::{
    CancelSupport, Capabilities, DecompressionSupport, EarlyDataSupport, RedirectSupport,
    ReuseSupport, TlsSupport,
};
use hclient_dns::IpLiteralOnly;
use hclient_h3::H3;
use hclient_native::Native;
use hclient_native::caps::combine;
use hclient_rt_tokio::TokioHandle;
use hclient_tls_rustls::Rustls;
use std::sync::Arc;

/// A TLS backend with an empty trust store. Nothing here connects, and an
/// empty `RootCertStore` keeps `webpki-roots` out of this crate's
/// dev-dependencies for a value no assertion reads.
/// A resolver that owns no key and says it has one, which is all
/// `presents_client_certs` reads. A real certificate would need `rcgen`
/// and a signer here to prove a property about *reporting*.
#[derive(Debug)]
struct HasCerts;

impl rustls::client::ResolvesClientCert for HasCerts {
    fn resolve(
        &self,
        _: &[&[u8]],
        _: &[rustls::SignatureScheme],
    ) -> Option<Arc<rustls::sign::CertifiedKey>> {
        None
    }
    fn has_certs(&self) -> bool {
        true
    }
}

/// The same backend as [`tls`], differing in the one field under test.
fn tls_with_client_cert() -> Rustls {
    let mut cfg = rustls::ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();
    cfg.client_auth_cert_resolver = Arc::new(HasCerts);
    Rustls::from_config(Arc::new(cfg))
}

fn tls() -> Rustls {
    let cfg = rustls::ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();
    Rustls::from_config(Arc::new(cfg))
}

type Tcp = Native<TokioHandle, Rustls, IpLiteralOnly>;
type Quic = H3<TokioHandle, Rustls, IpLiteralOnly>;

fn stacks() -> (Tcp, Quic) {
    let rt = TokioHandle::current().expect("inside #[tokio::test]");
    (
        Native::new(rt.clone(), tls(), IpLiteralOnly),
        H3::new(rt, tls(), IpLiteralOnly).expect("H3::new does no I/O"),
    )
}

// --- the two stacks as they actually are, today -------------------------

/// The set of fields on which the two members disagree, **measured** rather
/// than listed from the design document.
///
/// The obvious examples to cite are `redirects` and `full_duplex`, and a
/// list written by hand goes stale: `RedirectSupport::Configurable` was
/// deleted and `version_select` turned on for both while one was being
/// written. This is the set that is really
/// there, and it is asserted whole so that a member changing a capability
/// arrives here as a red test rather than as a silent change of what this
/// transport promises.
#[tokio::test(flavor = "multi_thread")]
async fn the_two_stacks_disagree_on_exactly_six_fields_today() {
    let (tcp, quic) = stacks();
    let (t, q) = (tcp.capabilities(), quic.capabilities());

    let mut differ: Vec<&str> = Vec::new();
    if t.streaming_request_body != q.streaming_request_body {
        differ.push("streaming_request_body");
    }
    if t.full_duplex != q.full_duplex {
        differ.push("full_duplex");
    }
    if t.request_trailers != q.request_trailers {
        differ.push("request_trailers");
    }
    if t.response_trailers != q.response_trailers {
        differ.push("response_trailers");
    }
    if t.redirects != q.redirects {
        differ.push("redirects");
    }
    if t.cancel_on_drop != q.cancel_on_drop {
        differ.push("cancel_on_drop");
    }
    if t.connection_reuse != q.connection_reuse {
        differ.push("connection_reuse");
    }
    if t.response_decompression != q.response_decompression {
        differ.push("response_decompression");
    }
    if t.early_data != q.early_data {
        differ.push("early_data");
    }
    if t.tls_config != q.tls_config {
        differ.push("tls_config");
    }
    if t.client_certs != q.client_certs {
        differ.push("client_certs");
    }
    if t.proxy != q.proxy {
        differ.push("proxy");
    }
    if t.owns_cookie_jar != q.owns_cookie_jar {
        differ.push("owns_cookie_jar");
    }
    if t.owns_cache != q.owns_cache {
        differ.push("owns_cache");
    }
    if t.version_select != q.version_select {
        differ.push("version_select");
    }
    if t.version_reported != q.version_reported {
        differ.push("version_reported");
    }
    if t.timeouts.connect != q.timeouts.connect {
        differ.push("timeouts.connect");
    }
    if t.timeouts.first_byte != q.timeouts.first_byte {
        differ.push("timeouts.first_byte");
    }
    if t.timeouts.between_bytes != q.timeouts.between_bytes {
        differ.push("timeouts.between_bytes");
    }
    if t.informational_1xx != q.informational_1xx {
        differ.push("informational_1xx");
    }
    if t.forbidden_request_headers != q.forbidden_request_headers {
        differ.push("forbidden_request_headers");
    }

    assert_eq!(
        differ,
        [
            "full_duplex",
            "request_trailers",
            "response_trailers",
            "early_data",
            "timeouts.first_byte",
            "timeouts.between_bytes",
        ],
        "the measured disagreement between hclient-native and hclient-h3 has moved; \
         decide what the composite says about the new field before changing this list"
    );
}

/// The composite's answer on each of those seven, from the real members.
///
/// Six take the weaker claim. `early_data` takes the stronger one, and
/// that is the field this rule was hardest to write: see `combine`'s doc.
#[tokio::test(flavor = "multi_thread")]
async fn the_stored_answer_holds_whichever_stack_serves_the_request() {
    let (tcp, quic) = stacks();
    let t = tcp.capabilities().clone();
    let q = quic.capabilities().clone();
    let c = tcp.http3(quic).expect("the two stacks agree today");
    let c = c.capabilities();

    // The six weaker claims, each asserted together with the member that
    // said otherwise — so a test that passed because both members had
    // changed their mind would be visibly wrong rather than quietly green.
    assert!(q.full_duplex && !t.full_duplex);
    assert!(!c.full_duplex, "HTTP/1.1 cannot do duplex at all");

    assert!(q.response_trailers && !t.response_trailers);
    assert!(!c.response_trailers);

    // The seventh, arrived in v0.4 when `hclient-native` stopped
    // under-declaring: it sends request trailers on both protocols it
    // speaks, and `hclient-h3` refuses them with a typed error. The
    // direction is the reverse of the rows above -- here TCP is the one
    // that can -- and the stored answer is still the weaker claim,
    // because a request the QUIC member serves gets no trailers at all.
    assert!(t.request_trailers && !q.request_trailers);
    assert!(!c.request_trailers);

    // `client_certs` was the seventh row here and is gone, because the
    // disagreement was never real: `hclient-h3` said `true` from a
    // constant and `hclient-native` took `Capabilities::none()`'s `false`,
    // so one TLS backend gave two answers depending on which stack held
    // it. Both read `TlsIdentity::presents_client_certs` now, and this
    // fixture's config says `with_no_client_auth()`, so the two agree —
    // asserted here rather than dropped, since "the row disappeared"
    // and "the field stopped being reported" look the same from a
    // shortened list.
    assert!(!t.client_certs && !q.client_certs);
    assert!(!c.client_certs);

    assert!(t.timeouts.first_byte && !q.timeouts.first_byte);
    assert!(!c.timeouts.first_byte);

    assert!(t.timeouts.between_bytes && !q.timeouts.between_bytes);
    assert!(!c.timeouts.between_bytes);

    // And the one both enforce, which must survive: a conjunction that
    // returned `false` for everything would satisfy every assertion above.
    assert!(t.timeouts.connect && q.timeouts.connect);
    assert!(c.timeouts.connect);

    // The stronger claim, and the reason is in `combine`'s doc: nothing in
    // `hclient` reads this field, so `None` would not stop a marked request
    // reaching the QUIC stack and going out in 0-RTT — the weaker-looking
    // value is the one that would be false.
    assert_eq!(t.early_data, EarlyDataSupport::None);
    assert_eq!(q.early_data, EarlyDataSupport::Supported);
    assert_eq!(c.early_data, EarlyDataSupport::Supported);

    // Untouched fields, to catch a floor computed from one member only: if
    // `combine` returned `Capabilities::none()` with the disagreements
    // filled in, every assertion above would still pass and these would
    // not.
    assert!(c.streaming_request_body, "both members stream");
    assert!(c.version_select);
    assert!(c.version_reported);
    assert_eq!(c.redirects, RedirectSupport::Transparent);
    assert_eq!(c.cancel_on_drop, CancelSupport::Supported);
    assert_eq!(c.connection_reuse, ReuseSupport::Supported);
    assert_eq!(c.tls_config, TlsSupport::Full);
}

/// The one refusal a caller can reach with the two members this workspace
/// ships — and it is an ordinary mistake, not a contrived one.
///
/// `Native::without_pool()` is a documented setting (it restores v0.1's
/// one-connection-per-request behaviour) and `hclient-h3` shares
/// connections by construction. `ReuseSupport::None` is not a weaker form
/// of `Supported`: it says every request gets a fresh connection, which is
/// false the moment the QUIC stack answers one.
#[tokio::test(flavor = "multi_thread")]
async fn a_pooling_disagreement_is_refused_at_construction_naming_the_field() {
    let rt = TokioHandle::current().expect("inside #[tokio::test]");
    let tcp = Native::new(rt.clone(), tls(), IpLiteralOnly).without_pool();
    let quic = H3::new(rt.clone(), tls(), IpLiteralOnly).expect("H3::new does no I/O");

    let err = tcp.http3(quic).expect_err("these two cannot be one");
    assert_eq!(err.field, "connection_reuse");
    assert_eq!(err.tcp, "None");
    assert_eq!(err.quic, "Supported");
    // The message names the field too: a caller reading a log rather than
    // matching on the type still learns which setting to change.
    assert!(err.to_string().contains("connection_reuse"), "{err}");
}

/// …and with the pool left on, the same two stacks build.
///
/// The control for the test above. Without it, a `Selecting::new` that
/// refused unconditionally would pass it.
#[tokio::test(flavor = "multi_thread")]
async fn the_same_two_stacks_with_the_pool_on_are_one_transport() {
    let (tcp, quic) = stacks();
    assert!(tcp.http3(quic).is_ok());
}

// --- the rule itself, on capability sets no member here produces --------

/// A pair of `Capabilities` differing in exactly one field, built from
/// `none()` so that everything else agrees by construction.
fn pair(f: impl Fn(&mut Capabilities, bool)) -> (Capabilities, Capabilities) {
    let (mut a, mut b) = (Capabilities::none(), Capabilities::none());
    f(&mut a, false);
    f(&mut b, true);
    (a, b)
}

#[test]
fn a_disagreement_on_any_unordered_enum_is_refused_and_names_its_field() {
    // `RedirectSupport` is the sharpest example: three variants, no
    // order between them, and `None` is not a weaker
    // `Transparent` — it is the stronger claim that redirects are
    // impossible.
    let (a, b) = pair(|c, on| {
        c.redirects = if on {
            RedirectSupport::Internal
        } else {
            RedirectSupport::Transparent
        }
    });
    assert_eq!(combine(&a, &b).unwrap_err().field, "redirects");

    // A duty owed on every dropped future, so a member that does not owe
    // it falsifies the claim. This is the contrast that makes `early_data`
    // different rather than inconsistent.
    let (a, b) = pair(|c, on| {
        c.cancel_on_drop = if on {
            CancelSupport::Supported
        } else {
            CancelSupport::None
        }
    });
    assert_eq!(combine(&a, &b).unwrap_err().field, "cancel_on_drop");

    // Getting this one wrong corrupts rather than degrades: `None` against
    // a member that already decoded makes `Client` decode twice.
    let (a, b) = pair(|c, on| {
        c.response_decompression = if on {
            DecompressionSupport::Internal
        } else {
            DecompressionSupport::None
        }
    });
    assert_eq!(combine(&a, &b).unwrap_err().field, "response_decompression");

    let (a, b) = pair(|c, on| {
        c.tls_config = if on {
            TlsSupport::Full
        } else {
            TlsSupport::None
        }
    });
    assert_eq!(combine(&a, &b).unwrap_err().field, "tls_config");
}

/// The two "the transport already does this itself" flags, which are
/// `bool`s and are still refusals.
///
/// This is the pair that shows the rule is about what a value *says*
/// rather than about its type. `false` here does not ask the caller to
/// assume less — it tells `Client` to run a jar of its own, which would
/// double up against a member that keeps one; and `true` tells it not to,
/// which drops cookies for the member that does not. Neither is weaker.
#[test]
fn owning_a_jar_or_a_cache_is_a_refusal_rather_than_a_conjunction() {
    let (a, b) = pair(|c, on| c.owns_cookie_jar = on);
    assert_eq!(combine(&a, &b).unwrap_err().field, "owns_cookie_jar");

    let (a, b) = pair(|c, on| c.owns_cache = on);
    assert_eq!(combine(&a, &b).unwrap_err().field, "owns_cache");
}

/// `forbidden_request_headers` refuses because the type leaves nothing
/// else: the honest combination is the union of the two lists, and
/// `&'static [HeaderName]` has nowhere to put a slice computed at
/// construction, because `capabilities()` returns a reference and the
/// answer must therefore be stored.
#[test]
fn two_different_forbidden_header_lists_have_no_honest_union_to_store() {
    let mut a = Capabilities::none();
    let mut b = Capabilities::none();
    a.forbidden_request_headers = &[http::header::COOKIE];
    b.forbidden_request_headers = &[http::header::ACCEPT_ENCODING];
    assert_eq!(
        combine(&a, &b).unwrap_err().field,
        "forbidden_request_headers"
    );

    // Equal lists are not a disagreement, including when they are equal
    // and non-empty.
    b.forbidden_request_headers = &[http::header::COOKIE];
    assert!(combine(&a, &b).is_ok());
}

/// Every `bool` that is a claim about what a caller may assume takes the
/// conjunction, in both directions.
///
/// Both directions, because a rule implemented as "take the first
/// member's value" passes a one-directional test on every field.
#[test]
fn a_capability_only_one_member_has_is_not_promised_by_the_pair() {
    for field in [
        "streaming_request_body",
        "full_duplex",
        "request_trailers",
        "response_trailers",
        "client_certs",
        "proxy",
        "informational_1xx",
        "version_select",
        "version_reported",
        "timeouts.connect",
        "timeouts.first_byte",
        "timeouts.between_bytes",
    ] {
        for swapped in [false, true] {
            let mut yes = Capabilities::none();
            set(&mut yes, field, true);
            let no = Capabilities::none();
            let (a, b) = if swapped { (&no, &yes) } else { (&yes, &no) };
            let c = combine(a, b).expect("a bool disagreement is never a refusal");
            assert!(
                !get(&c, field),
                "`{field}` was promised by a pair in which only one member has it (swapped: {swapped})"
            );
        }
        // …and both saying yes really does reach the composite, or the
        // assertion above would be satisfied by a function returning
        // `Capabilities::none()`.
        let mut yes = Capabilities::none();
        set(&mut yes, field, true);
        let c = combine(&yes, &yes).unwrap();
        assert!(
            get(&c, field),
            "`{field}` was lost although both members have it"
        );
    }
}

fn set(c: &mut Capabilities, field: &str, v: bool) {
    match field {
        "streaming_request_body" => c.streaming_request_body = v,
        "full_duplex" => c.full_duplex = v,
        "request_trailers" => c.request_trailers = v,
        "response_trailers" => c.response_trailers = v,
        "client_certs" => c.client_certs = v,
        "proxy" => c.proxy = v,
        "informational_1xx" => c.informational_1xx = v,
        "version_select" => c.version_select = v,
        "version_reported" => c.version_reported = v,
        "timeouts.connect" => c.timeouts.connect = v,
        "timeouts.first_byte" => c.timeouts.first_byte = v,
        "timeouts.between_bytes" => c.timeouts.between_bytes = v,
        other => panic!("unknown field `{other}`"),
    }
}

fn get(c: &Capabilities, field: &str) -> bool {
    match field {
        "streaming_request_body" => c.streaming_request_body,
        "full_duplex" => c.full_duplex,
        "request_trailers" => c.request_trailers,
        "response_trailers" => c.response_trailers,
        "client_certs" => c.client_certs,
        "proxy" => c.proxy,
        "informational_1xx" => c.informational_1xx,
        "version_select" => c.version_select,
        "version_reported" => c.version_reported,
        "timeouts.connect" => c.timeouts.connect,
        "timeouts.first_byte" => c.timeouts.first_byte,
        "timeouts.between_bytes" => c.timeouts.between_bytes,
        other => panic!("unknown field `{other}`"),
    }
}

/// `early_data` is the one field where either member having it is enough,
/// and it is asserted in both directions so that "take the QUIC member's
/// value" does not pass for it.
#[test]
fn either_member_offering_early_data_is_enough_for_the_pair_to_offer_it() {
    let none = Capabilities::none();
    let mut supported = Capabilities::none();
    supported.early_data = EarlyDataSupport::Supported;

    assert_eq!(
        combine(&none, &supported).unwrap().early_data,
        EarlyDataSupport::Supported
    );
    assert_eq!(
        combine(&supported, &none).unwrap().early_data,
        EarlyDataSupport::Supported
    );
    assert_eq!(
        combine(&none, &none).unwrap().early_data,
        EarlyDataSupport::None
    );
}

// --- the tripwire for a field nobody decided about ----------------------

/// `Capabilities` is `#[non_exhaustive]`, so no destructuring `let` outside
/// `hclient-core` can be made exhaustive and there is no compile error when
/// a field is added — it would simply arrive in [`combine`]'s output as
/// `Capabilities::none()`'s value, decided by nobody.
///
/// So the guard is this test. It reads the field names off `Debug`, which
/// is derived and therefore lists every field, and fails when the set
/// moves. Whoever adds a field decides what a pair of stacks says about it
/// and adds it here.
#[test]
fn every_capability_field_is_accounted_for_and_a_new_one_fails_this_test() {
    let printed = format!("{:?}", Capabilities::none());
    assert_eq!(
        top_level_fields(&printed),
        [
            "streaming_request_body",
            "full_duplex",
            "request_trailers",
            "response_trailers",
            "redirects",
            "cancel_on_drop",
            "connection_reuse",
            "response_decompression",
            "early_data",
            "tls_config",
            "client_certs",
            "proxy",
            "owns_cookie_jar",
            "owns_cache",
            "version_select",
            "version_reported",
            "timeouts",
            "informational_1xx",
            "forbidden_request_headers",
        ],
        "`Capabilities` has changed shape; `hclient_native::caps::combine` must say \
         what a pair of stacks reports for the new field before this list moves"
    );
}

/// The field names of a derived `Debug` for a struct, at the top level
/// only — nested `TimeoutSupport { .. }` contributes its own name and not
/// its members'.
fn top_level_fields(printed: &str) -> Vec<String> {
    let inner = printed
        .split_once('{')
        .expect("a derived struct Debug has a brace")
        .1;
    let mut depth = 0i32;
    let mut names = Vec::new();
    let mut current = String::new();
    for ch in inner.chars() {
        match ch {
            '{' | '[' | '(' => depth += 1,
            '}' | ']' | ')' => {
                depth -= 1;
                if depth < 0 {
                    break;
                }
            }
            ',' if depth == 0 => current.clear(),
            ':' if depth == 0 => {
                names.push(current.trim().to_owned());
                current.clear();
            }
            _ if depth == 0 => current.push(ch),
            _ => {}
        }
    }
    names
}

/// **The field follows the TLS backend on both stacks**, which is the
/// claim the removed disagreement row was hiding: the same connector
/// carrying a client certificate is reported by the TCP member, by the
/// QUIC member and by the composite alike. The control is the fixture
/// three lines up — an identical config differing only in its resolver,
/// where all three say `false`.
#[tokio::test]
async fn a_client_certificate_is_reported_by_both_stacks_and_by_the_pair() {
    let rt = TokioHandle::current().expect("inside #[tokio::test]");
    let tcp = Native::new(rt.clone(), tls_with_client_cert(), IpLiteralOnly);
    let quic =
        H3::new(rt.clone(), tls_with_client_cert(), IpLiteralOnly).expect("H3::new does no I/O");
    assert!(tcp.capabilities().client_certs, "the TCP member");
    assert!(quic.capabilities().client_certs, "the QUIC member");

    let pair = tcp.http3(quic).expect("the two agree");
    assert!(
        pair.capabilities().client_certs,
        "and the conjunction, which is only true because neither member is a constant"
    );
}
