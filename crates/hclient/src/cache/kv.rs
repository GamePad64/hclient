//! [`CacheStore`] over the byte seam.
//!
//! The argument lives on [`KvStore`] itself, because this module is
//! private and rustdoc publishes nothing from it.

use super::{CacheStore, Key, Selector, StoredResponse};
use hclient_core::kv::KeyValueStore;
use http::{HeaderMap, HeaderName, HeaderValue, StatusCode, Version};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use web_time::SystemTime;

/// The namespace this wrapper writes under.
const NS: &str = "cache";

/// A [`CacheStore`] over any [`KeyValueStore`], so a response cache can
/// share one backend with the client's cookie jar, HSTS set and
/// `Alt-Svc` memory instead of being a fourth `Mutex<HashMap>`.
///
/// # One key per variant, and the selector is in it
///
/// RFC 9111 keeps every `Vary` variant of one request under one cache
/// key, and this seam shows that shape: [`get`](CacheStore::get) answers
/// all of them, while [`remove`](CacheStore::remove) addresses exactly
/// one by its [`Selector`].
///
/// Only one of the two layouts over a byte store serves both. Holding
/// every variant under the request's key makes `get` a read and
/// `remove` a read-modify-write of the whole list — not atomic, and it
/// pulls every stored body back to drop one. So the selector goes into
/// the **key**: a variant is `<method>\u{1}<target>\u{1}<digest>`, each
/// variant is its own entry, and `remove` is an exact
/// [`remove`](KeyValueStore::remove).
///
/// What that costs is that `get` no longer knows the keys it wants —
/// the selectors are what it is trying to discover — so it is a
/// [`scan`](KeyValueStore::scan) over the request's prefix. That
/// operation is on the seam *because* of this: it was added after three
/// wrappers had each bent around its absence.
///
/// **The digest is what keeps a key bounded.** A selector holds whole
/// header values — an `Accept` line is routinely hundreds of bytes and
/// has no bound at all — so writing one into a key would make the key as
/// long as the request's headers. It is hashed instead, and the selector
/// itself is written into the **value**, which is where
/// [`get`](CacheStore::get) reads it from: the digest addresses, and the
/// stored copy is what answers.
///
/// A collision therefore costs a wrong *address* and not a wrong
/// *answer*. `HttpCache::lookup` filters what a store hands back by
/// `Selector` before it uses anything, so two variants colliding produce
/// an entry that fails that filter and is a miss. That is the same
/// safety argument `CacheStore`'s own documentation makes about a store
/// handing back the wrong entry, and it is why a 64-bit digest is
/// enough here where it would not be if the digest decided the answer.
///
/// # The capacity is the wrapper's, and the count is approximate
///
/// A byte store cannot count variants, so the bound lives here. It is
/// kept as a counter rather than derived, and it drifts when the store
/// expires entries on its own — **upward**, so this evicts earlier than
/// it needed to, which is the under-claiming direction. An undercount
/// needs a second process on one store, where the honest answer is that
/// the bound is per process.
///
/// Eviction picks its victim with a [`scan`](KeyValueStore::scan) of the
/// namespace, which is a walk — the same walk `cache::MemoryStore` does
/// under its lock today, and the reason a remote store should be given a
/// generous capacity rather than a tight one.
#[derive(Debug, Clone)]
pub struct KvStore<K> {
    kv: K,
    capacity: usize,
    held: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl<K> KvStore<K> {
    /// A response cache over `kv`, holding at most 512 variants.
    ///
    /// 512 is `cache::MemoryStore`'s figure and is arbitrary there too —
    /// no RFC states a minimum for a cache the way RFC 6265 §6.1 does
    /// for a jar.
    pub fn new(kv: K) -> Self {
        Self::with_capacity(kv, 512)
    }

    /// A response cache over `kv`, holding at most `capacity` variants.
    ///
    /// A capacity of zero stores nothing, which is
    /// `cache::MemoryStore`'s behaviour and the honest reading of the
    /// number.
    pub fn with_capacity(kv: K, capacity: usize) -> Self {
        Self {
            kv,
            capacity,
            held: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// How many variants this cache will hold.
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// The byte store underneath, for a caller that shares one between
    /// seams and wants to reach it.
    pub const fn inner(&self) -> &K {
        &self.kv
    }
}

/// Every variant of one request shares this prefix, which is what
/// [`scan`](KeyValueStore::scan) is given.
fn prefix_for(key: &Key) -> String {
    // `\u{1}` as the separator, for the design's reason: it occurs in
    // neither a method nor a request-target, so nothing needs escaping
    // and there is no escape to get wrong. The trailing one is part of
    // the prefix so that `GET\u{1}/a` cannot match `GET\u{1}/ab`.
    format!("{}\u{1}{}\u{1}", key.method(), key.target())
}

/// The key one variant is written under.
fn key_for(key: &Key, selector: &Selector) -> String {
    format!("{}{:016x}", prefix_for(key), digest(selector))
}

/// A 64-bit FNV-1a over the selector's fields, in the order it holds
/// them — which [`Selector`] guarantees is sorted and deduplicated.
///
/// Hand-written for this workspace's own reason: it is a dozen lines
/// whose defect would be loud, against a hashing crate in a graph that
/// carries none. `DefaultHasher` is the near neighbour and is refused
/// because `std` does not promise its output is stable between
/// releases — a key that changed with the toolchain would silently
/// orphan every entry a store had written down.
///
/// **An absent field is not an empty one** — §4.1 makes that
/// distinction and a selector that lost it would match the wrong
/// requests — so the two are fed different marker bytes.
fn digest(selector: &Selector) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut h = OFFSET;
    let mut eat = |bytes: &[u8]| {
        for b in bytes {
            h ^= u64::from(*b);
            h = h.wrapping_mul(PRIME);
        }
    };
    for (name, value) in selector.fields() {
        eat(name.as_str().as_bytes());
        eat(&[0]);
        match value {
            Some(v) => {
                eat(&[1]);
                eat(v.as_bytes());
            }
            None => eat(&[2]),
        }
        eat(&[0]);
    }
    h
}

// ---- the encoding ------------------------------------------------------
//
// Length-prefixed fields, because a stored response holds several
// variable-length parts and one of them — the body — is arbitrary bytes
// that can contain any separator. Hand-written for `hsts::kv`'s reason.
//
//     u16  status
//     u8   version, as the tag below
//     u64  requested_at, seconds since the epoch
//     u64  received_at, the same
//     u32  header count, then per header: u16 name len, name,
//          u32 value len, value
//     u32  selector field count, then per field: u16 name len, name,
//          u8 present, u32 value len, value
//     u64  body len, then the body
//
// Every count is checked against what is left rather than trusted, so a
// truncated or foreign value is refused rather than read as a shorter
// one.

/// HTTP versions, as one byte each. An unknown one is refused rather
/// than defaulted, because a stored response that came back as HTTP/1.1
/// when it was h2 is a wrong answer where a miss is only a slow one.
fn version_tag(v: Version) -> Option<u8> {
    Some(match v {
        Version::HTTP_09 => 0,
        Version::HTTP_10 => 1,
        Version::HTTP_11 => 2,
        Version::HTTP_2 => 3,
        Version::HTTP_3 => 4,
        _ => return None,
    })
}

fn version_of(tag: u8) -> Option<Version> {
    Some(match tag {
        0 => Version::HTTP_09,
        1 => Version::HTTP_10,
        2 => Version::HTTP_11,
        3 => Version::HTTP_2,
        4 => Version::HTTP_3,
        _ => return None,
    })
}

fn secs(t: SystemTime) -> u64 {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}

/// `None` for a response this encoder cannot represent — today only an
/// HTTP version it has no tag for. Refusing costs one cache entry.
fn encode(entry: &StoredResponse) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    out.extend_from_slice(&entry.status().as_u16().to_le_bytes());
    out.push(version_tag(entry.version())?);
    out.extend_from_slice(&secs(entry.requested_at()).to_le_bytes());
    out.extend_from_slice(&secs(entry.received_at()).to_le_bytes());

    let headers: Vec<(&HeaderName, &HeaderValue)> = entry.headers().iter().collect();
    out.extend_from_slice(&u32::try_from(headers.len()).ok()?.to_le_bytes());
    for (name, value) in headers {
        let name = name.as_str().as_bytes();
        out.extend_from_slice(&u16::try_from(name.len()).ok()?.to_le_bytes());
        out.extend_from_slice(name);
        let value = value.as_bytes();
        out.extend_from_slice(&u32::try_from(value.len()).ok()?.to_le_bytes());
        out.extend_from_slice(value);
    }

    let fields: Vec<(&HeaderName, Option<&HeaderValue>)> = entry.selector().fields().collect();
    out.extend_from_slice(&u32::try_from(fields.len()).ok()?.to_le_bytes());
    for (name, value) in fields {
        let name = name.as_str().as_bytes();
        out.extend_from_slice(&u16::try_from(name.len()).ok()?.to_le_bytes());
        out.extend_from_slice(name);
        match value {
            Some(v) => {
                out.push(1);
                let v = v.as_bytes();
                out.extend_from_slice(&u32::try_from(v.len()).ok()?.to_le_bytes());
                out.extend_from_slice(v);
            }
            // §4.1's *absent*, which matches only an absent field — a
            // different thing from an empty value, and a selector that
            // conflated them would match the wrong requests.
            None => out.push(0),
        }
    }

    let body = entry.body();
    out.extend_from_slice(&u64::try_from(body.len()).ok()?.to_le_bytes());
    out.extend_from_slice(body);
    Some(out)
}

/// A cursor that refuses rather than panicking, because every length it
/// reads came out of a store and none of them is trusted.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, rest) = self.0.split_at_checked(n)?;
        self.0 = rest;
        Some(head)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    /// A length-prefixed run, where the prefix is a `u32`.
    fn blob32(&mut self) -> Option<&'a [u8]> {
        let n = self.u32()? as usize;
        self.take(n)
    }
    /// A length-prefixed run, where the prefix is a `u16`.
    fn blob16(&mut self) -> Option<&'a [u8]> {
        let n = self.u16()? as usize;
        self.take(n)
    }
}

