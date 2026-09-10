//! Every seam a caller hands to [`Client`](crate::Client) by value,
//! erased so that none of them becomes a type parameter on it.
//!
//! Four today — the cookie jar's public suffix list and its store, the
//! cache's store, and the HSTS policy set's — and **the sentence here
//! used to name two**, which is the count-in-prose defect this workspace
//! records against itself elsewhere: nothing forces a list in a doc
//! comment to grow when the module does. The types below are the list
//! that cannot go stale.
//!
//! # Why erased at all, when the seams are already generic
//!
//! `crate::cookie::CookieJar<P, S>`, `crate::cache::HttpCache<S>` and
//! `crate::hsts::Hsts<S>` are generic, and until this module existed
//! `hclient` accepted only their defaulted forms — so a caller who wanted
//! a disk-backed cache, a store shared between processes, or a jar over
//! `NoList` could reach it in the module and not through the facade. The
//! seam existed a layer down and was unreachable a layer up.
//!
//! # Why not a type parameter on `Client`
//!
//! Two reasons, and the second is the one that decides it.
//!
//! `cached.rs` had already written down the first: a recording body holds
//! a handle to the cache, so `S` on the cache is `S` on the public
//! [`ClientBody`](crate::body::ClientBody) alias — **the arity of a public type
//! alias would change with a feature nobody in the graph asked for**, and
//! Cargo unifies features, so no crate could rely on either arity.
//!
//! The second is that a defaulted parameter needs a default *type*, and
//! `hclient-cookie` and `hclient-cache` are **optional dependencies**.
//! A `Client<.., P = crate::cookie::BuiltinList, S = ..>` would name two
//! types that do not exist in a build without those features, so the
//! declaration would have to be forked four ways. Erasure has no such
//! problem: the field is inside the same `#[cfg]` as the feature, and there
//! is no parameter to default when it is off.
//!
//! **The same argument has since been applied to the transport and the
//! clock**, which is why this module's reasoning outlived the shape it was
//! written about: `Client` was forked once more, for `DefaultTransport`,
//! and that fork is gone with the parameters. See
//! `hclient_core::erased`.
//!
//! # What it costs, said plainly
//!
//! `Send` bounds on the opt-in setters and nowhere else — spec amendment
//! C12. That is `Native::multiplexed()`'s shape exactly: no signature
//! anyone else meets acquires a bound, and a caller who hands in a
//! `!Send` list or store gets `E0277` on the line where they asked.
//!
//! The bounds are not new in substance. `Inner`'s own doc has said since
//! the jar landed that a `Client` is meant to cross a `tokio::spawn`, and
//! `BuiltinList` and both `MemoryStore`s are `Send` — so what this states
//! is the property the concrete types already had. Erasing without it
//! would make every `Client` in a build with the feature compiled in
//! `!Send`, configured jar or not, which is the feature-unification
//! hazard the paragraph above is about.
//!
//! **[`BoxSuffixList`] asks for `Sync` as well now, and a lock is what used to
//! supply it.** The jar sat in a `Mutex` in `Inner`, and `Mutex<T>` is
//! `Sync` whenever `T` is `Send` — so the list's `Sync` was being
//! manufactured by a lock rather than held by the list. Taking the lock
//! away when the jar took a store (`cookie::CookieStore`) took that with
//! it, and the bound moved to where the property actually has to hold. A
//! list that is genuinely `!Sync` was never usable from two threads; what
//! changed is that it now says so at the setter instead of working until
//! someone shared the client.
//!
//! Both wrappers implement the seam they erase, so a `CookieJar<BoxSuffixList>`
//! and an `HttpCache<BoxCacheStore>` are ordinary jars and caches with their
//! whole API — which is what lets `ClientBuilder::cookie_jar`(crate::Client::
//! cookies) and `Client::cache` keep handing back
//! a guard onto the real thing rather than onto a narrowed trait object.

#[cfg(any(feature = "cookies", feature = "cache", feature = "hsts"))]
use std::fmt::Debug;

/// A [`PublicSuffixList`](crate::cookie::PublicSuffixList) of any type.
///
/// Built by
/// [`ClientBuilder::cookie_jar`](crate::ClientBuilder::cookie_jar) from
/// whatever list the caller's jar was carrying; a caller
/// never needs to name it to configure one, only to name the jar's type
/// when reading it back.
#[cfg(feature = "cookies")]
pub struct BoxSuffixList(
    Box<dyn crate::cookie::PublicSuffixList + Send + Sync>, // send-bound-exception: amendment-C12
);

