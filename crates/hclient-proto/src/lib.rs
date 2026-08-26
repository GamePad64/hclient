#![no_std]
//! Pure state machines for hclient's protocol layers.
//!
//! Crate invariant: no `async fn`, no runtime dependency, anywhere. Anything
//! that depends on time takes `now` as a parameter. Enforced in CI.
#![forbid(unsafe_code)]

//! # `#![no_std]`, unconditionally
//!
//! The clockless, IO-less invariant above is most of what this costs:
//! every dependency here already builds `no_std + alloc`, so the manifest
//! work is `default-features = false` and nothing else. One item did not
//! come free — `f64::round()` in `backoff.rs` is a **`std`** method, since
//! `core` carries float arithmetic and none of the float functions. See
//! `docs/no-std.md`, and the note at that call site.
//!
//! `idn` is the one feature a bare-metal build must turn off: it reaches
//! `hclient-idn`, and Unicode tables are the whole flash budget of a part
//! this is for. A non-ASCII host is then `UriError::NonAsciiHost`, which
//! is what that feature's doc already promises.

extern crate alloc;
#[cfg(test)]
extern crate std;

/// What a `#![no_std]` crate's test modules lose: `Vec`, `String` and the
/// two macros are in `std`'s prelude and not in `core`'s.
///
/// Deliberately **not** `use std::prelude::v1::*` — that glob re-imports
/// `panic!`, which is already in `core`'s prelude, and the ambiguity is a
/// `-D warnings` error today and a hard error later
/// (rust-lang/rust#147319). `hclient-core` carries the same module for
/// the same reason, one name long — the difference is that these tests
/// build strings and vectors and those barely allocate.
#[cfg(test)]
mod test_prelude {
    pub use alloc::borrow::ToOwned;
    pub use alloc::string::{String, ToString};
    pub use alloc::vec;
    pub use alloc::vec::Vec;
}

pub mod backoff;
pub mod encode;
pub mod happy_eyeballs;
pub mod redirect;
pub mod sse;
pub mod uri;
