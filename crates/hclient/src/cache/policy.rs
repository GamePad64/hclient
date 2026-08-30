//! RFC 9111 itself: what may be stored, what may be reused, and what must
//! be asked about first.
//!
//! Everything here is a pure function of a request, a response, a store
//! and a `now`. Nothing reads a clock, opens a socket or spawns anything —
//! the same rule `hclient-cookie` and `hclient-proto` run under, and for
//! the same reason: it is what makes "a cache behaves the same behind
//! every backend" a structural fact rather than a consequence of everyone
//! happening to go through one client.

use crate::cache::error::NotStored;
use std::time::Duration;
// `web_time::SystemTime`, not `std::time`'s, and this file has no choice
// about it independently of `client.rs`: `lookup`, `storing` and
// `revalidated` below hand these timestamps to `super::store`'s
// `StoredResponse`, so the two must name one type. Off
// `wasm32-unknown-unknown` that type IS `std::time::SystemTime` and nothing
// here changes; on it, `std` has no clock and `now()` aborts, which is what
// made an HTTP cache in a browser unreachable rather than merely
// discouraged. `Duration` stays `std`'s — it is arithmetic, with no clock
// in it, and `web-time` re-exports std's own on every target anyway.
use web_time::SystemTime;

use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, Version};

use super::date::parse_http_date;
use super::directives::{RequestDirectives, ResponseDirectives};
use super::store::{CacheStore, Key, MemoryStore, Selector, StoredResponse};

/// The statuses this cache will store.
///
/// RFC 9110 §15.1's heuristically-cacheable list, plus `302` and `307`
/// (cacheable when they carry explicit expiration, §15.4.3/§15.4.8, which
/// is the only way anything is stored here anyway), **minus `206`**.
///
/// `206` is left out on purpose and it is the one omission worth arguing.
/// Storing a partial response is legal, and *using* one needs RFC 9111
/// §3.3 and §3.4 — combining ranges, and refusing to combine them across a
/// changed validator. A cache that stored `206`s without that machinery
/// would either serve a partial body as a complete one or keep entries it
/// can never use. `Range` requests bypass this cache entirely for the same
/// reason, which is stated where [`HttpCache::lookup`] refuses them.
const CACHEABLE_STATUSES: &[u16] = &[
    200, 203, 204, 300, 301, 302, 307, 308, 404, 405, 410, 414, 501,
];

/// Header fields a `304` must not carry into a stored response.
///
/// The first is RFC 9111 §3.2's own exception. The rest are
/// connection-specific (RFC 9110 §7.6.1) and describe the hop that
/// delivered the `304`, not the message that was stored — a stored
/// `Transfer-Encoding: chunked` would describe a framing the replay does
/// not use.
///
/// **`Content-Encoding` is the interesting one, and it is ours rather than
/// the RFC's list.** §3.2 excepts *"header fields that the cached response
/// depends upon"* and names this exact case in a note. The stored body is
/// the bytes the wire carried, still encoded; letting a `304` relabel them
/// would hand the decompressor above this cache a body that is not what
/// the label says.
const NOT_UPDATED_BY_304: &[HeaderName] = &[
    http::header::CONTENT_LENGTH,
    http::header::CONTENT_ENCODING,
    http::header::CONNECTION,
    http::header::TRANSFER_ENCODING,
    http::header::TE,
    http::header::TRAILER,
    http::header::UPGRADE,
    http::header::PROXY_AUTHENTICATE,
    http::header::PROXY_AUTHORIZATION,
];

/// How large a body this cache will hold.
///
/// One field, because there is one question. A count of *entries* is the
/// store's business and lives there ([`MemoryStore::with_capacity`]) —
/// see `store.rs`'s module doc for the split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Bytes. A response larger than this is streamed and not stored.
    ///
    /// 1 MiB, and the number is arbitrary in the way the store's capacity
    /// is: no RFC states one. What is not arbitrary is that there **is**
    /// one, and that it is checked twice — against `Content-Length` before
    /// a byte is recorded, and against the bytes actually recorded, since
    /// a response with no `Content-Length` is the ordinary case over
    /// HTTP/2 and chunked HTTP/1.1.
    pub max_body_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_body_bytes: 1024 * 1024,
        }
    }
}

