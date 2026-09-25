//! A byte-level key/value seam, under the stores this family already has.
//!
//! [`CookieStore`], [`HstsStore`], [`CacheStore`] and `AltSvcStore` each
//! answer a domain question — *what cookies apply to this host*, *is this
//! origin known to speak h3* — and each ships a `Mutex<HashMap>` to answer
//! it with. That is four implementations of expiry, eviction and locking,
//! and a fifth for anybody who wants those four on disk or in Redis.
//!
//! This is the one underneath them. It knows **bytes and nothing else**:
//! no key type, no value type, no vocabulary from any of the four. A
//! wrapper encodes its own keys and values, and what crosses this seam is
//! a namespace, a string key and opaque bytes.
//!
//! [`CookieStore`]: https://docs.rs/hclient/latest/hclient/cookie/trait.CookieStore.html
//! [`HstsStore`]: https://docs.rs/hclient/latest/hclient/hsts/trait.HstsStore.html
//! [`CacheStore`]: https://docs.rs/hclient/latest/hclient/cache/trait.CacheStore.html
//!
//! # What belongs here, and what does not
//!
//! Four memories, and the rule that admits them is narrower than *state
//! a crate keeps*: it is **state whose absence changes an answer**. A
//! cookie that does not arrive, a policy that lets a request go out in
//! clear text, an advertisement forgotten, a hit that becomes a miss the
//! server has to answer — each is a fact a server told this client, and
//! each is why those four want a backend that can outlive the process.
//!
//!
//! # Why bytes rather than a type parameter
//!
//!
//! Bytes are what lets **one** instance serve all four. The cost is
//! serialisation, and it lands in the wrappers — which is where the
//! knowledge of what a cookie *is* already lives.
//!
//! # Why associated future types rather than `async fn`
//!
//! `hclient::Client` boxes its stores `Send + Sync`, so something has to
//! prove a generic implementor's future `Send`, and a future nobody can
//! name cannot be proved anything about. An associated type lets each
//! implementor answer for itself: naming is not requiring. [`MemoryStore`]
//! answers [`Ready`], so the shape costs an in-memory store no allocation
//! and no suspension.
//!
//! # Why the clock is a type and not [`SystemTime`]
//!
//! A wall clock is not needed, because **a store compares and never
//! reads**. Every
//! arithmetic on a calendar time in this family — `checked_add` for a
//! lease, `duration_since` for an `Age` — happens in the rules *above*
//! the store; the store only ever asks whether an expiry has passed.
//! So [`Instant`](KeyValueStore::Instant) carries [`Timer::Instant`]'s
//! own bounds, `Copy + PartialOrd`, and every store in this family binds
//! it to `web_time::SystemTime` at its own use site.
//!
//! [`SystemTime`]: std::time::SystemTime
//! [`Timer::Instant`]: crate::timer::Timer::Instant
//!
//! # What a wrapper owes, and what this seam cannot check
//!
//! The rules stay above the seam, as they do for all four stores today:
//! a wrong store loses an entry or keeps rubbish, and cannot make a
//! stale response answer a request that forbade one. That property is
//! what makes handing this to a caller safe, and it is the wrapper's to
//! keep — nothing here can verify it.
//!
//! Two obligations are worth stating because no signature shows them.
//!
//! **A key's host component is reversed** — `com.example.b.a`, not
//! `a.b.example.com` — which turns RFC 6265's suffix match into a prefix
//! one, puts one host's entries in one range of an ordered store, and
//! makes *forget everything under this domain* a prefix operation. It
//! does **not** remove the need for [`get_many`](KeyValueStore::get_many):
//! a cookie lookup walks *up* from a host, so the candidates are a chain
//! of prefixes of each other rather than a common prefix of a set, and a
//! scan on `com.` would return the whole TLD.
//!
//! **An IP literal is never reversed**, because a reversed `1.0.0.127` is
//! a different address. A wrapper that reverses in one direction and not
//! the other leaks entries between hosts, so the reversal belongs in one
//! helper used both ways, with a test for v4 and v6.
//!
//! # Expiry, and who evicts
//!
//! [`put`](KeyValueStore::put) takes the expiry rather than a TTL,
//! because the caller holds the clock and an absolute instant survives a
//! store that outlives the process. A store may drop an expired entry
//! whenever it is already holding whatever lock or connection it needs;
//! it **must** not answer with one, which is the only promise a reader
//! can rely on.
//!
//! A bound on how much is held is deliberately *not* here. A byte store
//! cannot count cookies, and the counts that matter are per domain and
//! per kind — so capacity lives in the wrapper, which knows what an entry
//! is. See [`MemoryStore`] for what this one does instead.
//!
//! # Growing this seam: a new operation is a new trait
//!
//! [`KeyValueStore`] does not gain methods: one more required method
//! would break every store written outside this crate. A new operation —
//! a store-side TTL, a batch write, compare-and-swap — arrives **as a
//! trait that extends this one** (`trait KeyValueBatch: KeyValueStore { .. }`),
//! with its bound on the one wrapper or constructor that needs it. A
//! store that cannot do it implements nothing and loses only that
//! wrapper, as a compile error where it was asked for.

