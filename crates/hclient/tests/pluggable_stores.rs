//! The cookie jar's list and the cache's store, reaching through the
//! facade.
//!
//! Both seams have always existed one crate down — `CookieJar<P>` and
//! `HttpCache<S>` are generic — and until `AnyList`/`AnyStore` they
//! stopped at `ClientBuilder`, which took only the defaulted forms. So
//! what is asserted here is never *that the seam works* (each crate tests
//! its own) but that a value the caller supplied is the one the client
//! actually used.
//!
//! Which means neither test may look at the store or the list directly:
//! the first counts writes into a store the test still holds a handle to,
//! and the second reads a **behavioural** difference between two lists off
//! the wire, because a jar that ignored the supplied list would still
//! store and send cookies.
#![cfg(all(feature = "test-util", not(target_family = "wasm")))]

use hclient::Client;
use hclient::mock::MockTransport;
#[cfg(any(feature = "cookies", feature = "cache"))]
use std::sync::Arc;
#[cfg(any(feature = "cookies", feature = "cache"))]
use std::sync::atomic::Ordering;

/// A store that counts what is written into it and otherwise is
/// `MemoryStore`. The counter is the observer: it lives outside the
/// client, so a client that quietly used a store of its own would leave it
/// at zero.
#[cfg(feature = "cache")]
/// **No longer `Clone`, because `MemoryStore` is not.** The store's
/// methods take `&self` now, so it holds its own `Mutex` and a clone would
/// have been a second, independent cache wearing the same name.
#[derive(Debug, Default)]
struct CountingStore {
    inner: hclient::cache::MemoryStore,
    puts: Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(feature = "cache")]
/// **A decorator written from outside the crate, which is the point of
/// this file** — and the asynchronous seam costs it nothing beyond naming
/// the inner store's future types, because it forwards rather than waits.
impl hclient::cache::CacheStore for CountingStore {
    type Get<'a> = <hclient::cache::MemoryStore as hclient::cache::CacheStore>::Get<'a>;
    type Done<'a> = <hclient::cache::MemoryStore as hclient::cache::CacheStore>::Done<'a>;
    type Len<'a> = <hclient::cache::MemoryStore as hclient::cache::CacheStore>::Len<'a>;

    fn get<'a>(&'a self, key: &'a hclient::cache::Key) -> Self::Get<'a> {
        self.inner.get(key)
    }
    fn put<'a>(
        &'a self,
        key: &'a hclient::cache::Key,
        entry: hclient::cache::StoredResponse,
    ) -> Self::Done<'a> {
        self.puts.fetch_add(1, Ordering::Relaxed);
        self.inner.put(key, entry)
    }
    fn remove<'a>(
        &'a self,
        key: &'a hclient::cache::Key,
        selector: &'a hclient::cache::Selector,
    ) -> Self::Done<'a> {
        self.inner.remove(key, selector)
    }
    fn invalidate<'a>(&'a self, key: &'a hclient::cache::Key) -> Self::Done<'a> {
        self.inner.invalidate(key)
    }
    fn len(&self) -> Self::Len<'_> {
        self.inner.len()
    }
    fn clear(&self) -> Self::Done<'_> {
        self.inner.clear()
    }
}

/// **A store of the caller's own is the store the client writes to**, and
/// the one it reads back from — two requests, one reaching the transport.
#[cfg(feature = "cache")]
#[test]
fn the_callers_own_store_is_the_one_the_client_uses() {
    let puts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let store = CountingStore {
        inner: hclient::cache::MemoryStore::new(),
        puts: Arc::clone(&puts),
    };

    let t = MockTransport::new();
    for _ in 0..2 {
        t.push_response(
            http::Response::builder()
                .status(200)
                .header("cache-control", "max-age=600")
                .body("cached")
                .unwrap(),
        );
    }
    let c = Client::builder(t)
        .cache(hclient::cache::HttpCache::with_store(store))
        .build()
        .expect("build");

    for _ in 0..2 {
        let body = futures_executor::block_on(async {
            c.get("https://a/x").send().await?.collect().await
        })
        .expect("the request completes");
        assert_eq!(body.bytes(), &b"cached"[..]);
    }

    assert_eq!(
        puts.load(Ordering::Relaxed),
        1,
        "the response was written into the store the test is holding"
    );
    assert_eq!(
        c.transport_as::<MockTransport>()
            .expect("the mock")
            .requests()
            .len(),
        1,
        "and read back out of it: a second request would mean the store was write-only"
    );
}

