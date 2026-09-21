//! An `Alt-Svc` store over the byte seam must be `Clone`, and both copies
//! must see **one** memory.
//!
//! `Native` clones its `AltSvcCache` into its own routing half — two
//! sites in `lib.rs` — so a store that copied instead of sharing would
//! give the two halves separate memories, and an advertisement noted by
//! one would be invisible to the other. That is not a slow client, it is
//! a client that routes one request over QUIC and the next over TCP for
//! no reason it can report.
//!
//! Written because the first version of this wrapper was **not** `Clone`
//! at all: `hclient_core::kv::MemoryStore` had no `Clone` impl, so
//! `AltSvcCache<InMemory>::clone()` did not compile — and nothing said
//! so, because no test named the property until this one.
#![cfg(feature = "http3")]

use hclient_native::altsvc::{AltSvcCache, AltSvcStore, Entry, InMemory, Origin};
use std::time::SystemTime;

fn at(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs)
}

#[test]
fn the_store_clones_and_both_copies_share_one_memory() {
    let store = InMemory::default();
    let twin = store.clone();

    let origin = Origin::new("example.com", 443);
    futures_executor::block_on(store.put(&origin, Entry::new(at(100), true)));

    assert_eq!(
        futures_executor::block_on(twin.get(&origin)),
        Some(Entry::new(at(100), true)),
        "the clone sees what the original stored"
    );

    // And the other direction, so this cannot pass for a `Clone` that
    // happens to copy a non-empty map.
    let second = Origin::new("other.test", 443);
    futures_executor::block_on(twin.remove(&origin));
    futures_executor::block_on(twin.put(&second, Entry::new(at(200), false)));

    assert!(futures_executor::block_on(store.get(&origin)).is_none());
    assert_eq!(
        futures_executor::block_on(store.get(&second)),
        Some(Entry::new(at(200), false))
    );
}

/// And the cache over it clones, which is what `Native` actually does.
#[test]
fn a_cache_over_the_byte_store_is_clonable() {
    let cache: AltSvcCache<InMemory> = AltSvcCache::with_store(InMemory::default());
    let _twin = cache.clone();
}
