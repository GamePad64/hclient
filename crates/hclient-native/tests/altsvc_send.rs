//! `altsvc::InMemory`'s futures are `Send` **by inference**, so it can be
//! installed with `Native::alt_svc_store`.
//!
//! Written before the wrapper was wired anywhere, because this is exactly
//! the check `hclient::hsts::kv` did not have: a boxed `dyn Future` with
//! no `+ Send` compiles, passes every unit test, and is refused at the
//! setter in another crate — so the defect surfaces only where the store
//! is actually used.
#![cfg(feature = "http3")]

use hclient_native::altsvc::{AltSvcStore, Entry, InMemory, Origin};
use std::time::SystemTime;

fn assert_send<T: Send>(_: T) {}

#[test]
fn the_default_stores_futures_are_send_without_the_seam_declaring_it() {
    let s = InMemory::default();
    let o = Origin::new("example.com", 443);

    assert_send(s.get(&o));
    assert_send(s.put(&o, Entry::new(SystemTime::UNIX_EPOCH, false)));
    assert_send(s.remove(&o));
    assert_send(s.retain_persistent());
}

/// And it satisfies the setter's own bounds, which is the property the
/// assertions above only approximate: `for<'a> S::Get<'a>: Send` is a
/// higher-ranked claim, where `assert_send` fixes one lifetime.
#[test]
fn it_meets_the_setters_higher_ranked_bounds() {
    fn like_the_setter<S>(_: S)
    where
        S: AltSvcStore + Send + Sync + 'static,
        for<'a> S::Get<'a>: Send,
        for<'a> S::Done<'a>: Send,
    {
    }

    like_the_setter(InMemory::default());
}
