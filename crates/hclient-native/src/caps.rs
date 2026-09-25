//! One [`Capabilities`] for two stacks, and the refusal when there cannot
//! be one.
//!
//! # The rule, in one sentence
//!
//! **The stored value must be a statement that is true whichever member
//! serves the request** — because under a selecting transport the caller
//! does not know which one did, so a promise that holds for one and not the
//! other is not a promise.
//!
//! That single criterion produces all three of the answers below; none of
//! them is a policy chosen on top of it.
//!
//! - Where one of the two values is the **weaker claim** — it asks the
//!   caller to assume less and forbids them nothing the stronger one
//!   forbids — the weaker value is true of the composite and is stored.
//!   Every `bool` here is of this shape: `false` is "do not assume this",
//!   and it stays true when one member can do the thing and the other
//!   cannot.
//! - Where the two values are **different claims** rather than a stronger
//!   and a weaker one, no value is true of the composite and
//!   [`combine`] refuses, naming the field. Every remaining enum is of
//!   this shape, and so are the two flags that say *the transport already
//!   does this itself*: `owns_cookie_jar: false` makes `Client` run a jar
//!   (which would double up against the member that owns one) and `true`
//!   makes it run none (which drops cookies for the member that does not).
//!   Neither is weaker; both are wrong.
//! - [`Capabilities::early_data`] is the one field where the *stronger*
//!   value is the true one, and that is a property of what its variant
//!   says rather than an exception to the rule. See the `early_data` helper below.
//!
//! # Why not "report the meet"
//!
//! `RedirectSupport`'s three surviving variants are unordered, and
//! inventing an order over them to make a meet exist is deciding a
//! semantic question in order to satisfy a helper function. The
//! rule above never asks for an order — it asks which value is true — and
//! where the answer is "neither", it says so.
//!
//! # A field added to `Capabilities` later arrives here as `none()`'s value
//!
//! `Capabilities` is `#[non_exhaustive]`, so a destructuring `let` outside
//! `hclient-core` needs `..` and cannot be made exhaustive: there is no
//! compile-time guard that every field was considered. The tripwire is a
//! test instead —
//! `every_capability_field_is_accounted_for_and_a_new_one_fails_this_test`
//! in `tests/capabilities.rs` counts the fields off the `Debug` output and
//! fails when the count moves.

use crate::error::Disagreement;

use hclient_core::caps::Capabilities;
use std::fmt::Debug;

