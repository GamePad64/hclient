//! The failures, and the payloads [`crate::Error::source`] hands back.
//!
//! **This module was a door before it was a room, and that is what
//! decides its contents.** It existed as a re-export list, because
//! `hclient`'s rendered index had listed seventy-three items of which
//! about twelve were what a caller reaches for; the rest divided into
//! groups that each wanted one. The types are now *defined* here rather
//! than named here, and every path a consumer could already type is the
//! path it still is.
//!
//! What unites them is the layer rather than the subject: these are the
//! refusals **`Client` itself** makes, from the builder that will not
//! resolve a base URL to the body wrapper that stops a response past its
//! limit. That is why four of them are private — `BadLocation`,
//! `BodyVanishedBeforeRetry`, `SseRejected` and `ReconnectExhausted`
//! reach a caller only through `Error::source`, named here without links
//! because a link from public prose to a private item is what `just docs`
//! refuses, and a `pub` on any of them would promise a distinction nobody
//! can act on.
//!
//! # What is deliberately not here
//!
//! `cookie` and `cache` keep their own, and the reason
//! is measured rather than aesthetic: both were **crates** until this
//! year, so by this workspace's own convention each already had an error
//! module of its own, and the fold that made them modules was argued as
//! costing exactly one sentence in `docs/competitive-gaps.md` and nothing
//! else. Both module docs still say they are sans-io, clockless, and
//! reach for neither `Client` nor `hclient-core` — an error type shared
//! with this file's would end the last of those. [`crate::auth`] is the
//! third, for a different reason of the same shape: it is a **seam**,
//! whose whole point is a scheme written in somebody else's crate, and a
//! third party implementing [`crate::auth::AuthFlow`] meets its errors
//! there rather than here.
//!
//! # Two things did change, and neither is a path
//!
//! [`MultipartError`] and [`TrailersInAPart`] were reachable only as
//! `multipart::`, and are reachable as `error::` as well now — because a
//! `pub` item in this module is at this path whether or not it is named
//! in a list. That is the door becoming honest rather than a decision: it
//! calls itself *the failures*, and two of them were missing from it.
//! [`crate::multipart`] re-exports both under the names they had.
//!
//! And where the other module is **public**, rustdoc renders the arrow
//! rather than the item: `multipart::MultipartError` and
//! `lines::LineTooLong` are now a link to the page here, where before
//! this module carried a link to the page there. The pair flipped, which
//! is the change stating itself — the definition is here, so the page is
//! here. Nothing of the sort happens for `cookie`,
//! `cache` or `auth`, whose own `error` modules are
//! private, so rustdoc inlines each type into the module a caller
//! names.

use crate::multipart::MAX_REWIND_DEPTH;
use std::time::Duration;

/// The error's phase, category, and the two portable refusals — the
/// vocabulary every backend reports into, defined in `hclient-core`
/// because a transport must be able to raise one without depending on
/// this crate.
pub use hclient_core::error::{Phase, UnsupportedCapability, VersionNotAvailable};
/// A string that could not be made into a URI, from the sans-io leaf that
/// does the parsing.
pub use hclient_proto::uri::UriError;

/// The base URL is unfit to resolve this request against.
///
/// `pub`, not for looks: the caller must be
/// able to tell this apart from any other `ErrorKind::Other` via
/// `Error::source().downcast_ref::<InvalidBaseUrl>()` — the same trick
/// `mock::QueueEmpty` uses. Both fields are public so the diagnostic names
/// the specific pair, not just the fact.
///
/// `requested` is a `String`, not an `http::Uri`: resolution works on the
/// STRING before parsing (see `effective_uri`), and exactly the references
/// the base exists for aren't expressible as `http::Uri` at all.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("cannot resolve `{requested}` against base URL `{base}` (a base URL must be absolute)")]
#[non_exhaustive]
pub struct InvalidBaseUrl {
    pub base: http::Uri,
    pub requested: String,
}

