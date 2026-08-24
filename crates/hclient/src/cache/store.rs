//! Where stored responses live: the key, the entry, the seam, and the one
//! implementation this crate ships.
//!
//! # The split between this file and `policy.rs`
//!
//! **Policy decides what may be stored; storage decides what it can
//! hold.** RFC 9111 is entirely the first of those and says nothing at all
//! about the second — a cache that keeps one entry and a cache that keeps
//! a million are equally conformant. So [`CacheStore`] has no notion of
//! freshness, of `Vary`, or of a directive; it is a multimap from
//! [`Key`] to variants, and everything that could be got wrong against the
//! RFC is on the other side of it. That is what makes the seam safe to
//! hand to a caller: a wrong `CacheStore` loses entries or keeps too many,
//! and cannot serve a stale response to a request that forbade one.

use std::collections::HashMap;
use std::time::SystemTime;

use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, Version};

/// The **primary** cache key, RFC 9111 §2: the request method and the
/// target URI.
///
/// The URI is normalised (RFC 9110 §4.2.3) rather than taken verbatim:
/// scheme and host are lowercased and a default port is dropped, so
/// `HTTPS://Example.COM:443/x` and `https://example.com/x` are one key
/// rather than two. Nothing else is touched — in particular the query is
/// **not** reordered and `/a/../b` is **not** collapsed, because both are
/// origin-server business and a cache that guessed at them would serve one
/// resource's response for another's.
///
/// The method is in the key, which is why a `HEAD` response could never
/// answer a `GET` even if this cache stored one. It does not: see
/// [`super::HttpCache::lookup`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Key {
    method: Method,
    target: String,
}

impl Key {
    /// The key for a request, or `None` when the URI has no scheme or no
    /// authority.
    ///
    /// `None` rather than a key over whatever the URI does carry: an
    /// origin-form URI (`/x`) names a resource only relative to a
    /// connection, and two connections' `/x` are two resources. A cache
    /// with no authority in its key is a cache that mixes servers up.
    pub fn new(method: &Method, uri: &Uri) -> Option<Self> {
        let scheme = uri.scheme_str()?.to_ascii_lowercase();
        let authority = uri.authority()?;
        let host = authority.host().to_ascii_lowercase();
        let port = match (authority.port_u16(), scheme.as_str()) {
            (None, _) | (Some(80), "http") | (Some(443), "https") => String::new(),
            (Some(p), _) => format!(":{p}"),
        };
        let path = uri.path_and_query().map_or("/", |pq| pq.as_str());
        Some(Self {
            method: method.clone(),
            target: format!("{scheme}://{host}{port}{path}"),
        })
    }

    pub fn method(&self) -> &Method {
        &self.method
    }

    /// The normalised absolute URI this key names.
    pub fn target(&self) -> &str {
        &self.target
    }
}

/// The **secondary** key, RFC 9111 §4.1: the request header fields a stored
/// response's `Vary` nominated, with the values they had on the request
/// that produced it.
///
/// An absent field is stored as `None` and matches only an absent field,
/// which is the half of §4.1 that a `HashMap<HeaderName, HeaderValue>`
/// silently gets wrong: *"if the header field is absent from one request
/// and present in the other, it does not match"*.
///
/// # `Authorization` is always in here, and `Vary` did not put it there
///
/// RFC 9111 §3.5 restricts a **shared** cache's handling of a response to
/// an authenticated request and says nothing to a private one, which is
/// why [`super::HttpCache`] stores such responses at all. But "private"
/// means one user agent, not one principal, and a `Client` whose caller
/// changes a bearer token between requests is an ordinary thing to write.
/// The RFC's own answer to that is `Vary: Authorization`, sent by the
/// origin — a promise this cache cannot check and would be the only
/// casualty of.
///
/// So the credential is part of the selector unconditionally. The cost is
/// a miss where a correct origin would have allowed a hit; the cost of the
/// other choice is one principal's response served to another, which is
/// not a cost but a defect. It fails closed, and it can only ever produce
/// misses.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Selector(Vec<(HeaderName, Option<HeaderValue>)>);

impl Selector {
    /// The selector for a request, given the field names a response's
    /// `Vary` nominated.
    ///
    /// Sorted by name so that `Vary: Accept, Accept-Encoding` and
    /// `Vary: Accept-Encoding, Accept` produce one selector rather than
    /// two that never match each other.
    pub(crate) fn of(headers: &HeaderMap, names: &[HeaderName]) -> Self {
        let mut fields: Vec<(HeaderName, Option<HeaderValue>)> = names
            .iter()
            .chain(std::iter::once(&http::header::AUTHORIZATION))
            .map(|n| (n.clone(), combined(headers, n)))
            .collect();
        fields.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        fields.dedup_by(|a, b| a.0 == b.0);
        Self(fields)
    }