/// What to do about a request, decided before it is sent.
///
/// **Four states, and an `Option<StoredResponse>` would have been two of
/// them.** The three ways of not having an answer are different
/// instructions to the caller: [`Miss`](Self::Miss) says send the request
/// as it stands, [`Revalidate`](Self::Revalidate) says send it with these
/// fields added and bring the answer back here, and
/// [`Unsatisfiable`](Self::Unsatisfiable) says do not send anything at all
/// because the caller forbade it. Collapsing the middle one is the shape
/// that turns a conditional request into an unconditional one and doubles
/// a client's traffic without failing anything.
/// Deliberately **not** `#[non_exhaustive]`, unlike `Capabilities` one
/// crate over: the only consumer is a `match` in `hclient::Client` that
/// must handle every case, and a fifth variant arriving with no
/// corresponding branch there would be a silent fall-through into
/// whichever arm the wildcard happened to be.
#[derive(Debug)]
pub enum Lookup {
    /// Nothing usable. Send the request unchanged.
    Miss,
    /// Serve this without sending anything. Its `Age` field is already
    /// set, per RFC 9111 §5.1.
    Hit(StoredResponse),
    /// Send the request with `conditions` added, then hand the answer to
    /// [`HttpCache::revalidated`] (a `304`) or
    /// [`HttpCache::superseded`] (anything else).
    Revalidate {
        key: Key,
        stale: StoredResponse,
        /// `If-None-Match` and/or `If-Modified-Since`, RFC 9111 §4.3.1.
        conditions: Vec<(HeaderName, HeaderValue)>,
    },
    /// The request said `only-if-cached` and there is nothing to serve.
    /// RFC 9111 §5.2.1.7 makes this a `504 (Gateway Timeout)` generated by
    /// the cache, and **not** a request that goes out anyway.
    Unsatisfiable,
}

/// A response that may be stored, once its body has arrived.
///
/// Handed back by [`HttpCache::storing`] and consumed by
/// [`HttpCache::store`]. It exists as a separate value because the
/// decision is made when the head arrives and the commit can only happen
/// when the body ends — in `hclient` those are minutes apart and a caller
/// in between, which is the whole reason a client can cache a streaming
/// response without buffering it before the head.
#[derive(Debug, Clone)]
pub struct Storing {
    key: Key,
    status: StatusCode,
    version: Version,
    headers: HeaderMap,
    selector: Selector,
    requested_at: SystemTime,
    received_at: SystemTime,
    /// The `Content-Length` the head declared, if it declared one.
    declared_len: Option<u64>,
    /// [`Limits::max_body_bytes`], carried so that a recorder has
    /// everything it needs in one value and cannot read a limit that has
    /// since been changed.
    max_body_bytes: u64,
}

impl Storing {
    /// The cap a recorder must stop at. Beyond it the response is not
    /// stored — the recording is abandoned, and the body streams on
    /// untouched.
    pub fn max_body_bytes(&self) -> u64 {
        self.max_body_bytes
    }

    pub fn key(&self) -> &Key {
        &self.key
    }
}

