//! Every way a WebTransport session can refuse or end badly.
//!
//! **Split out of `lib.rs` because they are one subject and it was three
//! hundred lines**, interleaved with the session's own types — a reader
//! looking for `Session` met eight error types on the way, and a reader
//! looking for *which errors this crate has* had to know they were
//! scattered. Nothing about any of them changed, and no consumer's `use`
//! line moves: `lib.rs` re-exports each at the root it already had.
//!
//! What stayed behind is deliberate. [`crate::SessionClose`] is a
//! **value** — the code and reason a peer closed with, which
//! [`crate::Session::closed`] answers `Ok` with — and not an error, so it
//! lives with the session it describes. [`BadCloseCapsule`] is the error
//! of *failing to read* one, and is here.

/// The peer answered the CONNECT with something other than a 2xx.
///
/// A public source rather than a message, so that a caller can act on the
/// status without matching on a string — the same shape
/// `hclient-native`'s `PongNotReceived` uses, and for the same reason.
/// It is the server's answer, and replacing it with an error of ours would
/// hide a status the caller can act on: RFC 9220 makes `501 Not
/// Implemented` the answer to an unknown `:protocol`, which is a different
/// fact from `404`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the peer refused the WebTransport session with {status}")]
#[non_exhaustive]
pub struct SessionRefused {
    /// The status the peer answered the extended CONNECT with.
    pub status: http::StatusCode,
}

/// The peer's SETTINGS did not announce WebTransport.
///
/// Raised **before** the CONNECT goes out, because
/// draft-ietf-webtrans-http3 §3.1 says so: *"Clients MUST NOT attempt to
/// establish WebTransport sessions until they have received the settings
/// indicating WebTransport support from the server."* The two flags are
/// separate settings and both are required of a server, so the error says
/// which was missing rather than only that something was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "the peer does not support WebTransport (SETTINGS_ENABLE_WEBTRANSPORT={webtransport}, SETTINGS_ENABLE_CONNECT_PROTOCOL={extended_connect})"
)]
#[non_exhaustive]
pub struct NotSupportedByPeer {
    /// Whether the peer sent `SETTINGS_ENABLE_WEBTRANSPORT` with a
    /// non-zero value.
    pub webtransport: bool,
    /// Whether the peer sent `SETTINGS_ENABLE_CONNECT_PROTOCOL` with a
    /// non-zero value.
    pub extended_connect: bool,
}

/// The session URI was not `https`.
///
/// Refused rather than rewritten, and that is the point: `h3`'s
/// `Pseudo::request` defaults `:scheme` to `https` when the URI has none
/// and otherwise sends whatever it finds, so a `http://` session URI would
/// either go out claiming TLS or go out claiming plaintext over a QUIC
/// connection that is not — and neither is a thing to do silently.
///
/// # There is deliberately no companion "no authority" error
///
/// It would be a variant nothing could produce. `http::Uri` cannot hold a
/// scheme without an authority: `Uri::builder().scheme("https")
/// .path_and_query("/x").build()` is `InvalidUriParts(AuthorityMissing)`,
/// `"https:/x"` and `"https:///x"` are `InvalidFormat`, `"https://"` is
/// `Empty`, and `"https:echo"` parses with **no scheme at all** — so the
/// check above catches it. Measured, in this crate's own test run, before
/// the variant was deleted.
#[derive(Debug, Clone, thiserror::Error)]
#[error("WebTransport runs over HTTP/3, which is always TLS; `{scheme}` has no plaintext form")]
#[non_exhaustive]
pub struct NotHttps {
    /// The scheme the session URI carried, or `(no scheme)`.
    pub scheme: String,
}

/// Datagrams cannot be sent on this session.
///
/// Two reasons rather than one flag, because they are two different things
/// to fix: the first is the peer's HTTP/3 answer and the second is the
/// QUIC connection underneath it, and a caller that owns the endpoint can
/// act on the second alone. Both are reachable — a `h3` server can
/// announce WebTransport without `SETTINGS_H3_DATAGRAM`, and a `quinn`
/// endpoint with `datagram_receive_buffer_size(None)` sends no
/// `max_datagram_frame_size` — and both are exercised in this crate's
/// tests, which is the standard the WebTransport work set for a variant
/// existing at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum DatagramsUnavailable {
    /// The peer's SETTINGS carried no `SETTINGS_H3_DATAGRAM`, so
    /// RFC 9297 §2.1 forbids sending it HTTP Datagrams.
    ///
    /// This is a fact about the session, fixed when it was established:
    /// SETTINGS arrive once and cannot be revised.
    #[error("the peer's SETTINGS did not announce SETTINGS_H3_DATAGRAM")]
    NotAnnouncedByPeer,
    /// The QUIC connection carries no datagrams — the peer sent no
    /// `max_datagram_frame_size` transport parameter (RFC 9221 §3), or the
    /// local endpoint has them disabled.
    ///
    /// The two are one variant because quinn reports them as one: its
    /// `max_datagram_size` is `None` for either, and a client that does
    /// not own the endpoint cannot tell which applies.
    #[error("the QUIC connection carries no datagrams")]
    NotOnTheConnection,
}

