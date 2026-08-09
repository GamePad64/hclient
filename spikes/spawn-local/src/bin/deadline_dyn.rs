//! Negative control 5: **the other half of the acceptance doc's claim is
//! true — for the shape it had available.** Must NOT compile.
//!
//! `cargo build --bin deadline_dyn --features must-fail`
//!
//! `docs/v02-acceptance.md` says storing the sleep "would make *every*
//! response body `!Send`". With `Pin<Box<dyn Future>>` — the only way to
//! store an RPITIT's future — that is exactly right, and this bin is the
//! proof. What changes it is not boxing less, it is boxing a **named**
//! type: `Pin<Box<Tm::Sleep>>` in `--bin deadline_sleep` is `Send`,
//! because a box around a concrete type is transparent to auto traits.

use std::pin::Pin;

struct DeadlineDyn<B> {
    inner: Option<B>,
    /// The only field that differs from `deadline_sleep.rs`'s `B`.
    sleep: Pin<Box<dyn Future<Output = ()>>>,
}

fn assert_send<T: Send>() {}

fn main() {
    // The control: with a named sleep the same struct is `Send`.
    assert_send::<DeadlineNamed<u8>>();
    // And with `dyn` it is not.
    assert_send::<DeadlineDyn<u8>>();
}

struct DeadlineNamed<B> {
    inner: Option<B>,
    sleep: Pin<Box<tokio::time::Sleep>>,
}
