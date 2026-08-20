//! The cookie jar's public suffix list and the cache's store, erased so
//! that neither becomes a type parameter on [`Client`](crate::Client).
//!
//! # Why erased at all, when the seams are already generic
//!
//! `hclient_cookie::CookieJar<P>` and `hclient_cache::HttpCache<S>` are
//! generic, and until this module existed `hclient` accepted only their
//! defaulted forms — so a caller who wanted a disk-backed cache, a store
//! shared between processes, or a jar over
//! [`NoList`](hclient_cookie::NoList) could reach it in `hclient-cache` or
//! `hclient-cookie` and not through the facade. The seam existed one crate
//! down and was unreachable one crate up.
//!
//! # Why not a type parameter on `Client`
//!
//! Two reasons, and the second is the one that decides it.
//!
//! `cached.rs` had already written down the first: a recording body holds
//! a handle to the cache, so `S` on the cache is `S` on the public
//! [`ClientBody`](crate::ClientBody) alias — **the arity of a public type
//! alias would change with a feature nobody in the graph asked for**, and
//! Cargo unifies features, so no crate could rely on either arity.
//!
//! The second is that a defaulted parameter needs a default *type*, and
//! `hclient-cookie` and `hclient-cache` are **optional dependencies**.
//! `Client<T, Tm, P = hclient_cookie::BuiltinList, S = ..>` names two
//! types that do not exist in a build without those features, so the
//! declaration would have to be forked four ways — and `Client` is already
//! forked once, for `DefaultTransport`. Erasure has no such problem: the
//! field is inside the same `#[cfg]` as the feature, and there is no
//! parameter to default when it is off.
//!
//! # What it costs, said plainly
//!
//! One `Send` bound, on each of the two opt-in setters and nowhere else —
//! spec amendment C12. That is `Native::multiplexed()`'s shape exactly: no
//! signature anyone else meets acquires a bound, and a caller who hands in
//! a `!Send` list gets `E0277` on the line where they asked.
//!
//! The bound is not new in substance. `Inner`'s own doc has said since the
//! jar landed that the `Mutex` is there because *"a `Client` is meant to
//! cross a `tokio::spawn`"*, and `BuiltinList` and `MemoryStore` are both
//! `Send` — so what this states is the property the concrete types already
//! had. Erasing without it would make every `Client` in a build with the
//! feature compiled in `!Send`, configured cache or not, which is the
//! feature-unification hazard the paragraph above is about.
//!
//! Both wrappers implement the seam they erase, so a `CookieJar<AnyList>`
//! and an `HttpCache<AnyStore>` are ordinary jars and caches with their
//! whole API — which is what lets [`Client::cookies`](crate::Client::
//! cookies) and [`Client::cache`](crate::Client::cache) keep handing back
//! a guard onto the real thing rather than onto a narrowed trait object.

/// A [`PublicSuffixList`](hclient_cookie::PublicSuffixList) of any type.
///
/// Built by
/// [`ClientBuilder::cookie_jar`](crate::ClientBuilder::cookie_jar) from
/// whatever list the caller's jar was carrying; a caller
/// never needs to name it to configure one, only to name the jar's type
/// when reading it back.
#[cfg(feature = "cookies")]
pub struct AnyList(
    Box<dyn hclient_cookie::PublicSuffixList + Send>, // send-bound-exception: amendment-C12
);

#[cfg(feature = "cookies")]
impl AnyList {
    pub fn new<P>(list: P) -> Self
    where
        P: hclient_cookie::PublicSuffixList + Send + 'static, // send-bound-exception: amendment-C12
    {
        Self(Box::new(list))
    }
}

/// Hand-written because a trait object has no `Debug`, and the alternative
/// — a `Debug` supertrait on `PublicSuffixList` — would charge every
/// implementor of a sans-io seam for this crate's `#[derive(Debug)]`.
#[cfg(feature = "cookies")]
impl std::fmt::Debug for AnyList {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnyList")
            .field("has_list", &self.0.has_list())
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "cookies")]
impl hclient_cookie::PublicSuffixList for AnyList {
    fn is_public_suffix(&self, domain: &str) -> bool {
        self.0.is_public_suffix(domain)
    }
    fn has_list(&self) -> bool {
        self.0.has_list()
    }
}

/// A [`CacheStore`](hclient_cache::CacheStore) of any type.
///
/// [`AnyList`]'s counterpart, built by
/// [`ClientBuilder::cache`](crate::ClientBuilder::cache) from whatever
/// store the caller's cache was over.
#[cfg(feature = "cache")]
pub struct AnyStore(
    Box<dyn hclient_cache::CacheStore + Send>, // send-bound-exception: amendment-C12
);

#[cfg(feature = "cache")]
impl AnyStore {
    pub fn new<S>(store: S) -> Self
    where
        S: hclient_cache::CacheStore + Send + 'static, // send-bound-exception: amendment-C12
    {
        Self(Box::new(store))
    }
}

#[cfg(feature = "cache")]
impl std::fmt::Debug for AnyStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnyStore")
            .field("len", &self.0.len())
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "cache")]
impl hclient_cache::CacheStore for AnyStore {
    fn get(&self, key: &hclient_cache::Key) -> Vec<hclient_cache::StoredResponse> {
        self.0.get(key)
    }
    fn put(&mut self, key: &hclient_cache::Key, entry: hclient_cache::StoredResponse) {
        self.0.put(key, entry);
    }
    fn remove(&mut self, key: &hclient_cache::Key, selector: &hclient_cache::Selector) {
        self.0.remove(key, selector);
    }
    fn invalidate(&mut self, key: &hclient_cache::Key) {
        self.0.invalidate(key);
    }
    fn len(&self) -> usize {
        self.0.len()
    }
    fn clear(&mut self) {
        self.0.clear();
    }
}