/// The payload does not fit in a datagram.
///
/// Not a truncation and not a fragmentation: RFC 9221 datagrams are one
/// QUIC packet each, so a payload that does not fit has no smaller form
/// this crate could invent for it. The budget is what
/// [`crate::Session::max_datagram_size`] would have answered at the same moment,
/// and it moves with the path MTU estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a {payload}-byte datagram payload does not fit; the budget is {budget} bytes")]
pub struct DatagramTooLarge {
    /// The payload the caller offered.
    pub payload: usize,
    /// The largest payload that would have fitted.
    pub budget: usize,
}

/// A `CLOSE_WEBTRANSPORT_SESSION` capsule that could not be honoured.
///
/// # One type for both directions, because it is one limit
///
/// [`ReasonTooLong`](Self::ReasonTooLong) is raised by
/// [`crate::Session::close`] when the *caller's* reason is over the draft's limit
/// — refused before a byte leaves, because a peer that reads the draft
/// treats an over-long reason as a protocol error and kills the connection
/// rather than the session (`wtransport` 0.7.2 answers
/// `ErrorCode::Datagram` and its driver turns that into a connection
/// error). The other three are raised by [`crate::Session::closed`] about what
/// the peer sent. The limit is the same number in both directions, so it
/// is the same type; inventing a second one would be two names for
/// draft-ietf-webtrans-http3 §5's one sentence.
///
/// Every variant is reachable and every variant is exercised in this
/// crate's tests, which is the standard `DatagramsUnavailable` set for a
/// variant existing at all.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum BadCloseCapsule {
    /// The capsule's payload was shorter than the four bytes of the
    /// application error code (draft §5's `Application Error Code (32)`),
    /// so there is no code to report and no reason either.
    #[error("a close capsule with a {payload}-byte payload has no error code in it")]
    NoErrorCode {
        /// How many bytes the payload actually held.
        payload: usize,
    },
    /// The reason was longer than the draft's 1024 bytes.
    #[error("a {len}-byte close reason is over the draft's {max} bytes", max = Self::MAX_REASON)]
    ReasonTooLong {
        /// The length offered, or received.
        len: usize,
    },
    /// The reason was not UTF-8. The draft's field is
    /// `Application Error Message`, which it defines as UTF-8, so there is
    /// no lossy reading of it that would not be inventing text.
    #[error("the close reason was not UTF-8")]
    ReasonNotUtf8,
    /// The CONNECT stream ended in the middle of a capsule.
    ///
    /// Not a clean close: the peer said "a capsule of N bytes follows" and
    /// then stopped, so what it meant to say is unknown — which is a
    /// different fact from a stream that ends at a capsule boundary, and
    /// the whole reason this is an error rather than
    /// `SessionClose::ENDED_WITHOUT_A_CAPSULE`.
    #[error("the CONNECT stream ended with {have} bytes of an unfinished capsule")]
    Truncated {
        /// How many bytes of the unfinished capsule had arrived.
        have: usize,
    },
}

/// The peer's `SETTINGS_WT_MAX_SESSIONS` has no room for another session.
///
/// Raised by [`crate::Session::open_session`] **before** the CONNECT goes out, for
/// the reason [`BadCloseCapsule::ReasonTooLong`] is raised before the
/// capsule does: draft-ietf-webtrans-http3 §3.1 makes exceeding the limit
/// the peer's business to punish, and a peer that punishes it does so at
/// the *connection* level, taking every session on it down.
///
/// # Both numbers, because they are two different mistakes
///
/// `limit: 0` is a peer that announced WebTransport and offered no
/// sessions — `h3`'s own server builder produces exactly that unless
/// `max_webtransport_sessions` is called — and no `open_session` on it will
/// ever succeed. `limit: 1` with `open: 1` is the ordinary case and the one
/// the draft calls out by name: *"clients MUST NOT attempt to establish
/// more than one simultaneous WebTransport session"*. Dropping a session
/// gives its slot back, so the second is a wait and the first is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the peer allows {limit} simultaneous WebTransport session(s) and {open} are open")]
pub struct TooManySessions {
    /// The peer's `SETTINGS_WT_MAX_SESSIONS`.
    pub limit: u64,
    /// How many [`crate::Session`] handles were alive on this connection.
    pub open: u64,
}

/// [`crate::Session::close`] was called on a session that has already sent its
/// capsule.
///
/// A refusal rather than a silent `Ok`, and the reason is the argument:
/// `close` carries an application error code the peer will act on, so
/// answering `Ok` to a second call with a *different* code would tell the
/// caller that code reached the peer when nothing did. There is exactly
/// one `CLOSE_WEBTRANSPORT_SESSION` capsule per session, because the
/// stream it travels on is finished by the first one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("this session has already been closed")]
pub struct AlreadyClosed;
