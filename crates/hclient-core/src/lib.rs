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

/// The client identity a request asks to be presented, by a name the
/// caller invented.
///
/// A request extension, `RequireVersion`'s shape and for its reason: a
/// per-request choice the transport reads. **A label and never a
/// credential** — extensions reach `Transport::execute` and are readable
/// by any transport, including one this workspace did not write, which is
/// why digest's password travels as an argument instead.
///
/// What the name resolves to is the TLS backend's business, and that is
/// the only thing that can be the same on Windows, macOS, PKCS#11 and
/// Android at once: a certificate has no representation all four share,
/// and a store query is four different queries. See
/// `docs/mtls-design.md`.
///
/// A backend that does not know the name **refuses**; it does not connect
/// with its default identity.
///
/// `Cow<'static, str>` rather than an `Arc<str>`: a label is almost always
/// a literal, and `Cow::Borrowed` makes that case cost **nothing** — no
/// allocation and no refcount — where `Arc::from(&str)` allocates every
/// time. A computed label is `Cow::Owned` and pays a `String` clone per
/// hop, which is bounded by the redirect limit and is a few bytes.
///
/// **The field is private and the representation is not promised.** It
/// was an `Arc<str>` for a day; a label is a name and a caller has no
/// business knowing what holds it. [`Self::name`] is the whole of the
/// read side, and `Clone` is what a pool key needs — which is why the
/// key holds this type rather than the string inside it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ClientIdentity(Cow<'static, str>);

impl ClientIdentity {
    #[must_use]
    pub fn new(name: impl Into<Cow<'static, str>>) -> Self {
        Self(name.into())
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.0
    }
}

pub use body::{MAX_REWIND_DEPTH, Reduced, RequestBody, RetryKind, RewindFactory};
pub use caps::{
    AllowEarlyData, CancelSupport, Capabilities, DecompressionSupport, EarlyDataSupport,
    RedirectSupport, RequireVersion, ReuseSupport, TimeoutSupport, Timeouts, TlsSupport,
    check_version,
};
pub use error::{
    Error, ErrorKind, Phase, RewindTooDeep, UnsupportedCapability, VersionNotAvailable,
};
pub use host::bare_host;
use std::borrow::Cow;
