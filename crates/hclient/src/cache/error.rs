//! The one thing this cache refuses, and it refuses by not storing.
//!
//! [`NotStored`] is the whole of it, which is the shape of an RFC 9111
//! cache rather than a gap: a lookup cannot fail — a miss is an answer —
//! and neither can a revalidation, which is the transport's exchange and
//! reports as one. What is left is *storing*, where every branch of §3 is
//! a reason to decline, and declining is reported rather than swallowed
//! for the reason the jar reports its refusals: a response that silently
//! failed to cache is indistinguishable from one that cached and was
//! evicted, and only one of those is worth changing a header for.
//!
//! **Why this is here and not in `hclient`'s own `error.rs`.** This module
//! was the `hclient-cache` crate until this year, so by the convention that
//! puts a crate's errors in an `error.rs` it already had one; the fold that
//! made it a module was argued as costing exactly one sentence in
//! `docs/competitive-gaps.md` and nothing else. Its own doc still says it
//! is sans-io, clockless, and reaches for neither `Client` nor
//! `hclient-core` — and an error type shared with the client's would end
//! the last of those.
//!
//! [`NotStored`] is re-exported from [`crate::cache`], where it has always
//! been, so no consumer's `use` line moves.

use http::{Method, StatusCode};

/// Why a response was not stored.
///
/// Reported rather than swallowed, for the reason `hclient-cookie`'s
/// [`Rejected`] carries: *"the response silently was not cached"* is among
/// the harder things to debug in an HTTP client, and every variant here is
/// a rule someone will one day be surprised by. `hclient::Client` drops
/// these — one uncacheable response must not fail an exchange — so this is
/// for whoever drives the cache directly, and for the tests that pin which
/// rule fired.
///
/// [`Rejected`]: https://docs.rs/hclient-cookie
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum NotStored {
    /// Only `GET` responses are stored — see [`HttpCache::lookup`](super::HttpCache::lookup).
    #[error("responses to {0} are not stored")]
    Method(Method),
    /// The request carried its own conditional, or a `Range`.
    #[error("the request carried its own precondition or a Range, so this cache stood aside")]
    RequestStoodAside,
    /// RFC 9111 §5.2.1.5.
    #[error("the request said no-store")]
    RequestNoStore,
    /// The status is not one this cache can reuse — see
    /// `CACHEABLE_STATUSES`, and note that `206` is deliberately not on
    /// it.
    #[error("a {0} response is not stored")]
    Status(StatusCode),
    /// RFC 9111 §5.2.2.5.
    #[error("the response said no-store")]
    ResponseNoStore,
    /// RFC 9111 §4.1: `Vary: *` never matches, so storing it would fill
    /// the cache with entries nothing can reach.
    #[error("the response varies on `*`, which never matches a later request")]
    VaryAsterisk,
    /// Neither `max-age`/`Expires` nor `ETag`/`Last-Modified` — nothing
    /// this cache could ever do with the entry, since it assigns no
    /// heuristic lifetime. See [`HttpCache::storing`](super::HttpCache::storing).
    #[error("the response has neither explicit freshness nor a validator")]
    NothingToReuseItWith,
    /// The request URI is not absolute, so there is nothing to key on.
    #[error("the request URI has no scheme or authority")]
    NoKey,
    /// Larger than [`Limits::max_body_bytes`](super::Limits::max_body_bytes) — by the `Content-Length`
    /// the head declared, or by the bytes that actually arrived.
    #[error("the body is {bytes} bytes, over the {limit}-byte limit")]
    TooLarge { bytes: u64, limit: u64 },
    /// The body that arrived is not the length the head promised. A
    /// truncated entry served later is indistinguishable from a complete
    /// one, which is why this is a refusal and not a repair.
    #[error("the body is {bytes} bytes where Content-Length said {declared}")]
    LengthMismatch { bytes: u64, declared: u64 },
}