/// An HTTP response cache: RFC 9111's decisions over a [`CacheStore`].
///
/// **This is a private cache — a user-agent cache — and three of the RFC's
/// rules turn on that.** A shared cache serves many principals from one
/// store; this one belongs to whoever built the [`hclient::Client`] it
/// sits in.
///
/// - **`private` responses are stored.** §5.2.2.7 addresses shared caches;
///   refusing them here would leave the cache useless for exactly the
///   responses a user agent exists to hold.
/// - **`s-maxage` is not read at all**, and neither is `proxy-revalidate`
///   (§5.2.2.10, §5.2.2.8). They are instructions to a shared cache. A
///   response saying `max-age=0, s-maxage=600` is stale here immediately,
///   which is what the origin asked of a private cache.
/// - **A response to a request carrying `Authorization` is stored.** §3.5's
///   restriction is a shared cache's. What replaces it is narrower and
///   local: the credential is part of the [`Selector`] on every entry, so
///   a response fetched with one token is never served to a request
///   carrying another. See that type's doc comment for why that is not the
///   same as `Vary: Authorization`.
///
/// [`hclient::Client`]: https://docs.rs/hclient
///
/// ```
/// use std::time::{Duration, SystemTime};
/// use http::{HeaderMap, Method, Response, Uri};
/// use hclient::cache::{HttpCache, Lookup};
///
/// let mut cache = HttpCache::new();
/// let uri: Uri = "https://example.com/thing".parse().unwrap();
/// let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
///
/// let (parts, ()) = Response::builder()
///     .header("cache-control", "max-age=60")
///     .body(())
///     .unwrap()
///     .into_parts();
/// let storing = cache
///     .storing(&Method::GET, &uri, &HeaderMap::new(), &parts, t0, t0)
///     .expect("an explicit max-age is storable");
/// cache.store(storing, bytes::Bytes::from_static(b"hello")).unwrap();
///
/// let hit = cache.lookup(&Method::GET, &uri, &HeaderMap::new(), t0 + Duration::from_secs(30));
/// assert!(matches!(hit, Lookup::Hit(_)));
/// ```
#[derive(Debug, Clone)]
pub struct HttpCache<S = MemoryStore> {
    store: S,
    limits: Limits,
}

impl Default for HttpCache<MemoryStore> {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpCache<MemoryStore> {
    /// A cache over the in-memory store, with default [`Limits`].
    pub fn new() -> Self {
        Self::with_store(MemoryStore::new())
    }
}

impl<S: CacheStore> HttpCache<S> {
    /// A cache over a store of the caller's own.
    pub fn with_store(store: S) -> Self {
        Self {
            store,
            limits: Limits::default(),
        }
    }

    /// The same cache over a different store — the [`Limits`] carried
    /// across, the entries going wherever `f` puts them.
    ///
    /// `HttpCache` is a policy and `S` is where the bytes live, so the two
    /// should be separable after construction as well as at it. What asked
    /// for it is `hclient`, which holds one cache type for every caller and
    /// so must erase `S` behind a value of its own.
    pub fn map_store<R>(self, f: impl FnOnce(S) -> R) -> HttpCache<R> {
        HttpCache {
            store: f(self.store),
            limits: self.limits,
        }
    }

    #[must_use]
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    pub fn store_ref(&self) -> &S {
        &self.store
    }

    pub fn store_mut(&mut self) -> &mut S {
        &mut self.store
    }

