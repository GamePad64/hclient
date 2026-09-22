//! [`CookieStore`] over the byte seam.
//!
//! The argument lives on [`KvStore`] itself, because this module is
//! private and rustdoc publishes nothing from it.

use super::{Cookie, CookieKey, CookieStore, SameSite};
use hclient_core::kv::KeyValueStore;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;
use web_time::SystemTime;

/// The namespace this wrapper writes under.
const NS: &str = "cookie";

/// A [`CookieStore`] over any [`KeyValueStore`], so a jar can share one
/// backend with the client's HSTS set, response cache and `Alt-Svc`
/// memory instead of being a fourth `Mutex<HashMap>`.
///
/// # The whole cookie is encoded, not its record
///
/// [`CookieRecord`](super::CookieRecord) is the serialisable form of a
/// [`Cookie`] and is **not** what this writes, which is the trap that
/// module's own documentation records: `to_record` answers `None` for a
/// session cookie, because a cookie with no `Expires` is one that is
/// meant not to survive a restart. That is right for a *file* and wrong
/// for a store, since this store *is* where the jar keeps its cookies
/// while the process runs — a wrapper built on records would drop every
/// session cookie and report success, which is what the first outside
/// store did within five minutes of meeting this seam.
///
/// It also carries one field a record has not got. `seq` is the jar's
/// insertion counter and RFC 6265 §5.4's tiebreak among cookies of equal
/// path length, and §5.7's replacement keeps the *old* cookie's `seq` —
/// so a cookie that came back without one would jump the queue against
/// its neighbours on every reload.
///
/// # The key is the whole [`CookieKey`], and the domain is first
///
/// `<reversed domain>\u{1}<name>\u{1}<path>\u{1}<host-only>` — §5.7's
/// replacement key, exactly, so [`remove`](CookieStore::remove) and the
/// replacement inside [`put`](CookieStore::put) are **exact** rather
/// than a read of the domain and a rewrite of it.
///
/// The domain comes first and is reversed, which is what makes
/// [`get`](CookieStore::get) a prefix scan. An **IP literal is never
/// reversed**, because a reversed `1.0.0.127` is a different address,
/// and `candidate_domains` hands one back as its single candidate.
///
/// # Capacity is here, and the global count is approximate
///
/// A byte store cannot count cookies, so RFC 6265 §6.1's two bounds
/// live in this wrapper. `max_per_domain` is exact — its scan is one
/// prefix, which `put` is reading anyway. `max_cookies` is a counter,
/// because the alternative is a walk of the whole namespace on every
/// insertion: it is exact for one process and drifts **upward** when the
/// store expires entries on its own, so this evicts earlier than it
/// needed to, which is the under-claiming direction. An undercount needs
/// a second process on one store, where the honest answer is that the
/// bound is per process.
#[derive(Debug, Clone)]
pub struct KvStore<K> {
    kv: K,
    capacity: super::Capacity,
    held: Arc<AtomicUsize>,
}

impl<K> KvStore<K> {
    /// A jar's store over `kv`, with RFC 6265 §6.1's minimum bounds.
    pub fn new(kv: K) -> Self {
        Self::with_capacity(kv, super::Capacity::default())
    }