/// Seconds since the epoch as an instant, or `None` where the platform
/// cannot represent it.
///
/// **`checked_add`, and this is a defect this decoder shipped with.**
/// Every one of these values came out of a store and none is trusted, so
/// a length or a count that is wrong is refused — and a *timestamp* that
/// is wrong was being added to `UNIX_EPOCH` unchecked, which panics
/// rather than refusing. `SystemTime`'s range is the platform's: on
/// Windows it is narrower than on Linux, so `u64::MAX` seconds overflows
/// there and does not here, and the test that feeds this decoder rubbish
/// passed on every machine this workspace runs and failed on
/// `test (windows-latest)`.
///
/// Refusing loses one entry; panicking takes the caller's thread down
/// for a value a store handed back, which is the one outcome a decoder
/// written to refuse must not have.
fn at_epoch_plus(secs: u64) -> Option<SystemTime> {
    SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(secs))
}

/// The inverse of [`encode`], or `None` for bytes it did not write.
///
/// A store hands back whatever it holds — a truncated write, an older
/// format, another program's key — so this refuses rather than
/// assuming. Refusing costs a cache miss, which is slow; guessing would
/// hand [`HttpCache`](super::HttpCache) a response nobody stored.
fn decode(bytes: &[u8]) -> Option<StoredResponse> {
    let mut r = Reader(bytes);
    let status = StatusCode::from_u16(r.u16()?).ok()?;
    let version = version_of(r.u8()?)?;
    let requested_at = at_epoch_plus(r.u64()?)?;
    let received_at = at_epoch_plus(r.u64()?)?;

    let mut headers = HeaderMap::new();
    for _ in 0..r.u32()? {
        let name = HeaderName::from_bytes(r.blob16()?).ok()?;
        let value = HeaderValue::from_bytes(r.blob32()?).ok()?;
        // `append`, not `insert`: a response may carry several fields of
        // one name and `HeaderMap` keeps them, so inserting would fold a
        // multi-valued header down to its last value.
        headers.append(name, value);
    }

    let mut fields = Vec::new();
    for _ in 0..r.u32()? {
        let name = HeaderName::from_bytes(r.blob16()?).ok()?;
        let value = match r.u8()? {
            0 => None,
            1 => Some(HeaderValue::from_bytes(r.blob32()?).ok()?),
            // Not `!= 0`: this byte is written as 0 or 1, and reading
            // anything else as *present* would invent a value.
            _ => return None,
        };
        fields.push((name, value));
    }

    let body_len = usize::try_from(r.u64()?).ok()?;
    let body = bytes::Bytes::copy_from_slice(r.take(body_len)?);

    // Trailing bytes mean this is not what `encode` wrote, whatever else
    // parsed — so it is refused rather than read as a prefix.
    if !r.0.is_empty() {
        return None;
    }

    Some(StoredResponse::new(
        status,
        version,
        headers,
        body,
        Selector::from_fields(fields),
        requested_at,
        received_at,
    ))
}

