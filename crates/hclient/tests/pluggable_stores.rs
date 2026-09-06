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

// The gate matches its **caller**'s, which is `all(cookies,
// public-suffix)`: `cookies` alone built this function with nothing
// calling it, so a `--no-default-features --features cookies` build
// warned about dead code where `--all-features` saw nothing.
#[cfg(all(feature = "cookies", feature = "public-suffix"))]
fn set_cookie_then_ask(
    jar: hclient::cookie::CookieJar<impl hclient::cookie::PublicSuffixList + Send + Sync + 'static>,
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
///
/// `any(cache, cookies)`, not `cache`: both suspending stores below are
/// built on this, and gating it on the cache alone made a
/// `--no-default-features --features cookies,test-util` build fail while
/// `--all-features` stayed green — which is `test-no-default`'s whole
/// job.
#[cfg(any(feature = "cache", feature = "cookies"))]
struct Once<T> {
    value: Option<T>,
    polled: bool,
    counter: Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(any(feature = "cache", feature = "cookies"))]
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
#[cfg(any(feature = "cache", feature = "cookies"))]
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

// ── The same, for cookies ───────────────────────────────────────────────

/// A cookie store whose every answer suspends once before it arrives.
///
/// [`SuspendingStore`]'s twin, for the seam the jar took after the cache
/// did, and it exists for the same reason: `cookie::MemoryStore` answers
/// `Ready`, so a suite built on it alone would pass for a seam that had
/// never stopped being synchronous. What a jar on disk or in
/// `localStorage` does is suspend.
#[cfg(feature = "cookies")]
#[derive(Debug, Default)]
struct SuspendingCookieStore {
    inner: hclient::cookie::MemoryStore,
    suspends: Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(feature = "cookies")]
impl SuspendingCookieStore {
    fn once<T>(&self, value: T) -> Once<T> {
        Once {
            value: Some(value),
            polled: false,
            counter: Arc::clone(&self.suspends),
        }
    }
}

#[cfg(feature = "cookies")]
impl hclient::cookie::CookieStore for SuspendingCookieStore {
    type Get<'a> = Once<Vec<hclient::cookie::Cookie>>;
    type Done<'a> = Once<()>;
    type Len<'a> = Once<usize>;

    fn get<'a>(&'a self, domains: &'a [String]) -> Self::Get<'a> {
        self.once(now(self.inner.get(domains)))
    }
    fn all(&self) -> Self::Get<'_> {
        self.once(now(self.inner.all()))
    }
    fn put(&self, cookie: hclient::cookie::Cookie, at: web_time::SystemTime) -> Self::Done<'_> {
        now(self.inner.put(cookie, at));
        self.once(())
    }
    fn remove<'a>(&'a self, key: &'a hclient::cookie::CookieKey) -> Self::Done<'a> {
        now(self.inner.remove(key));
        self.once(())
    }
    fn touch<'a>(
        &'a self,
        keys: &'a [hclient::cookie::CookieKey],
        at: web_time::SystemTime,
    ) -> Self::Done<'a> {
        now(self.inner.touch(keys, at));
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

