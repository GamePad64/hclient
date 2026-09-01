//! The one refusal a name conversion can be.
//!
//! This crate has a single fallible entry point — [`crate::domain_to_ascii`]
//! — so everything here is one call's answer, and what the two variants
//! have in common is that neither is a failure to *reach* anything: no
//! socket is opened and no query is sent, so a refusal is always a
//! statement about the name or about the build, never about the network.
//! Which of those two it is, is exactly the distinction the type exists
//! to draw.
//!
//! [`IdnError`] is re-exported at the crate root, where it has always
//! been, so no consumer's `use` line moves.

/// What went wrong turning a domain into its A-label form.
///
/// Two variants because `hclient-proto` distinguishes two things a caller
/// can do something about: [`NotAnIdn`](IdnError::NotAnIdn) maps to
/// `UriError::NotAnIdn` ("this name is not usable"), and
/// [`NoImplementation`](IdnError::NoImplementation) maps to
/// `UriError::NonAsciiHost` ("this build cannot convert; send the A-label
/// yourself"). Collapsing them would tell a user to fix their domain when
/// the actual problem is the build they are running.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum IdnError {
    /// UTS 46 rejected the name: a disallowed code point, a bidi or
    /// joiner-context violation, invalid punycode under an `xn--` label,
    /// or an ASCII character the WHATWG URL Standard forbids in a domain.
    #[error("`{domain}` is not a usable internationalised domain name: UTS 46 rejected it")]
    NotAnIdn {
        /// The domain, as given.
        domain: String,
    },
    /// This build has an implementation it will not trust: the platform's
    /// own UTS 46 was absent, or it answered the acceptance probe
    /// differently from `idna`. The name itself may be perfectly valid.
    ///
    /// **The message named two features that no longer exist**, and the
    /// first run of the Android backend is what surfaced it: every call
    /// came back advising a reader to *enable the `bundled` feature*,
    /// which had been replaced by `idna` and which would not have been
    /// the cause anyway. A message is a claim like any other and goes
    /// stale the same way.
    #[error(
        "`{domain}` needs IDN conversion and this build has none it will trust: the platform's \
         own UTS 46 was not found, or it disagreed with `idna` on the acceptance probe. Build \
         with `--features idna` to carry the Unicode tables instead, or supply the host in its \
         A-label form — `münchen.de` is written `xn--mnchen-3ya.de`"
    )]
    NoImplementation {
        /// The domain, as given.
        domain: String,
    },
}