pin_project_lite::pin_project! {
    /// [`KvStore`]'s answer to [`CacheStore::get`]: a scan of the
    /// request's prefix, decoded.
    ///
    /// **Named rather than boxed**, so `Send` is inferred from `K` —
    /// a `dyn` with no auto traits removes the property rather than
    /// hiding it.
    #[derive(Debug)]
    pub struct Get<F> {
        #[pin]
        inner: F,
    }
}

impl<F: Future<Output = Vec<(String, Vec<u8>)>>> Future for Get<F> {
    type Output = Vec<StoredResponse>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.project().inner.poll(cx).map(|found| {
            found
                .into_iter()
                // A value this wrapper did not write is **skipped**,
                // never repaired: handing back a response nobody stored
                // is worse than a miss.
                .filter_map(|(_, bytes)| decode(&bytes))
                .collect()
        })
    }
}

pin_project_lite::pin_project! {
    /// [`KvStore`]'s answer to [`CacheStore::len`].
    #[derive(Debug)]
    pub struct Len<F> {
        #[pin]
        inner: F,
    }
}

impl<F: Future<Output = Vec<(String, Vec<u8>)>>> Future for Len<F> {
    type Output = usize;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.project().inner.poll(cx).map(|found| found.len())
    }
}

pin_project_lite::pin_project! {
    /// [`KvStore`]'s answer to everything that changes something.
    ///
    /// A write is *remove, then put*, because the byte seam appends and
    /// §4 makes a stored response replace the variant it supersedes; an
    /// eviction is a scan and then a remove. Named for [`Get`]'s reason.
    #[project = ChangeProj]
    #[derive(Debug)]
    pub enum Change<'a, K: KeyValueStore> {
        Running {
            #[pin]
            running: K::Done<'a>,
            store: &'a K,
            rest: Vec<Step>,
        },
        // Eviction: walk the namespace, drop the oldest, then write.
        Evicting {
            #[pin]
            scanning: K::Scan<'a>,
            store: &'a K,
            held: std::sync::Arc<std::sync::atomic::AtomicUsize>,
            capacity: usize,
            rest: Vec<Step>,
        },
        // A `Done` with no fields is what this wants to be, and
        // `pin_project_lite` 0.2.17 refuses an empty variant body — so
        // it carries a unit. Written here because the field reads as an
        // oversight otherwise.
        Done { _done: () },
    }
}