    /// A jar's store over `kv`, bounded by `capacity`.
    pub fn with_capacity(kv: K, capacity: super::Capacity) -> Self {
        Self {
            kv,
            capacity,
            held: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// The bounds this store holds itself to.
    pub const fn capacity(&self) -> super::Capacity {
        self.capacity
    }

    /// The byte store underneath, for a caller that shares one between
    /// seams and wants to reach it.
    pub const fn inner(&self) -> &K {
        &self.kv
    }
}

/// A domain with its labels reversed, so that one host's cookies sit in
/// one range of an ordered store and a lookup is a prefix.
///
/// **An IP literal is returned unchanged**: a reversed `1.0.0.127` is a
/// different address, and `candidate_domains` really does hand one back.
fn reverse_labels(domain: &str) -> String {
    if is_ip_literal(domain) {
        return domain.to_owned();
    }
    let mut out = String::with_capacity(domain.len());
    for label in domain.rsplit('.') {
        if !out.is_empty() {
            out.push('.');
        }
        out.push_str(label);
    }
    out
}

/// Whether `host` is an address rather than a name. Written as *would
/// reversing change the meaning* rather than as a parse.
fn is_ip_literal(host: &str) -> bool {
    host.contains(':')
        || (!host.is_empty()
            && host
                .split('.')
                .all(|l| !l.is_empty() && l.bytes().all(|b| b.is_ascii_digit())))
}

/// Every cookie of one domain shares this prefix.
///
/// The trailing separator is part of it, so that `com.example` cannot
/// match `com.examples`.
fn prefix_for(domain: &str) -> String {
    format!("{}\u{1}", reverse_labels(domain))
}

/// The key one cookie is written under — §5.7's replacement key.
fn key_for(key: &CookieKey) -> String {
    format!(
        "{}{}\u{1}{}\u{1}{}",
        prefix_for(key.domain()),
        key.name(),
        key.path(),
        u8::from(key.host_only()),
    )
}

// ---- the encoding ------------------------------------------------------
//
// Length-prefixed, because a cookie holds four strings and a value may
// contain any separator this could have chosen.
//
//     u64  expires, seconds since the epoch; u64::MAX is "no expiry",
//          which is a session cookie — see the type's own docs
//     u64  creation
//     u64  last_access
//     u64  seq
//     u8   flags: host_only | persistent<<1 | secure<<2 | http_only<<3
//     u8   same_site: 0 none, 1 Strict, 2 Lax, 3 None
//     u16  name len, name
//     u32  value len, value
//     u16  domain len, domain
//     u16  path len, path
//
// `u64::MAX` for an absent expiry rather than a flag byte, because the
// field is fixed-width either way and a sentinel that cannot be a real
// second — 584 billion years hence — costs nothing. Every length is
// checked against what is left rather than trusted.

fn secs(t: SystemTime) -> u64 {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}

/// The sentinel `expires` a session cookie is written with.
const NO_EXPIRY: u64 = u64::MAX;

fn same_site_tag(s: Option<SameSite>) -> u8 {
    match s {
        None => 0,
        Some(SameSite::Strict) => 1,
        Some(SameSite::Lax) => 2,
        Some(SameSite::None) => 3,
    }
}

/// What a `same_site` byte decoded to.
///
/// Two levels of *nothing* meet here and they are different facts —
/// `Absent` is a cookie with no `SameSite` attribute, where an unknown
/// tag means the value did not come from this encoder. An
/// `Option<Option<SameSite>>` says both and reads as neither.
enum SameSiteOf {
    Absent,
    Set(SameSite),
    Unknown,
}

fn same_site_of(tag: u8) -> SameSiteOf {
    match tag {
        0 => SameSiteOf::Absent,
        1 => SameSiteOf::Set(SameSite::Strict),
        2 => SameSiteOf::Set(SameSite::Lax),
        3 => SameSiteOf::Set(SameSite::None),
        _ => SameSiteOf::Unknown,
    }
}

/// `None` for a cookie this encoder cannot represent — today only one
/// whose name, value, domain or path is longer than its length field.
fn encode(c: &Cookie) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    out.extend_from_slice(&c.expires.map_or(NO_EXPIRY, secs).to_le_bytes());
    out.extend_from_slice(&secs(c.creation).to_le_bytes());
    out.extend_from_slice(&secs(c.last_access).to_le_bytes());
    out.extend_from_slice(&c.seq.to_le_bytes());
    out.push(
        u8::from(c.host_only)
            | u8::from(c.persistent) << 1
            | u8::from(c.secure) << 2
            | u8::from(c.http_only) << 3,
    );
    out.push(same_site_tag(c.same_site));

    let name = c.name.as_bytes();
    out.extend_from_slice(&u16::try_from(name.len()).ok()?.to_le_bytes());
    out.extend_from_slice(name);
    let value = c.value.as_bytes();
    out.extend_from_slice(&u32::try_from(value.len()).ok()?.to_le_bytes());
    out.extend_from_slice(value);
    let domain = c.domain.as_bytes();
    out.extend_from_slice(&u16::try_from(domain.len()).ok()?.to_le_bytes());
    out.extend_from_slice(domain);
    let path = c.path.as_bytes();
    out.extend_from_slice(&u16::try_from(path.len()).ok()?.to_le_bytes());
    out.extend_from_slice(path);
    Some(out)
}

/// A cursor that refuses rather than panicking: every length it reads
/// came out of a store and none of them is trusted.
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
    fn str16(&mut self) -> Option<String> {
        let n = self.u16()? as usize;
        std::str::from_utf8(self.take(n)?).ok().map(str::to_owned)
    }
    fn str32(&mut self) -> Option<String> {
        let n = self.u32()? as usize;
        std::str::from_utf8(self.take(n)?).ok().map(str::to_owned)
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
/// assuming. Refusing loses one cookie; guessing would put a cookie on
/// the wire that nobody stored, and the jar re-applies §5.1.3's
/// domain-match to whatever comes back so a wrong one is dropped at the
/// filter rather than sent.
fn decode(bytes: &[u8]) -> Option<Cookie> {
    let mut r = Reader(bytes);
    let expires = match r.u64()? {
        NO_EXPIRY => None,
        s => Some(at_epoch_plus(s)?),
    };
    let creation = at_epoch_plus(r.u64()?)?;
    let last_access = at_epoch_plus(r.u64()?)?;
    let seq = r.u64()?;
    let flags = r.u8()?;
    // Bits above the four this encoder writes mean the value came from
    // somewhere else, so it is refused rather than masked — masking
    // would read a foreign record as a cookie with plausible flags.
    if flags & !0b1111 != 0 {
        return None;
    }
    let same_site = match same_site_of(r.u8()?) {
        SameSiteOf::Absent => None,
        SameSiteOf::Set(v) => Some(v),
        // Refused rather than read as absent: `SameSite=Strict`
        // arriving as `None` is a cookie sent on requests it was meant
        // to sit out.
        SameSiteOf::Unknown => return None,
    };
    let name = r.str16()?;
    let value = r.str32()?;
    let domain = r.str16()?;
    let path = r.str16()?;

    // Trailing bytes mean this is not what `encode` wrote, whatever else
    // parsed.
    if !r.0.is_empty() {
        return None;
    }

    Some(Cookie {
        name,
        value,
        domain,
        path,
        expires,
        creation,
        last_access,
        seq,
        host_only: flags & 1 != 0,
        persistent: flags & 0b10 != 0,
        secure: flags & 0b100 != 0,
        http_only: flags & 0b1000 != 0,
        same_site,
    })
}

pin_project_lite::pin_project! {
    /// [`KvStore`]'s answer to [`CookieStore::get`] and
    /// [`all`](CookieStore::all): a scan, decoded.
    ///
    /// **Named rather than boxed**, so `Send` is inferred from `K` — a
    /// `dyn` with no auto traits removes the property rather than hiding
    /// it.
    #[derive(Debug)]
    pub struct Get<F> {
        #[pin]
        inner: F,
    }
}

impl<F: Future<Output = Vec<(String, Vec<u8>)>>> Future for Get<F> {
    type Output = Vec<Cookie>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.project().inner.poll(cx).map(|found| {
            let mut out: Vec<Cookie> = found
                .into_iter()
                // A value this wrapper did not write is **skipped**,
                // never repaired.
                .filter_map(|(_, bytes)| decode(&bytes))
                .collect();
            // `all` documents "oldest arrival first" and a scan has no
            // order, so the order is restored here — from `seq`, which
            // is what "arrival" means.
            out.sort_by_key(|c| c.seq);
            out
        })
    }
}

pin_project_lite::pin_project! {
    /// [`KvStore`]'s answer to [`CookieStore::len`].
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

/// One byte-store call a [`Done`] still owes.
#[derive(Debug)]
pub enum Step {
    Remove { key: String },
    Put { key: String, value: Vec<u8> },
}

pin_project_lite::pin_project! {
    /// [`KvStore`]'s answer to everything that changes something.
    ///
    /// Three shapes: a plain sequence of writes, a *read-then-write* for
    /// [`touch`](CookieStore::touch) and for eviction, and a finished
    /// one. Named for [`Get`]'s reason.
    #[project = DoneProj]
    #[derive(Debug)]
    pub enum Done<'a, K: KeyValueStore> {
        Running {
            #[pin]
            running: K::Done<'a>,
            store: &'a K,
            rest: Vec<Step>,
        },
        // Read the namespace, decide what to drop or rewrite, then do
        // it. `plan` is what turns the scan's answer into steps.
        Reading {
            #[pin]
            reading: K::Scan<'a>,
            store: &'a K,
            plan: Plan,
            rest: Vec<Step>,
        },
        // `pin_project_lite` 0.2.17 refuses an empty variant body, so
        // this carries a unit.
        Finished { _done: () },
    }
}

/// What a [`Done::Reading`] does with what it read.
#[derive(Debug)]
pub enum Plan {
    /// Set `last_access` on every cookie whose key is in the list —
    /// §5.4's step after a `Cookie` header has been produced.
    Touch { keys: Vec<String>, now: SystemTime },
    /// Drop the least recently used until what was read is under
    /// `bound`.
    ///
    /// Used for both of §6.1's limits, because they are one rule over
    /// two different reads: the global bound evicts over a scan of the
    /// namespace, the per-domain one over a scan of that domain's
    /// prefix. `held` is decremented per victim and is `None` for the
    /// per-domain pass, where the count has not changed — the cookie
    /// being made room for is still going in.
    Evict {
        bound: usize,
        held: Option<Arc<AtomicUsize>>,
    },
}

impl<K: KeyValueStore<Instant = SystemTime>> Done<'_, K> {
    fn start(store: &K, step: Step) -> K::Done<'_> {
        match step {
            Step::Remove { key } => store.remove(NS, key),
            Step::Put { key, value } => store.put(
                NS,
                key,
                value,
                // No expiry handed down, deliberately: a cookie's
                // `Expires` is already inside the encoded value, and
                // `CookieJar` applies §5.3's eviction of expired
                // cookies itself. Handing it to the store as well would
                // make that eviction unobservable — `hsts::KvStore`'s
                // argument, and this seam has the same shape.
                None,
                SystemTime::UNIX_EPOCH,
            ),
        }
    }

    fn next(store: &K, mut rest: Vec<Step>) -> Done<'_, K> {
        if rest.is_empty() {
            return Done::Finished { _done: () };
        }
        let first = rest.remove(0);
        Done::Running {
            running: Self::start(store, first),
            store,
            rest,
        }
    }
}

