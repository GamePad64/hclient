//! Where the cookies live: the key, the seam, and the one store this
//! crate ships.
//!
//! # The split between this file and `jar.rs`
//!
//! **The rules decide what may be stored and what applies to a request;
//! storage decides what it can hold.** RFC 6265bis is entirely the first
//! of those and says almost nothing about the second — a jar keeping ten
//! cookies and a jar keeping three thousand are equally conformant. So
//! [`CookieStore`] has no notion of a `Domain` attribute, of a public
//! suffix, of `Secure` or of a path: it is a multimap from an exact
//! domain string to cookies, and everything that could be got wrong
//! against the RFC is on the other side of it. That is what makes the
//! seam safe to hand to a caller: **a wrong `CookieStore` loses cookies
//! or keeps too many, and cannot send one to a host it was never scoped
//! to.** `cache::CacheStore`'s own module doc makes the same argument in
//! the same words, and it is the reason both seams exist where they do.
//!
//! The second half of that sentence is a property of the code rather than
//! a hope: [`CookieJar::matching`](super::CookieJar::matching) re-applies
//! §5.1.3's domain-match, §5.1.4's path-match, `Secure` and expiry to
//! **whatever** [`CookieStore::get`] hands back. So a store that answered
//! the wrong domains — through a bug, or through a file somebody edited —
//! loses the extra rows at the filter rather than putting them on the
//! wire.
//!
//! # Why the key is a domain and not a request
//!
//! A cache has an exact key — the method and the target URI — so
//! [`CacheStore::get`](crate::cache::CacheStore::get) is a lookup and
//! every rule is applied to what comes back. **Cookie retrieval has no
//! such key**: §5.1.3's domain-match is a suffix relation and §5.1.4's
//! path-match is a prefix one, so a store asked *"what applies to
//! `https://a.b.example.com/x`"* would have to implement RFC 6265 —
//! which is the thing the seam exists to keep on this side of it.
//!
//! What is exact is the domain a cookie was **stored** under, and the set
//! of domains that can match a host is bounded and computed from the host
//! alone: `a.b.example.com` is matched by cookies at `a.b.example.com`,
//! `b.example.com`, `example.com` and `com`, and by no others. So the
//! *rule* enumerates the candidates and the *store* answers an exact
//! lookup for each — [`candidate_domains`] is that enumeration, and it is
//! `domain_matches` turned inside out with a test asserting the two agree.
//!
//! # Why the futures, for a jar that fits in memory
//!
//! RFC 6265 §6.1's own bounds put a ceiling on a jar, so an in-memory
//! store is not merely adequate but the normal case — and it is what
//! [`MemoryStore`] is, answering [`std::future::Ready`] so that the shape
//! costs it no allocation and no suspension. What the futures buy is the
//! stores that were unwritable without them: on disk, in a database, in
//! the browser's own storage, for which a synchronous seam offered two
//! options — block the executor or do not exist.
//!
//! `&self` is not a second decision, it follows from the first. A store
//! that awaits cannot be held behind a `&mut` across that await, and a
//! store that is remote is shared by everyone talking to it already. So
//! synchronisation is the store's own, and [`crate::Client`] holds no
//! lock around its jar at all — which is what lets
//! [`Client::cookie_jar`](crate::Client::cookie_jar) hand back a borrow
//! where there was nothing to hand back before.

use std::collections::HashMap;
use std::future::{Future, Ready, ready};
use std::sync::Mutex;
// `web_time`, not `std::time`: on `wasm32-unknown-unknown` a cookie's
// timestamps come from a clock `std` does not have there, and on every
// other target `web_time::SystemTime` IS `std::time::SystemTime`, so no
// signature below changes. `no-std-wall-clock-in-the-client` keeps the
// plain import from coming back.
use web_time::SystemTime;

use super::jar::Cookie;
use super::matching::is_ip_literal;

/// RFC 6265bis §5.7's replacement key: the four fields that decide
/// whether an arriving cookie *is* one already held.
///
/// **The host-only flag is in it and is easy to leave out** — RFC 6265
/// itself did, and 6265bis added it. Without it, `a=1` set by
/// `example.com` and `a=2; Domain=example.com` set by the same host
/// collapse into one cookie, and the survivor is whichever arrived last:
/// a host-only cookie silently acquires a subdomain scope it was never
/// given, or loses the one it had.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CookieKey {
    name: String,
    domain: String,
    path: String,
    host_only: bool,
}