/// A redirect policy refused a hop.
///
/// Raised beside the unresolvable-`Location` error, because both are
/// `decide`'s answers
/// turned into errors at the same place. It replaced `TooMany(u8)`, which
/// could say only one of the things a policy can now refuse for.
#[derive(Debug, thiserror::Error)]
#[error("the redirect policy refused a {status} to {to} after {after_hops} hops: {why}")]
#[non_exhaustive]
pub struct RedirectRefused {
    pub to: http::Uri,
    pub status: http::StatusCode,
    /// The policy's own words — `"redirect limit reached"`,
    /// `"redirect leaves the origin"`, or whatever a caller's own policy
    /// returned.
    pub why: &'static str,
    /// How many hops had already been taken.
    ///
    /// The client's fact rather than the policy's, which is why it is
    /// here and not in the verdict: a `&'static str` cannot carry a
    /// number without allocating, and *how far did this get* is true of
    /// every refusal rather than only a limit.
    pub after_hops: u8,
}

#[derive(Debug, thiserror::Error)]
#[error("Location header is not a resolvable URI")]
pub(crate) struct BadLocation;

/// The body could not be rebuilt for a retry the policy had approved.
///
/// Unreachable while `replayable` gates every path that reaches it —
/// it exists so that the impossible case is a typed error a caller can
/// read rather than a panic in a client that had already worked.
#[derive(Debug, thiserror::Error)]
#[error("the request body could not be rebuilt for a retry")]
pub(crate) struct BodyVanishedBeforeRetry;

/// The source of an [`ErrorKind::Timeout`](crate::ErrorKind::Timeout)`(`[`Phase::Total`]`)`.
///
/// A named type rather than a string, for the same reason
/// [`InvalidBaseUrl`] is one: a caller has to be able to tell this
/// apart from any other timeout by
/// `Error::source().downcast_ref::<TotalTimeoutElapsed>()`, and to read
/// the bound that was actually in force.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the whole operation exceeded its total timeout of {0:?}")]
#[non_exhaustive]
pub struct TotalTimeoutElapsed(pub Duration);

/// The body yielded more bytes than the client's limit allowed.
///
/// Carries both numbers because the pair is what a caller acts on: the
/// limit tells them what they asked for, and the count tells them how
/// close the truth was to it.
#[derive(Debug, thiserror::Error)]
#[error("the response body exceeded the {limit}-byte limit (stopped at {seen})")]
#[non_exhaustive]
pub struct ResponseTooLarge {
    pub limit: u64,
    pub seen: u64,
}

/// A single line was longer than the stream's bound.
///
/// Carries both numbers for [`ResponseTooLarge`]'s reason:
/// the limit is what the caller asked for and the count is how close the
/// truth got to it.
#[derive(Debug, thiserror::Error)]
#[error("a single line exceeded the {limit}-byte limit (stopped at {seen})")]
#[non_exhaustive]
pub struct LineTooLong {
    pub limit: usize,
    pub seen: usize,
}

/// The response body did not decode as its `Content-Encoding` promised.
///
/// `pub` for the same reason [`TotalTimeoutElapsed`]
/// and [`InvalidBaseUrl`] are: a caller has to be able to tell this
/// apart from every other `ErrorKind::Decode` — a body that is not valid
/// gzip is a different problem from a body that is not valid UTF-8 — and
/// `Error::source().downcast_ref::<DecodeFailed>()` is the way.
#[derive(Debug, thiserror::Error)]
#[error("the response body is not valid `{coding}` data")]
#[non_exhaustive]
pub struct DecodeFailed {
    /// The coding that was attempted, as it appeared on the wire.
    ///
    /// **A [`Cow`](std::borrow::Cow) since the coding list became a
    /// caller's**, for the reason
    /// [`ClientBody::coding`](crate::body::ClientBody::coding) is one: a
    /// body holds its decoder rather than the
    /// [`ContentCoding`](crate::ContentCoding) that built it, so a coding
    /// somebody else wrote has no `'static` string here to be named by.
    /// The four this crate ships are still `Cow::Borrowed`, and a
    /// `d.coding == "gzip"` comparison reads exactly as it did.
    pub coding: std::borrow::Cow<'static, str>,
    #[source]
    pub(crate) source: std::io::Error,
}