/// The value both members can be held to, or the first field on which
/// there is none.
///
/// **The first**, not all of them: the shape is `UnsupportedCapability`'s,
/// which also names one setting, and a caller fixes one member at a time.
/// The order the fields are checked in is the order they are declared on
/// [`Capabilities`], so which one is reported is stable rather than
/// incidental.
///
/// Crate-private: its one caller is `Native::http3`, which is the only way
/// to build a transport over two stacks. It was public so that
/// `tests/capabilities.rs` could reach the refusals no member here
/// produces, and those are unit tests at the bottom of this file now: a
/// test is a reason to reach a function, not a reason to promise it.
///
/// # Errors
///
/// [`Disagreement`] on the first field where `tcp` and `quic` report
/// different claims that neither the weaker-claim-wins rule nor
/// `early_data`'s stronger-claim-wins rule can reconcile. `redirects`,
/// `cancel_on_drop`, `connection_reuse`, `response_decompression`,
/// `tls_config`, `owns_cookie_jar`, `owns_cache` and
/// `forbidden_request_headers` are checked in that order, and the error
/// names whichever one disagrees first.
pub(crate) fn combine(
    tcp: &Capabilities,
    quic: &Capabilities,
) -> Result<Capabilities, Disagreement> {
    // Built from `Capabilities::default()` and filled in field by field, for
    // the reason every backend here does the same: the struct is
    // `#[non_exhaustive]`, so a literal would not compile, and a field
    // added later must arrive as the conservative default rather than as a
    // compile error somebody silences by copying its neighbour.
    let mut c = Capabilities::default();

    // --- the weaker claim, which is `false` for every `bool` here -------
    //
    // `full_duplex` is the one that pays for the rule. `hclient-native`
    // already answers exactly this question one level down — HTTP/1.1
    // cannot do duplex, HTTP/2 can, one transport reports one value — and
    // its answer is the floor, written down beside the cost: over-claiming
    // `full_duplex` deadlocks a caller structured for bidirectional
    // streaming, where under-claiming costs it a buffered copy. The same
    // question one level up gets the same answer for the same reason; the
    // rule is imported from the crate that already had to make it, not
    // invented here.
    //
    // The rest follow without a second argument. `response_trailers:
    // false` costs a caller the trailers it would not have looked for;
    // `true` would have it look for trailers on a connection that cannot
    // carry them. `client_certs`, `proxy`, `informational_1xx`,
    // `streaming_request_body` and `request_trailers` are the same shape.
    c.streaming_request_body = tcp.streaming_request_body && quic.streaming_request_body;
    c.full_duplex = tcp.full_duplex && quic.full_duplex;
    c.request_trailers = tcp.request_trailers && quic.request_trailers;
    c.response_trailers = tcp.response_trailers && quic.response_trailers;
    c.client_certs = tcp.client_certs && quic.client_certs;
    c.proxy = tcp.proxy && quic.proxy;
    c.informational_1xx = tcp.informational_1xx && quic.informational_1xx;
    // `version_select: false` makes `Client` refuse a `RequireVersion` at
    // the `UnsupportedCapability` gate, and `version_reported: false` tells
    // a caller not to trust `Response::version()`. Both are the weaker
    // claim, and both are `true` on both stacks today, so the conjunction
    // changes nothing — it is here so that a member that stops honouring
    // demands takes the composite with it.
    c.version_select = tcp.version_select && quic.version_select;
    c.version_reported = tcp.version_reported && quic.version_reported;
    // A bound is enforced by the composite only if it is enforced whichever
    // stack runs, and this is the field where the two disagree in *both*
    // directions today: `hclient-native` enforces `first_byte` and
    // `between_bytes` and `hclient-h3` does not, while `connect` is
    // enforced by both. Declaring a bound that one stack silently ignores
    // is the exact no-op v0.2 W4 made this field exist to prevent.
    //
    // `resolve` read `tcp.timeouts.connect && quic.timeouts.connect` until
    // this line was rewritten — the **connect** fields of both members, on
    // the row that reports `resolve`. Latent rather than live: both stacks
    // answer `true` to both today, so the two expressions agree and no
    // test could tell them apart. It would have become a capability that
    // lies the moment one member stopped bounding one of the two, which is
    // precisely the drift the pair exists to catch.
    c.timeouts = hclient_core::caps::TimeoutSupport::none()
        .with_resolve(tcp.timeouts.resolve && quic.timeouts.resolve)
        .with_connect(tcp.timeouts.connect && quic.timeouts.connect)
        .with_first_byte(tcp.timeouts.first_byte && quic.timeouts.first_byte)
        .with_between_bytes(tcp.timeouts.between_bytes && quic.timeouts.between_bytes);

    // --- the field where the stronger value is the true one -------------
    c.early_data = early_data(tcp, quic);

    // --- different claims, so no value is true of the pair --------------
    //
    // Each of these is a statement about what happens on *every* request,
    // and a member that does the opposite falsifies it. There is no order
    // to fall back on and inventing one is P4's mistake.
    c.redirects = same("redirects", &tcp.redirects, &quic.redirects)?;
    // `cancel_on_drop` is the clearest of them, and the contrast with
    // `early_data` below is the whole distinction: `Supported` here is a
    // **duty owed on every dropped future**, so a member that does not owe
    // it makes the claim false, where `true` is an
    // ability that need not be exercised on any given request.
    c.cancel_on_drop = same("cancel_on_drop", &tcp.cancel_on_drop, &quic.cancel_on_drop)?;
    c.connection_reuse = same(
        "connection_reuse",
        &tcp.connection_reuse,
        &quic.connection_reuse,
    )?;
    // Neither direction is weaker, and getting it wrong corrupts rather
    // than degrades: `None` against a member that already decoded the body
    // makes `Client` decode it twice, and the other way round hands the
    // caller gzip bytes it was told were plain.
    c.response_decompression = same(
        "response_decompression",
        &tcp.response_decompression,
        &quic.response_decompression,
    )?;
    c.tls_config = same("tls_config", &tcp.tls_config, &quic.tls_config)?;
    // *The transport already does this itself.* Both values are wrong when
    // the members disagree — see this module's doc.
    c.owns_cookie_jar = same(
        "owns_cookie_jar",
        &tcp.owns_cookie_jar,
        &quic.owns_cookie_jar,
    )?;
    c.owns_cache = same("owns_cache", &tcp.owns_cache, &quic.owns_cache)?;
    // The honest combination is the **union** — a header one member refuses
    // to send is a header this transport may not promise to send — and the
    // type cannot hold one: `&'static [HeaderName]` has nowhere to put a
    // slice computed at construction, because `capabilities()` returns a
    // reference and the answer must therefore be stored. Equality is what
    // is left, and both stacks say `&[]` today.
    c.forbidden_request_headers = if tcp.forbidden_request_headers == quic.forbidden_request_headers
    {
        tcp.forbidden_request_headers
    } else {
        return Err(Disagreement::new(
            "forbidden_request_headers",
            &tcp.forbidden_request_headers,
            &quic.forbidden_request_headers,
        ));
    };

    Ok(c)
}

