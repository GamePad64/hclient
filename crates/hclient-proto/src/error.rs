//! Three refusals, and none of them is a failure to *do* anything.
//!
//! This crate is sans-io: no socket, no clock, no allocation of anybody
//! else's resources. So every error here is a **verdict on bytes somebody
//! else handed over** — a response head that is not one ([`HeadError`]),
//! a string that is not a URI ([`UriError`]), an SSE event past the limit
//! the caller set ([`SseError`]) — and each is total, in the sense that
//! the same input gives the same verdict on every platform, in any
//! process, with no state behind it.
//!
//! **That is why "incomplete" is not in here.** A head that has not
//! finished arriving is `Ok(None)` from `head::parse_response`, and the
//! module doc there says why: incomplete and malformed are different
//! facts, and a caller that could not tell them apart would either give
//! up on a slow proxy or wait for ever for a broken one. An error in this
//! module is always a decision that more bytes cannot change.
//!
//! Each type is re-exported from the module whose grammar produced it —
//! [`crate::head`], [`crate::uri`], [`crate::sse`] — where it has always
//! been, so no consumer's `use` line moves.

// Maintainer notes (not rendered):
//
// **No winnow trait is implemented for this type**, and that is what
// keeps `winnow` out of this crate's public API. A `ParserError` impl
// sat here until the freeze audit: it is invisible in rustdoc's item
// list and it is public surface all the same, so winnow's next major
// version would have been this crate's. The impl moved onto
// `head::ParseFailure`, a private newtype around this enum, at the cost
// of one `.0` where `parse_response` unwraps it — see there. Every other
// winnow user in this workspace already parsed with `ContextError` and
// exposed none of it; `head` was the one that did not.

/// What the bytes were not.
///
/// Every variant is reachable from a real peer, which is why the parser
/// carries this rather than winnow's `ContextError`: a caller of
/// `hclient-proxy` meets these as the source of a connect failure, and
/// *the proxy sent something that is not an HTTP response* is not an
/// answer anybody can act on.
///
/// No `winnow` trait is implemented for this type, which keeps `winnow`
/// out of this crate's public API.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum HeadError {
    /// The status line is not `HTTP/1.x SP <3 digits> [SP <reason>]`.
    #[error("the status line is not `HTTP/1.x SP <3 digits>`")]
    MalformedStatusLine,
    /// The three digits on the status line are not a valid status code.
    #[error("`{0}` is not a status code")]
    BadStatus(Box<str>),
    /// A header line is not `name: value`.
    #[error("a header line is not `name: value`")]
    MalformedHeader,
    /// A header line's name is not a valid header field name.
    #[error("`{0}` is not a header name")]
    BadHeaderName(Box<str>),
    /// A header line's value is not a valid header field value.
    #[error("the value of `{0}` is not a header value")]
    BadHeaderValue(Box<str>),
    /// A continuation line — RFC 9112 §5.2's obs-fold.
    #[error("obsolete line folding, which a client must reject rather than guess at")]
    ObsFold,
    /// A bare `LF` where the grammar writes `CRLF`.
    #[error("a bare LF line terminator, where the grammar writes CRLF")]
    BareLf,
}

/// Why a string could not be turned into an [`http::Uri`].
///
/// Both host-related variants exist in every build, whichever way the
/// `idn` feature is set: a feature that changes the shape of a public type
/// is not additive, and a caller matching on this enum should not have to
/// be compiled twice to do it. Only which of the two can actually occur
/// changes.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum UriError {
    /// The base is not usable as a base: RFC 3986 §5.2.1 requires an
    /// absolute URI, and there is nothing to resolve a relative reference
    /// against.
    #[error("`{base}` cannot be used as a base URL: a base needs a scheme and an authority")]
    UnusableBase {
        /// The offending base, as written.
        base: String,
    },
    /// The host is not ASCII and **no IDN implementation ran on it**: the
    /// name itself was never judged.
    ///
    /// Two causes, and the message names both because a caller cannot see
    /// which applies. The `idn` feature is off, so there is no
    /// implementation compiled in at all — this is the usual one. Or the
    /// feature is on and `hclient-idn` reported
    /// `IdnError::NoImplementation` (named without a link, because with
    /// the feature off that crate is not in the build to link to), which
    /// on a target whose backend is the platform's own — Windows, Apple —
    /// means the OS did not supply one this process can use; targets that
    /// take the bundled tables always have theirs.
    ///
    /// The way out is the same either way, which is why it is one variant
    /// and not two: send the A-label.
    #[error(
        "`{host}` is not an ASCII host and no IDN implementation in this build converted it: \
         either the `idn` feature of `hclient` is off (it is on by default), or it is on and \
         this machine supplied no UTS 46 implementation `hclient-idn` could use. Supply the \
         host in its A-label form instead — `münchen.de` is written `xn--mnchen-3ya.de`"
    )]
    NonAsciiHost {
        /// The host, as written.
        host: String,
    },
    /// The host is not ASCII and a UTS 46 implementation **ran and refused
    /// it**. Only reachable with the `idn` feature on.
    #[error("`{host}` is not a usable internationalised domain name: UTS 46 rejected it")]
    NotAnIdn {
        /// The host, as written.
        host: String,
    },
    /// Everything `http::Uri` itself rejects, with its own error as the
    /// source.
    #[error("`{uri}` is not a valid URI")]
    NotAUri {
        /// The string that was parsed, after any IDNA conversion.
        uri: String,
        /// What `http::Uri` said about it.
        source: http::uri::InvalidUri,
    },
}

// Maintainer notes (not rendered):
//
// One variant today, and `#[non_exhaustive]` all the same — answer 3 of
// the three this workspace records: it is handed *back* and only read,
// never built by a caller and never translated variant-by-variant into
// somebody else's enum. Checked rather than assumed: the only match on
// it outside this crate is a fuzz target's, and `hclient`'s SSE stream
// carries it as a source rather than mapping it. So a second refusal is
// an addition, where without the attribute it would be a major version
// — which is the reason its two siblings above carry it and the reason
// the odd one out was the oversight rather than the decision.

/// Why an SSE stream cannot be decoded any further.
///
/// The type is `#[non_exhaustive]` even with a single variant today: it
/// is handed *back* and only read, never built by a caller and never
/// translated variant-by-variant into somebody else's enum, so a second
/// refusal is an addition rather than a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SseError {
    /// The raw event size limit was exceeded. Fatal and **not retried**.
    #[error("SSE event exceeds {limit} bytes")]
    EventTooLarge {
        /// The limit that was exceeded, as the caller set it — not the
        /// size reached. The event is refused the moment it crosses, so
        /// there is no final size to report.
        limit: usize,
    },
}