/// What [`ClientBuilder::build`](crate::ClientBuilder::build) refuses.
///
/// # Why `build()` stopped returning [`UnsupportedCapability`] directly
///
/// It returned that type alone while every refusal at `build()` was one
/// answer to one question — *can the transport honour this setting* — and
/// [`InvalidCodingToken`] is not that question: nothing about a backend
/// decides whether a [`ContentCoding`](crate::ContentCoding)'s own token
/// is a token, and `UnsupportedCapability`'s two fields are
/// `&'static str`, so it could not carry a caller's spelling even if the
/// message *`backend X does not support Y`* had been the right sentence.
///
/// **The shape is `Client::new`'s, one layer down.** That constructor
/// faced the same fork — a narrow typed refusal beside a second cause of a
/// different kind — and this file records what it settled on: keep the
/// wider type, because the discriminant already draws the line the two
/// types were drawing, and a `try_` twin that also returned `Result` was
/// marking the one fallible about *more things* rather than the one
/// fallible at all. An enum is that answer where there is no `ErrorKind`
/// to lean on, and the variant is the discriminant.
///
/// Both variants keep their payload public and reachable, so the
/// downcast-free question — *which refusal was it* — is a `match`, and
/// nothing that used to be typed became a string.
///
/// `Client::new()` folds this into an `ErrorKind::Unsupported`
/// [`Error`](hclient_core::error::Error) exactly as it folded the narrow
/// type, so a caller of the convenience constructor sees no change at all.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum BuildError {
    /// A setting the chosen transport cannot honour.
    ///
    /// Every refusal `build()` made before the coding list became a
    /// caller's, and the whole of what `check_supported` answers.
    #[error(transparent)]
    Unsupported(#[from] UnsupportedCapability),
    /// A configured content coding whose token will not go in a header.
    #[error(transparent)]
    InvalidCodingToken(#[from] InvalidCodingToken),
}

impl BuildError {
    /// The capability refusal, where that is what this is.
    ///
    /// **An accessor rather than a `match` at nine call sites**, and the
    /// nine are what argued for it: every one of them asserts *which
    /// setting was refused* — `err.what == "cookie_jar"`,
    /// `err.backend.contains("MockTransport")` — which is the question
    /// this error exists to answer and which a `match` with an
    /// `unreachable!()` arm buries. It is [`Option`] rather than a panic
    /// for the ordinary reason: the other variant is reachable, and a
    /// caller who asks the wrong question should get `None` rather than an
    /// abort.
    ///
    /// The variant is still there for a caller who wants to branch on
    /// both, and both payloads are public. This is the shortcut for the
    /// common case, not a second way of saying the same thing.
    #[must_use]
    pub fn unsupported(&self) -> Option<&UnsupportedCapability> {
        match self {
            Self::Unsupported(u) => Some(u),
            Self::InvalidCodingToken(_) => None,
        }
    }

    /// The coding refusal, where that is what this is — [`unsupported`]'s
    /// twin, so that neither variant is the one a caller has to write a
    /// `match` for.
    ///
    /// [`unsupported`]: Self::unsupported
    #[must_use]
    pub fn invalid_coding_token(&self) -> Option<&InvalidCodingToken> {
        match self {
            Self::InvalidCodingToken(t) => Some(t),
            Self::Unsupported(_) => None,
        }
    }
}

/// A [`ContentCoding`](crate::ContentCoding) whose own name will not go in
/// a header.
///
/// Raised by [`ClientBuilder::build`](crate::ClientBuilder::build), never
/// per request, because that is where a configuration this client cannot
/// honour is refused — `check_supported`'s own shape one field over, where
/// what cannot be honoured is a setting the transport will not keep and
/// here it is a token `Accept-Encoding` will not carry.
///
/// **The alternative was a panic and the other alternative was silence**,
/// which is why this type exists rather than either. `accept_encoding`
/// assembled its value with an `expect` justified by *"every token is a
/// compile-time ASCII constant… nothing here comes from the network or the
/// caller"* — true of a closed set of codings and false the moment the
/// seam opened, so a coding answering `"my coding"` would have panicked
/// once per request. Skipping it instead would have been a client that
/// never asks for a coding the caller configured and never says so, which
/// is the *silently ignored setting* defect this workspace has closed four
/// times.
///
/// A `token` is RFC 9110 §5.6.2's production: one or more of the ASCII
/// alphanumerics and ``!#$%&'*+-.^_`|~``. The two characters worth naming
/// are the space and the comma, which are what would let one coding's
/// token be read as two.
#[derive(Debug, Clone, thiserror::Error)]
#[error(
    "the content coding `{coding}` offers `{token}`, which is not an RFC 9110 §5.6.2 token and cannot go in a header"
)]
#[non_exhaustive]
pub struct InvalidCodingToken {
    /// The offending spelling — the coding's own token, or one of its
    /// aliases.
    pub token: String,
    /// The coding it came from, by its
    /// [`token`](crate::ContentCoding::token), so that a caller with
    /// several configured knows which to look at. Equal to
    /// [`token`](Self::token) when it is the token itself that is
    /// malformed, which is the commoner case and reads as a repetition
    /// rather than as a puzzle.
    pub coding: String,
}