    /// What to do about a request that is about to be sent.
    ///
    /// # The three requests this never looks anything up for
    ///
    /// - **One carrying its own `If-None-Match` or `If-Modified-Since`.**
    ///   The caller is asking the origin a question, and a cache that
    ///   answered it from a stored copy would be answering a different
    ///   one. RFC 9111 §4.3.2 does let a cache evaluate a client's
    ///   precondition against a stored response; that is a second
    ///   validator comparison this vertical does not implement, and
    ///   standing aside is the honest version of not implementing it. The
    ///   same request is not stored either, for the mirror reason: what
    ///   comes back may be a `304` whose body is not the resource.
    /// - **One carrying `Range`.** See `CACHEABLE_STATUSES` for why
    ///   `206` is not stored; serving a range out of a complete stored
    ///   entry is the other half of the same missing machinery.
    /// - **One whose method is not `GET`.** The method is in the key
    ///   ([`Key`]), so a stored `HEAD` could only ever answer a `HEAD` —
    ///   a request nobody repeats — and RFC 9111 §4.3.5's use for one
    ///   (freshening a stored `GET`) needs a `HEAD` to have been made.
    ///   Unsafe methods do not look anything up, but they do
    ///   **invalidate**: see [`invalidated_by`](Self::invalidated_by).
    pub fn lookup(
        &mut self,
        method: &Method,
        uri: &Uri,
        headers: &HeaderMap,
        now: SystemTime,
    ) -> Lookup {
        let req = RequestDirectives::parse(headers);
        let miss = if req.only_if_cached {
            Lookup::Unsatisfiable
        } else {
            Lookup::Miss
        };

        if !usable_method(method) || bypasses(headers) || req.no_store {
            return miss;
        }
        let Some(key) = Key::new(method, uri) else {
            return miss;
        };

        // §4: among the variants whose selector matches, the most recently
        // received one. Nothing else here is allowed to see the others —
        // a cache that fell back to an older variant because the newest
        // was stale would serve a response the origin had already
        // replaced.
        let Some(stored) = self
            .store
            .get(&key)
            .into_iter()
            .filter(|e| selector_matches(e, headers))
            .max_by_key(StoredResponse::received_at)
        else {
            return miss;
        };

        let resp = ResponseDirectives::parse(stored.headers());
        let age = current_age(&stored, now);
        let lifetime = freshness_lifetime(&stored, &resp);

        if !req.no_cache && !resp.no_cache && is_fresh(age, lifetime, &req) {
            return Lookup::Hit(with_age(stored, age));
        }

        // §5.2.1.2: the caller may accept a stale response — unless the
        // origin said otherwise. `must-revalidate` (§5.2.2.2) and a
        // response `no-cache` (§5.2.2.4) both override the request, and
        // that override is the only reader `must_revalidate` has: without
        // `max-stale` there would be nothing for it to forbid, and a
        // directive nothing reads is one this workspace deletes rather
        // than lists as supported.
        if !req.no_cache
            && !resp.no_cache
            && !resp.must_revalidate
            && let Some(allowance) = req.max_stale
            && let Some(lifetime) = lifetime
        {
            let staleness = age.saturating_sub(lifetime);
            if allowance.is_none_or(|d| staleness <= d) {
                return Lookup::Hit(with_age(stored, age));
            }
        }

        let conditions = conditions_for(&stored);
        if conditions.is_empty() {
            // Nothing to validate with, so there is nothing a conditional
            // request would buy. The entry stays where it is: the
            // unconditional request about to go out will replace it if the
            // answer is storable.
            return miss;
        }
        Lookup::Revalidate {
            key,
            stale: stored,
            conditions,
        }
    }

    /// Whether a response may be stored, and under what key — decided from
    /// the head, before the body has arrived.
    ///
    /// # The storability rule, and the heuristic that is not here
    ///
    /// A response is stored when it is a `GET`, its status is one this
    /// cache can reuse, nothing said `no-store`, its `Vary` is not `*`,
    /// and it carries **either explicit freshness** (`max-age`, `Expires`)
    /// **or a validator** (`ETag`, `Last-Modified`).
    ///
    /// That last disjunction is where heuristic freshness (§4.2.2) would
    /// have gone, and its absence is load-bearing rather than a gap left
    /// for later. With a heuristic, any `200` becomes storable — including
    /// a `text/event-stream` that never ends, which `hclient` would then
    /// record into memory for as long as the caller kept reading it,
    /// bounded only by [`Limits::max_body_bytes`]. Without one, a response
    /// carrying neither a lifetime nor a validator can never be *used*, so
    /// storing it would buy a copy of the body and nothing else, and the
    /// rule above says exactly that.
    ///
    /// A response with a validator and no lifetime **is** stored, and is
    /// stale from the moment it lands: every later request for it is a
    /// conditional one, which is the bandwidth the cache exists to save
    /// and is a claim no heuristic had to invent.
    pub fn storing(
        &self,
        method: &Method,
        uri: &Uri,
        request_headers: &HeaderMap,
        parts: &http::response::Parts,
        requested_at: SystemTime,
        received_at: SystemTime,
    ) -> Result<Storing, NotStored> {
        if !usable_method(method) {
            return Err(NotStored::Method(method.clone()));
        }
        if bypasses(request_headers) {
            return Err(NotStored::RequestStoodAside);
        }
        if RequestDirectives::parse(request_headers).no_store {
            return Err(NotStored::RequestNoStore);
        }
        if !CACHEABLE_STATUSES.contains(&parts.status.as_u16()) {
            return Err(NotStored::Status(parts.status));
        }
        let resp = ResponseDirectives::parse(&parts.headers);
        if resp.no_store {
            return Err(NotStored::ResponseNoStore);
        }
        let names = vary_names(&parts.headers).ok_or(NotStored::VaryAsterisk)?;
        let has_lifetime =
            resp.max_age.is_some() || parts.headers.contains_key(http::header::EXPIRES);
        let has_validator = !conditions_from(&parts.headers).is_empty();
        if !has_lifetime && !has_validator {
            return Err(NotStored::NothingToReuseItWith);
        }
        let declared_len = parts
            .headers
            .get(http::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok());
        if let Some(n) = declared_len.filter(|n| *n > self.limits.max_body_bytes) {
            return Err(NotStored::TooLarge {
                bytes: n,
                limit: self.limits.max_body_bytes,
            });
        }
        Ok(Storing {
            key: Key::new(method, uri).ok_or(NotStored::NoKey)?,
            status: parts.status,
            version: parts.version,
            headers: parts.headers.clone(),
            selector: Selector::of(request_headers, &names),
            requested_at,
            received_at,
            declared_len,
            max_body_bytes: self.limits.max_body_bytes,
        })
    }