// Maintainer notes (not rendered):
//
// A cache of values the client computed from its own settings is not
// that, however much it looks like a map. `hclient-tls-rustls` keeps
// `(alpn, early_data) -> Arc<rustls::ClientConfig>`; losing an entry
// costs a config clone and changes no answer at all, so it needs no
// persistence, no expiry and no capacity — it is a memo on a pure
// function, and a `Mutex<HashMap>` is the whole of it. Putting one
// behind this seam would promise a durability it has no use for and
// charge a serialisation its value cannot pay: an `Arc<ClientConfig>`
// is not bytes.
//
// A `KeyValueStore<V>` would mean one store instance per use — and the
// dependency graph forces that anyway, so the parameter would buy
// nothing. `altsvc::Entry` lives in `hclient-native`; `Cookie`,
// `hsts::Entry` and `StoredResponse` live in `hclient`; and
// `hclient-native` does not depend on `hclient`. So there is no crate in
// which a typed store could name all four value types: not `hclient`,
// which cannot see altsvc's, not `hclient-native`, which cannot see the
// other three, and not here without moving every domain type into this
// crate.
//
// The same measurement `CookieStore` records, and it is worth not
// re-deriving. `hclient::Client` boxes its stores `Send + Sync`, so
// something has to prove a generic implementor's future `Send`. With
// `async fn` in the trait the erasure is `E0277`, because a generic impl
// cannot prove a property of a future it cannot name; return type
// notation names it and is `E0658` on stable; and rustc's own suggestion
// — `+ Send` on the seam — compiles and **excludes every single-threaded
// store**, refusing an implementor that holds an `Rc` across an await
// while accepting the same one holding an `Arc`.
//
// An associated type lets each implementor answer for itself, which is
// amendment C15 reached from one more direction: naming is not
// requiring. [`MemoryStore`] answers [`Ready`], so
// the shape costs an in-memory store no allocation and no suspension.
//
// This crate names no wall clock at all today, and adding one would cost
// every consumer of `hclient-core` the `web-time` crate — which on
// `wasm32-unknown-unknown` is the parent of `js-sys` and `wasm-bindgen`,
// measured at +6 crates for a wasm build with no transport.
//
// It is not needed, because **a store compares and never reads**. Every
// arithmetic on a calendar time in this family — `checked_add` for a
// lease, `duration_since` for an `Age` — happens in the rules *above*
// the store; the store only ever asks whether an expiry has passed.
// So [`Instant`](KeyValueStore::Instant) carries [`Timer::Instant`]'s
// own bounds, `Copy + PartialOrd`, and every store in this workspace
// binds it to `web_time::SystemTime` at its own use site — where
// `no-std-wall-clock-in-the-client` still covers it and where this
// crate's graph is not on the hook.
//
// [`KeyValueStore`] does not gain methods. Every operation it has is
// required, and a new one would be too: its future has to be an
// associated type so a consumer can name it and prove it `Send`, and an
// associated type cannot have a default on stable Rust. So one more
// method would break every store written outside this workspace, and
// because this crate's types cross every seam in the family, the major
// step would carry `hclient-rt`, `hclient-tls`, `hclient-dns`,
// `hclient-mock` and `hclient-wasi` with it.
//
// This is not hypothetical: the trait grew three operations in its first
// two days (`scan`, `scan_many`, `remove_prefix`), each on a real need,
// before it was frozen. The next ones are foreseeable too: a store-side
// TTL (Redis `EXPIRE`), a batch write, compare-and-swap.
//
// So a new operation arrives the way `hclient_rt::IpcConnect` arrived
// beside `TcpConnect`: **as a trait that extends this one**
// (`trait KeyValueBatch: KeyValueStore { .. }`), with its bound on the one
// wrapper or constructor that needs it. A store that cannot do it
// implements nothing and loses only that wrapper, as a compile error
// where it was asked for. What must not happen is the obvious edit: a
// method added here.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::future::{Future, Ready, ready};
use std::sync::{Arc, Mutex};

/// A store of opaque bytes, partitioned by namespace and keyed by string.
///
/// See the [module docs](self) for why this is bytes, why the futures are
/// named, and what a wrapper owes.
///
/// # The shape of an answer
///
/// [`get`](Self::get) answers **several** values, because that is what
/// the stores above need: a cache holds every `Vary` variant under one
/// key, and a domain holds many cookies. A store that holds at most one
/// value per key answers a `Vec` of length 0 or 1, which is what
/// `AltSvcStore`'s wrapper does — one shape rather than two, so a backend
/// author writes one thing.
// `len` is deliberately absent, and it is the one method the four stores
// above have that this does not. Two of them expose it only through
// inspection (`CookieJar::len`, `is_empty`), and a count that a remote
// store might disagree with is worse than none — the argument
// `AnyStore`'s `Debug` already makes one crate over. A wrapper that
// needs a count keeps its own.
pub trait KeyValueStore {
    /// The point in time expiries are compared against.
    ///
    /// [`Timer::Instant`](crate::timer::Timer::Instant)'s bounds, and for
    /// its reason: a store compares, so it needs an ordering and not an
    /// epoch. Every store in this workspace binds it to
    /// `web_time::SystemTime`. See the [module docs](self).
    type Instant: Copy + PartialOrd;

    /// The answer to [`get`](Self::get).
    ///
    /// Owned rather than borrowed: a remote store has nothing to lend.
    type Get<'a>: Future<Output = Vec<Vec<u8>>> + 'a
    where
        Self: 'a;

    /// The answer to [`get_many`](Self::get_many), one entry per
    /// requested key and in that order.
    type GetMany<'a>: Future<Output = Vec<Vec<Vec<u8>>>> + 'a
    where
        Self: 'a;

    /// The answer to [`scan`](Self::scan): `(key, value)` pairs, in no
    /// particular order.
    type Scan<'a>: Future<Output = Vec<(String, Vec<u8>)>> + 'a
    where
        Self: 'a;

    /// The answer to [`put`](Self::put), [`remove`](Self::remove),
    /// [`clear`](Self::clear) and
    /// [`remove_prefix`](Self::remove_prefix).
    type Done<'a>: Future<Output = ()> + 'a
    where
        Self: 'a;