/// A `4xx` or `5xx` the caller asked to be told about.
///
/// Carries the URL as well as the status, because by the time a caller is
/// looking at one they have usually stopped holding the response — and a
/// chain of redirects means the URL that failed is not the one they typed.
#[derive(Debug, Clone, thiserror::Error)]
#[error("the server answered {status} for {url}")]
#[non_exhaustive]
pub struct UnexpectedStatus {
    pub status: http::StatusCode,
    pub url: http::Uri,
}

/// What [`Collected::text_with_charset`](crate::Collected::text_with_charset) could not do.
#[cfg(feature = "charset")]
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CharsetError {
    /// The `charset` parameter named something the WHATWG Encoding
    /// Standard has no encoding for.
    #[error("the response declared `charset={label}`, which names no encoding")]
    UnknownLabel { label: String },
    /// The bytes are not a valid sequence in the charset that was used —
    /// which is the declared one unless a byte order mark overrode it.
    #[error("the response body is not valid {charset}")]
    Malformed { charset: &'static str },
}

/// A `multipart/form-data` body was set and so was a `Content-Type`.
///
/// The two cannot both stand: the header carries the boundary, so the
/// caller's value would describe a body that is not there. Refusing names
/// both, where overriding would lose the caller's header silently and
/// deferring would send bytes no receiver can parse.
#[derive(Debug, thiserror::Error)]
#[error(
    "a multipart body sets its own Content-Type, because the header carries the boundary — \
     remove the Content-Type, or build the body with multipart::Form::encode and set both"
)]
#[non_exhaustive]
pub struct ContentTypeIsNotOursToKeep;

/// RFC 7617 §2 makes `:` the separator between a username and a password,
/// so a username containing one is not representable — and encoding it
/// anyway would make `("a:b", "")` and `("a", "b")` the same bytes.
#[derive(Debug, thiserror::Error)]
#[error("a Basic-auth username may not contain a colon")]
#[non_exhaustive]
pub struct ColonInUsername;