impl<K: KeyValueStore<Instant = SystemTime>> Future for Done<'_, K> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        loop {
            match self.as_mut().project() {
                DoneProj::Finished { .. } => return Poll::Ready(()),
                DoneProj::Running {
                    running,
                    store,
                    rest,
                } => {
                    std::task::ready!(running.poll(cx));
                    if rest.is_empty() {
                        return Poll::Ready(());
                    }
                    let store = *store;
                    let rest = std::mem::take(rest);
                    self.set(Done::next(store, rest));
                }
                DoneProj::Reading {
                    reading,
                    store,
                    plan,
                    rest,
                } => {
                    let found = std::task::ready!(reading.poll(cx));
                    let store = *store;
                    let mut steps = std::mem::take(rest);
                    match plan {
                        Plan::Touch { keys, now } => {
                            let now = *now;
                            for (key, bytes) in found {
                                if !keys.contains(&key) {
                                    continue;
                                }
                                let Some(mut c) = decode(&bytes) else {
                                    continue;
                                };
                                c.last_access = now;
                                if let Some(value) = encode(&c) {
                                    // A rewrite in place: the key is the
                                    // same, so the remove is what keeps
                                    // the appending seam from holding two.
                                    steps.push(Step::Remove { key: key.clone() });
                                    steps.push(Step::Put { key, value });
                                }
                            }
                        }
                        Plan::Evict { bound, held } => {
                            let bound = *bound;
                            let held = held.clone();
                            let mut victims: Vec<(SystemTime, u64, String)> = found
                                .iter()
                                .filter_map(|(k, v)| {
                                    let c = decode(v)?;
                                    Some((c.last_access, c.seq, k.clone()))
                                })
                                .collect();
                            // §5.3's rule: least recently used, with the
                            // insertion order as the tiebreak — the same
                            // comparison `cookie::MemoryStore` makes.
                            victims.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
                            let over = found.len().saturating_sub(bound);
                            for (_, _, key) in victims.into_iter().take(over) {
                                if let Some(held) = &held {
                                    held.fetch_sub(1, Ordering::Relaxed);
                                }
                                steps.insert(0, Step::Remove { key });
                            }
                        }
                    }
                    self.set(Done::next(store, steps));
                }
            }
        }
    }
}