    /// Commit a response whose body has now arrived.
    ///
    /// Refuses two bodies rather than storing them: one longer than
    /// [`Limits::max_body_bytes`], and one whose length disagrees with the
    /// `Content-Length` the head declared. The second is not paranoia — a
    /// body the transport truncated is exactly the one a cache must not
    /// keep, because a truncated entry served later is indistinguishable
    /// from a complete one.
    pub fn store(&mut self, s: Storing, body: Bytes) -> Result<(), NotStored> {
        let len = body.len() as u64;
        if len > s.max_body_bytes {
            return Err(NotStored::TooLarge {
                bytes: len,
                limit: s.max_body_bytes,
            });
        }
        if let Some(declared) = s.declared_len.filter(|n| *n != len) {
            return Err(NotStored::LengthMismatch {
                bytes: len,
                declared,
            });
        }
        let entry = StoredResponse::new(
            s.status,
            s.version,
            s.headers,
            body,
            s.selector,
            s.requested_at,
            s.received_at,
        );
        self.store.put(&s.key, entry);
        Ok(())
    }

    /// A `304` answered the conditional request
    /// [`Lookup::Revalidate`] asked for: freshen the stored response and
    /// hand back what to serve.
    ///
    /// RFC 9111 §4.3.4 by way of §3.2 — every field in the `304` replaces
    /// the stored one, except those in `NOT_UPDATED_BY_304` and any the
    /// `304`'s own `Connection` header nominates.
    pub fn revalidated(
        &mut self,
        key: &Key,
        stale: StoredResponse,
        parts: &http::response::Parts,
        requested_at: SystemTime,
        received_at: SystemTime,
    ) -> StoredResponse {
        let mut fresh = stale;
        let hop = connection_nominated(&parts.headers);
        for name in parts.headers.keys() {
            if NOT_UPDATED_BY_304.contains(name) || hop.iter().any(|h| h == name) {
                continue;
            }
            let headers = fresh.headers_mut();
            headers.remove(name);
            for v in parts.headers.get_all(name) {
                headers.append(name.clone(), v.clone());
            }
        }
        fresh.freshen(requested_at, received_at);
        self.store.put(key, fresh.clone());
        let age = current_age(&fresh, received_at);
        with_age(fresh, age)
    }

    /// Anything other than a `304` answered the conditional request: the
    /// stored variant is gone.
    ///
    /// It is removed rather than left for the ordinary storing path to
    /// replace, because the two are not the same claim. A `500` to a
    /// revalidation tells us the entry is no longer the origin's answer,
    /// and it is not storable, so leaving it would let a later
    /// `max-stale` request serve a response we have direct evidence was
    /// superseded. Erring towards the empty cache is the direction that
    /// costs a request.
    pub fn superseded(&mut self, key: &Key, stale: &StoredResponse) {
        self.store.remove(key, stale.selector());
    }

