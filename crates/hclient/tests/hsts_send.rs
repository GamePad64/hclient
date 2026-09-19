//! `hsts::InMemory`'s futures are `Send` **by inference**, and a store
//! that is not stays usable outside a `Client`.
//!
//! The pair is the assertion. A boxed `dyn Future` with no `+ Send` —
//! the shape this wrapper shipped with for one commit — passes neither:
//! it removes the property rather than hiding it, so `ClientBuilder::hsts`
//! refused `Hsts::new()` although every future underneath it was `Send`.
#![cfg(feature = "hsts")]

use hclient::hsts::{Entry, HstsStore, InMemory};

fn assert_send<T: Send>(_: T) {}

#[test]
fn the_default_stores_futures_are_send_without_the_seam_declaring_it() {
    let s = InMemory::default();
    let domains = ["com.example".to_owned()];

    assert_send(s.get(&domains));
    assert_send(s.put(Entry::new(
        "example.com",
        std::time::SystemTime::UNIX_EPOCH,
        false,
    )));
    assert_send(s.remove("example.com"));
    assert_send(s.clear());
}

/// The control, and the reason the seam declares no `Send`: a byte store
/// holding an `Rc` is still an `HstsStore`. It cannot back a `Client` —
/// that is `ClientBuilder::hsts`'s bound, checked at the use site — but
/// nothing below refuses it.
#[test]
fn a_single_threaded_byte_store_is_still_usable_here() {
    use hclient_core::kv::KeyValueStore;
    use std::future::{Ready, ready};
    use std::time::SystemTime;

    #[derive(Default)]
    struct Rcish(#[allow(dead_code, reason = "held to make the type !Send")] std::rc::Rc<u8>);

    impl KeyValueStore for Rcish {
        type Instant = SystemTime;
        type Get<'a> = Ready<Vec<Vec<u8>>>;
        type GetMany<'a> = Ready<Vec<Vec<Vec<u8>>>>;
        type Done<'a> = Ready<()>;

        fn get<'a>(&'a self, _: &'a str, _: &'a str, _: SystemTime) -> Self::Get<'a> {
            ready(vec![])
        }
        fn get_many(&self, _: &str, keys: Vec<String>, _: SystemTime) -> Self::GetMany<'_> {
            ready(keys.iter().map(|_| vec![]).collect())
        }
        fn put(
            &self,
            _: &str,
            _: String,
            _: Vec<u8>,
            _: Option<SystemTime>,
            _: SystemTime,
        ) -> Self::Done<'_> {
            ready(())
        }
        fn remove(&self, _: &str, _: String) -> Self::Done<'_> {
            ready(())
        }
        fn clear<'a>(&'a self, _: &'a str) -> Self::Done<'a> {
            ready(())
        }
    }

    let s = hclient::hsts::KvStore::new(Rcish::default());
    let got = futures_executor::block_on(s.get(&["com.example".to_owned()]));
    assert!(got.is_empty());
}