    /// The field names this selector is keyed on, `Authorization`
    /// included.
    pub fn names(&self) -> impl Iterator<Item = &HeaderName> {
        self.0.iter().map(|(n, _)| n)
    }
}

/// Every value of `name`, joined with `", "` — §4.1's *"combining multiple
/// message header fields with the same field name"*, without which a
/// request sending `Accept: a` and `Accept: b` would match a stored
/// selector built from `Accept: a` alone.
fn combined(headers: &HeaderMap, name: &HeaderName) -> Option<HeaderValue> {
    let mut it = headers.get_all(name).iter();
    let first = it.next()?;
    let rest: Vec<&HeaderValue> = it.collect();
    if rest.is_empty() {
        return Some(first.clone());
    }
    let mut joined = first.as_bytes().to_vec();
    for v in rest {
        joined.extend_from_slice(b", ");
        joined.extend_from_slice(v.as_bytes());
    }
    HeaderValue::from_bytes(&joined).ok()
}

/// One stored response: the message as it came off the wire, and the four
/// facts needed to age it.
///
/// # What "as it came off the wire" excludes, and what it deliberately
/// includes
///
/// The body is the bytes the transport handed over — still carrying
/// whatever `Content-Encoding` the server chose. In `hclient` the
/// decompressor sits **above** the cache (`ClientBody` is
/// `Decompressed<Deadline<Cached<..>>>`), so a stored response is decoded
/// on the way out exactly as a fresh one is, by the same code, and a
/// `Vary: Accept-Encoding` entry is keyed on the encoding actually asked
/// for. Storing the decoded bytes would have required either relabelling
/// the response (a claim about a transformation the cache did not make) or
/// decoding it twice.
#[derive(Debug, Clone)]
pub struct StoredResponse {
    status: StatusCode,
    version: Version,
    headers: HeaderMap,
    body: Bytes,
    selector: Selector,
    /// When the request that produced this was sent — RFC 9111 §4.2.3's
    /// `request_time`. Half of the `response_delay` that corrects a
    /// received `Age`.
    requested_at: SystemTime,
    /// When the response head arrived — §4.2.3's `response_time`.
    received_at: SystemTime,
}

impl StoredResponse {
    pub(crate) fn new(
        status: StatusCode,
        version: Version,
        headers: HeaderMap,
        body: Bytes,
        selector: Selector,
        requested_at: SystemTime,
        received_at: SystemTime,
    ) -> Self {
        Self {
            status,
            version,
            headers,
            body,
            selector,
            requested_at,
            received_at,
        }
    }

    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// The version the message **arrived on**, replayed unchanged.
    ///
    /// It is a fact about the exchange that filled this entry rather than
    /// about the one being answered, and it is kept for the reason
    /// `Head::version` became an `Option` rather than defaulting to
    /// `HTTP/1.1`: `http::Response::version()` has no way to say *nothing
    /// was spoken*, so the choice is between a stale truth and an invented
    /// one. `http`'s builder default would report HTTP/1.1 for a body that
    /// came back over h2 — a wrong answer where this is merely an old one.
    pub fn version(&self) -> Version {
        self.version
    }

    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    pub(crate) fn headers_mut(&mut self) -> &mut HeaderMap {
        &mut self.headers
    }

    pub fn body(&self) -> &Bytes {
        &self.body
    }

    pub fn selector(&self) -> &Selector {
        &self.selector
    }

    pub fn requested_at(&self) -> SystemTime {
        self.requested_at
    }

    pub fn received_at(&self) -> SystemTime {
        self.received_at
    }

    pub(crate) fn freshen(&mut self, requested_at: SystemTime, received_at: SystemTime) {
        self.requested_at = requested_at;
        self.received_at = received_at;
    }
}

/// Where stored responses are kept.
///
/// A multimap from [`Key`] to the variants stored under it, and nothing
/// more — see this module's doc comment for why every RFC 9111 decision is
/// deliberately on the other side of this trait.
///
/// **No `Send` or `Sync` bound**, here or anywhere in this workspace's
/// seams: a store built on `Rc<RefCell<..>>` for a single-threaded runtime
/// is a legitimate implementation, and a bound here would forbid it. Where
/// a store has to cross a thread — as it does the moment `hclient::Client`
/// holds one, since a `Client` is meant to survive a `tokio::spawn` — the
/// bound belongs at that call site and not on this trait.
pub trait CacheStore {
    /// Every variant stored under `key`, in no particular order.
    ///
    /// Owned rather than borrowed: the caller holds a lock while asking,
    /// and everything in a [`StoredResponse`] clones cheaply — `Bytes` is
    /// a refcount, and the header map is the only real copy. Lending
    /// instead would put the lock's lifetime into the policy's signatures
    /// and out into `hclient`'s.
    fn get(&self, key: &Key) -> Vec<StoredResponse>;

