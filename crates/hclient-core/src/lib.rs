#![no_std]
//! Plugin contract for hclient: the traits a backend, a runtime or a
//! resolver implements, and the vocabulary types they exchange.
//!
//! # The `Send` rule
//!
//! **The seam traits declare no `Send`/`Sync` bounds.** `Transport`,
//! `Timer` and the middleware traits leave Send-ness to auto-traits
//! through `impl Future`, because a bound declared where the type is
//! abstract is forced on every backend — including ones that cannot meet
//! it, such as a single-threaded embedded runtime whose connect future
//! holds a `RefCell`.
//!
//! Bounds do appear in three places, and each is a value a caller hands
//! over rather than a demand on an implementor:
//!
//! - [`Error`]'s source is `Send + Sync`, or a client could not build an
//!   error from a backend's at all.
//! - [`RequestBody`]'s rewind factory and streaming arm.
//! - [`unversioned::erased`]'s two aliases, which a facade writes at its
//!   own use site to put a transport behind an `Arc`. It is **not a
//!   seam**: a blanket impl covers every `Transport`, so no backend
//!   implements or is taxed by it, and one that cannot meet the bound is
//!   refused at a constructor rather than at a trait.
//!
//! Every such site carries a `send-bound-exception` marker naming the
//! amendment that admits it, and
//! `scripts/no-send-or-sync-in-the-core-surface.sh` fails closed on one
//! that does not. `grep` is therefore the authority on which sites exist;
//! this list says what kind they are.
#![forbid(unsafe_code)]

//! # Why `#![no_std]`, unconditionally
//!
//! Not behind a feature, for the reason `bytes` and rustls are not either:
//! a `std`/`no_std` split is two preludes, and the difference shows up as
//! imports that are "unused" in one of them and load-bearing in the other
//! — a warning that teaches a reader to delete the wrong line. Nothing
//! here wants anything from `std`: measured, there is not one genuinely
//! std-only item in this crate's library code, which is what
//! [`Timer::Instant`](unversioned::Timer::Instant) being an associated
//! type bought long before anyone asked for a device.
//!
//! `alloc` is mandatory and stays so. `#[cfg(test)] extern crate std` is
//! how the test modules keep their prelude.
//!
//! **This is not yet a bare-metal build**, and the missing piece is not
//! ours: `http` 1.x carries a `compile_error!` for `no_std`. The cross
//! check that proves this crate compiles for `riscv32imac-unknown-none-elf`
//! needs a patched `http`, so it is a spike rather than a CI job.
//!
//! **That does not leave the attribute unguarded, which this comment said
//! for one commit and was wrong about.** `#![no_std]` takes `std` out of
//! *this crate's* extern prelude, so a `std::` path added here is
//! `E0433: unresolved module or unlinked crate` on an ordinary host
//! `cargo check` — no cross-build and no patched `http` involved. Checked
//! in the failing direction rather than assumed, by adding a
//! `std::collections::HashMap` to `host.rs` and watching it fail. What is
//! genuinely unguarded is the other half: nothing here would notice a
//! *dependency* that cannot go `no_std`, and only the cross build would.
//! `docs/no-std.md` has both halves.

extern crate alloc;
#[cfg(test)]
extern crate std;

/// What this crate's test modules lose to `#![no_std]`: `ToString` lives
/// in `alloc`'s prelude and not in `core`'s.
///
/// Deliberately **not** `use std::prelude::v1::*` — that glob re-imports
/// `panic!`, which is already in `core`'s prelude, and the ambiguity is a
/// `-D warnings` error today and a hard error later
/// (rust-lang/rust#147319). Naming what is used has no such edge, and the
/// list is short because these tests barely allocate.
#[cfg(test)]
mod test_prelude {
    pub use alloc::string::ToString;
}

mod body;
mod caps;
mod error;
mod host;
pub mod unversioned;

pub use body::{RequestBody, RetryKind, RewindFactory};
pub use caps::{
    AllowEarlyData, CancelSupport, Capabilities, DecompressionSupport, EarlyDataSupport,
    RedirectSupport, RequireVersion, ReuseSupport, TimeoutSupport, Timeouts, TlsSupport,
    UnsupportedCapability, VersionNotAvailable, check_version,
};
pub use error::{Error, ErrorKind, Phase};
pub use host::bare_host;