/// **A cookie store that suspends is written to and read from**, which is
/// what the jar's seam is for.
///
/// Two requests: the first is answered with a `Set-Cookie` the store must
/// keep, the second must carry it back — and the store answered `Pending`
/// on both journeys. With a synchronous seam this store could not exist
/// at all.
///
/// The suspend counter is what makes this a test of the seam rather than
/// a second run of the `MemoryStore` one: without it, a store that
/// quietly stopped suspending would pass.
#[cfg(feature = "cookies")]
#[test]
fn a_cookie_store_that_suspends_is_written_to_and_read_from() {
    use hclient::cookie::{BuiltinList, CookieJar};

    let suspends = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let store = SuspendingCookieStore {
        inner: hclient::cookie::MemoryStore::new(),
        suspends: Arc::clone(&suspends),
    };

    let t = MockTransport::new();
    t.push_response(
        http::Response::builder()
            .status(200)
            // Host-only, with no `Domain` attribute: this test is about the
            // store and not the public suffix list, and a `Domain` would
            // make it silently vacuous in a build without
            // `public-suffix`, where every name is a public suffix.
            .header("set-cookie", "sid=abc")
            .body("")
            .expect("response"),
    );
    t.push_response(
        http::Response::builder()
            .status(200)
            .body("")
            .expect("response"),
    );
    let c = Client::builder(t.clone())
        .cookie_jar(CookieJar::with_store(BuiltinList, store))
        .build()
        .expect("client");

    futures_executor::block_on(c.get("https://www.example.com/one").send()).expect("one");
    futures_executor::block_on(c.get("https://www.example.com/two").send()).expect("two");

    let sent = t.requests();
    assert_eq!(
        sent[1].headers.get("cookie").map(|v| v.to_str().unwrap()),
        Some("sid=abc"),
        "the second request carried what the suspending store kept"
    );
    assert!(
        suspends.load(Ordering::Relaxed) >= 4,
        "the store suspended on the way in and out; it suspended {} times",
        suspends.load(Ordering::Relaxed)
    );
}

// ── A store that keeps no `StoredResponse` at all ───────────────────────

/// What an entry looks like once it has left the process.
///
/// Deliberately plain data — no type of this crate's survives in it — so
/// that a hit served out of [`ReloadingStore`] is evidence the round trip
/// is **exact**, selector included, rather than evidence that a `Clone`
/// was handed back.
#[cfg(feature = "cache")]
#[derive(Clone)]
struct OnDisk {
    status: u16,
    version: &'static str,
    headers: Vec<(String, Vec<u8>)>,
    body: Vec<u8>,
    /// `None` is §4.1's *absent*, and keeping it apart from an empty
    /// value is the half of the selector a `HashMap` loses.
    selector: Vec<(String, Option<Vec<u8>>)>,
    requested_at: web_time::SystemTime,
    received_at: web_time::SystemTime,
}

/// **The store the cache seam advertises and could not serve.** It writes
/// every entry down to [`OnDisk`] and rebuilds one on every read, so
/// nothing of `hclient`'s is held between `put` and `get`.
///
/// This is the cache's counterpart to `SuspendingCookieStore` above and
/// answers a different question: that one asks whether the seam really
/// waits, this one whether an entry can make the journey out and back at
/// all. Until `StoredResponse::new` and `Selector::from_fields` were
/// public it could not be written outside this crate — checked from a
/// scratch consumer rather than assumed, which is how the gap was found.
#[cfg(feature = "cache")]
#[derive(Default)]
struct ReloadingStore(std::sync::Mutex<Vec<(hclient::cache::Key, OnDisk)>>);

#[cfg(feature = "cache")]
fn write_down(e: &hclient::cache::StoredResponse) -> OnDisk {
    OnDisk {
        status: e.status().as_u16(),
        version: match e.version() {
            http::Version::HTTP_2 => "h2",
            http::Version::HTTP_3 => "h3",
            _ => "http/1.1",
        },
        headers: e
            .headers()
            .iter()
            .map(|(n, v)| (n.to_string(), v.as_bytes().to_vec()))
            .collect(),
        body: e.body().to_vec(),
        selector: e
            .selector()
            .fields()
            .map(|(n, v)| (n.to_string(), v.map(|v| v.as_bytes().to_vec())))
            .collect(),
        requested_at: e.requested_at(),
        received_at: e.received_at(),
    }
}

