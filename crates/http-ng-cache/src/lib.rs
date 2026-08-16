//! An HTTP response cache: RFC 9111 freshness, validation, `Vary` and the
//! directives on both sides of the exchange — with no I/O and no clock of
//! its own.
//!
//! # Why a crate and not a module in `http-ng`
//!
//! The same reason `http-ng-cookie` is one, and the same reason
//! `http-ng-proto` is. A cache is *used* by `http-ng`, but the substance of
//! it — §3's storability, §4's reuse, §4.2's arithmetic — is a pure
//! function of a request, a response, a store and a `now`. Putting it here
//! makes "caching behaves the same on every backend" a structural fact
//! rather than a consequence of everyone happening to go through `Client`.
//!
//! This crate is a leaf: `bytes`, `http` and `thiserror`. Not
//! `http-ng-core`, not `http-ng`, not any transport.
//!
//! # It is a private cache, and three RFC rules turn on that
//!
//! A *shared* cache serves many principals from one store; this one is a
//! user agent's. [`HttpCache`]'s own doc comment has the three — `private`
//! is stored, `s-maxage` is not read at all, and a response to an
//! authenticated request is stored with the credential in its
//! [`Selector`] rather than refused.
//!
//! # The counterpart this crate is
//!
//! `http_ng_core::Capabilities::owns_cache` has been a `bool` set by one
//! backend (`http-ng-fetch`, because the browser keeps its own cache) and
//! read by nobody. It has a reader now: a `Client` configured with a cache
//! against a transport that owns one is an `UnsupportedCapability` at
//! `build()`, exactly as a client-side cookie jar against `http-ng-fetch`
//! already was.
//!
//! # Where the interesting behaviour is
//!
//! - **Four answers to one question** — [`Lookup`], because *send it*,
//!   *send it with these fields added* and *do not send it at all* are
//!   three different instructions and an `Option` carries two.
//! - **A validator is enough to store on** — [`HttpCache::storing`],
//!   where the absence of heuristic freshness is load-bearing rather than
//!   a gap.
//! - **The credential is always in the secondary key** — [`Selector`],
//!   which is a narrowing of RFC 9111 §3.5 that a private cache needs and
//!   the RFC does not require.
//! - **A `304` does not relabel the stored bytes** — `policy.rs`'s
//!   `NOT_UPDATED_BY_304`, where `Content-Encoding` is excluded for a
//!   reason `http-ng`'s decompressor makes concrete.
//! - **`HTTP-date` is not the cookie `Expires` grammar** — `date.rs`,
//!   whose module doc has the two strings that separate them.
//!
//! # What this crate deliberately does not do
//!
//! - **It does not fetch, and it does not revalidate.** [`Lookup`] says
//!   what to send; sending it is the caller's.
//! - **`stale-while-revalidate` and `stale-if-error` (RFC 5861) are not
//!   implemented.** The first needs somewhere to run the revalidation
//!   after the response has been handed over, and the one thing
//!   `http-ng` will not do is spawn on a caller's behalf — the same
//!   sentence `http-ng-h3`'s body pump and the WebSocket keep-alive are
//!   each written under. The second needs the *error* to reach the cache,
//!   which means a seam on the way back that does not exist yet.
//! - **It does not store `206`, and `Range` requests bypass it.** RFC 9111
//!   §3.3/§3.4 — see `policy.rs`'s `CACHEABLE_STATUSES`.
//! - **It does not evaluate a caller's own precondition** (§4.3.2). A
//!   request carrying `If-None-Match` or `If-Modified-Since` is asking the
//!   origin, and this cache stands aside for it.
//! - **It has no background sweep.** Expired entries are filtered on
//!   lookup and displaced on storing; a cache nobody touches keeps them
//!   until it is touched. Same reason the native pool has no reaper and
//!   the jar has no sweep — there is nothing here to run one.

#![forbid(unsafe_code)]

mod date;
mod directives;
mod policy;
mod store;

pub use directives::{RequestDirectives, ResponseDirectives};
pub use policy::{HttpCache, Limits, Lookup, NotStored, Storing};
pub use store::{CacheStore, Key, MemoryStore, Selector, StoredResponse};