    /// Store `entry`, replacing any variant already stored under `key`
    /// with the same [`Selector`].
    fn put(&mut self, key: &Key, entry: StoredResponse);

    /// Remove the one variant under `key` whose selector matches.
    fn remove(&mut self, key: &Key, selector: &Selector);

    /// Remove every variant under `key` — RFC 9111 §4.4's invalidation.
    fn invalidate(&mut self, key: &Key);

    /// How many variants are held, across every key.
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn clear(&mut self);
}

/// The store this crate ships: a `HashMap` in memory, bounded by a count
/// of variants.
///
/// **Eviction is by oldest `received_at`, and it is not LRU.** LRU would
/// need a last-access time, which would need [`CacheStore::get`] to take
/// `&mut self` and a `now` — a wider seam, for a policy this type has no
/// evidence is better. Oldest-first is what a cache with no access record
/// can honestly do, and it is stated rather than implied by a name.
#[derive(Debug, Clone)]
pub struct MemoryStore {
    entries: HashMap<Key, Vec<StoredResponse>>,
    capacity: usize,
    held: usize,
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryStore {
    /// Room for 512 variants.
    ///
    /// A number rather than "unbounded": a cache with no bound is a
    /// memory-exhaustion bug with a server on the other end of it, which
    /// is the same sentence `hclient-cookie`'s `Limits` carries and the
    /// same reason. 512 is not derived from anything — no RFC states a
    /// minimum for a cache the way RFC 6265 §6.1 does for a jar — so it is
    /// named as arbitrary here rather than dressed up, and
    /// [`with_capacity`](Self::with_capacity) is the answer for a caller
    /// who knows better.
    pub fn new() -> Self {
        Self::with_capacity(512)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            capacity,
            held: 0,
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    fn evict_until_room(&mut self) {
        while self.held >= self.capacity && self.held > 0 {
            let Some((key, idx)) = self
                .entries
                .iter()
                .flat_map(|(k, v)| v.iter().enumerate().map(move |(i, e)| (k, i, e)))
                .min_by_key(|(_, _, e)| e.received_at)
                .map(|(k, i, _)| (k.clone(), i))
            else {
                return;
            };
            self.drop_at(&key, idx);
        }
    }

    fn drop_at(&mut self, key: &Key, idx: usize) {
        if let Some(v) = self.entries.get_mut(key) {
            v.remove(idx);
            self.held -= 1;
            if v.is_empty() {
                self.entries.remove(key);
            }
        }
    }
}

impl CacheStore for MemoryStore {
    fn get(&self, key: &Key) -> Vec<StoredResponse> {
        self.entries.get(key).cloned().unwrap_or_default()
    }

    fn put(&mut self, key: &Key, entry: StoredResponse) {
        self.remove(key, &entry.selector.clone());
        if self.capacity == 0 {
            return;
        }
        self.evict_until_room();
        self.entries.entry(key.clone()).or_default().push(entry);
        self.held += 1;
    }

    fn remove(&mut self, key: &Key, selector: &Selector) {
        let Some(v) = self.entries.get_mut(key) else {
            return;
        };
        let before = v.len();
        v.retain(|e| &e.selector != selector);
        self.held -= before - v.len();
        if v.is_empty() {
            self.entries.remove(key);
        }
    }

    fn invalidate(&mut self, key: &Key) {
        if let Some(v) = self.entries.remove(key) {
            self.held -= v.len();
        }
    }

    fn len(&self) -> usize {
        self.held
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.held = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn key(u: &str) -> Key {
        Key::new(&Method::GET, &u.parse().unwrap()).expect("absolute uri")
    }

    fn entry(at: u64) -> StoredResponse {
        StoredResponse::new(
            StatusCode::OK,
            Version::HTTP_11,
            HeaderMap::new(),
            Bytes::new(),
            Selector::default(),
            SystemTime::UNIX_EPOCH,
            SystemTime::UNIX_EPOCH + Duration::from_secs(at),
        )
    }

    #[test]
    fn the_key_normalises_scheme_host_and_a_default_port() {
        assert_eq!(
            key("https://Example.COM:443/x"),
            key("HTTPS://example.com/x")
        );
        assert_ne!(
            key("https://example.com:8443/x"),
            key("https://example.com/x")
        );
        assert_ne!(key("http://example.com/x"), key("https://example.com/x"));
    }

    #[test]
    fn the_key_does_not_touch_the_path_or_the_query() {
        assert_ne!(
            key("https://e.test/a?x=1&y=2"),
            key("https://e.test/a?y=2&x=1")
        );
        assert_ne!(key("https://e.test/a/../b"), key("https://e.test/b"));
    }

    #[test]
    fn an_origin_form_uri_has_no_key() {
        assert!(Key::new(&Method::GET, &"/x".parse().unwrap()).is_none());
    }

    #[test]
    fn a_selector_distinguishes_an_absent_field_from_an_empty_one() {
        let mut with = HeaderMap::new();
        with.insert(http::header::ACCEPT, HeaderValue::from_static(""));
        let names = [http::header::ACCEPT];
        assert_ne!(
            Selector::of(&with, &names),
            Selector::of(&HeaderMap::new(), &names)
        );
    }

    #[test]
    fn a_selector_is_order_independent_in_the_vary_field_names() {
        let mut h = HeaderMap::new();
        h.insert(http::header::ACCEPT, HeaderValue::from_static("a"));
        h.insert(
            http::header::ACCEPT_ENCODING,
            HeaderValue::from_static("gzip"),
        );
        assert_eq!(
            Selector::of(&h, &[http::header::ACCEPT, http::header::ACCEPT_ENCODING]),
            Selector::of(&h, &[http::header::ACCEPT_ENCODING, http::header::ACCEPT])
        );
    }

    #[test]
    fn authorization_is_in_every_selector_without_vary_naming_it() {
        let mut a = HeaderMap::new();
        a.insert(
            http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer a"),
        );
        let mut b = HeaderMap::new();
        b.insert(
            http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer b"),
        );
        assert_ne!(Selector::of(&a, &[]), Selector::of(&b, &[]));
        assert_eq!(
            Selector::of(&a, &[]),
            Selector::of(&a, &[http::header::AUTHORIZATION]),
            "a Vary that does name it must not produce a second, different selector"
        );
    }

    #[test]
    fn repeated_request_fields_are_combined_rather_than_read_once() {
        let mut one = HeaderMap::new();
        one.append(http::header::ACCEPT, HeaderValue::from_static("a"));
        let mut two = one.clone();
        two.append(http::header::ACCEPT, HeaderValue::from_static("b"));
        assert_ne!(
            Selector::of(&one, &[http::header::ACCEPT]),
            Selector::of(&two, &[http::header::ACCEPT])
        );
    }

    #[test]
    fn a_put_replaces_the_variant_with_the_same_selector() {
        let mut s = MemoryStore::new();
        let k = key("https://e.test/x");
        s.put(&k, entry(1));
        s.put(&k, entry(2));
        assert_eq!(s.len(), 1);
        assert_eq!(s.get(&k)[0].received_at(), entry(2).received_at());
    }

    #[test]
    fn eviction_drops_the_oldest_response_first() {
        let mut s = MemoryStore::with_capacity(2);
        s.put(&key("https://e.test/a"), entry(30));
        s.put(&key("https://e.test/b"), entry(10));
        s.put(&key("https://e.test/c"), entry(20));
        assert_eq!(s.len(), 2);
        assert!(
            s.get(&key("https://e.test/b")).is_empty(),
            "the oldest went"
        );
        assert!(!s.get(&key("https://e.test/a")).is_empty());
        assert!(!s.get(&key("https://e.test/c")).is_empty());
    }

    #[test]
    fn a_zero_capacity_store_holds_nothing_rather_than_looping() {
        let mut s = MemoryStore::with_capacity(0);
        s.put(&key("https://e.test/a"), entry(1));
        assert_eq!(s.len(), 0);
    }

    #[test]
    fn invalidation_takes_every_variant_under_the_key() {
        let mut s = MemoryStore::new();
        let k = key("https://e.test/x");
        let mut h = HeaderMap::new();
        h.insert(http::header::ACCEPT, HeaderValue::from_static("a"));
        let mut e2 = entry(2);
        e2.selector = Selector::of(&h, &[http::header::ACCEPT]);
        s.put(&k, entry(1));
        s.put(&k, e2);
        assert_eq!(s.len(), 2);
        s.invalidate(&k);
        assert_eq!(s.len(), 0);
    }
}