#[cfg(feature = "cache")]
fn read_back(d: &OnDisk) -> hclient::cache::StoredResponse {
    let mut headers = http::HeaderMap::new();
    for (n, v) in &d.headers {
        headers.append(
            n.parse::<http::HeaderName>().expect("a name we wrote"),
            http::HeaderValue::from_bytes(v).expect("a value we wrote"),
        );
    }
    let selector = hclient::cache::Selector::from_fields(d.selector.iter().map(|(n, v)| {
        (
            n.parse::<http::HeaderName>().expect("a name we wrote"),
            v.as_ref()
                .map(|v| http::HeaderValue::from_bytes(v).expect("a value we wrote")),
        )
    }));
    hclient::cache::StoredResponse::new(
        http::StatusCode::from_u16(d.status).expect("a status we wrote"),
        match d.version {
            "h2" => http::Version::HTTP_2,
            "h3" => http::Version::HTTP_3,
            _ => http::Version::HTTP_11,
        },
        headers,
        bytes::Bytes::from(d.body.clone()),
        selector,
        d.requested_at,
        d.received_at,
    )
}

#[cfg(feature = "cache")]
impl hclient::cache::CacheStore for ReloadingStore {
    type Get<'a> = std::future::Ready<Vec<hclient::cache::StoredResponse>>;
    type Done<'a> = std::future::Ready<()>;
    type Len<'a> = std::future::Ready<usize>;

    fn get<'a>(&'a self, key: &'a hclient::cache::Key) -> Self::Get<'a> {
        let held = self.0.lock().expect("lock");
        std::future::ready(
            held.iter()
                .filter(|(k, _)| k == key)
                .map(|(_, d)| read_back(d))
                .collect(),
        )
    }
    fn put<'a>(
        &'a self,
        key: &'a hclient::cache::Key,
        entry: hclient::cache::StoredResponse,
    ) -> Self::Done<'a> {
        let mut held = self.0.lock().expect("lock");
        let down = write_down(&entry);
        held.retain(|(k, d)| k != key || d.selector != down.selector);
        held.push((key.clone(), down));
        std::future::ready(())
    }
    fn remove<'a>(
        &'a self,
        key: &'a hclient::cache::Key,
        selector: &'a hclient::cache::Selector,
    ) -> Self::Done<'a> {
        let rows: Vec<(String, Option<Vec<u8>>)> = selector
            .fields()
            .map(|(n, v)| (n.to_string(), v.map(|v| v.as_bytes().to_vec())))
            .collect();
        self.0
            .lock()
            .expect("lock")
            .retain(|(k, d)| k != key || d.selector != rows);
        std::future::ready(())
    }
    fn invalidate<'a>(&'a self, key: &'a hclient::cache::Key) -> Self::Done<'a> {
        self.0.lock().expect("lock").retain(|(k, _)| k != key);
        std::future::ready(())
    }
    fn len(&self) -> Self::Len<'_> {
        std::future::ready(self.0.lock().expect("lock").len())
    }
    fn clear(&self) -> Self::Done<'_> {
        self.0.lock().expect("lock").clear();
        std::future::ready(())
    }
}