/// One byte-store call a [`Change`] still owes.
#[derive(Debug)]
pub enum Step {
    Remove { key: String },
    Put { key: String, value: Vec<u8> },
    RemovePrefix { prefix: String },
}

impl<K: KeyValueStore<Instant = SystemTime>> Change<'_, K> {
    fn start(store: &K, step: Step) -> K::Done<'_> {
        match step {
            Step::Remove { key } => store.remove(NS, key),
            Step::Put { key, value } => store.put(
                NS,
                key,
                value,
                // No expiry: RFC 9111 freshness is not a TTL. A stale
                // entry is still usable — a validation can revive it
                // with a `304`, and `stale-if-error` can serve it — so a
                // store dropping it at its `Expires` would throw away an
                // entry the rules above still want. `HttpCache` decides.
                None,
                SystemTime::UNIX_EPOCH,
            ),
            Step::RemovePrefix { prefix } => store.remove_prefix(NS, prefix),
        }
    }
}

impl<K: KeyValueStore<Instant = SystemTime>> Future for Change<'_, K> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        loop {
            match self.as_mut().project() {
                ChangeProj::Done { .. } => return Poll::Ready(()),
                ChangeProj::Running {
                    running,
                    store,
                    rest,
                } => {
                    std::task::ready!(running.poll(cx));
                    if rest.is_empty() {
                        return Poll::Ready(());
                    }
                    let store = *store;
                    let next = rest.remove(0);
                    let rest = std::mem::take(rest);
                    let running = Change::<K>::start(store, next);
                    self.set(Change::Running {
                        running,
                        store,
                        rest,
                    });
                }
                ChangeProj::Evicting {
                    scanning,
                    store,
                    held,
                    capacity,
                    rest,
                } => {
                    let found = std::task::ready!(scanning.poll(cx));
                    let store = *store;
                    let capacity = *capacity;
                    let held = held.clone();
                    let mut rest = std::mem::take(rest);

                    // The victim is the oldest by `received_at`, which is
                    // `cache::MemoryStore`'s rule — read out of the
                    // encoded value rather than kept beside it, so there
                    // is one statement of it.
                    let mut victims: Vec<(u64, String)> = found
                        .iter()
                        .filter_map(|(k, v)| Some((received_at_of(v)?, k.clone())))
                        .collect();
                    victims.sort_by_key(|(t, _)| *t);

                    let over = found.len().saturating_sub(capacity.saturating_sub(1));
                    for (_, key) in victims.into_iter().take(over) {
                        held.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                        rest.insert(0, Step::Remove { key });
                    }

                    if rest.is_empty() {
                        self.set(Change::Done { _done: () });
                    } else {
                        let next = rest.remove(0);
                        let running = Change::<K>::start(store, next);
                        self.set(Change::Running {
                            running,
                            store,
                            rest,
                        });
                    }
                }
            }
        }
    }
}

/// `received_at` alone, read out of an encoded value without decoding
/// the rest of it — an eviction compares timestamps and has no use for
/// the body it would otherwise pull back.
fn received_at_of(bytes: &[u8]) -> Option<u64> {
    let mut r = Reader(bytes);
    r.take(2 + 1 + 8)?;
    r.u64()
}

impl<K: KeyValueStore<Instant = SystemTime>> CacheStore for KvStore<K> {
    type Get<'a>
        = Get<K::Scan<'a>>
    where
        Self: 'a;
    type Done<'a>
        = Change<'a, K>
    where
        Self: 'a;
    type Len<'a>
        = Len<K::Scan<'a>>
    where
        Self: 'a;

    fn get<'a>(&'a self, key: &'a Key) -> Self::Get<'a> {
        // `now` is the epoch because this wrapper stores no expiry — see
        // `Change::start`. Passing a real clock would need one, and this
        // module has not got one.
        Get {
            inner: self.kv.scan(NS, prefix_for(key), SystemTime::UNIX_EPOCH),
        }
    }