#[cfg(feature = "cookies")]
impl BoxSuffixList {
    pub fn new<P>(list: P) -> Self
    where
        P: crate::cookie::PublicSuffixList + Send + Sync + 'static, // send-bound-exception: amendment-C12
    {
        Self(Box::new(list))
    }
}

/// Hand-written because a trait object has no `Debug`, and the alternative
/// — a `Debug` supertrait on `PublicSuffixList` — would charge every
/// implementor of a sans-io seam for this crate's `#[derive(Debug)]`.
#[cfg(feature = "cookies")]
impl Debug for BoxSuffixList {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoxSuffixList")
            .field("has_list", &self.0.has_list())
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "cookies")]
impl crate::cookie::PublicSuffixList for BoxSuffixList {
    fn is_public_suffix(&self, domain: &str) -> bool {
        self.0.is_public_suffix(domain)
    }
    fn has_list(&self) -> bool {
        self.0.has_list()
    }
}

/// The object-safe half of [`CacheStore`](crate::cache::CacheStore).
///
/// The seam names its futures as associated types, which is what lets a
/// single-threaded store answer for itself — and is exactly what makes it
/// not `dyn`-compatible. So there are two traits, and the split is
/// [`BoxTransport`](hclient_core::BoxTransport)'s one
/// crate over, down to the blanket impl: **a store author writes
/// nothing**, and the boxing happens where the type is still concrete, so
/// `Send` is inferred rather than proved.
///
/// The box is the price of erasure and it is paid once per operation, on
/// a path that is already about to touch a `HashMap` or a socket.
#[cfg(feature = "cache")]
trait DynCacheStore {
    fn get_boxed<'a>(
        &'a self,
        key: &'a crate::cache::Key,
    ) -> futures_core::future::BoxFuture<'a, Vec<crate::cache::StoredResponse>>;
    fn put_boxed<'a>(
        &'a self,
        key: &'a crate::cache::Key,
        entry: crate::cache::StoredResponse,
    ) -> futures_core::future::BoxFuture<'a, ()>;
    fn remove_boxed<'a>(
        &'a self,
        key: &'a crate::cache::Key,
        selector: &'a crate::cache::Selector,
    ) -> futures_core::future::BoxFuture<'a, ()>;
    fn invalidate_boxed<'a>(
        &'a self,
        key: &'a crate::cache::Key,
    ) -> futures_core::future::BoxFuture<'a, ()>;
    fn len_boxed(&self) -> futures_core::future::BoxFuture<'_, usize>;
    fn clear_boxed(&self) -> futures_core::future::BoxFuture<'_, ()>;
}

#[cfg(feature = "cache")]
impl<S> DynCacheStore for S
where
    S: crate::cache::CacheStore,
    for<'a> S::Get<'a>: Send,  // send-bound-exception: amendment-C12
    for<'a> S::Done<'a>: Send, // send-bound-exception: amendment-C12
    for<'a> S::Len<'a>: Send,  // send-bound-exception: amendment-C12
{
    fn get_boxed<'a>(
        &'a self,
        key: &'a crate::cache::Key,
    ) -> futures_core::future::BoxFuture<'a, Vec<crate::cache::StoredResponse>> {
        Box::pin(self.get(key))
    }
    fn put_boxed<'a>(
        &'a self,
        key: &'a crate::cache::Key,
        entry: crate::cache::StoredResponse,
    ) -> futures_core::future::BoxFuture<'a, ()> {
        Box::pin(self.put(key, entry))
    }
    fn remove_boxed<'a>(
        &'a self,
        key: &'a crate::cache::Key,
        selector: &'a crate::cache::Selector,
    ) -> futures_core::future::BoxFuture<'a, ()> {
        Box::pin(self.remove(key, selector))
    }
    fn invalidate_boxed<'a>(
        &'a self,
        key: &'a crate::cache::Key,
    ) -> futures_core::future::BoxFuture<'a, ()> {
        Box::pin(self.invalidate(key))
    }
    fn len_boxed(&self) -> futures_core::future::BoxFuture<'_, usize> {
        Box::pin(self.len())
    }
    fn clear_boxed(&self) -> futures_core::future::BoxFuture<'_, ()> {
        Box::pin(self.clear())
    }
}

