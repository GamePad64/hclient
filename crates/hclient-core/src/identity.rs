//! The name a request asks a TLS backend to present a certificate for.

use std::borrow::Cow;

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
