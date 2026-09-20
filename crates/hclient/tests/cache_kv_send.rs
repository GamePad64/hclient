//! `cache::InMemory`'s futures are `Send` **by inference**, so it can be
//! installed with `ClientBuilder::cache`.
//!
//! Written before the wrapper is wired anywhere, because this is the
//! check `hclient::hsts::kv` did not have: a boxed `dyn Future` with no
//! `+ Send` compiles, passes every unit test, and is refused only at the
//! setter in another file.
#![cfg(feature = "cache")]

use hclient::cache::{CacheStore, InMemory, Key, Selector, StoredResponse};
use http::{HeaderMap, Method, StatusCode, Version};
use std::time::SystemTime;

fn assert_send<T: Send>(_: T) {}

fn key() -> Key {
    Key::new(&Method::GET, &"https://example.com/".parse().unwrap()).expect("a cacheable key")
}

fn entry() -> StoredResponse {
    StoredResponse::new(
        StatusCode::OK,
        Version::HTTP_11,
        HeaderMap::new(),
        bytes::Bytes::from_static(b"body"),
        Selector::from_fields([]),
        SystemTime::UNIX_EPOCH,
        SystemTime::UNIX_EPOCH,
    )
}

#[test]
fn the_default_stores_futures_are_send_without_the_seam_declaring_it() {
    let s = InMemory::new(hclient_core::kv::MemoryStore::default());
    let k = key();
    let sel = Selector::from_fields([]);

    assert_send(s.get(&k));
    assert_send(s.put(&k, entry()));
    assert_send(s.remove(&k, &sel));
    assert_send(s.invalidate(&k));
    assert_send(s.len());
    assert_send(s.clear());
}

/// And it meets the setter's own higher-ranked bounds, which the
/// assertions above only approximate.
#[test]
fn it_meets_the_setters_higher_ranked_bounds() {
    fn like_the_setter<S>(_: S)
    where
        S: CacheStore + Send + Sync + 'static,
        for<'a> S::Get<'a>: Send,
        for<'a> S::Done<'a>: Send,
        for<'a> S::Len<'a>: Send,
    {
    }

    like_the_setter(InMemory::new(hclient_core::kv::MemoryStore::default()));
}