    /// Every unexpired value held under `key` in `ns`, in no particular
    /// order.
    fn get<'a>(&'a self, ns: &'a str, key: &'a str, now: Self::Instant) -> Self::Get<'a>;

    // Maintainer notes (not rendered):
    //
    // A batch rather than one call per key, and not a convenience: a
    // cookie lookup asks about every ancestor of a host at once, so a
    // remote store should pay one round trip for a request rather than
    // one per label. [`touch`] one seam up is the same decision.
    //
    // [`touch`]: https://docs.rs/hclient/latest/hclient/cookie/trait.CookieStore.html
    /// [`get`](Self::get) for several keys at once, answering one `Vec`
    /// per key, **in the order given** and of the same length as `keys`.
    ///
    /// A batch rather than one call per key, and not a convenience: a
    /// cookie lookup asks about every ancestor of a host at once, so a
    /// remote store should pay one round trip for a request rather than
    /// one per label.
    ///
    /// **`keys` is owned where every other key here is borrowed**, and
    /// the asymmetry is the wrapper's rather than a slip. A single key
    /// is something a caller already has; a *batch* is always computed —
    /// the four seams above each derive theirs, reversing a host's
    /// labels — so a borrowed slice would have to be kept alive beside
    /// the future by whoever built it, which is a self-referential
    /// struct and needs `unsafe` in a crate that forbids it. Handing
    /// ownership over costs one `Vec` per lookup and lets a wrapper's
    /// future be an ordinary named type, whose `Send` is then *inferred*
    /// rather than declared.
    fn get_many(&self, ns: &str, keys: Vec<String>, now: Self::Instant) -> Self::GetMany<'_>;

    /// Add `value` under `key`, **beside** whatever is already there.
    ///
    /// Replacement is the wrapper's, because what makes two values the
    /// same entry is a domain question: two cookies with one name and
    /// path are one cookie, where two cache entries under one key are
    /// two `Vary` variants and both are kept. A wrapper that replaces
    /// calls [`remove`](Self::remove) first.
    ///
    /// `expires` is absolute and `None` means *no expiry of its own* —
    /// a session cookie, or an entry whose lifetime the wrapper
    /// enforces. `now` is what lets a store drop expired entries while
    /// it is already holding its lock; a store that ignores it is
    /// correct and keeps rubbish until something reads it.
    fn put(
        &self,
        ns: &str,
        key: String,
        value: Vec<u8>,
        expires: Option<Self::Instant>,
        now: Self::Instant,
    ) -> Self::Done<'_>;

    /// Drop everything held under `key` in `ns`.
    ///
    /// **`key` is owned, like [`get_many`](Self::get_many)'s and unlike
    /// [`get`](Self::get)'s**, and for that method's reason: a wrapper
    /// derives this key rather than receiving it, and a `remove` is
    /// usually the first half of a replacement — so a borrowed key would
    /// have to outlive a future that also holds it, which is a
    /// self-referential struct.
    fn remove(&self, ns: &str, key: String) -> Self::Done<'_>;

    /// Drop everything in `ns`, leaving every other namespace alone.
    fn clear<'a>(&'a self, ns: &'a str) -> Self::Done<'a>;

    // Maintainer notes (not rendered):
    //
    // # Why the seam has this at all
    //
    // It is the one operation that is **not** addressed by an exact
    // key, and it was added only after three of the four wrappers above
    // had each bent around its absence. A response cache holds every
    // `Vary` variant of one request and cannot know their selectors
    // before it reads them; a cookie jar holds many cookies under one
    // domain and evicts by comparing them; an `Alt-Svc` memory forgets
    // a whole class at once. Each of those is *enumerate what is under
    // this prefix*, and without it a wrapper has to either fold the
    // distinction into the key — which works when the classes are known
    // in advance, as `persist` is, and not otherwise — or read and
    // rewrite a whole list to change one member, which is neither cheap
    // nor atomic.
    /// Every unexpired `(key, value)` in `ns` whose key begins with
    /// `prefix`, in no particular order. An empty prefix is the whole
    /// namespace.
    ///
    /// # What it is for
    ///
    /// It is the one operation that is **not** addressed by an exact
    /// key. A response cache holds every
    /// `Vary` variant of one request and cannot know their selectors
    /// before it reads them; a cookie jar holds many cookies under one
    /// domain and evicts by comparing them; an `Alt-Svc` memory forgets
    /// a whole class at once. Each of those is *enumerate what is under
    /// this prefix*.
    ///
    /// **It stays a byte operation**, which is what makes it admissible:
    /// a prefix is a string, and this seam still knows nothing of
    /// cookies, variants or advertisements. The wrapper builds the
    /// prefix out of its own vocabulary, exactly as it builds a key.
    ///
    /// # What an implementor owes, and what it may not promise
    ///
    /// The answer is a **snapshot of no particular instant**. An
    /// in-memory store can take it under one lock; a remote one cannot,
    /// and `SCAN` on Redis is explicitly a cursor that may miss a key
    /// written during the walk and may repeat one. So a wrapper must
    /// treat this as *what was there, roughly, a moment ago* — good for
    /// choosing an eviction victim or listing variants, and never as the
    /// basis of a claim that something is absent.
    ///
    /// Ordering is unspecified for the same reason. A wrapper that needs
    /// an order sorts what comes back.
    /// `prefix` is owned, like [`get_many`](Self::get_many)'s keys and
    /// for that method's reason: a wrapper *derives* a prefix from its
    /// own vocabulary rather than receiving one, so a borrowed prefix
    /// would have to outlive a future that also holds it.
    fn scan(&self, ns: &str, prefix: String, now: Self::Instant) -> Self::Scan<'_>;

    /// [`scan`](Self::scan) for several prefixes at once, answering the
    /// union of what each matches — with **no duplicates**, so a pair of
    /// prefixes where one extends the other answers each value once.
    ///
    /// A batch for [`get_many`](Self::get_many)'s reason, and the same
    /// consumer: a cookie lookup asks about every ancestor of a host,
    /// and reversing the labels makes each ancestor a prefix — but of
    /// *each other* rather than of one common string, so a single scan
    /// cannot express it. A jar over a remote store should still pay one
    /// round trip for a request rather than one per label.
    ///
    /// The union rather than one answer per prefix, unlike `get_many`'s
    /// parallel list: a caller of this is gathering, and the key comes
    /// back beside each value, so which prefix matched is recoverable
    /// where it matters and costs nothing to discard where it does not.
    fn scan_many(&self, ns: &str, prefixes: Vec<String>, now: Self::Instant) -> Self::Scan<'_>;

    /// Drop every value in `ns` whose key begins with `prefix`.
    ///
    /// [`scan`](Self::scan) followed by a [`remove`](Self::remove) each
    /// would do it, and this exists because that reads every value back
    /// in order to throw it away — a cache invalidating one URL's
    /// variants would pull their bodies over a socket first. A store
    /// with a range delete does it without them.
    ///
    /// An empty prefix is [`clear`](Self::clear), and an implementor may
    /// answer it that way.
    fn remove_prefix(&self, ns: &str, prefix: String) -> Self::Done<'_>;
}