    fn put<'a>(&'a self, key: &'a Key, entry: StoredResponse) -> Self::Done<'a> {
        let Some(value) = encode(&entry) else {
            // A response this encoder cannot represent is not stored,
            // which costs a cache miss and nothing else.
            return Change::Done { _done: () };
        };
        if self.capacity == 0 {
            return Change::Done { _done: () };
        }
        let key = key_for(key, entry.selector());

        // Replace, then write — the seam appends, and §4 makes a stored
        // response replace the variant with the same selector. The
        // counter is advanced here rather than after the write, so two
        // concurrent writers cannot both see room.
        let rest = vec![Step::Put {
            key: key.clone(),
            value,
        }];
        let held = self.held.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;

        if held > self.capacity {
            Change::Evicting {
                scanning: self.kv.scan(NS, String::new(), SystemTime::UNIX_EPOCH),
                store: &self.kv,
                held: self.held.clone(),
                capacity: self.capacity,
                rest: {
                    let mut rest = rest;
                    rest.insert(0, Step::Remove { key });
                    rest
                },
            }
        } else {
            Change::Running {
                running: self.kv.remove(NS, key),
                store: &self.kv,
                rest,
            }
        }
    }

    fn remove<'a>(&'a self, key: &'a Key, selector: &'a Selector) -> Self::Done<'a> {
        // Exact, which is what the selector-in-the-key layout buys: no
        // read, no rewrite of the neighbouring variants.
        self.held.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        Change::Running {
            running: self.kv.remove(NS, key_for(key, selector)),
            store: &self.kv,
            rest: Vec::new(),
        }
    }

    fn invalidate<'a>(&'a self, key: &'a Key) -> Self::Done<'a> {
        // RFC 9111 §4.4, and one operation rather than a read of every
        // variant: `remove_prefix` is on the seam for this.
        Change::Running {
            running: self.kv.remove_prefix(NS, prefix_for(key)),
            store: &self.kv,
            rest: Vec::new(),
        }
    }

    fn len(&self) -> Self::Len<'_> {
        // The scan rather than the counter, because the counter is
        // approximate by construction and this is the one method whose
        // whole job is to answer the question.
        Len {
            inner: self.kv.scan(NS, String::new(), SystemTime::UNIX_EPOCH),
        }
    }

    fn clear(&self) -> Self::Done<'_> {
        self.held.store(0, std::sync::atomic::Ordering::Relaxed);
        Change::Running {
            running: self.kv.clear(NS),
            store: &self.kv,
            rest: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_executor::block_on;
    use hclient_core::kv::MemoryStore as Kv;
    use http::Method;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn store() -> KvStore<Kv<SystemTime>> {
        KvStore::new(Kv::new())
    }

    fn key(target: &str) -> Key {
        Key::new(&Method::GET, &target.parse().unwrap()).expect("cacheable")
    }

    fn sel(fields: &[(&str, Option<&str>)]) -> Selector {
        Selector::from_fields(fields.iter().map(|(n, v)| {
            (
                HeaderName::from_bytes(n.as_bytes()).expect("name"),
                v.map(|v| HeaderValue::from_str(v).expect("value")),
            )
        }))
    }

    fn entry(body: &str, selector: Selector, received: u64) -> StoredResponse {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", HeaderValue::from_static("text/plain"));
        StoredResponse::new(
            StatusCode::OK,
            Version::HTTP_11,
            headers,
            bytes::Bytes::copy_from_slice(body.as_bytes()),
            selector,
            at(received),
            at(received),
        )
    }

    // ---- the encoding ------------------------------------------------

    /// Every field survives, over several shapes rather than one: a
    /// decoder that dropped a field would still round-trip whichever
    /// value happened to be its default.
    #[test]
    fn every_field_survives_the_round_trip() {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", HeaderValue::from_static("application/json"));
        headers.append("set-cookie", HeaderValue::from_static("a=1"));
        headers.append("set-cookie", HeaderValue::from_static("b=2"));

        let e = StoredResponse::new(
            StatusCode::NOT_MODIFIED,
            Version::HTTP_2,
            headers,
            bytes::Bytes::from_static(&[0, 1, 0xff, b'\n']),
            sel(&[("accept", Some("text/html")), ("accept-encoding", None)]),
            at(10),
            at(20),
        );

        let back = decode(&encode(&e).expect("encodable")).expect("decodable");
        assert_eq!(back.status(), e.status());
        assert_eq!(back.version(), e.version());
        assert_eq!(back.body(), e.body());
        assert_eq!(back.requested_at(), e.requested_at());
        assert_eq!(back.received_at(), e.received_at());
        assert_eq!(back.headers(), e.headers());
        assert_eq!(back.selector(), e.selector());
    }

    /// **A repeated header keeps every value.** `HeaderMap` holds them
    /// all, and a decoder using `insert` would fold `Set-Cookie: a` and
    /// `Set-Cookie: b` down to the last one — losing a cookie on every
    /// cache hit.
    #[test]
    fn a_repeated_header_survives_with_all_its_values() {
        let mut headers = HeaderMap::new();
        headers.append("set-cookie", HeaderValue::from_static("a=1"));
        headers.append("set-cookie", HeaderValue::from_static("b=2"));
        let e = StoredResponse::new(
            StatusCode::OK,
            Version::HTTP_11,
            headers,
            bytes::Bytes::new(),
            sel(&[]),
            at(0),
            at(0),
        );

        let back = decode(&encode(&e).expect("encodable")).expect("decodable");
        let got: Vec<&HeaderValue> = back.headers().get_all("set-cookie").iter().collect();
        assert_eq!(got.len(), 2, "both values, not just the last");
    }

    /// §4.1's *absent* is not an empty value, and a selector that
    /// conflated them would match the wrong requests.
    #[test]
    fn an_absent_selector_field_is_not_an_empty_one() {
        let absent = entry("b", sel(&[("accept", None)]), 0);
        let empty = entry("b", sel(&[("accept", Some(""))]), 0);

        assert_ne!(absent.selector(), empty.selector(), "the fixture's premise");
        let a = decode(&encode(&absent).expect("enc")).expect("dec");
        let e = decode(&encode(&empty).expect("enc")).expect("dec");
        assert_eq!(a.selector(), absent.selector());
        assert_eq!(e.selector(), empty.selector());
        assert_ne!(a.selector(), e.selector());
    }

    /// A timestamp too large for this platform is refused rather than
    /// panicking — see `cookie::kv`'s test of the same name for the
    /// defect and for why `u64::MAX` is the honest value to use.
    #[test]
    fn a_timestamp_that_overflows_the_clock_is_refused_rather_than_panicking() {
        let mut bytes = encode(&entry("body", sel(&[]), 0)).expect("enc");
        // `requested_at` follows the two-byte status and the version tag.
        bytes[3..11].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(decode(&bytes).is_none());
    }

    /// A store hands back whatever it holds. Refusing costs a miss;
    /// guessing would hand back a response nobody stored.
    #[test]
    fn bytes_this_encoder_did_not_write_are_refused() {
        let good = encode(&entry("body", sel(&[]), 0)).expect("encodable");

        assert!(decode(&[]).is_none(), "empty");
        for cut in [1, 5, 11, good.len() - 1] {
            assert!(decode(&good[..cut]).is_none(), "truncated at {cut}");
        }

        let mut trailing = good.clone();
        trailing.push(0);
        assert!(decode(&trailing).is_none(), "trailing bytes");

        let mut bad_version = good.clone();
        bad_version[2] = 99;
        assert!(decode(&bad_version).is_none(), "an unknown HTTP version");
    }

    /// The presence byte is read as 0 or 1 and never as `!= 0`: anything
    /// else is a value this decoder did not write, and reading it as
    /// *present* would invent a selector value.
    #[test]
    fn a_selector_presence_byte_that_is_neither_zero_nor_one_is_refused() {
        let e = entry("b", sel(&[("accept", None)]), 0);
        let mut bytes = encode(&e).expect("encodable");
        // Found rather than computed from the tail: an offset derived
        // from the length is wrong by however long the body is, which
        // is how the first version of this test failed.
        let idx = bytes
            .windows(b"accept".len())
            .position(|w| w == b"accept")
            .expect("the selector's one field name")
            + b"accept".len();
        assert_eq!(bytes[idx], 0, "the fixture's premise: an absent field");
        bytes[idx] = 2;
        assert!(decode(&bytes).is_none(), "an absent field's byte");

        // **And again where a value really follows**, which is the case
        // that discriminates: with the byte read as `!= 0` the first
        // half above still refuses, because there is no length behind an
        // absent field to read — so it would pass over a decoder that
        // had stopped checking. Here the bytes after it parse, and only
        // a decoder that rejects the byte itself says no.
        let present = entry("b", sel(&[("accept", Some("text/html"))]), 0);
        let mut bytes = encode(&present).expect("encodable");
        let idx = bytes
            .windows(b"accept".len())
            .rposition(|w| w == b"accept")
            .expect("the selector's one field name")
            + b"accept".len();
        assert_eq!(bytes[idx], 1, "the fixture's premise: a present field");
        bytes[idx] = 2;
        assert!(decode(&bytes).is_none(), "a present field's byte");
    }

    /// `received_at` is read out of the encoded value without decoding
    /// the body beside it, which is what makes eviction cheap.
    #[test]
    fn the_eviction_timestamp_is_readable_without_decoding_the_rest() {
        let e = entry("body", sel(&[]), 1234);
        let bytes = encode(&e).expect("encodable");
        assert_eq!(received_at_of(&bytes), Some(1234));
        assert!(
            received_at_of(&bytes[..4]).is_none(),
            "and refuses a short one"
        );
    }

    // ---- the key -----------------------------------------------------

    /// Two selectors that differ produce two keys; the same selector
    /// produces the same key however it was built.
    #[test]
    fn the_key_follows_the_selector() {
        let k = key("https://example.com/");
        let a = sel(&[("accept", Some("text/html"))]);
        let b = sel(&[("accept", Some("application/json"))]);

        assert_ne!(key_for(&k, &a), key_for(&k, &b));
        assert_eq!(
            key_for(&k, &a),
            key_for(&k, &sel(&[("accept", Some("text/html"))]))
        );
    }

    /// The digest separates *absent* from *empty*, which the value's own
    /// encoding also does — two statements of one rule, and both are
    /// needed: the key decides where an entry lands and the value
    /// decides what it matches.
    #[test]
    fn the_digest_separates_an_absent_field_from_an_empty_one() {
        let k = key("https://example.com/");
        assert_ne!(
            key_for(&k, &sel(&[("accept", None)])),
            key_for(&k, &sel(&[("accept", Some(""))]))
        );
    }

    /// **The prefix ends at a separator**, so one request's variants
    /// cannot be found under another whose target is a prefix of it.
    #[test]
    fn one_targets_prefix_does_not_match_a_longer_target() {
        let short = key("https://example.com/a");
        let long = key("https://example.com/ab");
        assert!(!prefix_for(&long).starts_with(&prefix_for(&short)));
    }

    /// The method is part of the key, so `GET` and `HEAD` of one URL are
    /// two entries.
    #[test]
    fn two_methods_of_one_url_are_two_entries() {
        let get = Key::new(&Method::GET, &"https://a.test/".parse().unwrap()).expect("k");
        let head = Key::new(&Method::HEAD, &"https://a.test/".parse().unwrap()).expect("k");
        assert_ne!(prefix_for(&get), prefix_for(&head));
    }

    // ---- the seam ----------------------------------------------------

    #[test]
    fn a_stored_entry_comes_back() {
        let s = store();
        let k = key("https://example.com/");
        block_on(s.put(&k, entry("hello", sel(&[]), 0)));

        let got = block_on(s.get(&k));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].body(), &bytes::Bytes::from_static(b"hello"));
    }

    #[test]
    fn an_unknown_key_answers_nothing() {
        let s = store();
        assert!(block_on(s.get(&key("https://example.com/"))).is_empty());
    }

    /// **Every variant under one key comes back**, which is what the
    /// `Vary` machinery above needs: `HttpCache::lookup` picks among
    /// them by selector.
    #[test]
    fn every_variant_under_one_key_comes_back() {
        let s = store();
        let k = key("https://example.com/");
        block_on(s.put(&k, entry("html", sel(&[("accept", Some("text/html"))]), 0)));
        block_on(s.put(
            &k,
            entry("json", sel(&[("accept", Some("application/json"))]), 0),
        ));

        let mut bodies: Vec<String> = block_on(s.get(&k))
            .iter()
            .map(|e| String::from_utf8_lossy(e.body()).into_owned())
            .collect();
        bodies.sort();
        assert_eq!(bodies, ["html", "json"]);
    }

    /// And a neighbouring key's variants do not, which the prefix is
    /// what guarantees.
    #[test]
    fn another_keys_variants_are_not_answered() {
        let s = store();
        block_on(s.put(&key("https://example.com/a"), entry("a", sel(&[]), 0)));
        block_on(s.put(&key("https://example.com/b"), entry("b", sel(&[]), 0)));

        let got = block_on(s.get(&key("https://example.com/a")));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].body(), &bytes::Bytes::from_static(b"a"));
    }

    /// §4: a stored response **replaces** the variant with the same
    /// selector. The byte seam appends, so the wrapper removes first.
    #[test]
    fn a_second_put_with_one_selector_replaces_rather_than_accumulating() {
        let s = store();
        let k = key("https://example.com/");
        let selector = sel(&[("accept", Some("text/html"))]);
        block_on(s.put(&k, entry("first", selector.clone(), 0)));
        block_on(s.put(&k, entry("second", selector, 1)));

        let got = block_on(s.get(&k));
        assert_eq!(got.len(), 1, "one variant, not two");
        assert_eq!(got[0].body(), &bytes::Bytes::from_static(b"second"));
    }

    /// **`remove` addresses one variant**, which is what putting the
    /// selector in the key buys: its neighbours are untouched and
    /// nothing was read to do it.
    #[test]
    fn remove_drops_one_variant_and_leaves_the_others() {
        let s = store();
        let k = key("https://example.com/");
        let html = sel(&[("accept", Some("text/html"))]);
        block_on(s.put(&k, entry("html", html.clone(), 0)));
        block_on(s.put(
            &k,
            entry("json", sel(&[("accept", Some("application/json"))]), 0),
        ));

        block_on(s.remove(&k, &html));

        let got = block_on(s.get(&k));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].body(), &bytes::Bytes::from_static(b"json"));
    }

    /// RFC 9111 §4.4: invalidation drops **every** variant of one key.
    #[test]
    fn invalidate_drops_every_variant_of_one_key_and_no_others() {
        let s = store();
        let k = key("https://example.com/a");
        block_on(s.put(&k, entry("html", sel(&[("accept", Some("text/html"))]), 0)));
        block_on(s.put(
            &k,
            entry("json", sel(&[("accept", Some("application/json"))]), 0),
        ));
        block_on(s.put(&key("https://example.com/b"), entry("other", sel(&[]), 0)));

        block_on(s.invalidate(&k));

        assert!(block_on(s.get(&k)).is_empty());
        assert_eq!(block_on(s.get(&key("https://example.com/b"))).len(), 1);
    }

    #[test]
    fn len_counts_variants_across_keys_and_clear_empties_it() {
        let s = store();
        block_on(s.put(&key("https://a.test/"), entry("a", sel(&[]), 0)));
        block_on(s.put(
            &key("https://b.test/"),
            entry("b", sel(&[("accept", Some("x"))]), 0),
        ));
        assert_eq!(block_on(s.len()), 2);

        block_on(s.clear());
        assert_eq!(block_on(s.len()), 0);
    }

    /// `clear` is this namespace's alone, which is what lets one byte
    /// store back all four seams.
    #[test]
    fn clear_leaves_another_seams_namespace_alone() {
        let kv = Kv::<SystemTime>::new();
        block_on(kv.put(
            "cookie",
            "com.example".to_owned(),
            b"c".to_vec(),
            None,
            at(0),
        ));
        let s = KvStore::new(kv);
        block_on(s.put(&key("https://a.test/"), entry("a", sel(&[]), 0)));

        block_on(s.clear());

        assert_eq!(
            block_on(s.inner().get("cookie", "com.example", at(0))),
            vec![b"c".to_vec()]
        );
    }

    /// A value this wrapper did not write is **skipped**, never
    /// repaired. Written through the byte seam directly, because nothing
    /// above can produce one — which is the point: a store outlives a
    /// format.
    #[test]
    fn a_value_the_decoder_refuses_is_skipped_rather_than_substituted() {
        let kv = Kv::<SystemTime>::new();
        let k = key("https://example.com/");
        block_on(kv.put(
            NS,
            format!("{}deadbeefdeadbeef", prefix_for(&k)),
            b"not an entry".to_vec(),
            None,
            at(0),
        ));
        let s = KvStore::new(kv);

        assert!(block_on(s.get(&k)).is_empty());
    }

    /// And a readable neighbour survives it.
    #[test]
    fn one_unreadable_variant_does_not_hide_the_readable_ones() {
        let kv = Kv::<SystemTime>::new();
        let k = key("https://example.com/");
        block_on(kv.put(
            NS,
            format!("{}deadbeefdeadbeef", prefix_for(&k)),
            b"rubbish".to_vec(),
            None,
            at(0),
        ));
        let s = KvStore::new(kv);
        block_on(s.put(&k, entry("good", sel(&[]), 0)));

        let got = block_on(s.get(&k));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].body(), &bytes::Bytes::from_static(b"good"));
    }

    // ---- the capacity ------------------------------------------------

    /// The bound holds, and the victim is the oldest by `received_at` —
    /// `cache::MemoryStore`'s rule.
    #[test]
    fn a_full_cache_evicts_the_oldest_variant() {
        let s = KvStore::with_capacity(Kv::<SystemTime>::new(), 2);
        block_on(s.put(&key("https://a.test/"), entry("a", sel(&[]), 10)));
        block_on(s.put(&key("https://b.test/"), entry("b", sel(&[]), 20)));
        block_on(s.put(&key("https://c.test/"), entry("c", sel(&[]), 30)));

        assert_eq!(block_on(s.len()), 2, "the bound holds");
        assert!(
            block_on(s.get(&key("https://a.test/"))).is_empty(),
            "the oldest went"
        );
        assert_eq!(block_on(s.get(&key("https://c.test/"))).len(), 1);
    }

    /// A capacity of zero stores nothing, which is what the number says
    /// and what `cache::MemoryStore` does.
    #[test]
    fn a_capacity_of_zero_stores_nothing() {
        let s = KvStore::with_capacity(Kv::<SystemTime>::new(), 0);
        block_on(s.put(&key("https://a.test/"), entry("a", sel(&[]), 0)));
        assert_eq!(block_on(s.len()), 0);
    }
}