/// A [`CacheStore`](crate::cache::CacheStore) of any type.
///
/// [`BoxSuffixList`]'s counterpart, built by
/// [`ClientBuilder::cache`](crate::ClientBuilder::cache) from whatever
/// store the caller's cache was over.
#[cfg(feature = "cache")]
pub struct BoxCacheStore(
    Box<dyn DynCacheStore + Send + Sync>, // send-bound-exception: amendment-C12
);

#[cfg(feature = "cache")]
impl BoxCacheStore {
    pub fn new<S>(store: S) -> Self
    where
        S: crate::cache::CacheStore + Send + Sync + 'static, // send-bound-exception: amendment-C12
        for<'a> S::Get<'a>: Send,                            // send-bound-exception: amendment-C12
        for<'a> S::Done<'a>: Send,                           // send-bound-exception: amendment-C12
        for<'a> S::Len<'a>: Send,                            // send-bound-exception: amendment-C12
    {
        Self(Box::new(store))
    }
}

#[cfg(feature = "cache")]
/// **The count is gone from the `Debug`, and that is the seam's doing.**
/// `len` is a future now, and a `Debug` cannot await one — a store on
/// disk or in Redis would have to be asked over the network to print
/// itself. Printing a number a remote store might not agree with is worse
/// than printing none.
impl Debug for BoxCacheStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoxCacheStore").finish_non_exhaustive()
    }
}

#[cfg(feature = "cache")]
impl crate::cache::CacheStore for BoxCacheStore {
    type Get<'a> = futures_core::future::BoxFuture<'a, Vec<crate::cache::StoredResponse>>;
    type Done<'a> = futures_core::future::BoxFuture<'a, ()>;
    type Len<'a> = futures_core::future::BoxFuture<'a, usize>;

    fn get<'a>(&'a self, key: &'a crate::cache::Key) -> Self::Get<'a> {
        self.0.get_boxed(key)
    }
    fn put<'a>(
        &'a self,
        key: &'a crate::cache::Key,
        entry: crate::cache::StoredResponse,
    ) -> Self::Done<'a> {
        self.0.put_boxed(key, entry)
    }
    fn remove<'a>(
        &'a self,
        key: &'a crate::cache::Key,
        selector: &'a crate::cache::Selector,
    ) -> Self::Done<'a> {
        self.0.remove_boxed(key, selector)
    }
    fn invalidate<'a>(&'a self, key: &'a crate::cache::Key) -> Self::Done<'a> {
        self.0.invalidate_boxed(key)
    }
    fn len(&self) -> Self::Len<'_> {
        self.0.len_boxed()
    }
    fn clear(&self) -> Self::Done<'_> {
        self.0.clear_boxed()
    }
}

/// The object-safe half of [`CookieStore`](crate::cookie::CookieStore).
///
/// [`BoxCacheStore`]'s twin, for the same reason and with the same
/// split: the seam names its futures as associated types, which is what
/// lets a single-threaded store answer for itself and is exactly what
/// makes it not `dyn`-compatible. The blanket impl means **a store author
/// writes nothing**, and the boxing happens where the type is still
/// concrete, so `Send` is inferred rather than proved.
#[cfg(feature = "cookies")]
trait DynCookieStore {
    fn get_boxed<'a>(
        &'a self,
        domains: &'a [String],
    ) -> futures_core::future::BoxFuture<'a, Vec<crate::cookie::Cookie>>;
    fn all_boxed(&self) -> futures_core::future::BoxFuture<'_, Vec<crate::cookie::Cookie>>;
    fn put_boxed(
        &self,
        cookie: crate::cookie::Cookie,
        now: web_time::SystemTime,
    ) -> futures_core::future::BoxFuture<'_, ()>;
    fn remove_boxed<'a>(
        &'a self,
        key: &'a crate::cookie::CookieKey,
    ) -> futures_core::future::BoxFuture<'a, ()>;
    fn touch_boxed<'a>(
        &'a self,
        keys: &'a [crate::cookie::CookieKey],
        now: web_time::SystemTime,
    ) -> futures_core::future::BoxFuture<'a, ()>;
    fn len_boxed(&self) -> futures_core::future::BoxFuture<'_, usize>;
    fn clear_boxed(&self) -> futures_core::future::BoxFuture<'_, ()>;
}

