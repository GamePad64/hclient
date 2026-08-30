//! Why a `Set-Cookie` was not a cookie, and why a cookie was not stored.
//!
//! Two types for two steps, and keeping them apart is this module's whole
//! model: [`ParseError`] is a verdict on the **header** — RFC 6265bis
//! §5.2's "ignore the set-cookie-string entirely" steps, decided with no
//! request in hand — and [`Rejected`] is a verdict on the **cookie in
//! context**, which needs the request's host, scheme and path. A jar can
//! reach the second only past the first, and `Rejected::Malformed` is the
//! `#[from]` that says so.
//!
//! **What they share is that they are reported rather than swallowed.**
//! Every one is a refusal the RFC requires, and every one is a place a
//! cookie would otherwise end up somewhere it does not belong — so the
//! alternative to a name for it is *the cookie silently did not arrive*,
//! which is among the harder things to debug in an HTTP client.
//!
//! **Why these are here and not in `hclient`'s own `error.rs`.** This
//! module was the `hclient-cookie` crate until this year, so by the
//! convention that puts a crate's errors in an `error.rs` it already had
//! one; the fold that made it a module was argued as costing exactly one
//! sentence in `docs/competitive-gaps.md` and nothing else. This module's
//! own doc still says it is sans-io, clockless, and reaches for neither
//! `Client` nor `hclient-core` — and an error type shared with the
//! client's would end the last of those, which is the merge finally
//! costing something it said it did not.
//!
//! Both are re-exported from [`crate::cookie`], where they have always
//! been, so no consumer's `use` line moves.

/// Why a `Set-Cookie` header is not a cookie at all.
///
/// Every variant here corresponds to an "ignore the set-cookie-string
/// entirely" step in §5.2 — as opposed to the per-attribute failures, which
/// are not errors and are simply dropped.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ParseError {
    /// §5.2 step 1: the name-value pair contains no `=`.
    ///
    /// Browsers historically accepted a bare `Set-Cookie: value` as a
    /// nameless cookie; 6265bis removed that and so does this.
    #[error("the name-value pair contains no `=`")]
    NoNameValueSeparator,
    /// §5.2 step 4: an empty name.
    #[error("the cookie name is empty")]
    EmptyName,
    /// §5.2: a control character in the name or the value.
    #[error("the cookie name or value contains a control character")]
    ControlCharacter,
    /// Not a rule of §5.2, and named as such: this crate stores names and
    /// values as `String`, so a header that is not UTF-8 has nowhere to go.
    ///
    /// RFC 6265's own `cookie-octet` production is ASCII, so a conforming
    /// server never trips this; a browser would keep the bytes. The trade
    /// is deliberate — every other field here is a `str` and a `Vec<u8>`
    /// pair of accessors for this one would leak into the whole public API.
    #[error("the cookie name or value is not valid UTF-8")]
    NonUtf8,
}

/// Why a cookie was not stored — by [`CookieJar::store`](super::CookieJar::store) from a
/// `Set-Cookie`, or by [`CookieJar::restore`](super::CookieJar::restore)
/// from a saved [`CookieRecord`](super::CookieRecord).
///
/// Everything here is a refusal the RFC requires, and each one is a place a
/// cookie would otherwise end up somewhere it does not belong. They are
/// reported rather than swallowed because "the cookie silently did not
/// arrive" is among the harder things to debug in an HTTP client.
///
/// **One error type for both entry points rather than two**, because
/// they are the same refusals asked of two different inputs: §4.1.3's
/// name prefixes, the public-suffix rule and [`Limits`](super::Limits) apply to a
/// restored cookie exactly as they apply to a fresh one. The three
/// variants only `restore` can produce are marked as such; the two only
/// `store` can produce name a request that `restore` does not have.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Rejected {
    /// The header is not a cookie at all — see [`ParseError`].
    #[error("malformed Set-Cookie: {0}")]
    Malformed(#[from] ParseError),
    /// The request URI has no host, so there is nothing to scope a cookie
    /// to.
    #[error("the request URI has no host")]
    NoHost,
    /// §5.7: the `Domain` attribute does not domain-match the request host.
    /// The sibling-domain case (`Domain=example.com` from
    /// `notexample.com`) lands here.
    #[error("Domain={domain} does not match request host {host}")]
    DomainMismatch { domain: String, host: String },
    /// §5.7: the `Domain` attribute is a public suffix and is not the
    /// request host itself — `Domain=co.uk`, which would put one cookie on
    /// every registrant under `.uk`.
    #[error("Domain={domain} is a public suffix")]
    DomainIsPublicSuffix { domain: String },
    /// The same refusal, from a build with no list to consult: this crate
    /// without the `public-suffix` feature, or a jar built on
    /// [`NoList`](super::NoList). Every `Domain` attribute is refused
    /// there, so the cause is the build rather than the cookie.
    #[error("Domain={domain} cannot be checked: this build has no public suffix list")]
    NoPublicSuffixList { domain: String },
    /// §5.7: a `Secure` cookie offered over a scheme that is not secure.
    #[error("a Secure cookie was offered over a non-secure request")]
    SecureOverInsecure,
    /// §4.1.3: a `__Secure-` name without `Secure`, or over a non-secure
    /// request.
    #[error("the __Secure- prefix requires a Secure cookie over a secure request")]
    SecurePrefix,
    /// §4.1.3: a `__Host-` name that is not `Secure`, not host-only, or
    /// not scoped to `/`.
    #[error("the __Host- prefix requires Secure, no Domain, and Path=/")]
    HostPrefix,
    /// [`Limits::max_name_value_bytes`](super::Limits::max_name_value_bytes).
    #[error("name and value are {bytes} bytes, over the {limit}-byte limit")]
    TooLarge { bytes: usize, limit: usize },
    /// [`restore`](super::CookieJar::restore) only: a record whose
    /// `domain` is empty, or is nothing but the leading `.` §5.2.3 strips.
    /// A `Set-Cookie` cannot reach this — an empty `Domain` attribute is
    /// no attribute at all (§5.2.3) and the request host stands in.
    #[error("the record has no domain")]
    EmptyDomain,
    /// [`restore`](super::CookieJar::restore) only: a record whose `path`
    /// is not absolute. A `Set-Cookie` cannot reach this either — §5.2.4
    /// makes such a `Path` unspecified and the default path stands in,
    /// and there is no request here for a default to be derived from.
    #[error("Path={path} is not absolute")]
    RelativePath { path: String },
    /// [`restore`](super::CookieJar::restore) only: a record scoped to an
    /// IP literal that is **not** host-only.
    ///
    /// §5.7 makes an IP-literal host host-only unconditionally, so
    /// [`store`](super::CookieJar::store) cannot produce this pair — and
    /// §5.1.3's domain-match is not written to survive it. Measured:
    /// `domain_matches("evil.1.2.3.4", "1.2.3.4")` is **true**, because
    /// the IP test in that rule asks whether the *request host* is a
    /// literal, and `evil.1.2.3.4` is an ordinary name. So the pair is a
    /// cookie for every name ending in `.1.2.3.4`.
    #[error("Domain={domain} is an IP literal, so the cookie must be host-only")]
    IpDomainNotHostOnly { domain: String },
}