    /// RFC 9111 §4.4: a non-error answer to an unsafe method invalidates
    /// everything stored for that URI.
    ///
    /// "Unsafe" is every method except `GET`, `HEAD`, `OPTIONS` and
    /// `TRACE` — RFC 9110 §9.2.1's safe set — so `POST`, `PUT`, `DELETE`,
    /// `PATCH` and anything unrecognised all invalidate. Unrecognised is
    /// the direction that fails closed: a method this code has never heard
    /// of is more likely to change state than not.
    ///
    /// **What is deliberately not done is the `Location` and
    /// `Content-Location` half.** §4.4 makes that a `MAY`, guarded by a
    /// `MUST NOT` against crossing an origin — so the whole of it is
    /// optional, and the part that is not optional is the guard on doing
    /// it. Declining costs a stale entry for a resource the caller did not
    /// name; doing it costs an origin comparison that has to be right.
    pub fn invalidated_by(&mut self, method: &Method, uri: &Uri, status: StatusCode) {
        if is_safe(method) || status.is_client_error() || status.is_server_error() {
            return;
        }
        for m in [Method::GET, Method::HEAD] {
            if let Some(k) = Key::new(&m, uri) {
                self.store.invalidate(&k);
            }
        }
    }
}

/// The one method whose responses this cache stores and serves.
fn usable_method(m: &Method) -> bool {
    m == Method::GET
}

fn is_safe(m: &Method) -> bool {
    matches!(
        *m,
        Method::GET | Method::HEAD | Method::OPTIONS | Method::TRACE
    )
}

/// A request this cache stands aside for entirely — see
/// [`HttpCache::lookup`]'s doc comment for both cases.
fn bypasses(headers: &HeaderMap) -> bool {
    headers.contains_key(http::header::IF_NONE_MATCH)
        || headers.contains_key(http::header::IF_MODIFIED_SINCE)
        || headers.contains_key(http::header::RANGE)
}

/// The field names a response's `Vary` nominates, or `None` for `Vary: *`
/// — which RFC 9111 §4.1 makes a permanent mismatch, so a response
/// carrying it is not stored rather than stored and never matched.
fn vary_names(headers: &HeaderMap) -> Option<Vec<HeaderName>> {
    let mut names = Vec::new();
    for value in headers.get_all(http::header::VARY) {
        for token in value.as_bytes().split(|b| *b == b',') {
            let token = std::str::from_utf8(token).ok()?.trim();
            if token.is_empty() {
                continue;
            }
            if token == "*" {
                return None;
            }
            names.push(HeaderName::from_bytes(token.as_bytes()).ok()?);
        }
    }
    Some(names)
}

fn selector_matches(stored: &StoredResponse, headers: &HeaderMap) -> bool {
    let names: Vec<HeaderName> = stored.selector().names().cloned().collect();
    Selector::of(headers, &names) == *stored.selector()
}

/// RFC 9111 §4.3.1: an `ETag` gives `If-None-Match`, a `Last-Modified`
/// gives `If-Modified-Since`, and a response with both produces both —
/// which the RFC asks for so that a server which ignores one still gets
/// asked with the other.
fn conditions_for(stored: &StoredResponse) -> Vec<(HeaderName, HeaderValue)> {
    conditions_from(stored.headers())
}

fn conditions_from(headers: &HeaderMap) -> Vec<(HeaderName, HeaderValue)> {
    let mut out = Vec::new();
    if let Some(etag) = headers.get(http::header::ETAG) {
        out.push((http::header::IF_NONE_MATCH, etag.clone()));
    }
    if let Some(lm) = headers.get(http::header::LAST_MODIFIED) {
        out.push((http::header::IF_MODIFIED_SINCE, lm.clone()));
    }
    out
}

/// The field names a message's `Connection` header nominates as
/// connection-specific — RFC 9110 §7.6.1. They describe the hop that
/// carried the `304` and must not survive into a stored response.
fn connection_nominated(headers: &HeaderMap) -> Vec<HeaderName> {
    headers
        .get_all(http::header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|t| HeaderName::from_bytes(t.trim().as_bytes()).ok())
        .collect()
}

/// RFC 9111 §4.2.3, written out.
///
/// The two halves it takes the maximum of measure different things and
/// each can be the larger: `apparent_age` is what the origin's own `Date`
/// says, and is wrong when the two clocks disagree; `corrected_age_value`
/// is what an intermediary's `Age` says plus the time this request spent
/// in flight, and is wrong when nothing sent an `Age`. Taking the maximum
/// is the RFC's way of never *under*-stating an age, which is the only
/// direction that can serve a response past its lifetime.
fn current_age(e: &StoredResponse, now: SystemTime) -> Duration {
    let date = date_of(e).unwrap_or(e.received_at());
    let apparent = e
        .received_at()
        .duration_since(date)
        .unwrap_or(Duration::ZERO);
    let age_value = e
        .headers()
        .get(http::header::AGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map_or(Duration::ZERO, Duration::from_secs);
    let response_delay = e
        .received_at()
        .duration_since(e.requested_at())
        .unwrap_or(Duration::ZERO);
    let corrected = age_value.saturating_add(response_delay);
    let initial = apparent.max(corrected);
    let resident = now
        .duration_since(e.received_at())
        .unwrap_or(Duration::ZERO);
    initial.saturating_add(resident)
}

/// RFC 9111 §4.2.1, with no heuristic — see [`HttpCache::storing`].
///
/// `None` means *no lifetime was given*, which is not the same as a
/// lifetime of zero: a response with neither is never fresh and is never
/// stale **by an amount**, so `max-stale` cannot reach it. That
/// distinction is the reason this returns an `Option` and not a
/// `Duration`.
fn freshness_lifetime(e: &StoredResponse, resp: &ResponseDirectives) -> Option<Duration> {
    if let Some(max_age) = resp.max_age {
        return Some(max_age);
    }
    let expires = e.headers().get(http::header::EXPIRES)?;
    let date = date_of(e).unwrap_or(e.received_at());
    // §5.3: a value that is not an HTTP-date — `0` being the one every
    // server sends — means already expired. `None` from the parser
    // therefore becomes a lifetime of zero rather than *no lifetime*,
    // which is the difference between "stale, revalidate it" and "nothing
    // was said".
    let Some(seconds) = parse_http_date(expires.as_bytes()) else {
        return Some(Duration::ZERO);
    };
    Some(
        from_unix(seconds)
            .duration_since(date)
            .unwrap_or(Duration::ZERO),
    )
}

fn date_of(e: &StoredResponse) -> Option<SystemTime> {
    e.headers()
        .get(http::header::DATE)
        .and_then(|v| parse_http_date(v.as_bytes()))
        .map(from_unix)
}

fn from_unix(seconds: i64) -> SystemTime {
    if seconds >= 0 {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds.unsigned_abs())
    } else {
        SystemTime::UNIX_EPOCH
            .checked_sub(Duration::from_secs(seconds.unsigned_abs()))
            .unwrap_or(SystemTime::UNIX_EPOCH)
    }
}

/// §4.2 plus the three request directives that narrow it (§5.2.1.1,
/// §5.2.1.3).
fn is_fresh(age: Duration, lifetime: Option<Duration>, req: &RequestDirectives) -> bool {
    let Some(lifetime) = lifetime else {
        return false;
    };
    if lifetime <= age {
        return false;
    }
    if req.max_age.is_some_and(|m| age > m) {
        return false;
    }
    if let Some(min) = req.min_fresh
        && lifetime - age < min
    {
        return false;
    }
    true
}

/// RFC 9111 §5.1: a cache reusing a stored response **must** generate an
/// `Age`, replacing any already there.
fn with_age(mut e: StoredResponse, age: Duration) -> StoredResponse {
    let value = HeaderValue::from_str(&age.as_secs().to_string())
        .unwrap_or_else(|_| HeaderValue::from_static("0"));
    e.headers_mut().insert(http::header::AGE, value);
    e
}