impl<K: KeyValueStore<Instant = SystemTime>> CookieStore for KvStore<K> {
    type Get<'a>
        = Get<K::Scan<'a>>
    where
        Self: 'a;
    type Done<'a>
        = Done<'a, K>
    where
        Self: 'a;
    type Len<'a>
        = Len<K::Scan<'a>>
    where
        Self: 'a;

    fn get<'a>(&'a self, domains: &'a [String]) -> Self::Get<'a> {
        // One call for every candidate, which is what `scan_many` is
        // for: §5.1.3's candidates are each a prefix, but of one another
        // rather than of a common string, so a single scan cannot
        // express them.
        //
        // `now` is the epoch because this wrapper hands the store no
        // expiry — see `Done::start`.
        Get {
            inner: self.kv.scan_many(
                NS,
                domains.iter().map(|d| prefix_for(d)).collect(),
                SystemTime::UNIX_EPOCH,
            ),
        }
    }

    fn all(&self) -> Self::Get<'_> {
        Get {
            inner: self.kv.scan(NS, String::new(), SystemTime::UNIX_EPOCH),
        }
    }

    fn put(&self, cookie: Cookie, _now: SystemTime) -> Self::Done<'_> {
        let key = key_for(&CookieKey::of(&cookie));
        let domain_prefix = prefix_for(cookie.domain());
        let Some(value) = encode(&cookie) else {
            return Done::Finished { _done: () };
        };

        // Replace, then write — the seam appends, and §5.7 makes an
        // arriving cookie replace the one with its key. The counter
        // advances before the write so two concurrent writers cannot
        // both see room; a replacement of an existing cookie therefore
        // over-counts by one until something evicts, which is the
        // upward drift this type documents.
        let rest = vec![Step::Put {
            key: key.clone(),
            value,
        }];
        let held = self.held.fetch_add(1, Ordering::Relaxed) + 1;

        let mut rest = rest;
        rest.insert(0, Step::Remove { key });

        if held > self.capacity.max_cookies {
            // The global bound: a scan of the namespace, which is the
            // walk `cookie::MemoryStore` does under its lock today and
            // the reason a remote store wants a generous bound.
            Done::Reading {
                reading: self.kv.scan(NS, String::new(), SystemTime::UNIX_EPOCH),
                store: &self.kv,
                plan: Plan::Evict {
                    bound: self.capacity.max_cookies.saturating_sub(1),
                    held: Some(self.held.clone()),
                },
                rest,
            }
        } else {
            // The per-domain bound: one prefix, which is cheap enough to
            // ask on every insertion. Room is made **before** the
            // insertion so the bound is a ceiling on what is held rather
            // than on what is held between two operations — and so that
            // a cookie coming back from storage with an old
            // `last_access` cannot be evicted by its own arrival.
            Done::Reading {
                reading: self.kv.scan(NS, domain_prefix, SystemTime::UNIX_EPOCH),
                store: &self.kv,
                plan: Plan::Evict {
                    bound: self.capacity.max_per_domain.saturating_sub(1),
                    // The global count is unchanged by a per-domain
                    // eviction *and* by the insertion it makes room for,
                    // so it is not touched here — it was advanced above.
                    held: None,
                },
                rest,
            }
        }
    }

    fn remove<'a>(&'a self, key: &'a CookieKey) -> Self::Done<'a> {
        // Exact, which is what the whole key in the key buys.
        self.held.fetch_sub(1, Ordering::Relaxed);
        Done::Running {
            running: self.kv.remove(NS, key_for(key)),
            store: &self.kv,
            rest: Vec::new(),
        }
    }

    fn touch<'a>(&'a self, keys: &'a [CookieKey], now: SystemTime) -> Self::Done<'a> {
        if keys.is_empty() {
            return Done::Finished { _done: () };
        }
        // A read of exactly the domains involved rather than of the
        // namespace: §5.4 touches the cookies one request matched, and
        // they share few domains.
        let mut prefixes: Vec<String> = keys.iter().map(|k| prefix_for(k.domain())).collect();
        prefixes.sort();
        prefixes.dedup();
        Done::Reading {
            reading: self.kv.scan_many(NS, prefixes, SystemTime::UNIX_EPOCH),
            store: &self.kv,
            plan: Plan::Touch {
                keys: keys.iter().map(key_for).collect(),
                now,
            },
            rest: Vec::new(),
        }
    }

    fn len(&self) -> Self::Len<'_> {
        Len {
            inner: self.kv.scan(NS, String::new(), SystemTime::UNIX_EPOCH),
        }
    }

    fn clear(&self) -> Self::Done<'_> {
        self.held.store(0, Ordering::Relaxed);
        Done::Running {
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

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn store() -> KvStore<Kv<SystemTime>> {
        KvStore::new(Kv::new())
    }

    /// A cookie built field by field, because this wrapper encodes the
    /// fields rather than a `CookieRecord` — which is the whole point.
    fn cookie(name: &str, domain: &str, expires: Option<u64>, seq: u64) -> Cookie {
        Cookie {
            name: name.to_owned(),
            value: format!("{name}-value"),
            domain: domain.to_owned(),
            path: "/".to_owned(),
            expires: expires.map(at),
            creation: at(1),
            last_access: at(1),
            seq,
            host_only: false,
            persistent: expires.is_some(),
            secure: false,
            http_only: false,
            same_site: None,
        }
    }

    // ---- the encoding ------------------------------------------------

    /// Every field survives, over several shapes: a decoder that dropped
    /// one would still round-trip whichever value happened to be its
    /// default.
    #[test]
    fn every_field_survives_the_round_trip() {
        let mut c = cookie("sid", "example.com", Some(100), 7);
        c.value = "a=b; c\u{1}d\u{0}e".to_owned();
        c.path = "/deep/path".to_owned();
        c.host_only = true;
        c.secure = true;
        c.http_only = true;
        c.same_site = Some(SameSite::Strict);
        c.creation = at(11);
        c.last_access = at(22);

        let back = decode(&encode(&c).expect("encodable")).expect("decodable");
        assert_eq!(back.name, c.name);
        assert_eq!(back.value, c.value, "a value may hold any separator");
        assert_eq!(back.domain, c.domain);
        assert_eq!(back.path, c.path);
        assert_eq!(back.expires, c.expires);
        assert_eq!(back.creation, c.creation);
        assert_eq!(back.last_access, c.last_access);
        assert_eq!(back.seq, c.seq);
        assert_eq!(back.host_only, c.host_only);
        assert_eq!(back.persistent, c.persistent);
        assert_eq!(back.secure, c.secure);
        assert_eq!(back.http_only, c.http_only);
        assert_eq!(back.same_site, c.same_site);
    }

    /// **A session cookie survives**, which `CookieRecord` cannot carry
    /// — `to_record` answers `None` for one, and a wrapper built on
    /// records would drop every one of them and report success.
    #[test]
    fn a_session_cookie_survives_where_a_record_could_not_hold_it() {
        let c = cookie("sid", "example.com", None, 0);
        assert!(c.to_record().is_none(), "the fixture's premise");

        let back = decode(&encode(&c).expect("encodable")).expect("decodable");
        assert_eq!(back.expires, None);
        assert!(!back.persistent);
    }

    /// And through the store, which is where the defect would show: a
    /// plain `sid=abc` must reach the second request.
    #[test]
    fn a_session_cookie_stored_comes_back() {
        let s = store();
        block_on(s.put(cookie("sid", "example.com", None, 0), at(1)));

        let got = block_on(s.get(&["example.com".to_owned()]));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "sid");
        assert_eq!(got[0].expires, None);
    }

    /// `seq` is §5.4's tiebreak and §5.7's replacement keeps the old
    /// cookie's, so a store that lost it would reorder cookies of equal
    /// path length on every reload.
    #[test]
    fn seq_survives_because_no_record_carries_it() {
        let c = cookie("sid", "example.com", Some(100), 42);
        let record = c.to_record().expect("persistent, so it has one");
        let from_record = Cookie::from_record(&record);
        assert_eq!(from_record.seq, 0, "the record cannot carry it");

        let back = decode(&encode(&c).expect("encodable")).expect("decodable");
        assert_eq!(back.seq, 42, "this encoding does");
    }

    /// **A timestamp too large for this platform is refused, not a
    /// panic** — the defect this decoder shipped with, and the one
    /// `test (windows-latest)` found because `SystemTime`'s range is
    /// narrower there: `u64::MAX` seconds overflowed on Windows and not
    /// on Linux, so every machine this workspace runs was green over it.
    ///
    /// The value is unrepresentable on both platforms, so the check is
    /// not a fact about the runner.
    #[test]
    fn a_timestamp_that_overflows_the_clock_is_refused_rather_than_panicking() {
        assert!(
            SystemTime::UNIX_EPOCH
                .checked_add(Duration::from_secs(u64::MAX))
                .is_none(),
            "the fixture's premise: this many seconds is not a SystemTime anywhere"
        );

        let mut bytes = encode(&cookie("sid", "example.com", Some(100), 0)).expect("enc");
        // `creation`, the second fixed-width field — not `expires`,
        // whose `u64::MAX` is this encoding's *no expiry* sentinel and
        // is therefore the one value that must NOT be read as a time.
        bytes[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(decode(&bytes).is_none());
    }

    /// A store hands back whatever it holds. Refusing loses one cookie;
    /// guessing would put a cookie on the wire nobody stored.
    #[test]
    fn bytes_this_encoder_did_not_write_are_refused() {
        let good = encode(&cookie("sid", "example.com", Some(100), 0)).expect("encodable");

        assert!(decode(&[]).is_none(), "empty");
        for cut in [1, 8, 33, good.len() - 1] {
            assert!(decode(&good[..cut]).is_none(), "truncated at {cut}");
        }

        let mut trailing = good.clone();
        trailing.push(0);
        assert!(decode(&trailing).is_none(), "trailing bytes");
    }

    /// The flag byte carries four bits and any other is a value this
    /// encoder did not write — masking would read a foreign record as a
    /// cookie with plausible flags.
    #[test]
    fn a_flag_byte_with_unknown_bits_is_refused() {
        let mut bytes = encode(&cookie("sid", "example.com", Some(100), 0)).expect("enc");
        bytes[32] |= 0b1_0000;
        assert!(decode(&bytes).is_none());
    }

    /// An unknown `SameSite` tag is refused rather than read as absent:
    /// `SameSite=Strict` arriving as `None` is a cookie sent on requests
    /// it was meant to sit out.
    #[test]
    fn an_unknown_same_site_tag_is_refused() {
        let mut bytes = encode(&cookie("sid", "example.com", Some(100), 0)).expect("enc");
        bytes[33] = 9;
        assert!(decode(&bytes).is_none());
    }

    // ---- the key -----------------------------------------------------

    /// §5.7's replacement key is all four parts, so cookies differing in
    /// any one of them are different cookies.
    #[test]
    fn the_key_is_the_whole_replacement_key() {
        let base = CookieKey::of(&cookie("sid", "example.com", None, 0));
        let mut other = cookie("sid", "example.com", None, 0);
        other.path = "/other".to_owned();
        assert_ne!(key_for(&base), key_for(&CookieKey::of(&other)));

        let mut host_only = cookie("sid", "example.com", None, 0);
        host_only.host_only = true;
        assert_ne!(key_for(&base), key_for(&CookieKey::of(&host_only)));

        let named = cookie("other", "example.com", None, 0);
        assert_ne!(key_for(&base), key_for(&CookieKey::of(&named)));
    }

    #[test]
    fn a_domain_is_reversed_and_an_ip_literal_is_not() {
        assert_eq!(prefix_for("a.b.example.com"), "com.example.b.a\u{1}");
        assert_eq!(prefix_for("127.0.0.1"), "127.0.0.1\u{1}");
        assert_eq!(prefix_for("::1"), "::1\u{1}");
        assert_ne!(prefix_for("127.0.0.1"), prefix_for("1.0.0.127"));
    }

    /// The trailing separator is part of the prefix, so one domain
    /// cannot answer with another whose reversed form extends it.
    #[test]
    fn one_domains_prefix_does_not_match_a_longer_domain() {
        let s = store();
        block_on(s.put(cookie("a", "examples.com", None, 0), at(1)));

        assert!(
            block_on(s.get(&["example.com".to_owned()])).is_empty(),
            "`com.examples` must not answer a query for `com.example`"
        );
    }

    // ---- the seam ----------------------------------------------------

    #[test]
    fn a_stored_cookie_comes_back_under_its_own_domain_only() {
        let s = store();
        block_on(s.put(cookie("a", "example.com", None, 0), at(1)));
        block_on(s.put(cookie("b", "other.test", None, 1), at(1)));

        let got = block_on(s.get(&["example.com".to_owned()]));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "a");
    }

    /// **`get` answers exact domains and never a suffix**, which is the
    /// seam's own first obligation: a store applying §5.1.3 itself would
    /// be answering a question the jar has already answered.
    #[test]
    fn get_answers_exact_domains_and_not_their_subdomains() {
        let s = store();
        block_on(s.put(cookie("a", "a.example.com", None, 0), at(1)));

        assert!(block_on(s.get(&["example.com".to_owned()])).is_empty());
        assert_eq!(block_on(s.get(&["a.example.com".to_owned()])).len(), 1);
    }

    /// The batch is what `scan_many` exists for: a jar asks about every
    /// ancestor of a host at once.
    #[test]
    fn a_batch_of_domains_answers_each_of_them_once() {
        let s = store();
        block_on(s.put(cookie("a", "a.b.example.com", None, 0), at(1)));
        block_on(s.put(cookie("b", "example.com", None, 1), at(1)));

        let mut names: Vec<String> = block_on(s.get(&[
            "a.b.example.com".to_owned(),
            "b.example.com".to_owned(),
            "example.com".to_owned(),
            "com".to_owned(),
        ]))
        .into_iter()
        .map(|c| c.name)
        .collect();
        names.sort();
        assert_eq!(names, ["a", "b"]);
    }

    /// **And each cookie once**, although a descendant's prefix extends
    /// its ancestor's: `scan_many` answers a union, so a cookie matching
    /// two prefixes is not doubled.
    #[test]
    fn a_cookie_matching_two_candidate_prefixes_is_answered_once() {
        let s = store();
        // `com.example.b.a` begins with `com.example.b`, so a naive
        // concatenation of per-prefix scans would answer this twice.
        block_on(s.put(cookie("a", "a.b.example.com", None, 0), at(1)));

        let got = block_on(s.get(&["a.b.example.com".to_owned(), "b.example.com".to_owned()]));
        assert_eq!(got.len(), 1, "a union, not a concatenation");
    }

    /// §5.7: an arriving cookie replaces the one with its key. The byte
    /// seam appends, so the wrapper removes first.
    #[test]
    fn a_second_put_with_one_key_replaces_rather_than_accumulating() {
        let s = store();
        block_on(s.put(cookie("sid", "example.com", None, 0), at(1)));
        let mut second = cookie("sid", "example.com", None, 0);
        second.value = "second".to_owned();
        block_on(s.put(second, at(2)));

        let got = block_on(s.get(&["example.com".to_owned()]));
        assert_eq!(got.len(), 1, "one cookie, not two");
        assert_eq!(got[0].value, "second");
    }

    #[test]
    fn remove_drops_one_cookie_and_leaves_its_neighbours() {
        let s = store();
        let target = cookie("a", "example.com", None, 0);
        block_on(s.put(target.clone(), at(1)));
        block_on(s.put(cookie("b", "example.com", None, 1), at(1)));

        block_on(s.remove(&CookieKey::of(&target)));

        let got = block_on(s.get(&["example.com".to_owned()]));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "b");
    }

    /// §5.4's step after a `Cookie` header has been produced, and the
    /// thing eviction reads.
    #[test]
    fn touch_moves_last_access_on_the_named_cookies_only() {
        let s = store();
        let touched = cookie("a", "example.com", None, 0);
        block_on(s.put(touched.clone(), at(1)));
        block_on(s.put(cookie("b", "example.com", None, 1), at(1)));

        block_on(s.touch(&[CookieKey::of(&touched)], at(500)));

        let got = block_on(s.get(&["example.com".to_owned()]));
        let a = got.iter().find(|c| c.name == "a").expect("a");
        let b = got.iter().find(|c| c.name == "b").expect("b");
        assert_eq!(a.last_access, at(500));
        assert_eq!(b.last_access, at(1), "its neighbour is untouched");
    }

    /// A touch must not turn one cookie into two, which it would if the
    /// rewrite forgot that the seam appends.
    #[test]
    fn touch_rewrites_in_place_rather_than_adding() {
        let s = store();
        let c = cookie("a", "example.com", None, 0);
        block_on(s.put(c.clone(), at(1)));

        block_on(s.touch(&[CookieKey::of(&c)], at(500)));

        assert_eq!(block_on(s.len()), 1);
    }

    /// And the rest of the cookie survives the rewrite — a touch that
    /// re-encoded from a default would quietly reset `seq` or the value.
    #[test]
    fn touch_keeps_every_other_field() {
        let s = store();
        let mut c = cookie("a", "example.com", Some(100), 42);
        c.value = "kept".to_owned();
        c.secure = true;
        block_on(s.put(c.clone(), at(1)));

        block_on(s.touch(&[CookieKey::of(&c)], at(500)));

        let got = block_on(s.get(&["example.com".to_owned()]));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].value, "kept");
        assert_eq!(got[0].seq, 42);
        assert!(got[0].secure);
        assert_eq!(got[0].expires, Some(at(100)));
    }

    /// `all` documents "oldest arrival first", and a scan has no order —
    /// so the order is restored from `seq`, which is what arrival means.
    #[test]
    fn all_answers_in_arrival_order() {
        let s = store();
        block_on(s.put(cookie("third", "c.test", None, 2), at(1)));
        block_on(s.put(cookie("first", "a.test", None, 0), at(1)));
        block_on(s.put(cookie("second", "b.test", None, 1), at(1)));

        let names: Vec<String> = block_on(s.all()).into_iter().map(|c| c.name).collect();
        assert_eq!(names, ["first", "second", "third"]);
    }

    #[test]
    fn len_counts_across_domains_and_clear_empties_it() {
        let s = store();
        block_on(s.put(cookie("a", "a.test", None, 0), at(1)));
        block_on(s.put(cookie("b", "b.test", None, 1), at(1)));
        assert_eq!(block_on(s.len()), 2);

        block_on(s.clear());
        assert_eq!(block_on(s.len()), 0);
    }

    /// `clear` is this namespace's alone, which is what lets one byte
    /// store back all four seams.
    #[test]
    fn clear_leaves_another_seams_namespace_alone() {
        let kv = Kv::<SystemTime>::new();
        block_on(kv.put("hsts", "com.example".to_owned(), b"h".to_vec(), None, at(0)));
        let s = KvStore::new(kv);
        block_on(s.put(cookie("a", "example.com", None, 0), at(1)));

        block_on(s.clear());

        assert_eq!(
            block_on(s.inner().get("hsts", "com.example", at(0))),
            vec![b"h".to_vec()]
        );
    }

    /// A value this wrapper did not write is skipped, never repaired.
    /// Written through the byte seam directly, because nothing above can
    /// produce one.
    #[test]
    fn a_value_the_decoder_refuses_is_skipped_rather_than_substituted() {
        let kv = Kv::<SystemTime>::new();
        block_on(kv.put(
            NS,
            format!("{}sid\u{1}/\u{1}0", prefix_for("example.com")),
            b"not a cookie".to_vec(),
            None,
            at(0),
        ));
        let s = KvStore::new(kv);

        assert!(block_on(s.get(&["example.com".to_owned()])).is_empty());
    }

    // ---- the capacity ------------------------------------------------

    /// §6.1's per-domain bound, and the victim is the least recently
    /// used — `cookie::MemoryStore`'s rule.
    #[test]
    fn a_full_domain_evicts_its_least_recently_used_cookie() {
        let s = KvStore::with_capacity(
            Kv::<SystemTime>::new(),
            super::super::Capacity {
                max_cookies: 100,
                max_per_domain: 2,
            },
        );
        let mut old = cookie("old", "example.com", None, 0);
        old.last_access = at(10);
        let mut mid = cookie("mid", "example.com", None, 1);
        mid.last_access = at(20);
        let mut new = cookie("new", "example.com", None, 2);
        new.last_access = at(30);

        block_on(s.put(old, at(1)));
        block_on(s.put(mid, at(1)));
        block_on(s.put(new, at(1)));

        let mut names: Vec<String> = block_on(s.get(&["example.com".to_owned()]))
            .into_iter()
            .map(|c| c.name)
            .collect();
        names.sort();
        assert_eq!(names, ["mid", "new"], "the least recently used went");
    }

    /// And the bound is per domain: a neighbour's cookies are not
    /// counted against it.
    #[test]
    fn the_per_domain_bound_does_not_count_another_domains_cookies() {
        let s = KvStore::with_capacity(
            Kv::<SystemTime>::new(),
            super::super::Capacity {
                max_cookies: 100,
                max_per_domain: 2,
            },
        );
        block_on(s.put(cookie("a", "a.test", None, 0), at(1)));
        block_on(s.put(cookie("b", "b.test", None, 1), at(1)));
        block_on(s.put(cookie("c", "c.test", None, 2), at(1)));

        assert_eq!(block_on(s.len()), 3, "three domains, one cookie each");
    }

    /// §6.1's global bound.
    #[test]
    fn a_full_jar_evicts_across_domains() {
        let s = KvStore::with_capacity(
            Kv::<SystemTime>::new(),
            super::super::Capacity {
                max_cookies: 2,
                max_per_domain: 50,
            },
        );
        let mut old = cookie("old", "a.test", None, 0);
        old.last_access = at(10);
        let mut mid = cookie("mid", "b.test", None, 1);
        mid.last_access = at(20);
        let mut new = cookie("new", "c.test", None, 2);
        new.last_access = at(30);

        block_on(s.put(old, at(1)));
        block_on(s.put(mid, at(1)));
        block_on(s.put(new, at(1)));

        assert_eq!(block_on(s.len()), 2, "the bound holds");
        assert!(
            block_on(s.get(&["a.test".to_owned()])).is_empty(),
            "the least recently used went"
        );
    }
}
