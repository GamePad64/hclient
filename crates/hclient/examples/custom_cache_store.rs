//! A cache store of your own — the six methods, written out.
//!
//! [`hclient::cache::CacheStore`] is the seam for putting cached responses
//! somewhere other than memory: on disk, in Redis, in `moka::future`. It
//! asks **six** methods and gives no defaults, which is the largest
//! obligation this crate places on an outside implementor — and until this
//! file there was no implementor outside it at all. An example is a
//! consumer, and *writing a consumer is a different measurement from
//! writing a test*: the tests here were written beside the trait by
//! someone who knew where the doors were.
//!
//! ```text
//! cargo run -p hclient --example custom_cache_store --features test-util,cache
//! ```
//!
//! # Two things the shape decides for you
//!
//! **Every method is a future and every one takes `&self`.** The second
//! follows from the first: a store that awaits cannot be held behind a
//! `&mut` across that await, and a store that is remote is already shared
//! by everyone talking to it. So the synchronisation is yours — the
//! `Mutex` below is this store's, not the client's, and `Client` holds no
//! lock of its own while asking.
//!
//! **The futures are associated types rather than `async fn`.** `Client`
//! boxes its cache `Send + Sync`, and an `async fn` in a trait produces a
//! future with no name, which cannot be bounded. Naming it is what lets
//! each implementor answer for its own auto traits — the store below is
//! synchronous, so [`std::future::Ready`] costs it no allocation and no
//! suspension.
//!
//! A real remote store returns its own future type here and does the I/O
//! in it; nothing about the seam changes.
#![cfg(all(feature = "test-util", feature = "cache"))]

use std::collections::HashMap;
use std::future::Ready;
use std::sync::Mutex;

use hclient::Client;
use hclient::cache::{CacheStore, HttpCache, Key, Selector, StoredResponse};
use hclient::mock::MockTransport;

/// A store that counts what it was asked, so this example can show that
/// the client really goes through it rather than around it.
#[derive(Default)]
struct CountingStore {
    /// One `Mutex` around the whole map. A store that reached the network
    /// would hold a connection pool here instead, and the point is the
    /// same: `&self` means the choice is the store's.
    entries: Mutex<HashMap<Key, Vec<StoredResponse>>>,
    gets: Mutex<usize>,
    puts: Mutex<usize>,
}

impl CacheStore for CountingStore {
    // Synchronous work, so the answer is ready before it is awaited.
    type Get<'a> = Ready<Vec<StoredResponse>>;
    type Done<'a> = Ready<()>;
    type Len<'a> = Ready<usize>;

    /// Every entry stored under this key. **A key can hold more than one**
    /// — that is `Vary`: two responses to the same URL that differ by a
    /// request header are two entries, and the client picks between them
    /// with the [`Selector`] each carries.
    fn get<'a>(&'a self, key: &'a Key) -> Self::Get<'a> {
        *self.gets.lock().unwrap() += 1;
        let entries = self.entries.lock().unwrap();
        std::future::ready(entries.get(key).cloned().unwrap_or_default())
    }

    /// Store one. Replacing an entry with the same `Selector` is the
    /// store's job, not the client's — otherwise a `Vary` variant would
    /// accumulate a copy per request.
    fn put<'a>(&'a self, key: &'a Key, entry: StoredResponse) -> Self::Done<'a> {
        *self.puts.lock().unwrap() += 1;
        let mut entries = self.entries.lock().unwrap();
        let slot = entries.entry(key.clone()).or_default();
        slot.retain(|e| e.selector() != entry.selector());
        slot.push(entry);
        std::future::ready(())
    }

    /// One variant, named by its selector.
    fn remove<'a>(&'a self, key: &'a Key, selector: &'a Selector) -> Self::Done<'a> {
        let mut entries = self.entries.lock().unwrap();
        if let Some(slot) = entries.get_mut(key) {
            slot.retain(|e| e.selector() != selector);
        }
        std::future::ready(())
    }

    /// Every variant under one key. This is what an unsafe method on the
    /// same URL calls — RFC 9111 §4.4 — so it takes the key and not a
    /// selector.
    fn invalidate<'a>(&'a self, key: &'a Key) -> Self::Done<'a> {
        self.entries.lock().unwrap().remove(key);
        std::future::ready(())
    }

    fn len(&self) -> Self::Len<'_> {
        let n = self.entries.lock().unwrap().values().map(Vec::len).sum();
        std::future::ready(n)
    }

    fn clear(&self) -> Self::Done<'_> {
        self.entries.lock().unwrap().clear();
        std::future::ready(())
    }
}

fn main() {
    let transport = MockTransport::new();
    // One cacheable answer, and a second the client must never need.
    for _ in 0..2 {
        transport.push_response(
            http::Response::builder()
                .status(200)
                .header("cache-control", "max-age=60")
                .body("cached body")
                .unwrap(),
        );
    }

    let client = Client::builder(transport.clone())
        .base_url("https://example.test".parse().unwrap())
        .cache(HttpCache::with_store(CountingStore::default()))
        .build()
        .expect("the mock owns no cache of its own");

    let fetch = || {
        futures_executor::block_on(async {
            client.get("/thing").send().await?.collect().await?.text()
        })
        .expect("scripted")
    };

    assert_eq!(fetch(), "cached body");
    assert_eq!(fetch(), "cached body");

    // **The second answer never reached the transport.** That is the whole
    // claim: one request on the wire for two calls, because the store
    // answered the second.
    assert_eq!(
        transport.requests().len(),
        1,
        "the second call was served from the store"
    );
    assert_eq!(transport.queued(), 1, "the spare response went unused");

    println!("requests on the wire: {}", transport.requests().len());
    println!("responses left unused: {}", transport.queued());
}