#[cfg(feature = "cookies")]
fn set_cookie_then_ask(
    jar: hclient::cookie::CookieJar<impl hclient::cookie::PublicSuffixList + Send + 'static>,
) -> Option<String> {
    let t = MockTransport::new();
    t.push_response(
        http::Response::builder()
            .status(200)
            .header("set-cookie", "sid=abc; Domain=example.com")
            .body("")
            .unwrap(),
    );
    t.push_response(http::Response::builder().status(200).body("").unwrap());
    let c = Client::builder(t).cookie_jar(jar).build().expect("build");
    futures_executor::block_on(c.get("https://www.example.com/one").send()).expect("one");
    futures_executor::block_on(c.get("https://api.example.com/two").send()).expect("two");
    c.transport_as::<MockTransport>()
        .expect("the mock")
        .requests()[1]
        .headers
        .get("cookie")
        .map(|v| v.to_str().unwrap().to_owned())
}

/// **The list the caller supplied is the one that decides**, read off the
/// wire rather than out of the jar.
///
/// `Domain=example.com` is a registrable name to the compiled-in list, so
/// the cookie travels to a sibling host; to [`NoList`], which answers
/// *"public suffix"* for everything, the same attribute is a refusal. Two
/// clients, one differing argument, opposite bytes on the second request.
///
/// The pair is the assertion. A client that ignored the list entirely and
/// always used the builtin would pass the first half; one that dropped
/// every `Domain=` would pass the second.
///
/// `public-suffix` as well as `cookies`, and the second gate is younger
/// than the first: until the jar was a module of this crate there was no
/// way to build it without the list, so this configuration could not be
/// reached. The first half of the pair *is* the compiled-in list, so with
/// the flag off both clients answer alike and the assertion is about
/// nothing.
#[cfg(all(feature = "cookies", feature = "public-suffix"))]
#[test]
fn the_callers_own_public_suffix_list_is_the_one_that_decides() {
    assert_eq!(
        set_cookie_then_ask(hclient::cookie::CookieJar::new()).as_deref(),
        Some("sid=abc"),
        "example.com is registrable to the compiled-in list, so a sibling host gets the cookie"
    );
    assert_eq!(
        set_cookie_then_ask(hclient::cookie::CookieJar::with_public_suffix_list(
            hclient::cookie::NoList
        )),
        None,
        "and to NoList every name is a public suffix, so the Domain attribute is refused"
    );
}

/// **Erasing did not cost the client its auto traits**, which is the whole
/// subject of amendment C12's `Send` bound: without it a `Client` in a
/// build with either feature compiled in would stop crossing a
/// `tokio::spawn` whether or not anything was configured.
#[test]
fn a_client_with_both_configured_still_crosses_a_spawn() {
    fn assert_send_sync<T: Send + Sync>(_: &T) {}
    let c = Client::builder(MockTransport::new());
    #[cfg(feature = "cookies")]
    let c = c.cookie_jar(hclient::cookie::CookieJar::new());
    #[cfg(feature = "cache")]
    let c = c.cache(hclient::cache::HttpCache::new());
    let c = c.build().expect("build");
    assert_send_sync(&c);
}

// ── A store that actually waits ─────────────────────────────────────────

/// A store whose every answer suspends once before it arrives.
///
/// **This is the store the asynchronous seam exists for**, and nothing
/// else in this workspace is one: `MemoryStore` answers `Ready`, so a
/// suite built on it alone would pass for a seam that had never stopped
/// being synchronous. What a disk or a Redis store does is suspend, and
/// this does exactly that and nothing more.
#[cfg(feature = "cache")]
#[derive(Debug, Default)]
struct SuspendingStore {
    inner: hclient::cache::MemoryStore,
    suspends: Arc<std::sync::atomic::AtomicUsize>,
}

/// Answers `Pending` the first time it is polled, waking immediately, and
/// the wrapped value the second.
#[cfg(feature = "cache")]
struct Once<T> {
    value: Option<T>,
    polled: bool,
    counter: Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(feature = "cache")]
