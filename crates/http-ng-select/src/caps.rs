//! One [`Capabilities`] for two stacks, and the refusal when there cannot
//! be one.
//!
//! # The rule, in one sentence
//!
//! **The stored value must be a statement that is true whichever member
//! serves the request** — because under a selecting transport the caller
//! does not know which one did, so a promise that holds for one and not the
//! other is not a promise (`docs/v04-design.md` §W1).
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
//!   says rather than an exception to the rule. See [`early_data`] below.
//!
//! # Why not "report the meet"
//!
//! `docs/v04-design.md` P4: `RedirectSupport`'s three surviving variants
//! are unordered, and inventing an order over them to make a meet exist is
//! deciding a semantic question in order to satisfy a helper function. The
//! rule above never asks for an order — it asks which value is true — and
//! where the answer is "neither", it says so.
//!
//! # A field added to `Capabilities` later arrives here as `none()`'s value
//!
//! `Capabilities` is `#[non_exhaustive]`, so a destructuring `let` outside
//! `http-ng-core` needs `..` and cannot be made exhaustive: there is no
//! compile-time guard that every field was considered. The tripwire is a
//! test instead —
//! `every_capability_field_is_accounted_for_and_a_new_one_fails_this_test`
//! in `tests/capabilities.rs` counts the fields off the `Debug` output and
//! fails when the count moves.

use http_ng_core::Capabilities;

/// Two stacks that cannot be given one honest answer for one field.
///
/// Returned from [`combine`], and therefore from
/// [`Selecting::new`](crate::Selecting::new) — the same shape as
/// `UnsupportedCapability` at `ClientBuilder::build()`, and for the same
/// reason: the error arrives where the mistake was made, rather than as a
/// surprise on the first request that happens to take the other stack.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "the two stacks disagree on `{field}`, and neither value is true of the pair: the TCP stack says `{tcp}`, the QUIC stack says `{quic}`"
)]
pub struct Disagreement {
    /// The `Capabilities` field, by its own name.
    pub field: &'static str,
    /// What `http-ng-native` said, formatted with `Debug`.
    pub tcp: String,
    /// What `http-ng-h3` said, formatted with `Debug`.
    pub quic: String,
}

impl Disagreement {
    fn new<V: std::fmt::Debug>(field: &'static str, tcp: &V, quic: &V) -> Self {
        Self {
            field,
            tcp: format!("{tcp:?}"),
            quic: format!("{quic:?}"),
        }
    }
}

/// The value both members can be held to, or the first field on which
/// there is none.
///
/// **The first**, not all of them: the shape is `UnsupportedCapability`'s,
/// which also names one setting, and a caller fixes one member at a time.
/// The order the fields are checked in is the order they are declared on
/// [`Capabilities`], so which one is reported is stable rather than
/// incidental.
///
/// Public because the decision is worth reading and worth testing directly:
/// only one of the refusals below is reachable from a `Native` and an `H3`
/// built in this workspace today (`connection_reuse`, via
/// `Native::without_pool`), and a rule whose other arms can only be
/// exercised by a member that does not exist yet would otherwise be
/// unpinned.
pub fn combine(tcp: &Capabilities, quic: &Capabilities) -> Result<Capabilities, Disagreement> {
    // Built from `Capabilities::none()` and filled in field by field, for
    // the reason every backend here does the same: the struct is
    // `#[non_exhaustive]`, so a literal would not compile, and a field
    // added later must arrive as the conservative default rather than as a
    // compile error somebody silences by copying its neighbour.
    let mut c = Capabilities::none();

    // --- the weaker claim, which is `false` for every `bool` here -------
    //
    // `full_duplex` is the one that pays for the rule. `http-ng-native`
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
    // directions today: `http-ng-native` enforces `first_byte` and
    // `between_bytes` and `http-ng-h3` does not, while `connect` is
    // enforced by both. Declaring a bound that one stack silently ignores
    // is the exact no-op v0.2 W4 made this field exist to prevent.
    c.timeouts = http_ng_core::TimeoutSupport {
        resolve: tcp.timeouts.connect && quic.timeouts.connect,
        connect: tcp.timeouts.connect && quic.timeouts.connect,
        first_byte: tcp.timeouts.first_byte && quic.timeouts.first_byte,
        between_bytes: tcp.timeouts.between_bytes && quic.timeouts.between_bytes,
    };

    // --- the field where the stronger value is the true one -------------
    c.early_data = early_data(tcp, quic);

    // --- different claims, so no value is true of the pair --------------
    //
    // Each of these is a statement about what happens on *every* request,
    // and a member that does the opposite falsifies it. There is no order
    // to fall back on and inventing one is P4's mistake.
    c.redirects = same("redirects", &tcp.redirects, &quic.redirects)?;
    // `CancelSupport` is the clearest of them, and the contrast with
    // `early_data` below is the whole distinction: `Supported` here is a
    // **duty owed on every dropped future**, so a member that does not owe
    // it makes the claim false, where `EarlyDataSupport::Supported` is an
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
    // slice computed at construction, which is the same wall
    // `docs/v04-design.md` P3 hit from the other side (`capabilities()`
    // returns a reference, so the answer must be stored). Equality is what
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
/// module's rule. [`EarlyDataSupport::Supported`] says the transport *can*
/// place a request the caller marked with `AllowEarlyData` into early data
/// — it promises nothing about any particular request, and `http-ng-h3`
/// alone already does not place the first request to an origin there,
/// because there is no session ticket yet. So "this transport can offer
/// early data for a marked request" stays true of the pair, while
/// [`EarlyDataSupport::None`] — "this transport never offers early data" —
/// is false of it, and false in the direction that matters: nothing in
/// `http-ng` reads this field, so reporting `None` would not stop a marked
/// request reaching the QUIC stack and going out in 0-RTT anyway. The
/// weaker-looking value is the lie.
///
/// The safety decision is untouched, and it is the reason this can be said
/// at all: early data is entered only for a request the **caller** marked,
/// per request, and this transport does not mark anything on their behalf.
///
/// [`EarlyDataSupport::Supported`]: http_ng_core::EarlyDataSupport::Supported
/// [`EarlyDataSupport::None`]: http_ng_core::EarlyDataSupport::None
fn early_data(tcp: &Capabilities, quic: &Capabilities) -> http_ng_core::EarlyDataSupport {
    use http_ng_core::EarlyDataSupport::{None, Supported};
    match (tcp.early_data, quic.early_data) {
        (None, None) => None,
        _ => Supported,
    }
}

/// The value if both members give it, and a [`Disagreement`] naming the
/// field if they do not.
fn same<V: PartialEq + Copy + std::fmt::Debug>(
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