// Maintainer notes (not rendered):
//
// What a plain client uses, and the reference for what the methods
// above mean. It answers [`Ready`], so the seam's
// shape costs it no allocation and no suspension — which is the whole
// of what associated future types buy over `#[async_trait]`, measured
// at 1,000 allocations against 0 over a store that answers immediately.
/// The store this crate ships: a `HashMap` per namespace, in memory.
///
/// What a plain client uses, and the reference for what the methods
/// above mean. It answers [`Ready`], so the seam's
/// shape costs it no allocation and no suspension.
///
/// **The `Mutex` is what `&self` on the seam costs an in-memory store.**
/// It is the cheaper half of that trade: one uncontended lock per
/// operation, against a `&mut` seam that could not be held across an
/// await at all and so could not have a remote store behind it.
///
/// # No capacity here, deliberately
///
/// A byte store cannot count cookies, and the bounds that matter — RFC
/// 6265 §6.1's 3000 per jar and 50 per domain — are counts of *entries*
/// of a particular kind. So capacity belongs to the wrapper, which knows
/// what it is storing; this holds what it is given and drops what has
/// expired. A wrapper whose bound is global keeps its own count, which
/// drifts upward when entries expire here unobserved — evicting earlier
/// than it needed to, which is the under-claiming direction.
///
/// # `Clone` shares the map, and that is a requirement rather than a
/// convenience
///
/// A consumer of a store is routinely cloned — `AltSvcCache` is cloned
/// into `Native`'s own routing half, and both copies must see one
/// memory — so a `Clone` that *copied* would hand the two halves
/// separate stores and lose every entry one of them wrote. It shares,
/// which also makes one instance behind four wrappers the ordinary case
/// rather than something a caller has to arrange.
#[derive(Debug)]
pub struct MemoryStore<I> {
    namespaces: Arc<Mutex<Namespaces<I>>>,
}

// Hand-written rather than derived: a derive would demand `I: Clone`,
// which is true of every `Instant` here but is not what this says — the
// `Arc` is what clones, and the instants inside it are untouched.
impl<I> Clone for MemoryStore<I> {
    fn clone(&self) -> Self {
        Self {
            namespaces: Arc::clone(&self.namespaces),
        }
    }
}

/// What a [`MemoryStore`] holds: a map of keys per namespace, and the
/// values under each key.
///
/// A name rather than the nest written out, because the nest appears in
/// three signatures and clippy is right that it reads as noise in all
/// three.
type Namespaces<I> = HashMap<Box<str>, HashMap<Box<str>, Vec<Slot<I>>>>;

/// One stored value and the instant it stops being answerable.
#[derive(Debug)]
struct Slot<I> {
    value: Vec<u8>,
    /// `None` is *no expiry of its own* — see [`KeyValueStore::put`].
    expires: Option<I>,
}

impl<I: Copy + PartialOrd> Slot<I> {
    /// Whether this value may still be answered with.
    ///
    /// **The incomparable case is decided explicitly**, which is why
    /// this is `partial_cmp` and not `expires > now`:
    /// [`Instant`](KeyValueStore::Instant) is only `PartialOrd`, so two
    /// instants may not be ordered at all, and a value this store cannot
    /// *prove* expired is one it keeps. Keeping rubbish is the safe
    /// direction — the wrapper above applies its own rules to whatever
    /// comes back and drops what it does not want — where dropping a
    /// live value is a lost cookie nothing can recover.
    fn is_live(&self, now: I) -> bool {
        self.expires
            .is_none_or(|e| !matches!(e.partial_cmp(&now), Some(Ordering::Less | Ordering::Equal)))
    }
}

impl<I> Default for MemoryStore<I> {
    fn default() -> Self {
        Self {
            namespaces: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl<I> MemoryStore<I> {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self
    where
        Self: Default,
    {
        Self::default()
    }
}

impl<I: Copy + PartialOrd> KeyValueStore for MemoryStore<I> {
    type Instant = I;
    type Get<'a>
        = Ready<Vec<Vec<u8>>>
    where
        Self: 'a;
    type GetMany<'a>
        = Ready<Vec<Vec<Vec<u8>>>>
    where
        Self: 'a;
    type Scan<'a>
        = Ready<Vec<(String, Vec<u8>)>>
    where
        Self: 'a;
    type Done<'a>
        = Ready<()>
    where
        Self: 'a;

    fn get<'a>(&'a self, ns: &'a str, key: &'a str, now: Self::Instant) -> Self::Get<'a> {
        let held = self.namespaces.lock().expect("kv store lock");
        ready(read_one(&held, ns, key, now))
    }

    fn get_many(&self, ns: &str, keys: Vec<String>, now: Self::Instant) -> Self::GetMany<'_> {
        let held = self.namespaces.lock().expect("kv store lock");
        // One lock for the batch, which is the point of the method: the
        // remote store it exists for pays one round trip, and this one
        // pays one acquisition.
        ready(keys.iter().map(|k| read_one(&held, ns, k, now)).collect())
    }