impl<T: Unpin> std::future::Future for Once<T> {
    type Output = T;
    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<T> {
        if self.polled {
            let v = self.value.take().expect("polled after completion");
            return std::task::Poll::Ready(v);
        }
        self.polled = true;
        self.counter.fetch_add(1, Ordering::Relaxed);
        cx.waker().wake_by_ref();
        std::task::Poll::Pending
    }
}

/// `MemoryStore` answers `Ready`, so one poll with a noop waker takes its
/// value out — `block_on` cannot be used here, because this runs *inside*
/// the executor the test itself is driving.
#[cfg(feature = "cache")]
fn now<T>(f: impl std::future::Future<Output = T>) -> T {
    let mut f = std::pin::pin!(f);
    match f
        .as_mut()
        .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
    {
        std::task::Poll::Ready(v) => v,
        std::task::Poll::Pending => unreachable!("MemoryStore never suspends"),
    }
}

#[cfg(feature = "cache")]
impl SuspendingStore {
    fn once<T>(&self, value: T) -> Once<T> {
        Once {
            value: Some(value),
            polled: false,
            counter: Arc::clone(&self.suspends),
        }
    }
}

#[cfg(feature = "cache")]
impl hclient::cache::CacheStore for SuspendingStore {
    type Get<'a> = Once<Vec<hclient::cache::StoredResponse>>;
    type Done<'a> = Once<()>;
    type Len<'a> = Once<usize>;

    fn get<'a>(&'a self, key: &'a hclient::cache::Key) -> Self::Get<'a> {
        self.once(now(self.inner.get(key)))
    }
    fn put<'a>(
        &'a self,
        key: &'a hclient::cache::Key,
        entry: hclient::cache::StoredResponse,
    ) -> Self::Done<'a> {
        now(self.inner.put(key, entry));
        self.once(())
    }
    fn remove<'a>(
        &'a self,
        key: &'a hclient::cache::Key,
        selector: &'a hclient::cache::Selector,
    ) -> Self::Done<'a> {
        now(self.inner.remove(key, selector));
        self.once(())
    }
    fn invalidate<'a>(&'a self, key: &'a hclient::cache::Key) -> Self::Done<'a> {
        now(self.inner.invalidate(key));
        self.once(())
    }
    fn len(&self) -> Self::Len<'_> {
        self.once(now(self.inner.len()))
    }
    fn clear(&self) -> Self::Done<'_> {
        now(self.inner.clear());
        self.once(())
    }
}

/// **A store that suspends is served from, which is what the whole change
/// is for.**
///
/// Two requests, one reaching the transport: the second is answered out
/// of a store that answered `Pending` on the way in and on the way out.
/// With a synchronous seam this store could not exist at all — it would
/// have had to block the executor or not be written.
///
/// The suspend counter is the half that makes it a test rather than a
/// re-run of the `MemoryStore` one: without it, a store that quietly
/// stopped suspending would pass.
#[cfg(feature = "cache")]
#[test]
fn a_store_that_suspends_is_written_to_and_served_from() {
    let suspends = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let store = SuspendingStore {
        inner: hclient::cache::MemoryStore::new(),
        suspends: Arc::clone(&suspends),
    };

    let t = MockTransport::new();
    t.push_response(
        http::Response::builder()
            .header("cache-control", "max-age=600")
            .body("cached")
            .expect("response"),
    );
    let c = Client::builder(t.clone())
        .cache(hclient::cache::HttpCache::with_store(store))
        .build()
        .expect("client");

    let first = futures_executor::block_on(async {
        c.get("https://e.test/x").send().await?.collect().await
    })
    .expect("first");
    assert_eq!(first.text().expect("utf-8"), "cached");

    let second = futures_executor::block_on(async {
        c.get("https://e.test/x").send().await?.collect().await
    })
    .expect("second");
    assert_eq!(second.text().expect("utf-8"), "cached");

    assert_eq!(
        t.requests().len(),
        1,
        "the second answer came from the store"
    );
    assert!(
        suspends.load(Ordering::Relaxed) >= 3,
        "the store suspended on the way in and out; it suspended {} times",
        suspends.load(Ordering::Relaxed)
    );
}