impl CookieKey {
    /// The key a cookie is stored under.
    pub fn of(cookie: &Cookie) -> Self {
        Self {
            name: cookie.name().to_owned(),
            domain: cookie.domain().to_owned(),
            path: cookie.path().to_owned(),
            host_only: cookie.host_only(),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The exact domain this cookie is stored under — lowercased and
    /// without the leading `.` some servers send. This is the string
    /// [`CookieStore::get`] matches on.
    pub fn domain(&self) -> &str {
        &self.domain
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn host_only(&self) -> bool {
        self.host_only
    }
}

/// Every domain a cookie could be stored under and still apply to `host`,
/// most specific first.
///
/// This is [`super::matching::domain_matches`] enumerated rather than
/// tested, and the two are pinned against each other in
/// `tests/cookies.rs` — the exact-string half of retrieval is the store's,
/// and it can only be exact if this list is complete.
///
/// An IP literal has no labels to be a suffix of (§5.1.3's last
/// condition), so it is its own only candidate: `1.2.3.4` must not
/// receive cookies scoped to `2.3.4`.
pub(super) fn candidate_domains(host: &str) -> Vec<String> {
    let mut out = vec![host.to_owned()];
    if is_ip_literal(host) {
        return out;
    }
    let mut rest = host;
    while let Some((_, tail)) = rest.split_once('.') {
        out.push(tail.to_owned());
        rest = tail;
    }
    out
}

/// Where a [`CookieJar`](super::CookieJar) keeps its cookies.
///
/// Implement it to put a jar on disk, in a database or in the browser's
/// own storage; [`MemoryStore`] is what a plain `CookieJar::new()` uses
/// and is the reference for what the methods mean.
///
/// **A store that outlives the process writes
/// [`CookieRecord`](super::CookieRecord)s and reads them back with
/// [`Cookie::from_record`]** — [`Cookie`]'s own fields are private, and
/// that pair is the serialisable form this module already argues for.
///
/// # And a record is not a representation, which is the trap
///
/// [`Cookie::to_record`] answers `None` for a **session cookie**, and
/// that is right for a *file*: a cookie with no `Expires` is one that is
/// meant not to survive a restart. It is wrong for a **store**, because
/// this store is not a mirror of the jar — it *is* where the jar keeps
/// its cookies, session ones included, for as long as the process runs.
/// A store whose only representation is `CookieRecord` therefore drops
/// every session cookie on the floor and reports success.
///
/// So a persisting store holds two things: what it has, as [`Cookie`],
/// and what it would write down, which is the strict subset that has a
/// record. Written here because it is not visible from the signatures —
/// [`put`](Self::put) takes a `Cookie` and nothing says the round trip
/// through a record is lossy — and because the first store written
/// against this seam outside the workspace fell into it inside five
/// minutes: a plain `sid=abc` never reached the second request, and the
/// seam had done exactly what it was asked.
///
/// # The obligations
///
/// Three, and none of them is an RFC 6265 rule — see this module's
/// documentation for why that line is where it is:
///
/// 1. [`get`](Self::get) answers **exact** domain matches, and nothing
///    else. A store that applied a suffix rule of its own would be
///    answering a question the jar has already answered.
/// 2. [`put`](Self::put) replaces by [`CookieKey`]. Two cookies with one
///    key is the state §5.7 exists to prevent.
/// 3. The store is **bounded**, and evicts least-recently-used first —
///    [`Cookie::last_access`] is the key, and RFC 6265 §6.1's minimums
///    are what [`MemoryStore`] takes as its defaults. A store that keeps
///    everything is a memory-exhaustion bug with a server on the other
///    end of it.
///
/// # Associated futures, not `async fn` — measured, five ways
///
/// `async fn` in traits is stable, and it is the obvious way to write
/// this. It was tried on a scratch crate before this shape was kept, and
/// the three outcomes are why it is not used here.
///
/// [`Client`](crate::Client) boxes its jar `Send + Sync`, so something
/// has to prove the store's futures `Send`.
///
/// 1. **`async fn` on the trait, `Send` box in the erasure** — `E0277`,
///    *`impl Future<Output = ..>` cannot be sent between threads
///    safely*. An `async fn` in a trait is an RPITIT, and a generic impl
///    cannot prove a property of a future it cannot name. This is the
///    rule this workspace states everywhere: at a concrete type `Send`
///    is inferred, in a generic impl it must be proven.
/// 2. **Return type notation** — `Store<get(..): Send>` is exactly the
///    language feature for naming one, and it is `E0658`, *return type
///    notation is experimental*, on this crate's stable toolchain. The
///    workspace has measured its full cost once already and declined it.
/// 3. **`+ Send` on the seam itself**, which is what rustc suggests —
///    `fn get(&self) -> impl Future<Output = ..> + Send`. It compiles,
///    and it **excludes every single-threaded store**: an
///    implementor holding an `Rc` across an await is rejected at its own
///    `impl`, with the bound named as the cause. The control is the same
///    store holding an `Arc`, which compiles. So the cost is not a
///    ceremony, it is a store the browser or a single-threaded device
///    could have written and now cannot.
///
/// 4. **`#[async_trait]`**, which is where this shape came from before
///    1.75 made it a language feature. It is option 3 with a macro and a
///    heap allocation: the plain form writes `Pin<Box<dyn Future + Send>>`
///    into the trait, so it fails on the same `Rc`-holding store and with
///    the same cause; `#[async_trait(?Send)]` writes a plain box, so the
///    erasure [`Client`](crate::Client) needs stops compiling instead —
///    `E0308`, the two box types. Both halves were built. The choice is
///    fixed at the trait rather than per implementor, which is the whole
///    of the objection, and the allocation is what it costs on top:
///    measured with a counting allocator over 1,000 calls to a store that
///    answers immediately, **1,000 allocations against 0**. `MemoryStore`
///    answers [`Ready`] and allocates nothing today.
/// 5. **`trait_variant`**, rust-lang's own crate for this question, and
///    the only alternative that carries this workspace's exact shape.
///    `#[trait_variant::make(SendStore: Send)]` writes a *second* trait
///    whose futures are `Send`, with a blanket impl making every
///    `SendStore` a `Store`. Built against the three-sided constraint —
///    a single-threaded store in a bare jar, a threaded store erased
///    through [`ClientBuilder::cookie_jar`](crate::ClientBuilder::cookie_jar),
///    and the erased jar still `Send + Sync` — **all three compile**, and
///    it allocates nothing: measured at 0 per 1,000 calls, the same as
///    this shape and unlike `#[async_trait]`.
///
///    **What it costs is that the seam is two traits and the author's
///    choice between them is one-way.** A store whose futures are
///    genuinely `Send`, written against the trait the seam is *named*
///    after, works in a bare jar and is refused at the erasure —
///    `E0277`, *the trait bound `..: SendStore` is not satisfied* — and
///    the author cannot add the second impl beside the first, because
///    the macro's own blanket impl conflicts (`E0119`). The repair is to
///    delete the impl and rewrite it against the other name. So the
///    property is *declared per store* rather than inferred, which is
///    option 3's objection scoped down rather than removed.
///
/// An associated future type is the answer that costs nothing: **naming is not requiring**, so each implementor answers for
/// its own auto traits — amendment C15, `TcpConnect::Connecting`'s
/// argument one seam down and [`CacheStore`](crate::cache::CacheStore)'s
/// one module over. **One trait, one impl per store, and the property is
/// read off the concrete type**: the same `MemoryStore` source is a jar's
/// store and a `Client`'s, and its author wrote nothing about `Send` at
/// all. That is the whole of what separates it from 5.
///
/// A store author still never writes the erasure:
/// [`BoxCookieStore`](crate::erased::BoxCookieStore) is a blanket impl
/// over a private object-safe trait, so `Send` is inferred where the type
/// is still concrete, and it is demanded only on
/// [`ClientBuilder::cookie_jar`](crate::ClientBuilder::cookie_jar).
// `len` without `is_empty`, and the reason is `CacheStore`'s verbatim: a
// defaulted `is_empty` would have to name a future built from `len`'s, and
// there is no way to write that type without boxing every implementor's
// answer. `len().await == 0` is what a caller writes — which is exactly
// what `CookieJar::is_empty` does.
#[allow(clippy::len_without_is_empty)]
pub trait CookieStore {
    /// The answer to [`get`](Self::get) and [`all`](Self::all).
    ///
    /// Owned rather than borrowed: a remote store has nothing to lend.
    /// One type for the two, because they answer the same thing — a set
    /// of cookies — and two identical associated types would be two
    /// places for an implementor to disagree with itself.
    type Get<'a>: Future<Output = Vec<Cookie>> + 'a
    where
        Self: 'a;
    /// The answer to [`put`](Self::put), [`remove`](Self::remove),
    /// [`touch`](Self::touch) and [`clear`](Self::clear).
    type Done<'a>: Future<Output = ()> + 'a
    where
        Self: 'a;
    /// The answer to [`len`](Self::len).
    type Len<'a>: Future<Output = usize> + 'a
    where
        Self: 'a;

    /// Every cookie whose domain is **exactly** one of `domains`, in no
    /// particular order.
    fn get<'a>(&'a self, domains: &'a [String]) -> Self::Get<'a>;

    /// Every cookie held, oldest arrival first.
    ///
    /// Only inspection reads this — [`CookieJar::cookies`] and
    /// [`CookieJar::records`] — so the order is a convenience rather than
    /// a rule: §5.4's ordering is applied by the jar to what
    /// [`get`](Self::get) answers, and does not depend on this at all.
    ///
    /// [`CookieJar::cookies`]: super::CookieJar::cookies
    /// [`CookieJar::records`]: super::CookieJar::records
    fn all(&self) -> Self::Get<'_>;

    /// Store `cookie`, replacing any cookie with the same [`CookieKey`],
    /// and evict to stay within the bound.
    ///
    /// `now` is what lets a store drop expired entries while it is
    /// already holding whatever lock or connection it needs; it is
    /// otherwise unused, and a store that ignores it is correct but
    /// keeps rubbish until something reads it.
    fn put(&self, cookie: Cookie, now: SystemTime) -> Self::Done<'_>;

    /// Remove the cookie with this key, if it is held.
    fn remove<'a>(&'a self, key: &'a CookieKey) -> Self::Done<'a>;

    /// Set the last-access time of each named cookie to `now` — §5.4's
    /// step after a `Cookie` header has been produced, and the thing
    /// eviction reads.
    ///
    /// A batch rather than one call per cookie, because a request's
    /// matching set is produced all at once and a remote store should
    /// pay one round trip for it rather than one per cookie.
    fn touch<'a>(&'a self, keys: &'a [CookieKey], now: SystemTime) -> Self::Done<'a>;

    /// How many cookies are held, expired ones included.
    fn len(&self) -> Self::Len<'_>;

    /// Drop everything.
    fn clear(&self) -> Self::Done<'_>;
}

/// How large a [`MemoryStore`] is allowed to get.
///
/// A jar with no bound is a memory-exhaustion bug with a server on the
/// other end of it, so the bound is part of the type rather than a later
/// hardening. The defaults are RFC 6265 §6.1's minimums, which is the
/// smallest set of numbers that cannot be called arbitrary.
///
/// **These are the store's and not the jar's**, and the line is the one
/// [`crate::cache`] already draws: a count of entries is what a store can
/// hold, where [`Limits::max_name_value_bytes`](super::Limits) is a
/// *refusal* decided before a cookie exists. A store that gets these
/// wrong loses cookies; a jar that got the refusal wrong would store one
/// it was told not to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capacity {
    /// Total cookies across all domains. RFC 6265 §6.1: at least 3000.
    pub max_cookies: usize,
    /// Cookies for any one domain. RFC 6265 §6.1: at least 50.
    pub max_per_domain: usize,
}

impl Default for Capacity {
    fn default() -> Self {
        Self {
            max_cookies: 3000,
            max_per_domain: 50,
        }
    }
}

/// The store this crate ships: a `HashMap` keyed by domain, in memory.
///
/// **The `Mutex` is what `&self` on the seam costs an in-memory store,
/// and it is the cheaper half of that trade** — one uncontended lock per
/// operation here, against a network round trip for the stores the shape
/// exists for. It is `std::sync::Mutex` and not an async one
/// deliberately: nothing under this lock awaits, so holding it across an
/// await is impossible rather than merely discouraged.
///
/// Keyed by domain rather than a flat list because that is the lookup
/// [`CookieStore::get`] is asked for and the grouping
/// [`Capacity::max_per_domain`] is counted over — both would otherwise be
/// a walk of the whole jar.
#[derive(Debug)]
pub struct MemoryStore {
    entries: Mutex<Held>,
    capacity: Capacity,
}

/// What the lock protects. `held` is a count across every domain and is
/// kept beside the map rather than derived, because [`CookieStore::len`]
/// is on the seam and summing the map on every call would make a
/// bookkeeping figure cost a walk.
#[derive(Debug, Default)]
struct Held {
    by_domain: HashMap<String, Vec<Cookie>>,
    held: usize,
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryStore {
    /// Room for RFC 6265 §6.1's minimums.
    pub fn new() -> Self {
        Self::with_capacity(Capacity::default())
    }

    /// Room for a bound of the caller's own.
    pub fn with_capacity(capacity: Capacity) -> Self {
        Self {
            entries: Mutex::new(Held::default()),
            capacity,
        }
    }

    pub fn capacity(&self) -> Capacity {
        self.capacity
    }
}

impl Held {
    /// Drop everything that expired at or before `now`.
    fn purge_expired(&mut self, now: SystemTime) {
        let mut dropped = 0;
        self.by_domain.retain(|_, cookies| {
            let before = cookies.len();
            cookies.retain(|c| !c.is_expired(now));
            dropped += before - cookies.len();
            !cookies.is_empty()
        });
        self.held -= dropped;
    }

    /// RFC 6265 §5.3's eviction key: least recently used, then earliest
    /// inserted, within one domain or across every one.
    fn evict_lru(&mut self, domain: Option<&str>) -> bool {
        let victim = self
            .by_domain
            .iter()
            .filter(|(d, _)| domain.is_none_or(|want| d.as_str() == want))
            .flat_map(|(d, cookies)| {
                cookies
                    .iter()
                    .enumerate()
                    .map(move |(i, c)| (d.clone(), i, c))
            })
            .min_by(|(_, _, a), (_, _, b)| {
                a.last_access()
                    .cmp(&b.last_access())
                    .then(a.seq.cmp(&b.seq))
            })
            .map(|(d, i, _)| (d, i));
        let Some((d, i)) = victim else {
            return false;
        };
        let cookies = self
            .by_domain
            .get_mut(&d)
            .expect("the domain was just read");
        cookies.remove(i);
        self.held -= 1;
        if cookies.is_empty() {
            self.by_domain.remove(&d);
        }
        true
    }
}

impl CookieStore for MemoryStore {
    type Get<'a> = Ready<Vec<Cookie>>;
    type Done<'a> = Ready<()>;
    type Len<'a> = Ready<usize>;

    fn get<'a>(&'a self, domains: &'a [String]) -> Self::Get<'a> {
        let held = self.entries.lock().expect("cookie store lock");
        let mut out = Vec::new();
        for domain in domains {
            if let Some(cookies) = held.by_domain.get(domain.as_str()) {
                out.extend(cookies.iter().cloned());
            }
        }
        ready(out)
    }

    fn all(&self) -> Self::Get<'_> {
        let held = self.entries.lock().expect("cookie store lock");
        let mut out: Vec<Cookie> = held.by_domain.values().flatten().cloned().collect();
        out.sort_by_key(|c| c.seq);
        ready(out)
    }

    fn put(&self, cookie: Cookie, now: SystemTime) -> Self::Done<'_> {
        let mut held = self.entries.lock().expect("cookie store lock");
        held.purge_expired(now);

        let key = CookieKey::of(&cookie);
        let domain = key.domain().to_owned();

        // An arriving cookie that has **already** expired is a deletion —
        // a `Max-Age=0`, or an `Expires` in the past — and it is the same
        // rule as the purge above rather than a second test that could
        // disagree with it: `is_expired` decides both, asked here on the
        // way in because there is nothing to gain by storing something
        // only to sweep it on the next call.
        if cookie.is_expired(now) {
            if let Some(cookies) = held.by_domain.get_mut(&domain) {
                let before = cookies.len();
                cookies.retain(|c| CookieKey::of(c) != key);
                let dropped = before - cookies.len();
                let empty = cookies.is_empty();
                held.held -= dropped;
                if empty {
                    held.by_domain.remove(&domain);
                }
            }
            return ready(());
        }

        if let Some(slot) = held.by_domain.get_mut(&domain)
            && let Some(i) = slot.iter().position(|c| CookieKey::of(c) == key)
        {
            slot[i] = cookie;
            return ready(());
        }

        // Room is made **before** the insertion, so the bound is a
        // ceiling on what is held rather than on what is held between two
        // operations — and so that a cookie coming back from storage with
        // an old `last_access` cannot be evicted by its own arrival.
        while held
            .by_domain
            .get(&domain)
            .is_some_and(|c| c.len() >= self.capacity.max_per_domain)
        {
            if !held.evict_lru(Some(&domain)) {
                break;
            }
        }
        while held.held >= self.capacity.max_cookies {
            if !held.evict_lru(None) {
                break;
            }
        }

        held.by_domain.entry(domain).or_default().push(cookie);
        held.held += 1;
        ready(())
    }

    fn remove<'a>(&'a self, key: &'a CookieKey) -> Self::Done<'a> {
        let mut held = self.entries.lock().expect("cookie store lock");
        if let Some(cookies) = held.by_domain.get_mut(key.domain()) {
            let before = cookies.len();
            cookies.retain(|c| CookieKey::of(c) != *key);
            let dropped = before - cookies.len();
            let empty = cookies.is_empty();
            held.held -= dropped;
            if empty {
                held.by_domain.remove(key.domain());
            }
        }
        ready(())
    }

    fn touch<'a>(&'a self, keys: &'a [CookieKey], now: SystemTime) -> Self::Done<'a> {
        let mut held = self.entries.lock().expect("cookie store lock");
        for key in keys {
            if let Some(cookies) = held.by_domain.get_mut(key.domain()) {
                for c in cookies.iter_mut().filter(|c| CookieKey::of(c) == *key) {
                    c.last_access = now;
                }
            }
        }
        ready(())
    }

    fn len(&self) -> Self::Len<'_> {
        ready(self.entries.lock().expect("cookie store lock").held)
    }

