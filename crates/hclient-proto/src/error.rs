//! One refusal, and it is not a failure to *do* anything.
//!
//! This crate is sans-io: no socket, no clock, no allocation of anybody
//! else's resources. So its error is a **verdict on bytes somebody else
//! handed over** — a response head that is not one ([`HeadError`]) — and
//! it is total, in the sense that the same input gives the same verdict on
//! every platform, in any process, with no state behind it.
//!
//! **That is why "incomplete" is not in here.** A head that has not
//! finished arriving is `Ok(None)` from `head::parse_response`, and the
//! module doc there says why: incomplete and malformed are different
//! facts, and a caller that could not tell them apart would either give
//! up on a slow proxy or wait for ever for a broken one. An error in this
//! module is always a decision that more bytes cannot change.
//!
//! It is re-exported from [`crate::head`], the module whose grammar
//! produced it.

// Maintainer notes (not rendered):
//
// `UriError` and `SseError` lived here beside it until the modules that
// raise them moved into `hclient`, which is the only crate that ever used
// them; they are in `hclient`'s own `error.rs` now.
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