/// `Supported` if either member offers early data.
///
/// **The only field here whose answer is the stronger of the two values**,
/// and the reason is what the variant says rather than an exception to this
/// module's rule. [`true`] says the transport *can*
/// place a request the caller marked with `AllowEarlyData` into early data
/// — it promises nothing about any particular request, and `hclient-h3`
/// alone already does not place the first request to an origin there,
/// because there is no session ticket yet. So "this transport can offer
/// early data for a marked request" stays true of the pair, while `false`
/// — "this transport never offers early data" — is false of it, and false
/// in the direction that matters: nothing in `hclient` reads this field,
/// so reporting `false` would not stop a marked request reaching the QUIC
/// stack and going out in 0-RTT anyway. The weaker-looking value is the
/// lie.
///
/// The safety decision is untouched, and it is the reason this can be said
/// at all: early data is entered only for a request the **caller** marked,
/// per request, and this transport does not mark anything on their behalf.
///
fn early_data(tcp: &Capabilities, quic: &Capabilities) -> bool {
    tcp.early_data || quic.early_data
}

/// The value if both members give it, and a [`Disagreement`] naming the
/// field if they do not.
fn same<V: PartialEq + Copy + Debug>(
    field: &'static str,
    tcp: &V,
    quic: &V,
) -> Result<V, Disagreement> {
    if tcp == quic {
        Ok(*tcp)
    } else {
        Err(Disagreement::new(field, tcp, quic))
    }
}

/// The rule, driven with hand-assembled `Capabilities` rather than with
/// real members: only one of its refusals (`connection_reuse`) is reachable
/// from a `Native` and an `H3` built in this workspace, and
/// `tests/capabilities.rs` pins that one and the measured composite. These
/// were in that file once, which is why [`combine`] was public with no
/// caller outside this crate.
#[cfg(test)]
mod tests {
    use super::combine;
    use hclient_core::caps::{Capabilities, RedirectSupport, TlsSupport};

    // --- the rule itself, on capability sets no member here produces --------

    /// A pair of `Capabilities` differing in exactly one field, built from
    /// `none()` so that everything else agrees by construction.
    fn pair(f: impl Fn(&mut Capabilities, bool)) -> (Capabilities, Capabilities) {
        let (mut a, mut b) = (Capabilities::default(), Capabilities::default());
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
        let (a, b) = pair(|c, on| c.cancel_on_drop = on);
        assert_eq!(combine(&a, &b).unwrap_err().field, "cancel_on_drop");

        // Getting this one wrong corrupts rather than degrades: `false` against
        // a member that already decoded makes `Client` decode twice.
        let (a, b) = pair(|c, on| c.response_decompression = on);
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
        let mut a = Capabilities::default();
        let mut b = Capabilities::default();
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
            // `resolve` was absent from this list from the day the field
            // arrived in v0.4 until the composite was rewritten, and the
            // composite computed it from **`connect`** on both members the
            // whole time. Both stacks answer `true` to both, so no run could
            // tell the two expressions apart — a capability that would have
            // started lying the moment one member stopped bounding one of
            // them. A list of field names is exactly as complete as the last
            // person to extend it.
            "timeouts.resolve",
            "timeouts.connect",
            "timeouts.first_byte",
            "timeouts.between_bytes",
        ] {
            for swapped in [false, true] {
                let mut yes = Capabilities::default();
                set(&mut yes, field, true);
                let no = Capabilities::default();
                let (a, b) = if swapped { (&no, &yes) } else { (&yes, &no) };
                let c = combine(a, b).expect("a bool disagreement is never a refusal");
                assert!(
                    !get(&c, field),
                    "`{field}` was promised by a pair in which only one member has it (swapped: {swapped})"
                );
            }
            // …and both saying yes really does reach the composite, or the
            // assertion above would be satisfied by a function returning
            // `Capabilities::default()`.
            let mut yes = Capabilities::default();
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
            "timeouts.resolve" => c.timeouts.resolve = v,
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
            "timeouts.resolve" => c.timeouts.resolve,
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
        let none = Capabilities::default();
        let mut supported = Capabilities::default();
        supported.early_data = true;

        assert!(combine(&none, &supported).unwrap().early_data);
        assert!(combine(&supported, &none).unwrap().early_data);
        assert!(!combine(&none, &none).unwrap().early_data);
    }

    // --- the tripwire for a field nobody decided about ----------------------

    /// `Capabilities` is `#[non_exhaustive]`, so no destructuring `let` outside
    /// `hclient-core` can be made exhaustive and there is no compile error when
    /// a field is added — it would simply arrive in [`combine`]'s output as
    /// `Capabilities::default()`'s value, decided by nobody.
    ///
    /// So the guard is this test. It reads the field names off `Debug`, which
    /// is derived and therefore lists every field, and fails when the set
    /// moves. Whoever adds a field decides what a pair of stacks says about it
    /// and adds it here.
    #[test]
    fn every_capability_field_is_accounted_for_and_a_new_one_fails_this_test() {
        let printed = format!("{:?}", Capabilities::default());
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
            "`Capabilities` has changed shape; `caps::combine` must say \
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
}