    fn put(
        &self,
        ns: &str,
        key: String,
        value: Vec<u8>,
        expires: Option<Self::Instant>,
        now: Self::Instant,
    ) -> Self::Done<'_> {
        let mut held = self.namespaces.lock().expect("kv store lock");
        let slots = held
            .entry(ns.into())
            .or_default()
            .entry(key.into())
            .or_default();
        // Swept on the way in, while the lock is already held — the
        // cheapest moment there is, and what keeps a key that is written
        // often from growing without bound. Only this key's own slots:
        // sweeping the namespace would make one write cost a scan, which
        // is the shape a remote store cannot afford.
        slots.retain(|s| s.is_live(now));
        slots.push(Slot { value, expires });
        ready(())
    }

    fn remove(&self, ns: &str, key: String) -> Self::Done<'_> {
        let mut held = self.namespaces.lock().expect("kv store lock");
        if let Some(keys) = held.get_mut(ns) {
            keys.remove(key.as_str());
            // An emptied namespace goes too, so a store that has been
            // cleared by removal holds no more than one that was never
            // written.
            if keys.is_empty() {
                held.remove(ns);
            }
        }
        ready(())
    }

    fn clear<'a>(&'a self, ns: &'a str) -> Self::Done<'a> {
        let mut held = self.namespaces.lock().expect("kv store lock");
        held.remove(ns);
        ready(())
    }

    fn scan(&self, ns: &str, prefix: String, now: Self::Instant) -> Self::Scan<'_> {
        let held = self.namespaces.lock().expect("kv store lock");
        // One lock for the walk, so this store's answer really is the
        // snapshot the seam allows it to be. A remote one cannot promise
        // that, which is why the seam promises less.
        ready(
            held.get(ns)
                .into_iter()
                .flat_map(|keys| keys.iter())
                .filter(|(k, _)| k.starts_with(&prefix))
                .flat_map(|(k, slots)| {
                    slots
                        .iter()
                        .filter(|s| s.is_live(now))
                        .map(move |s| (k.to_string(), s.value.clone()))
                })
                .collect(),
        )
    }

    fn scan_many(&self, ns: &str, prefixes: Vec<String>, now: Self::Instant) -> Self::Scan<'_> {
        let held = self.namespaces.lock().expect("kv store lock");
        ready(
            held.get(ns)
                .into_iter()
                .flat_map(|keys| keys.iter())
                // One pass over the namespace rather than one per
                // prefix, which is also what makes the answer a union
                // rather than a concatenation: a key matching two
                // prefixes is visited once.
                .filter(|(k, _)| prefixes.iter().any(|p| k.starts_with(p.as_str())))
                .flat_map(|(k, slots)| {
                    slots
                        .iter()
                        .filter(|s| s.is_live(now))
                        .map(move |s| (k.to_string(), s.value.clone()))
                })
                .collect(),
        )
    }

    fn remove_prefix(&self, ns: &str, prefix: String) -> Self::Done<'_> {
        let mut held = self.namespaces.lock().expect("kv store lock");
        if let Some(keys) = held.get_mut(ns) {
            keys.retain(|k, _| !k.starts_with(&prefix));
            // An emptied namespace goes, as it does for `remove`.
            if keys.is_empty() {
                held.remove(ns);
            }
        }
        ready(())
    }
}

