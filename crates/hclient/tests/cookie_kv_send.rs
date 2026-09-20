//! `cookie::InMemory`'s futures are `Send` **by inference**, so it can
//! back a `CookieJar` installed with `ClientBuilder::cookie_jar`.
#![cfg(feature = "cookies")]

use hclient::cookie::{CookieStore, InMemory};

fn assert_send<T: Send>(_: T) {}

#[test]
fn the_default_stores_futures_are_send_without_the_seam_declaring_it() {
    let s = InMemory::new(hclient_core::kv::MemoryStore::default());
    let domains = ["com.example".to_owned()];

    assert_send(s.get(&domains));
    assert_send(s.all());
    assert_send(s.len());
    assert_send(s.clear());
}

/// And it meets the bounds `ClientBuilder::cookie_jar` puts on the
/// store behind a jar, which the assertions above only approximate.
#[test]
fn it_meets_the_setters_higher_ranked_bounds() {
    fn like_the_setter<S>(_: S)
    where
        S: CookieStore + Send + Sync + 'static,
        for<'a> S::Get<'a>: Send,
        for<'a> S::Done<'a>: Send,
        for<'a> S::Len<'a>: Send,
    {
    }

    like_the_setter(InMemory::new(hclient_core::kv::MemoryStore::default()));
}
