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
#[derive(Debug, Clone, Default)]
struct CountingStore {
    inner: hclient::cache::MemoryStore,
    puts: Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(feature = "cache")]
impl hclient::cache::CacheStore for CountingStore {
    fn get(&self, key: &hclient::cache::Key) -> Vec<hclient::cache::StoredResponse> {
        self.inner.get(key)
    }
    fn put(&mut self, key: &hclient::cache::Key, entry: hclient::cache::StoredResponse) {
        self.puts.fetch_add(1, Ordering::Relaxed);
        self.inner.put(key, entry);
    }
    fn remove(&mut self, key: &hclient::cache::Key, selector: &hclient::cache::Selector) {
        self.inner.remove(key, selector);
    }
    fn invalidate(&mut self, key: &hclient::cache::Key) {
        self.inner.invalidate(key);
    }
    fn len(&self) -> usize {
        self.inner.len()
    }
    fn clear(&mut self) {
        self.inner.clear();
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