#[cfg(feature = "cookies")]
impl<S> DynCookieStore for S
where
    S: crate::cookie::CookieStore,
    for<'a> S::Get<'a>: Send,  // send-bound-exception: amendment-C12
    for<'a> S::Done<'a>: Send, // send-bound-exception: amendment-C12
    for<'a> S::Len<'a>: Send,  // send-bound-exception: amendment-C12
{
    fn get_boxed<'a>(
        &'a self,
        domains: &'a [String],
    ) -> futures_core::future::BoxFuture<'a, Vec<crate::cookie::Cookie>> {
        Box::pin(self.get(domains))
    }
    fn all_boxed(&self) -> futures_core::future::BoxFuture<'_, Vec<crate::cookie::Cookie>> {
        Box::pin(self.all())
    }
    fn put_boxed(
        &self,
        cookie: crate::cookie::Cookie,
        now: web_time::SystemTime,
    ) -> futures_core::future::BoxFuture<'_, ()> {
        Box::pin(self.put(cookie, now))
    }
    fn remove_boxed<'a>(
        &'a self,
        key: &'a crate::cookie::CookieKey,
    ) -> futures_core::future::BoxFuture<'a, ()> {
        Box::pin(self.remove(key))
    }
    fn touch_boxed<'a>(
        &'a self,
        keys: &'a [crate::cookie::CookieKey],
        now: web_time::SystemTime,
    ) -> futures_core::future::BoxFuture<'a, ()> {
        Box::pin(self.touch(keys, now))
    }
    fn len_boxed(&self) -> futures_core::future::BoxFuture<'_, usize> {
        Box::pin(self.len())
    }
    fn clear_boxed(&self) -> futures_core::future::BoxFuture<'_, ()> {
        Box::pin(self.clear())
    }
}

/// A [`CookieStore`](crate::cookie::CookieStore) of any type.
///
/// [`BoxCacheStore`]'s counterpart, built by
/// [`ClientBuilder::cookie_jar`](crate::ClientBuilder::cookie_jar) from
/// whatever store the caller's jar was over — so a `CookieJar<BoxSuffixList,
/// BoxCookieStore>` is an ordinary jar with both of its seams erased.
#[cfg(feature = "cookies")]
pub struct BoxCookieStore(
    Box<dyn DynCookieStore + Send + Sync>, // send-bound-exception: amendment-C12
);

#[cfg(feature = "cookies")]
impl BoxCookieStore {
    pub fn new<S>(store: S) -> Self
    where
        S: crate::cookie::CookieStore + Send + Sync + 'static, // send-bound-exception: amendment-C12
        for<'a> S::Get<'a>: Send,  // send-bound-exception: amendment-C12
        for<'a> S::Done<'a>: Send, // send-bound-exception: amendment-C12
        for<'a> S::Len<'a>: Send,  // send-bound-exception: amendment-C12
    {
        Self(Box::new(store))
    }
}

#[cfg(feature = "cookies")]
/// **The count is gone from the `Debug`, and it is the seam's doing** —
/// `BoxCacheStore`'s sentence verbatim, for the same reason: `len` is a future
/// now, and a `Debug` cannot await one.
impl Debug for BoxCookieStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoxCookieStore").finish_non_exhaustive()
    }
}

#[cfg(feature = "cookies")]
impl crate::cookie::CookieStore for BoxCookieStore {
    type Get<'a> = futures_core::future::BoxFuture<'a, Vec<crate::cookie::Cookie>>;
    type Done<'a> = futures_core::future::BoxFuture<'a, ()>;
    type Len<'a> = futures_core::future::BoxFuture<'a, usize>;

    fn get<'a>(&'a self, domains: &'a [String]) -> Self::Get<'a> {
        self.0.get_boxed(domains)
    }
    fn all(&self) -> Self::Get<'_> {
        self.0.all_boxed()
    }
    fn put(&self, cookie: crate::cookie::Cookie, now: web_time::SystemTime) -> Self::Done<'_> {
        self.0.put_boxed(cookie, now)
    }
    fn remove<'a>(&'a self, key: &'a crate::cookie::CookieKey) -> Self::Done<'a> {
        self.0.remove_boxed(key)
    }
    fn touch<'a>(
        &'a self,
        keys: &'a [crate::cookie::CookieKey],
        now: web_time::SystemTime,
    ) -> Self::Done<'a> {
        self.0.touch_boxed(keys, now)
    }
    fn len(&self) -> Self::Len<'_> {
        self.0.len_boxed()
    }
    fn clear(&self) -> Self::Done<'_> {
        self.0.clear_boxed()
    }
}