/// What can go wrong while turning a [`Form`](crate::multipart::Form) into bytes.
///
/// Every variant is a *build* failure: it is raised before a connection
/// is opened, and reaches the caller out of `send()` like any other
/// builder error.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MultipartError {
    /// The operating system would not supply randomness for a boundary.
    ///
    /// There is deliberately no fallback — see this module's
    /// documentation on why a fixed boundary is the one value that must
    /// never be emitted.
    /// The cause is boxed rather than typed, and that is a stability
    /// decision rather than a style one: it was `getrandom::Error`, so a
    /// major release of a crate this one uses for sixteen bytes would have
    /// been a breaking change here, and a caller matching this variant had
    /// to depend on `getrandom` to name what they caught. The chain is
    /// unchanged — `source()` still reaches the OS's reason — and so is
    /// the message.
    #[error("no entropy available for a multipart boundary: {0}")]
    NoEntropy(#[source] Box<dyn std::error::Error + Send + Sync>), // send-bound-exception: amendment-C1

    /// A caller-supplied boundary that RFC 2046 §5.1.1 does not allow:
    /// empty, longer than 70 characters, ending in a space, or carrying a
    /// character outside `bcharsnospace` plus space.
    #[error("not an RFC 2046 boundary: {0:?}")]
    InvalidBoundary(String),

    /// A field name or file name carrying a C0 control byte other than
    /// CR, LF or HTAB, or DEL.
    ///
    /// CR and LF are escaped (`%0D`, `%0A`); these have no escape any
    /// receiver agrees on, and would corrupt the part's header block.
    #[error(
        "{field} contains control byte {byte:#04x}, which has no representation in a part header"
    )]
    ControlByte {
        /// `"a field name"` or `"a file name"`.
        field: &'static str,
        /// The offending byte.
        byte: u8,
    },

    /// A part's `Content-Type` is not a valid header value.
    #[error("a part's Content-Type is not a valid header value: {0}")]
    InvalidContentType(#[from] http::header::InvalidHeaderValue),

    /// A part's body was a [`RequestBody::Rewindable`](hclient_core::body::RequestBody::Rewindable) whose factory kept
    /// returning another one.
    ///
    /// The same bound, for the same reason, as `hclient-fetch`'s and
    /// `hclient-wasi`'s: a factory that always rewinds to a factory would
    /// otherwise unwrap for ever.
    #[error(
        "a part's RequestBody::Rewindable factory nested {MAX_REWIND_DEPTH} levels deep or more"
    )]
    RewindTooDeep,
}

/// A part's body yielded a trailers frame.
///
/// `multipart/form-data` has nowhere to put one: a part ends at the next
/// delimiter and carries no trailer section. Dropping the frame would
/// send a well-formed request missing data the caller supplied, so the
/// body fails instead — the same call this workspace makes for undeclared
/// HTTP/1 request trailers one crate over.
#[derive(Debug, thiserror::Error)]
#[error("a multipart part's body emitted trailers, which multipart/form-data cannot carry")]
#[non_exhaustive]
pub struct TrailersInAPart;

#[derive(Debug, thiserror::Error)]
#[error("not an SSE stream: {0}")]
pub(crate) struct SseRejected(pub(crate) &'static str);

/// Surfaced exactly once, via `next()`, when `Backoff::delay` returns
/// `None` (the configured `max_attempts` is exhausted): "the stream ended"
/// (a clean, un-reconnected EOF, or a terminal error) and "we gave up
/// retrying" must be distinguishable, not two different shapes of silence.
/// `downcast_ref::<ReconnectExhausted>()` on `Error::source()` is how a
/// caller tells them apart — the same idiom as `mock::QueueEmpty` and
/// `client::TooMany`.
#[derive(Debug, thiserror::Error)]
#[error("gave up reconnecting the SSE stream after {attempts} attempt(s)")]
pub(crate) struct ReconnectExhausted {
    /// How many reconnect attempts were actually made before giving up —
    /// NOT `max_attempts` restated: `Backoff::max_attempts` is the ceiling,
    /// this is what was observed, useful for a log line without re-reading
    /// the configured policy.
    pub(crate) attempts: u32,
}
