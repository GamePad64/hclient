//! The cookie jar's public suffix list and the cache's store, erased so
//! that neither becomes a type parameter on [`Client`](crate::Client).
//!
//! # Why erased at all, when the seams are already generic
//!
//! `crate::cookie::CookieJar<P>` and `crate::cache::HttpCache<S>` are
//! generic, and until this module existed `hclient` accepted only their
//! defaulted forms — so a caller who wanted a disk-backed cache, a store
//! shared between processes, or a jar over
//! [`NoList`](crate::cookie::NoList) could reach it in `hclient-cache` or
//! `hclient-cookie` and not through the facade. The seam existed one crate
//! down and was unreachable one crate up.
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
//! `hclient_core::unversioned::erased`.
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
//! whole API — which is what lets [`crate::ClientBuilder::cookie_jar`](crate::Client::
//! cookies) and [`Client::cache`](crate::Client::cache) keep handing back
//! a guard onto the real thing rather than onto a narrowed trait object.

#[cfg(any(feature = "cookies", feature = "cache"))]
use std::fmt::Debug;

/// A [`PublicSuffixList`](crate::cookie::PublicSuffixList) of any type.
///
/// Built by
/// [`ClientBuilder::cookie_jar`](crate::ClientBuilder::cookie_jar) from
/// whatever list the caller's jar was carrying; a caller
/// never needs to name it to configure one, only to name the jar's type
/// when reading it back.
#[cfg(feature = "cookies")]
pub struct AnyList(
    Box<dyn crate::cookie::PublicSuffixList + Send>, // send-bound-exception: amendment-C12
);

#[cfg(feature = "cookies")]
impl AnyList {
    pub fn new<P>(list: P) -> Self
    where
        P: crate::cookie::PublicSuffixList + Send + 'static, // send-bound-exception: amendment-C12
    {
        Self(Box::new(list))
    }
}

/// Hand-written because a trait object has no `Debug`, and the alternative
/// — a `Debug` supertrait on `PublicSuffixList` — would charge every
/// implementor of a sans-io seam for this crate's `#[derive(Debug)]`.
#[cfg(feature = "cookies")]
impl Debug for AnyList {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnyList")
            .field("has_list", &self.0.has_list())
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "cookies")]
impl crate::cookie::PublicSuffixList for AnyList {
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
/// [`BoxedTransport`](hclient_core::unversioned::BoxedTransport)'s one
/// crate over, down to the blanket impl: **a store author writes
/// nothing**, and the boxing happens where the type is still concrete, so
/// `Send` is inferred rather than proved.
///
/// The box is the price of erasure and it is paid once per operation, on
/// a path that is already about to touch a `HashMap` or a socket.
#[cfg(feature = "cache")]
trait BoxedCacheStore {
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
impl<S> BoxedCacheStore for S
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
/// [`AnyList`]'s counterpart, built by
/// [`ClientBuilder::cache`](crate::ClientBuilder::cache) from whatever
/// store the caller's cache was over.
#[cfg(feature = "cache")]
pub struct AnyStore(
    Box<dyn BoxedCacheStore + Send + Sync>, // send-bound-exception: amendment-C12
);

#[cfg(feature = "cache")]
impl AnyStore {
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
impl Debug for AnyStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnyStore").finish_non_exhaustive()
    }
}

#[cfg(feature = "cache")]
impl crate::cache::CacheStore for AnyStore {
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