/// The object-safe half of [`HstsStore`](crate::hsts::HstsStore).
///
/// [`BoxCookieStore`]'s twin, for the same reason and with the same
/// split: the seam names its futures as associated types, which is what
/// lets a single-threaded store answer for itself and is exactly what
/// makes it not `dyn`-compatible. The blanket impl means **a store author
/// writes nothing**, and the boxing happens where the type is still
/// concrete, so `Send` is inferred rather than proved.
#[cfg(feature = "hsts")]
trait DynHstsStore {
    fn get_boxed<'a>(
        &'a self,
        domains: &'a [String],
    ) -> futures_core::future::BoxFuture<'a, Vec<crate::hsts::Entry>>;
    fn put_boxed(&self, entry: crate::hsts::Entry) -> futures_core::future::BoxFuture<'_, ()>;
    fn remove_boxed<'a>(&'a self, domain: &'a str) -> futures_core::future::BoxFuture<'a, ()>;
    fn clear_boxed(&self) -> futures_core::future::BoxFuture<'_, ()>;
}

#[cfg(feature = "hsts")]
impl<S> DynHstsStore for S
where
    S: crate::hsts::HstsStore,
    for<'a> S::Get<'a>: Send,  // send-bound-exception: amendment-C12
    for<'a> S::Done<'a>: Send, // send-bound-exception: amendment-C12
{
    fn get_boxed<'a>(
        &'a self,
        domains: &'a [String],
    ) -> futures_core::future::BoxFuture<'a, Vec<crate::hsts::Entry>> {
        Box::pin(self.get(domains))
    }
    fn put_boxed(&self, entry: crate::hsts::Entry) -> futures_core::future::BoxFuture<'_, ()> {
        Box::pin(self.put(entry))
    }
    fn remove_boxed<'a>(&'a self, domain: &'a str) -> futures_core::future::BoxFuture<'a, ()> {
        Box::pin(self.remove(domain))
    }
    fn clear_boxed(&self) -> futures_core::future::BoxFuture<'_, ()> {
        Box::pin(self.clear())
    }
}

/// An [`HstsStore`](crate::hsts::HstsStore) of any type.
///
/// [`BoxCookieStore`]'s counterpart, built by
/// [`ClientBuilder::hsts`](crate::ClientBuilder::hsts) from whatever store
/// the caller's [`Hsts`](crate::hsts::Hsts) was over — so an
/// `Hsts<BoxHstsStore>` is the ordinary rules with its one seam erased.
#[cfg(feature = "hsts")]
pub struct BoxHstsStore(
    Box<dyn DynHstsStore + Send + Sync>, // send-bound-exception: amendment-C12
);

#[cfg(feature = "hsts")]
impl BoxHstsStore {
    pub fn new<S>(store: S) -> Self
    where
        S: crate::hsts::HstsStore + Send + Sync + 'static, // send-bound-exception: amendment-C12
        for<'a> S::Get<'a>: Send,                          // send-bound-exception: amendment-C12
        for<'a> S::Done<'a>: Send,                         // send-bound-exception: amendment-C12
    {
        Self(Box::new(store))
    }
}

#[cfg(feature = "hsts")]
impl Debug for BoxHstsStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoxHstsStore").finish_non_exhaustive()
    }
}

#[cfg(feature = "hsts")]
impl crate::hsts::HstsStore for BoxHstsStore {
    type Get<'a> = futures_core::future::BoxFuture<'a, Vec<crate::hsts::Entry>>;
    type Done<'a> = futures_core::future::BoxFuture<'a, ()>;

    fn get<'a>(&'a self, domains: &'a [String]) -> Self::Get<'a> {
        self.0.get_boxed(domains)
    }
    fn put(&self, entry: crate::hsts::Entry) -> Self::Done<'_> {
        self.0.put_boxed(entry)
    }
    fn remove<'a>(&'a self, domain: &'a str) -> Self::Done<'a> {
        self.0.remove_boxed(domain)
    }
    fn clear(&self) -> Self::Done<'_> {
        self.0.clear_boxed()
    }
}