/// The read both [`get`](KeyValueStore::get) and
/// [`get_many`](KeyValueStore::get_many) do, written once so the two
/// cannot disagree about what "unexpired" means.
fn read_one<I: Copy + PartialOrd>(
    held: &Namespaces<I>,
    ns: &str,
    key: &str,
    now: I,
) -> Vec<Vec<u8>> {
    held.get(ns)
        .and_then(|keys| keys.get(key))
        .map(|slots| {
            slots
                .iter()
                .filter(|s| s.is_live(now))
                .map(|s| s.value.clone())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    /// `std`'s clock, not `web_time`'s, and deliberately: this is a test,
    /// it runs on the host, and this crate has no `web-time` — which is
    /// the whole reason [`KeyValueStore::Instant`] is a type.
    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn store() -> MemoryStore<SystemTime> {
        MemoryStore::new()
    }

    /// Poll a future that is known to be [`Ready`]. Every future this
    /// store answers is, so there is nothing to drive.
    fn now_or_never<F: Future>(f: F) -> F::Output {
        use std::task::{Context, Poll, Waker};
        let mut f = Box::pin(f);
        match f.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(v) => v,
            Poll::Pending => panic!("this store answers `Ready`"),
        }
    }

    #[test]
    fn a_value_put_is_a_value_got() {
        let kv = store();
        now_or_never(kv.put("ns", "k".to_owned(), b"v".to_vec(), None, at(0)));
        assert_eq!(now_or_never(kv.get("ns", "k", at(0))), vec![b"v".to_vec()]);
    }

    /// A clock whose instants are **genuinely incomparable** in one
    /// direction, which `SystemTime` cannot be: it is totally ordered, so
    /// no test written on it can reach the branch
    /// [`Slot::is_live`] documents.
    ///
    /// Two readings of two different clocks — a laptop's and a server's —
    /// are the real shape of this: both are wall-clock instants and
    /// neither is before the other.
    #[derive(Debug, Clone, Copy, PartialEq)]
    struct Unsynced {
        clock: u8,
        tick: u64,
    }

    impl PartialOrd for Unsynced {
        fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
            (self.clock == other.clock).then(|| self.tick.cmp(&other.tick))
        }
    }

    /// **A value this store cannot prove expired is one it keeps**, which
    /// is the branch `is_live` exists for and the direction that matters:
    /// keeping rubbish costs a wrapper one filtered-out entry, where
    /// dropping a live value is a cookie nothing can recover.
    ///
    /// Unreachable on a totally ordered clock, so it takes a clock of its
    /// own — and without it the mutation that treats *incomparable* as
    /// *expired* passes the whole suite.
    #[test]
    fn a_value_whose_expiry_is_incomparable_with_now_is_kept() {
        let kv: MemoryStore<Unsynced> = MemoryStore::new();
        let expires = Unsynced { clock: 1, tick: 10 };
        let now = Unsynced { clock: 2, tick: 99 };
        assert_eq!(
            expires.partial_cmp(&now),
            None,
            "the fixture's own premise: these two are not ordered"
        );

        now_or_never(kv.put("ns", "k".to_owned(), b"v".to_vec(), Some(expires), now));

        assert_eq!(
            now_or_never(kv.get("ns", "k", now)),
            vec![b"v".to_vec()],
            "kept, because nothing proved it expired"
        );
    }

    /// The control for the test above, on the same clock: where the two
    /// instants *are* comparable it expires as it does anywhere else. Without
    /// this pair, a store that simply never expired anything would pass.
    #[test]
    fn the_same_clock_expires_normally_when_its_instants_are_comparable() {
        let kv: MemoryStore<Unsynced> = MemoryStore::new();
        let expires = Unsynced { clock: 1, tick: 10 };
        let now = Unsynced { clock: 1, tick: 11 };

        now_or_never(kv.put("ns", "k".to_owned(), b"v".to_vec(), Some(expires), now));

        assert!(now_or_never(kv.get("ns", "k", now)).is_empty());
    }

    /// **Several values under one key, which is decision 3 of the
    /// design.** The cache holds every `Vary` variant under one key and a
    /// domain holds many cookies, so a `put` that replaced would make
    /// storing one cookie drop every other cookie of that domain.
    #[test]
    fn a_second_put_stores_beside_the_first_rather_than_replacing_it() {
        let kv = store();
        now_or_never(kv.put("ns", "k".to_owned(), b"one".to_vec(), None, at(0)));
        now_or_never(kv.put("ns", "k".to_owned(), b"two".to_vec(), None, at(0)));

        let mut got = now_or_never(kv.get("ns", "k", at(0)));
        got.sort();
        assert_eq!(got, vec![b"one".to_vec(), b"two".to_vec()]);
    }

    /// **The namespace is the whole of what lets one store serve four
    /// seams**, so two seams writing the same key must not see each
    /// other — a cookie for `com.example` and an HSTS entry for
    /// `com.example` share a key and are different facts.
    ///
    /// Asserted over *many* namespaces rather than two, and on each one
    /// in turn, because a `HashMap`'s iteration order is arbitrary: a
    /// read that ignored the namespace and answered the first one it
    /// found would be right by luck about one of two, and this test
    /// passed under exactly that mutation before it was written this
    /// way. With eight, every read but one has to be wrong.
    #[test]
    fn one_key_in_many_namespaces_is_many_separate_entries() {
        let kv = store();
        let namespaces = ["cookie", "hsts", "cache", "altsvc", "a", "b", "c", "d"];
        for ns in namespaces {
            now_or_never(kv.put(
                ns,
                "com.example".to_owned(),
                ns.as_bytes().to_vec(),
                None,
                at(0),
            ));
        }

        for ns in namespaces {
            assert_eq!(
                now_or_never(kv.get(ns, "com.example", at(0))),
                vec![ns.as_bytes().to_vec()],
                "namespace {ns} answered with another namespace's value"
            );
        }
    }

    /// The write is namespaced too, which the read test above cannot
    /// separate: a store that read correctly and *wrote* to one shared
    /// map would answer every namespace with everything.
    #[test]
    fn a_put_in_one_namespace_is_invisible_in_another() {
        let kv = store();
        now_or_never(kv.put("cookie", "k".to_owned(), b"c".to_vec(), None, at(0)));

        assert!(
            now_or_never(kv.get("hsts", "k", at(0))).is_empty(),
            "an unwritten namespace answers nothing"
        );
    }

    /// **The one promise a reader can rely on**: an expired entry is
    /// never answered with. Whether it is still held is the store's own
    /// business — see the sweep test below.
    #[test]
    fn an_expired_value_is_not_answered() {
        let kv = store();
        now_or_never(kv.put("ns", "k".to_owned(), b"v".to_vec(), Some(at(10)), at(0)));

        assert_eq!(
            now_or_never(kv.get("ns", "k", at(9))),
            vec![b"v".to_vec()],
            "still live one second before its expiry"
        );
        assert!(
            now_or_never(kv.get("ns", "k", at(10))).is_empty(),
            "the expiry instant itself is past it"
        );
    }

    /// `None` is *no expiry of its own* — a session cookie — and it is
    /// the case a store gets wrong by treating the absence as `now`.
    #[test]
    fn a_value_with_no_expiry_outlives_any_now() {
        let kv = store();
        now_or_never(kv.put("ns", "k".to_owned(), b"v".to_vec(), None, at(0)));
        assert_eq!(
            now_or_never(kv.get("ns", "k", at(u64::from(u32::MAX)))),
            vec![b"v".to_vec()]
        );
    }

    /// Expiry is per value, not per key, because the values under one key
    /// arrive at different times: two cookies of one domain expire
    /// independently.
    #[test]
    fn one_value_expiring_leaves_its_neighbour_under_the_same_key() {
        let kv = store();
        now_or_never(kv.put("ns", "k".to_owned(), b"short".to_vec(), Some(at(10)), at(0)));
        now_or_never(kv.put("ns", "k".to_owned(), b"long".to_vec(), Some(at(100)), at(0)));

        assert_eq!(
            now_or_never(kv.get("ns", "k", at(50))),
            vec![b"long".to_vec()]
        );
    }

    /// **The batch answers one entry per key, in order and of the same
    /// length** — which is what makes it usable for a cookie lookup,
    /// where the caller pairs each answer with the ancestor it asked
    /// about. A missing key is an empty `Vec` and not a shortened
    /// answer, or the pairing silently shifts.
    #[test]
    fn get_many_answers_one_entry_per_key_in_order_including_the_misses() {
        let kv = store();
        now_or_never(kv.put("ns", "com.example".to_owned(), b"a".to_vec(), None, at(0)));
        now_or_never(kv.put("ns", "com.example.b".to_owned(), b"b".to_vec(), None, at(0)));

        let keys = ["com.example.b.a", "com.example.b", "com.example", "com"];
        let got =
            now_or_never(kv.get_many("ns", keys.iter().map(|k| (*k).to_owned()).collect(), at(0)));

        assert_eq!(
            got,
            vec![
                Vec::<Vec<u8>>::new(),
                vec![b"b".to_vec()],
                vec![b"a".to_vec()],
                Vec::<Vec<u8>>::new(),
            ]
        );
    }

    /// The two reads must agree about what "unexpired" means, which is
    /// why they share `read_one`. Written as a test anyway, because the
    /// sharing is an implementation detail a later change can undo.
    #[test]
    fn get_many_hides_an_expired_value_exactly_as_get_does() {
        let kv = store();
        now_or_never(kv.put("ns", "k".to_owned(), b"v".to_vec(), Some(at(10)), at(0)));

        assert_eq!(
            now_or_never(kv.get_many("ns", vec!["k".to_owned()], at(20))),
            vec![Vec::<Vec<u8>>::new()]
        );
        assert!(now_or_never(kv.get("ns", "k", at(20))).is_empty());
    }

    #[test]
    fn remove_drops_every_value_under_the_key_and_leaves_its_neighbours() {
        let kv = store();
        now_or_never(kv.put("ns", "k".to_owned(), b"one".to_vec(), None, at(0)));
        now_or_never(kv.put("ns", "k".to_owned(), b"two".to_vec(), None, at(0)));
        now_or_never(kv.put("ns", "other".to_owned(), b"keep".to_vec(), None, at(0)));

        now_or_never(kv.remove("ns", "k".to_owned()));

        assert!(now_or_never(kv.get("ns", "k", at(0))).is_empty());
        assert_eq!(
            now_or_never(kv.get("ns", "other", at(0))),
            vec![b"keep".to_vec()]
        );
    }

    /// `clear` is scoped to its namespace, which is what lets a client
    /// drop its cookies without dropping its HSTS entries — two seams,
    /// one store.
    #[test]
    fn clear_empties_one_namespace_and_leaves_the_others() {
        let kv = store();
        now_or_never(kv.put("cookie", "k".to_owned(), b"c".to_vec(), None, at(0)));
        now_or_never(kv.put("hsts", "k".to_owned(), b"h".to_vec(), None, at(0)));

        now_or_never(kv.clear("cookie"));

        assert!(now_or_never(kv.get("cookie", "k", at(0))).is_empty());
        assert_eq!(
            now_or_never(kv.get("hsts", "k", at(0))),
            vec![b"h".to_vec()]
        );
    }

    /// **A key written often does not grow without bound**, which is the
    /// half of expiry a reader cannot observe through `get`: the entries
    /// are gone rather than merely hidden. Asserted through the store's
    /// own internals, because there is deliberately no `len` on the seam.
    #[test]
    fn a_put_sweeps_the_expired_values_of_the_key_it_writes() {
        let kv = store();
        for _ in 0..8 {
            now_or_never(kv.put("ns", "k".to_owned(), b"old".to_vec(), Some(at(10)), at(0)));
        }
        assert_eq!(
            kv.namespaces.lock().unwrap()["ns"]["k"].len(),
            8,
            "all eight are held while they are live"
        );

        now_or_never(kv.put("ns", "k".to_owned(), b"new".to_vec(), None, at(20)));

        assert_eq!(
            kv.namespaces.lock().unwrap()["ns"]["k"].len(),
            1,
            "the eight expired ones went with the ninth write"
        );
    }

    /// The sweep is this key's own. Sweeping the namespace would make
    /// one write cost a scan of every key in it, which is the shape a
    /// remote store cannot afford — so a neighbour's expired values
    /// survive a write here, and are dropped when *it* is next written
    /// or simply never answered.
    #[test]
    fn a_put_does_not_sweep_a_neighbouring_key() {
        let kv = store();
        now_or_never(kv.put("ns", "stale".to_owned(), b"v".to_vec(), Some(at(10)), at(0)));
        now_or_never(kv.put("ns", "fresh".to_owned(), b"v".to_vec(), None, at(20)));

        assert_eq!(
            kv.namespaces.lock().unwrap()["ns"]["stale"].len(),
            1,
            "held, and unreachable through `get`"
        );
        assert!(now_or_never(kv.get("ns", "stale", at(20))).is_empty());
    }

    // ---- scan and remove_prefix --------------------------------------

    /// The prefix selects, and an empty one is the whole namespace.
    #[test]
    fn scan_answers_the_pairs_under_a_prefix_and_no_others() {
        let kv = store();
        for key in ["com.example", "com.example.a", "com.other", "net.example"] {
            now_or_never(kv.put("ns", key.to_owned(), key.as_bytes().to_vec(), None, at(0)));
        }

        let mut got: Vec<String> = now_or_never(kv.scan("ns", "com.example".to_owned(), at(0)))
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        got.sort();
        assert_eq!(got, ["com.example", "com.example.a"]);

        let all = now_or_never(kv.scan("ns", String::new(), at(0)));
        assert_eq!(all.len(), 4, "an empty prefix is the whole namespace");
    }

    /// The value travels with the key, which is what an eviction victim
    /// needs: a wrapper compares the values and removes by the key.
    #[test]
    fn scan_answers_the_value_beside_its_key() {
        let kv = store();
        now_or_never(kv.put("ns", "k".to_owned(), b"v".to_vec(), None, at(0)));
        assert_eq!(
            now_or_never(kv.scan("ns", String::new(), at(0))),
            vec![("k".to_owned(), b"v".to_vec())]
        );
    }

    /// Several values under one key each get a pair, so a cache's
    /// variants arrive as variants rather than as one entry.
    #[test]
    fn scan_answers_one_pair_per_value_not_per_key() {
        let kv = store();
        now_or_never(kv.put("ns", "k".to_owned(), b"one".to_vec(), None, at(0)));
        now_or_never(kv.put("ns", "k".to_owned(), b"two".to_vec(), None, at(0)));

        let mut got = now_or_never(kv.scan("ns", String::new(), at(0)));
        got.sort();
        assert_eq!(
            got,
            vec![
                ("k".to_owned(), b"one".to_vec()),
                ("k".to_owned(), b"two".to_vec())
            ]
        );
    }

    /// Expiry applies here as it does to every read — the one promise
    /// this seam makes.
    #[test]
    fn scan_hides_an_expired_value() {
        let kv = store();
        now_or_never(kv.put("ns", "k".to_owned(), b"v".to_vec(), Some(at(10)), at(0)));
        assert!(now_or_never(kv.scan("ns", String::new(), at(10))).is_empty());
        assert_eq!(now_or_never(kv.scan("ns", String::new(), at(9))).len(), 1);
    }

    /// The namespace bounds it, which is what lets one store back four
    /// seams: a cache scanning its own keys must not meet a cookie.
    #[test]
    fn scan_does_not_cross_namespaces() {
        let kv = store();
        now_or_never(kv.put("cookie", "k".to_owned(), b"c".to_vec(), None, at(0)));
        now_or_never(kv.put("cache", "k".to_owned(), b"r".to_vec(), None, at(0)));

        assert_eq!(
            now_or_never(kv.scan("cache", String::new(), at(0))),
            vec![("k".to_owned(), b"r".to_vec())]
        );
    }

    /// **The union, not a concatenation**: a key matching two prefixes
    /// comes back once. A cookie lookup asks about every ancestor of a
    /// host, and a descendant's reversed key really does begin with its
    /// ancestor's prefix — so a per-prefix walk would double it.
    #[test]
    fn scan_many_answers_a_union_rather_than_one_pass_per_prefix() {
        let kv = store();
        now_or_never(kv.put(
            "ns",
            "com.example.b.a".to_owned(),
            b"v".to_vec(),
            None,
            at(0),
        ));

        let got = now_or_never(kv.scan_many(
            "ns",
            vec!["com.example.b".to_owned(), "com.example".to_owned()],
            at(0),
        ));
        assert_eq!(got.len(), 1, "one value, though two prefixes match it");
    }

    /// And it really does answer every prefix, not just the first.
    #[test]
    fn scan_many_answers_every_prefix_it_is_given() {
        let kv = store();
        now_or_never(kv.put("ns", "com.a".to_owned(), b"a".to_vec(), None, at(0)));
        now_or_never(kv.put("ns", "net.b".to_owned(), b"b".to_vec(), None, at(0)));
        now_or_never(kv.put("ns", "org.c".to_owned(), b"c".to_vec(), None, at(0)));

        let mut got: Vec<String> =
            now_or_never(kv.scan_many("ns", vec!["com.".to_owned(), "org.".to_owned()], at(0)))
                .into_iter()
                .map(|(k, _)| k)
                .collect();
        got.sort();
        assert_eq!(got, ["com.a", "org.c"], "and nothing the prefixes miss");
    }

    #[test]
    fn scan_many_hides_expired_values_as_scan_does() {
        let kv = store();
        now_or_never(kv.put("ns", "k".to_owned(), b"v".to_vec(), Some(at(10)), at(0)));
        assert!(now_or_never(kv.scan_many("ns", vec!["k".to_owned()], at(10))).is_empty());
    }

    #[test]
    fn remove_prefix_drops_the_matching_keys_and_leaves_the_rest() {
        let kv = store();
        for key in ["com.example", "com.example.a", "com.other"] {
            now_or_never(kv.put("ns", key.to_owned(), b"v".to_vec(), None, at(0)));
        }

        now_or_never(kv.remove_prefix("ns", "com.example".to_owned()));

        let mut left: Vec<String> = now_or_never(kv.scan("ns", String::new(), at(0)))
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        left.sort();
        assert_eq!(left, ["com.other"]);
    }

    /// An empty prefix is `clear`, and an implementor may answer it that
    /// way — so this pins the behaviour rather than the route.
    #[test]
    fn remove_prefix_with_an_empty_prefix_empties_the_namespace() {
        let kv = store();
        now_or_never(kv.put("ns", "a".to_owned(), b"v".to_vec(), None, at(0)));
        now_or_never(kv.put("other", "a".to_owned(), b"v".to_vec(), None, at(0)));

        now_or_never(kv.remove_prefix("ns", String::new()));

        assert!(now_or_never(kv.scan("ns", String::new(), at(0))).is_empty());
        assert_eq!(
            now_or_never(kv.scan("other", String::new(), at(0))).len(),
            1,
            "and no other namespace with it"
        );
    }

    /// An emptied namespace goes with its last key, so a store cleared
    /// by removal holds no more than one that was never written.
    #[test]
    fn removing_the_last_key_of_a_namespace_drops_the_namespace() {
        let kv = store();
        now_or_never(kv.put("ns", "k".to_owned(), b"v".to_vec(), None, at(0)));
        now_or_never(kv.remove("ns", "k".to_owned()));
        assert!(kv.namespaces.lock().unwrap().is_empty());
    }
}