    fn clear(&self) -> Self::Done<'_> {
        let mut held = self.entries.lock().expect("cookie store lock");
        held.by_domain.clear();
        held.held = 0;
        ready(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cookie::matching::domain_matches;

    /// **The seam's load-bearing equivalence, and the reason it is a test
    /// rather than a comment.**
    ///
    /// [`CookieStore::get`] answers exact strings, so the jar is exact
    /// only if [`candidate_domains`] enumerates precisely the domains
    /// §5.1.3's suffix rule would accept. Enumerating too few loses a
    /// cookie; enumerating too many sends one to a host it was never
    /// scoped to — the second being the direction the whole module exists
    /// to keep on this side of the seam.
    ///
    /// It is checked against `domain_matches` itself rather than against
    /// a second list, so the two cannot drift: a change to the rule that
    /// this enumeration did not follow fails here.
    #[test]
    fn the_candidate_domains_are_exactly_what_the_suffix_rule_accepts() {
        // Hosts and candidate domains drawn from the same pool, so every
        // pair the rule can be asked about is asked about — including the
        // ones that must *not* match.
        const NAMES: &[&str] = &[
            "a.b.example.com",
            "b.example.com",
            "example.com",
            "com",
            "notexample.com",
            "xample.com",
            "www.example.com",
            "localhost",
            "1.2.3.4",
            "2.3.4",
            "[::1]",
            "",
        ];

        for host in NAMES {
            let candidates = candidate_domains(host);
            for domain in NAMES {
                let by_rule = domain_matches(host, domain);
                let by_enumeration = candidates.iter().any(|c| c == domain);
                assert_eq!(
                    by_rule, by_enumeration,
                    "host {host:?} against domain {domain:?}: the rule says \
                     {by_rule} and the enumeration says {by_enumeration}"
                );
            }
        }
    }

    /// The IP-literal arm on its own, because it is the one row where the
    /// enumeration has to *stop* rather than continue: `1.2.3.4` must not
    /// be handed cookies scoped to `2.3.4`, and a suffix walk that did not
    /// know that would produce exactly that candidate.
    #[test]
    fn an_ip_literal_is_its_own_only_candidate() {
        assert_eq!(candidate_domains("1.2.3.4"), vec!["1.2.3.4".to_owned()]);
    }
}
