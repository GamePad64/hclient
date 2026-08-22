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