/// **An entry that left the process and came back answers the request.**
///
/// Two requests, one reaching the transport. Between them the response
/// existed only as `OnDisk`, so the hit is evidence that the status, the
/// version, the headers, the body, both §4.2.3 timestamps *and* the
/// selector all survived — a selector that came back in the wrong order
/// would compare unequal and the second request would go to the wire.
#[cfg(feature = "cache")]
#[test]
fn an_entry_written_down_and_rebuilt_still_answers() {
    let store = ReloadingStore::default();

    let t = MockTransport::new();
    t.push_response(
        http::Response::builder()
            .header("cache-control", "max-age=600")
            .header("vary", "accept-encoding")
            .body("reloaded")
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
    assert_eq!(first.text().expect("utf-8"), "reloaded");

    let second = futures_executor::block_on(async {
        c.get("https://e.test/x").send().await?.collect().await
    })
    .expect("second");
    assert_eq!(second.text().expect("utf-8"), "reloaded");

    assert_eq!(
        t.requests().len(),
        1,
        "the second answer was rebuilt from plain data, not re-fetched"
    );
}

// ── what one response costs a store that is not in memory ───────────────

/// A cookie store that records every operation it was asked for.
///
/// **The counter is the point.** `MemoryStore` answers
/// [`std::future::Ready`], so the difference between one read and six is
/// invisible through it — and invisible is exactly what it was until a
/// store outside this workspace logged its own statements. What a store
/// on the far side of a file, a socket or a `sqlite` handle pays is
/// round trips, and round trips are what this counts.
#[cfg(feature = "cookies")]
#[derive(Default)]
struct TalkativeStore {
    inner: hclient::cookie::MemoryStore,
    reads: Arc<std::sync::atomic::AtomicUsize>,
    writes: Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(feature = "cookies")]
impl hclient::cookie::CookieStore for TalkativeStore {
    type Get<'a> = <hclient::cookie::MemoryStore as hclient::cookie::CookieStore>::Get<'a>;
    type Done<'a> = <hclient::cookie::MemoryStore as hclient::cookie::CookieStore>::Done<'a>;
    type Len<'a> = <hclient::cookie::MemoryStore as hclient::cookie::CookieStore>::Len<'a>;

    fn get<'a>(&'a self, domains: &'a [String]) -> Self::Get<'a> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.inner.get(domains)
    }
    fn all(&self) -> Self::Get<'_> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.inner.all()
    }
    fn put(&self, cookie: hclient::cookie::Cookie, at: web_time::SystemTime) -> Self::Done<'_> {
        self.writes.fetch_add(1, Ordering::Relaxed);
        self.inner.put(cookie, at)
    }
    fn remove<'a>(&'a self, key: &'a hclient::cookie::CookieKey) -> Self::Done<'a> {
        self.inner.remove(key)
    }
    fn touch<'a>(
        &'a self,
        keys: &'a [hclient::cookie::CookieKey],
        at: web_time::SystemTime,
    ) -> Self::Done<'a> {
        self.inner.touch(keys, at)
    }
    fn len(&self) -> Self::Len<'_> {
        self.inner.len()
    }
    fn clear(&self) -> Self::Done<'_> {
        self.inner.clear()
    }
}

/// **A response carrying many `Set-Cookie` headers asks the store once.**
///
/// It asked once *per header* until this was measured: every header went
/// through `CookieJar::store`, and each of those read what the jar already
/// held so §5.7's replacement could keep the old cookie's `seq`. Five
/// headers cost six reads. They cost two now — one for the request's
/// `Cookie` header, one for the whole batch — and the write count is
/// unchanged, because a write per cookie is what storing five cookies is.
///
/// The bound is stated as *at most* rather than as today's number: what
/// must not come back is the growth with the header count, and a test
/// pinned to an exact figure gets relaxed rather than read.
#[cfg(feature = "cookies")]
#[test]
fn a_response_full_of_set_cookie_headers_asks_the_store_once() {
    let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let writes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let store = TalkativeStore {
        inner: hclient::cookie::MemoryStore::new(),
        reads: Arc::clone(&reads),
        writes: Arc::clone(&writes),
    };

    let t = MockTransport::new();
    let mut b = http::Response::builder().status(200);
    for (n, v) in [("a", "1"), ("b", "2"), ("c", "3"), ("d", "4"), ("e", "5")] {
        b = b.header("set-cookie", format!("{n}={v}; Max-Age=600"));
    }
    t.push_response(b.body("").expect("response"));

    let c = Client::builder(t)
        .cookie_jar(hclient::cookie::CookieJar::with_store(
            hclient::cookie::BuiltinList,
            store,
        ))
        .build()
        .expect("client");
    futures_executor::block_on(c.get("https://e.test/x").send()).expect("request");

    assert_eq!(writes.load(Ordering::Relaxed), 5, "one write per cookie");
    assert!(
        reads.load(Ordering::Relaxed) <= 2,
        "five Set-Cookie headers cost {} reads; the batch is meant to cost one",
        reads.load(Ordering::Relaxed)
    );
}
