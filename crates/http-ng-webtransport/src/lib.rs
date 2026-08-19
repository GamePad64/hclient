//! WebTransport sessions over this workspace's HTTP/3.
//!
//! `conn` is a `quinn::Connection` that negotiated ALPN `h3` — see "Where
//! the connection comes from" below, which is the part that is missing
//! rather than the part that is here.
//!
//! ```no_run
//! # async fn example(
//! #     conn: quinn::Connection,
//! #     uri: http::Uri,
//! #     second: http::Uri,
//! # ) -> Result<(), Box<dyn std::error::Error>> {
//! let session = http_ng_webtransport::Session::connect(conn, &uri).await?;
//! let (mut send, mut _recv) = session.open_bi().await?;
//! send.write_all(b"ping").await?;
//! // A second session on the same connection, within the peer's
//! // `SETTINGS_WT_MAX_SESSIONS`.
//! let beside = session.open_session(&second).await?;
//! # let _ = beside;
//! # Ok(())
//! # }
//! ```
//!
//! # Why this is not the WebSocket seam
//!
//! `docs/w4-upgrade-seam.md` §4 decided it and this crate does not
//! re-litigate it: a WebSocket is **one** message channel — `Stream<Item =
//! Message> + Sink<Message>` — and a WebTransport session is a
//! **multiplexer**: streams opened on demand, in both directions, plus
//! datagrams. The intersection of the two method sets is as empty as
//! `QuicTlsConnect`'s was with `TlsConnect`, and the failure mode of
//! forcing one onto the other is the same one: an adapter that type-checks
//! *with an empty body*. Nothing here reuses
//! [`Message`](http_ng_core::unversioned::Message).
//!
//! # Why there is no trait here
//!
//! `WebSocketConnect` lives in `http-ng-core::unversioned` because two
//! backends implement it, and the second one — the browser — is what
//! proved the shape. There is exactly one thing in this workspace that can
//! open a WebTransport session, so a trait here would be a shape nobody
//! has tested, which is the objection `docs/w4-upgrade-seam.md` §5 raises
//! against declaring a seam before a backend fits it. The trait belongs
//! beside `WebSocketConnect` when there is a second implementer — the
//! browser's own `WebTransport` global, which has shipped in all four
//! engines (§4) — and not before.
//!
//! # Where the connection comes from
//!
//! [`Session::connect`] takes a `quinn::Connection` that has already
//! negotiated ALPN `h3`. That is the shape §2 of the same document rejects
//! as a **public** seam and §8 accepts as an **internal** one, for the
//! reason given there: "a shape can be wrong at one level and right at the
//! next". Everything on the other side of it — a QUIC endpoint over
//! `http_ng_rt::{UdpBind, Spawn, Timer}`, a `QuicTlsConnect` backend,
//! resolution, and the pool policy — already exists, once, in
//! `http-ng-h3`, and none of it is reachable from outside that crate.
//!
//! So this crate is the half nobody provides for a client, and the half it
//! is missing is a **finding** rather than a design: what `http-ng-h3`
//! would have to expose, and why a WebTransport session cannot share one
//! of its pooled connections even if it did, is
//! `docs/v04-w2-webtransport.md` §4.
//!
//! # Nothing is spawned
//!
//! `http-ng-h3` spawns a connection driver because **a pooled QUIC
//! connection that nobody polls is dying** and between requests the pool is
//! the only thing holding it. Neither half of that is true here. The QUIC
//! connection is driven by the endpoint driver `quinn` already runs for the
//! endpoint it came from, and a session is the caller's own object rather
//! than a pool entry, so there are no future requests on whose behalf
//! anything must stay awake.
//!
//! The h3 *control* stream is polled exactly once, inside
//! [`Session::connect`], to receive the peer's SETTINGS — which the draft
//! makes a precondition of sending the CONNECT at all — and never again.
//! What that costs is named rather than discovered, and now measured: a
//! `GOAWAY` arriving later is not observed. It arrives on the control
//! stream, which is the driver's, and the driver is held rather than
//! polled. `tests/goaway.rs` is four assertions about why leaving it that
//! way is a choice rather than an oversight, and one about what the choice
//! costs — a round trip and a typed `H3_REQUEST_REJECTED`, because the peer
//! enforces the rule this client cannot see.
//!
//! The **CONNECT** stream is a different stream and is now read:
//! [`Session::closed`] is the caller's own future over it, spawning
//! nothing, so a caller that never awaits it never learns the session
//! ended — the same trade [`Session::recv_datagram`] makes.
//!
//! # The capsule protocol
//!
//! A session ends cleanly by a `CLOSE_WEBTRANSPORT_SESSION` capsule
//! (draft-ietf-webtrans-http3 §5) carrying an application error code and a
//! reason, sent on the CONNECT stream and followed by its FIN.
//! [`Session::close`] writes one and [`Session::closed`] reads the peer's,
//! which is what lets a caller tell a clean close from a connection that
//! vanished: `Ok` against `Err`, the distinction `http-ng-fetch` draws for
//! a WebSocket with `wasClean`.
//!
//! The framing splits cleanly in two and only half of it is ours. RFC 9297
//! §3.2 carries capsules in the payload of HTTP/3 DATA frames, and that
//! layer is `h3`'s — `RequestStream::send_data` and `poll_recv_data`, on
//! the CONNECT stream this crate has held since v0.4 W2 without reading it.
//! The capsule itself is fifty-nine lines here, encoder and decoder
//! together, because **`h3` 0.0.8 has no capsule code at all** and neither
//! does `h3-datagram` 0.0.2 or the crate named `h3-webtransport` 0.1.2.
//! See `close_capsule`.
//!
//! # More than one session on one connection
//!
//! [`Session::open_session`] opens another, bounded by the peer's
//! `SETTINGS_WT_MAX_SESSIONS`. Two things about it are worth knowing before
//! reading it, and both were recorded here as blockers and turned out
//! otherwise.
//!
//! **The limit is readable**, where `docs/v04-w2-webtransport.md` §3(c) said
//! it was not: not from `h3::config::Settings`, which has no getter for it,
//! but from the SETTINGS **frame** [`Session::connect`] already awaits and
//! used to discard. See `PeerSettings`.
//!
//! **A second h3 client is not the way**, and that part was right — it is a
//! *connection* error, `H3_STREAM_CREATION_ERROR`, which takes the first
//! session with it, and `tests/sessions.rs` executes the prediction. So
//! `Shared` holds one h3 client and every session clones its
//! `SendRequest`.
//!
//! # Datagrams
//!
//! [`Session::send_datagram`] and [`Session::recv_datagram`] carry
//! RFC 9297 HTTP Datagrams over the QUIC DATAGRAM extension (RFC 9221),
//! which is the feature WebTransport exists for that streams do not
//! already give: unreliable, unordered, no head-of-line blocking. The wire
//! format is the Quarter Stream ID — this session's CONNECT stream ID
//! divided by four — as a variable-length integer, then the payload; the
//! draft adds no framing of its own.
//!
//! **Neither `h3-datagram` nor `h3-quinn`'s `datagram` feature is used**,
//! and that is not a preference. `h3-datagram` 0.0.2's `Datagram::encode`
//! encodes the Quarter Stream ID into a local buffer and then builds its
//! `EncodedDatagram` from a **freshly zeroed array**, discarding it — so
//! every datagram it writes carries a Quarter Stream ID of zero, of the
//! right length. A session on stream 0 is unaffected and every other
//! session addresses its datagrams to the wrong one. Measured rather than
//! read: `docs/v04-w2-datagrams.md` §3. Beyond that, this crate already
//! owns the QUIC varint for the stream header, for the reason on
//! `put_varint`, and the datagram header is the same two lines.
//!
//! # What is deliberately not here
//!
//! - **`DRAIN_WEBTRANSPORT_SESSION`.** The other capsule the draft
//!   defines, and the one a caller cannot act on from here: a drain says
//!   *stop opening streams, I will close soon*, which is not an end, so
//!   [`Session::closed`] — a future that resolves once, when the session
//!   is over — has no honest place to report it. It is skipped along with
//!   every other unknown capsule type, which is what RFC 9297 §3.2
//!   requires of a receiver anyway. `docs/v04-w2-capsules.md` §7 says what
//!   surfacing it would need.
//! - **Server-initiated streams.** A server-opened *unidirectional*
//!   WebTransport stream is not merely unimplemented here, it is
//!   unreachable: `h3`'s client driver classifies it as
//!   `AcceptedRecvStream::WebTransportUni` and then discards it, because
//!   the arm that keeps it is guarded by `enable_webtransport`, which
//!   `h3` 0.0.8's **client** builder has no setter for. See
//!   `docs/v04-w2-webtransport.md` §3.
#![forbid(unsafe_code)]

use bytes::{Buf, Bytes};
use h3::ConnectionState as _;
use h3::connection::ConnectionInner;
use h3::proto::frame::{Frame, SettingId};
use http_ng_core::{Error, ErrorKind};
use std::collections::HashMap;
use std::future::poll_fn;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

/// The identifier of a WebTransport session.
///
/// It is the QUIC stream ID of the CONNECT stream that established the
/// session (draft-ietf-webtrans-http3 §4.2), and it is what every stream
/// belonging to the session carries in its header — which is why it is
/// public: a caller reading a raw QUIC stream from somewhere else needs it
/// to tell whose stream it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionId(u64);

impl SessionId {
    /// The stream ID, as a variable-length integer's worth of value.
    pub fn value(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The peer answered the CONNECT with something other than a 2xx.
///
/// A public source rather than a message, so that a caller can act on the
/// status without matching on a string — the same shape
/// `http-ng-native`'s `PongNotReceived` uses, and for the same reason.
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
/// [`Session::max_datagram_size`] would have answered at the same moment,
/// and it moves with the path MTU estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a {payload}-byte datagram payload does not fit; the budget is {budget} bytes")]
pub struct DatagramTooLarge {
    /// The payload the caller offered.
    pub payload: usize,
    /// The largest payload that would have fitted.
    pub budget: usize,
}

/// How a session ended, when it ended cleanly.
///
/// draft-ietf-webtrans-http3 §5 gives an ending session an *application*
/// error code and a reason string, carried in a
/// `CLOSE_WEBTRANSPORT_SESSION` capsule on the CONNECT stream. Both are the
/// application's, not the protocol's: nothing in this crate or in HTTP/3
/// assigns them a meaning, which is why `code` is a bare `u32` rather than
/// an enum of ours.
///
/// # A stream that simply ends is this, with zeroes
///
/// The draft: *"Cleanly terminating a CONNECT stream without sending a
/// `CLOSE_WEBTRANSPORT_SESSION` capsule SHALL be semantically equivalent to
/// terminating it with a `CLOSE_WEBTRANSPORT_SESSION` capsule that has an
/// error code of 0 and an empty error string."* So a peer that just closes
/// its half is reported here as `{ code: 0, reason: "" }` and **not** as a
/// separate variant: the specification says the two are the same fact, and
/// a distinction the wire does not carry is one a caller would learn to
/// mistrust. `wtransport` 0.7.2 reads it the same way, in
/// `src/driver/streams/connect.rs`, and that is an implementation sharing
/// no code with this one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionClose {
    /// The application error code the peer closed with.
    pub code: u32,
    /// The reason the peer gave, which is very often empty.
    pub reason: String,
}

impl SessionClose {
    /// What the draft says a bare FIN on the CONNECT stream means.
    const ENDED_WITHOUT_A_CAPSULE: Self = Self {
        code: 0,
        reason: String::new(),
    };
}

/// A `CLOSE_WEBTRANSPORT_SESSION` capsule that could not be honoured.
///
/// # One type for both directions, because it is one limit
///
/// [`ReasonTooLong`](Self::ReasonTooLong) is raised by
/// [`Session::close`] when the *caller's* reason is over the draft's limit
/// — refused before a byte leaves, because a peer that reads the draft
/// treats an over-long reason as a protocol error and kills the connection
/// rather than the session (`wtransport` 0.7.2 answers
/// `ErrorCode::Datagram` and its driver turns that into a connection
/// error). The other three are raised by [`Session::closed`] about what
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

impl BadCloseCapsule {
    /// draft-ietf-webtrans-http3 §5's limit on the reason string:
    /// `Application Error Message (..8192)`, in bits.
    pub const MAX_REASON: usize = 1024;
}

/// The peer's `SETTINGS_WT_MAX_SESSIONS` has no room for another session.
///
/// Raised by [`Session::open_session`] **before** the CONNECT goes out, for
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
    /// How many [`Session`] handles were alive on this connection.
    pub open: u64,
}

/// [`Session::close`] was called on a session that has already sent its
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

/// An open WebTransport session: a multiplexer over one QUIC connection.
///
/// # What it holds, and why none of it is dead weight
///
/// The **CONNECT stream** is here in two halves, and they are the session's
/// beginning and its end. The session lives exactly as long as that stream
/// (draft §5), a `CLOSE_WEBTRANSPORT_SESSION` capsule travels on it
/// ([`close`](Self::close)), and the peer's travels back
/// ([`closed`](Self::closed)). Until v0.4 it was held and never read, which
/// is why the crate doc used to say a session's end could not be observed.
///
/// Two more things are owned and never polled, and dropping either would
/// end the connection under the session:
///
/// - the h3 **`SendRequest`**, because `h3` counts them and marks the
///   connection closed with `H3_NO_ERROR` when the last one drops;
/// - the h3 **connection driver**, because it owns the *control* stream,
///   and a control stream that ends is `H3_CLOSED_CRITICAL_STREAM` — a
///   connection error, not a stream one.
///
/// The second is the one worth knowing about: the driver is held to keep a
/// stream open, not to be polled. See the crate doc.
///
/// Both live in `Shared` rather than here, because they belong to the
/// **connection** and not to any one session — which is what
/// [`open_session`](Self::open_session) needed and what this crate spent
/// its whole first version without.
///
/// # Why the two halves are behind mutexes
///
/// Every other method here takes `&self` — `open_bi`, `send_datagram`,
/// `recv_datagram` — because the thing underneath them is a
/// `quinn::Connection`, which has interior mutability of its own. `h3`'s
/// `RequestStream` does not, so keeping `close` and `closed` on `&self`
/// takes two `Mutex`es. It is worth the two, because `&mut self` on either
/// would stop a caller doing the one thing a session is for: waiting for
/// the peer's close *while* opening streams, which is `&self` and `&mut
/// self` at once and does not compile.
///
/// Neither lock is ever held across an `await`. [`closed`](Self::closed)
/// locks inside a `poll` and drops the guard before returning;
/// [`close`](Self::close) **takes** the send half out from under its lock
/// and then awaits, which is also what makes a second `close` an
/// [`AlreadyClosed`] rather than a deadlock.
pub struct Session {
    /// Everything this session shares with its siblings on the same QUIC
    /// connection — including, until v0.4, the two anchors that used to
    /// live here by value.
    shared: Arc<Shared>,
    id: SessionId,
    /// The CONNECT stream's send half, `None` once [`Session::close`] has
    /// taken it. Dropping it is what FINs the stream, so it is taken by
    /// value rather than borrowed.
    connect_send: Mutex<Option<ConnectSend>>,
    /// The CONNECT stream's receive half, plus the bytes read past the end
    /// of the last capsule and the answer once it is known.
    connect_recv: Mutex<CloseWatch>,
}

/// One QUIC connection's HTTP/3 client, and everything its sessions share.
///
/// # Why this is a separate allocation rather than fields on `Session`
///
/// Until v0.4 it *was* fields on `Session`, and the consequence was
/// recorded as this crate's last open item: *"here a `Session` owns the h3
/// client, so there is one."* A second [`Session::connect`] on the same
/// `quinn::Connection` builds a **second** h3 client, whose control stream
/// the peer already has one of — RFC 9114 §6.2.1 — and which, measured
/// against `h3`'s own server, does not error at all: it **hangs**, because
/// a connection has exactly one server control stream and the first client
/// took it, so the peer's SETTINGS never arrive at the second.
/// `tests/sessions.rs` pins that.
///
/// Sharing it is what makes [`Session::open_session`] possible, and the
/// three things below are shared because they are facts about the
/// connection rather than about any one session.
struct Shared {
    /// The raw QUIC connection. WebTransport streams are QUIC streams with
    /// a header, opened beside h3's rather than through it — which is why
    /// this is here and `h3` is not asked to open them.
    conn: quinn::Connection,
    /// What the peer's SETTINGS said about `SETTINGS_H3_DATAGRAM`, read
    /// once at establishment because that is when the frame arrives and
    /// SETTINGS cannot be changed afterwards. It is not a third flag on
    /// the gate — see [`Session::max_datagram_size`].
    peer_datagrams: bool,
    /// The peer's `SETTINGS_WT_MAX_SESSIONS`, and the only reader of it is
    /// [`Session::open_session`]. See `PeerSettings` for where it comes
    /// from, which is not where this crate's documentation used to say it
    /// could not be got from.
    max_sessions: u64,
    /// How many [`Session`] handles are alive on this connection.
    /// Incremented on establishment and decremented in `Drop`, which is
    /// what makes the draft's word *simultaneous* mean something.
    open: AtomicU64,
    /// Datagrams one session read off the connection that belong to
    /// another. See `Shared::hand_over`.
    parked: Mutex<HashMap<u64, Parked>>,
    /// The h3 client. Held so that `h3`'s sender count never reaches zero
    /// — it marks the connection closed with `H3_NO_ERROR` when the last
    /// `SendRequest` drops — and **cloned** for every CONNECT, which is
    /// what `SendRequest: Clone` is for.
    send: h3::client::SendRequest<h3_quinn::OpenStreams, Bytes>,
    /// The h3 connection driver. Held and never polled: it owns the
    /// *control* stream, and a control stream that ends is
    /// `H3_CLOSED_CRITICAL_STREAM` — a connection error, not a stream one.
    ///
    /// Never polled is the load-bearing half, and it is what makes a
    /// `GOAWAY` invisible here. `docs/v04-goaway-and-sessions.md` §2 has
    /// the four measurements behind leaving it that way, and
    /// `tests/goaway.rs` asserts them.
    _driver: h3::client::Connection<h3_quinn::Connection, Bytes>,
}

/// The CONNECT stream's send half — what a `CLOSE_WEBTRANSPORT_SESSION`
/// capsule is written to.
type ConnectSend = h3::client::RequestStream<h3_quinn::SendStream<Bytes>, Bytes>;

/// The CONNECT stream's receive half — what the peer's capsule arrives on.
type ConnectRecv = h3::client::RequestStream<h3_quinn::RecvStream, Bytes>;

// Hand-written: none of the three anchors is `Debug`, and requiring it of
// them would mean asking `h3` for an impl in order to print a field whose
// whole purpose is to exist. What a reader wants from a `Session` is which
// session it is and where it goes, which is what this prints.
impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("id", &self.id)
            .field("remote", &self.shared.conn.remote_address())
            .finish_non_exhaustive()
    }
}

/// A session's handle is what the draft's word *simultaneous* counts, so
/// the count is kept here rather than inferred from the CONNECT stream.
///
/// Deliberately not the same event as the session *ending*: a session that
/// has been [`close`](Session::close)d, or whose peer closed it, is over on
/// the wire but its handle is still the caller's, and a slot that came back
/// while the caller could still call [`Session::closed`] on it would be a
/// count of something nobody named. Dropping the handle is the moment the
/// caller has finished with the session, which is exactly the moment a slot
/// is free.
impl Drop for Session {
    fn drop(&mut self) {
        self.shared.open.fetch_sub(1, Ordering::Relaxed);
        self.shared
            .parked
            .lock()
            .expect("no panic can be held while this lock is taken")
            .remove(&self.quarter_stream_id());
    }
}

impl Session {
    /// Establish a session over an existing QUIC connection.
    ///
    /// The connection must have negotiated ALPN `h3` and must not already
    /// be carrying an HTTP/3 client of its own: this builds one, and two
    /// h3 clients on one QUIC connection would open two control streams,
    /// which RFC 9114 §6.2.1 makes a connection error. That is the whole
    /// of the answer to "can a session share a connection with ordinary
    /// requests" — see `docs/v04-w2-webtransport.md` §4.
    ///
    /// # Order of operations, and why the SETTINGS wait is first
    ///
    /// 1. an h3 client is built on the connection, announcing extended
    ///    CONNECT in its own SETTINGS;
    /// 2. the peer's SETTINGS are **awaited**, and refused if they do not
    ///    announce WebTransport — draft-ietf-webtrans-http3 §3.1 makes
    ///    this a MUST NOT rather than a courtesy, and a client that sent
    ///    the CONNECT anyway would be asking a server that never offered;
    /// 3. the extended CONNECT goes out with `:protocol = webtransport`;
    /// 4. a non-2xx answer is a [`SessionRefused`].
    ///
    /// Nothing is spawned and no timer is read. A caller who wants a bound
    /// on any of this puts one around the future, which is the same answer
    /// `Timeouts` gives one layer up.
    pub async fn connect(conn: quinn::Connection, uri: &http::Uri) -> Result<Self, Error> {
        check_https(uri)?;
        // No authority check follows, and that is a fact about `http::Uri`
        // rather than an omission — see [`NotHttps`].

        let (mut driver, send) = h3::client::builder()
            .enable_extended_connect(true)
            // The client's half of RFC 9297 §2.1: an endpoint may not
            // *receive* HTTP Datagrams it never said it understood, so this
            // line is what lets the peer send any at all. It is the setter
            // §3 of `docs/v04-w2-webtransport.md` found missing for
            // WebTransport and present for datagrams — the whole reason
            // datagrams could be built here and the session announcement
            // could not.
            .enable_datagram(true)
            .build::<h3_quinn::Connection, h3_quinn::OpenStreams, Bytes>(h3_quinn::Connection::new(
                conn.clone(),
            ))
            .await
            .map_err(connect_error)?;

        let peer = settings_announce_webtransport(&mut driver.inner, &send).await?;

        let shared = Arc::new(Shared {
            conn,
            peer_datagrams: peer.datagrams,
            max_sessions: peer.max_sessions,
            open: AtomicU64::new(0),
            parked: Mutex::new(HashMap::new()),
            send,
            _driver: driver,
        });
        // Claimed rather than reserved: the first session is not checked
        // against the peer's limit, for the reason
        // [`open_session`](Self::open_session) gives.
        establish(shared.claim(), uri).await
    }

    /// Open a **second** WebTransport session on the same QUIC connection.
    ///
    /// This is one extended CONNECT more, on the h3 client the first
    /// session already built — not a second h3 client, which is the thing
    /// that does not work (see `Shared`). The new session is a peer of
    /// this one in every way: it has its own CONNECT stream, its own ID,
    /// its own [`closed`](Self::closed), and its own datagrams. Dropping
    /// either leaves the other running; the connection outlives both, and
    /// the last one dropped takes the h3 client with it.
    ///
    /// # The limit is the peer's, and reading it is the whole finding
    ///
    /// draft-ietf-webtrans-http3 §3.1 bounds the number of simultaneous
    /// sessions by the peer's `SETTINGS_WT_MAX_SESSIONS`, whose default is
    /// **0** — *"the endpoint is not willing to receive any WebTransport
    /// sessions"* — and whose value of `1` the draft makes an explicit
    /// *"clients MUST NOT attempt to establish more than one simultaneous
    /// WebTransport session"*. So this method is only correct if that
    /// number can be read, and this crate's own documentation used to say
    /// it could not: `h3::config::Settings` has getters for three flags
    /// and none for this one.
    ///
    /// That was true of `h3::config::Settings` and false of `h3`. The
    /// number arrives in the SETTINGS **frame** that
    /// [`connect`](Self::connect) already awaits and used to discard with a
    /// `_`, and `h3::proto::frame::Settings::get` is public under the same
    /// feature this crate already takes. See `PeerSettings`.
    ///
    /// # What it does not bound
    ///
    /// The **first** session is not checked against this number, and that
    /// is deliberate rather than an oversight. The gate on establishment is
    /// [`NotSupportedByPeer`]'s two flags, for the reason written there;
    /// adding a third condition would refuse every peer that announces
    /// WebTransport and leaves the limit at zero, which is exactly what
    /// `h3`'s own **server** builder produces unless
    /// `max_webtransport_sessions` is called — a peer whose two settings
    /// disagree, and one that works. The same argument
    /// `SETTINGS_H3_DATAGRAM` is kept off the gate by.
    ///
    /// So the number governs how many *more*, which is the only question a
    /// caller of this method is asking.
    ///
    /// # Errors
    ///
    /// [`TooManySessions`] when the peer's limit is already spent, before
    /// anything is sent — the same shape as
    /// [`BadCloseCapsule::ReasonTooLong`] on [`close`](Self::close), and
    /// for the same reason: a peer that enforces the limit answers a
    /// session over it with a *connection* error.
    ///
    /// Otherwise whatever [`connect`](Self::connect) would answer for the
    /// same URI, minus the two it cannot reach: the peer's SETTINGS have
    /// already arrived and already passed, so [`NotSupportedByPeer`] is
    /// behind us.
    pub async fn open_session(&self, uri: &http::Uri) -> Result<Self, Error> {
        check_https(uri)?;
        // The slot is taken **before** the CONNECT goes out, and given
        // back by the guard's `Drop` if it never arrives — so two callers
        // racing this method cannot both find the last slot free.
        establish(self.shared.reserve()?, uri).await
    }

    /// The session's ID — the CONNECT stream's QUIC stream ID.
    pub fn id(&self) -> SessionId {
        self.id
    }

    /// Open a bidirectional WebTransport stream.
    ///
    /// The stream's header — the signal value `0x41` followed by the
    /// session ID, both as QUIC variable-length integers
    /// (draft-ietf-webtrans-http3 §4.2) — is written before the pair is
    /// handed back, so what the caller receives is a stream positioned at
    /// its own first application byte. Writing it here rather than leaving
    /// it to the caller is what makes the returned `quinn::SendStream`
    /// honest: there is no second step whose omission would put
    /// application bytes where a header belongs.
    pub async fn open_bi(&self) -> Result<(quinn::SendStream, quinn::RecvStream), Error> {
        let (mut send, recv) = self
            .shared
            .conn
            .open_bi()
            .await
            .map_err(|e| Error::new(ErrorKind::Connect, e))?;
        let mut header = Vec::with_capacity(16);
        put_varint(&mut header, WEBTRANSPORT_STREAM);
        put_varint(&mut header, self.id.0);
        send.write_all(&header)
            .await
            .map_err(|e| Error::new(ErrorKind::Body, e))?;
        Ok((send, recv))
    }

    /// End the session cleanly, with an application error code and a
    /// reason the peer will see.
    ///
    /// This is draft-ietf-webtrans-http3 §5's clean termination: a
    /// `CLOSE_WEBTRANSPORT_SESSION` capsule (RFC 9297 §3 framing, capsule
    /// type `0x2843`) on the CONNECT stream, followed by the FIN that ends
    /// it. Both halves matter — the capsule is what carries the code and
    /// the reason, and the FIN is what tells a peer reading capsules that
    /// no more are coming.
    ///
    /// # Why this is not the same as dropping the `Session`
    ///
    /// Dropping ends the session too, and until v0.4 it was the only way:
    /// the send half's `Drop` finishes the QUIC stream, which the draft
    /// makes *"semantically equivalent to … an error code of 0 and an
    /// empty error string"*. So `close(0, "")` and `drop` do put the same
    /// meaning on the wire, and a `close` that could only ever say that
    /// would indeed be a second name for one behaviour — which is what
    /// this crate's own documentation said when it declined to write one.
    /// What makes it a different method is the code and the reason: those
    /// have no other way out.
    ///
    /// # Why it takes `&self`, and what it costs
    ///
    /// The send half is **taken** out from under its lock, so the guard is
    /// not held across the write, and the session keeps no "closed" flag.
    /// A second call has nothing to take and answers [`AlreadyClosed`].
    ///
    /// It deliberately does **not** stop [`open_bi`](Self::open_bi) or
    /// [`send_datagram`](Self::send_datagram) afterwards, and that is a
    /// decision rather than an oversight: a flag would be true when *we*
    /// closed and false when the **peer** did, since the peer's close is
    /// only ever noticed by a caller who awaited
    /// [`closed`](Self::closed) — so it would be a guard with one of its
    /// two cases missing, which is exactly the shape this crate deleted
    /// `BadSessionUri::NoAuthority` for. What actually stops a stream
    /// opened afterwards is the peer, which is where the session's state
    /// really lives.
    ///
    /// # Errors
    ///
    /// [`BadCloseCapsule::ReasonTooLong`] when the reason is over the
    /// draft's 1024 bytes, and **nothing is sent** in that case — the
    /// session is still open and still usable. That is not politeness: a
    /// peer that enforces the limit treats an over-long reason as a
    /// protocol error and kills the *connection*, so sending it would turn
    /// a clean close into the one thing a clean close exists to avoid.
    ///
    /// [`AlreadyClosed`] on a second call, and whatever the wire says if
    /// the write itself fails.
    pub async fn close(&self, error_code: u32, reason: &str) -> Result<(), Error> {
        if reason.len() > BadCloseCapsule::MAX_REASON {
            return Err(Error::new(
                ErrorKind::Unsupported,
                BadCloseCapsule::ReasonTooLong { len: reason.len() },
            ));
        }
        // Taken, not borrowed: the guard must not be held across the write
        // below, and dropping the half at the end of this function is what
        // puts the FIN after the capsule.
        let taken = self
            .connect_send
            .lock()
            .expect("no panic can be held while this lock is taken")
            .take();
        let Some(mut writer) = taken else {
            return Err(Error::new(ErrorKind::Unsupported, AlreadyClosed));
        };

        // `send_data` is `h3`'s, and it is the whole of the HTTP/3 framing
        // this needs: RFC 9297 §3.2 puts capsules in the payload of DATA
        // frames, and `send_data` writes one DATA frame around whatever it
        // is given. What is written *inside* it is this crate's, because
        // `h3` 0.0.8 has no capsule of any kind — see `close_capsule`.
        writer
            .send_data(Bytes::from(close_capsule(error_code, reason)))
            .await
            .map_err(stream_error)?;
        // The FIN, and it is a `drop` rather than `h3`'s `finish()` on
        // purpose. `quinn::SendStream::drop` finishes the stream, so the
        // FIN lands either way; `finish()` would additionally write `h3`'s
        // once-per-connection GREASE frame after the capsule, which both
        // peers measured here skip but neither needs to be asked to.
        drop(writer);
        Ok(())
    }

    /// Wait for the session to end, and say whether it ended cleanly.
    ///
    /// `Ok` is a clean close — the peer's `CLOSE_WEBTRANSPORT_SESSION`
    /// capsule, or a bare FIN on the CONNECT stream, which
    /// draft-ietf-webtrans-http3 §5 makes the same thing with zeroes. `Err`
    /// is a session that ended some other way: the CONNECT stream reset,
    /// the QUIC connection lost, or a capsule that could not be read
    /// ([`BadCloseCapsule`]).
    ///
    /// **That difference is the whole point of this method.** Before it,
    /// the only way a caller learned the session was over was a stream
    /// operation failing, which says nothing about whether the peer meant
    /// it. It is the distinction `http-ng-fetch` draws for a WebSocket with
    /// `wasClean`, and the error kind agrees with that one deliberately
    /// rather than inventing a second vocabulary: an unclean end is
    /// [`ErrorKind::Body`], never `ErrorKind::Timeout`, because no
    /// `Timeouts` field is in force on an open session.
    ///
    /// # Nothing is spawned, so nothing observes this but the caller
    ///
    /// The returned future is the only thing reading the CONNECT stream. A
    /// caller that never awaits it never learns the session ended — the
    /// same trade as [`recv_datagram`](Self::recv_datagram) and as
    /// `http-ng-ws-tungstenite`'s keep-alive, and for the same reason: the
    /// session is the caller's object, and the QUIC connection is driven by
    /// the endpoint driver `quinn` already runs.
    ///
    /// # Asking twice gives the same answer
    ///
    /// The end is a fact, not a queue, so it is remembered. Without that, a
    /// second call would read the already-ended stream, see EOF, and report
    /// the *bare-FIN* close — turning a session that was reset, or one that
    /// closed with code 7, into `{ code: 0, reason: "" }` on the second
    /// asking.
    ///
    /// # Capsules that are not a close
    ///
    /// Any other capsule type is skipped and the wait continues, which is
    /// RFC 9297 §3.2's requirement that unknown capsule types be ignored.
    /// `DRAIN_WEBTRANSPORT_SESSION` is one of them: it is skipped rather
    /// than surfaced, because a drain is not an end and this method answers
    /// one question. `docs/v04-w2-capsules.md` §7 says what surfacing it
    /// would need.
    pub async fn closed(&self) -> Result<SessionClose, Error> {
        poll_fn(|cx| {
            self.connect_recv
                .lock()
                .expect("no panic can be held while this lock is taken")
                .poll(cx)
        })
        .await
    }

    /// The largest datagram payload that can be sent **right now**, or
    /// `None` if datagrams cannot be sent on this session at all.
    ///
    /// The number is quinn's own limit for the QUIC DATAGRAM frame minus
    /// this session's header — the Quarter Stream ID varint, 1 to 8 bytes
    /// depending on the session — so it is a budget for the caller's own
    /// bytes and not for the frame. It moves with the path MTU estimate,
    /// which is why it is a method rather than a field.
    ///
    /// # `None` has two causes and they are one answer
    ///
    /// Either the peer's SETTINGS carried no `SETTINGS_H3_DATAGRAM`
    /// (RFC 9297 §2.1 forbids sending HTTP Datagrams to an endpoint that
    /// did not ask for them), or the QUIC connection carries no datagrams
    /// — the peer sent no `max_datagram_frame_size` transport parameter,
    /// or the local endpoint disabled them. Both mean *you cannot send*,
    /// which is the whole question this method asks, so they share an
    /// answer here; [`send_datagram`](Self::send_datagram) tells them
    /// apart, because there the caller has already decided to send and the
    /// reason is actionable.
    ///
    /// Note that this is a claim about **sending**. Receiving is governed
    /// by what *this* endpoint announced, which [`Session::connect`]
    /// always sets, so a session whose `max_datagram_size` is `None` can
    /// still have datagrams arrive on it.
    pub fn max_datagram_size(&self) -> Option<usize> {
        if !self.shared.peer_datagrams {
            return None;
        }
        self.shared
            .conn
            .max_datagram_size()?
            .checked_sub(varint_len(self.quarter_stream_id()))
    }

    /// Send one datagram. It may be lost, and that is the point.
    ///
    /// WebTransport datagrams are the feature streams do not already give:
    /// unreliable, unordered, and free of head-of-line blocking. Nothing
    /// here retransmits, acknowledges or orders them, and a caller that
    /// needs any of those wants [`open_bi`](Self::open_bi) instead.
    ///
    /// It is not `async`, and that is the shape rather than an oversight:
    /// there is no flush to await and no delivery to wait for. A datagram
    /// that does not fit in the send buffer displaces an older one — see
    /// `quinn::Connection::send_datagram` — which is the same trade a UDP
    /// socket makes and the reason the method can answer immediately.
    ///
    /// # On the wire
    ///
    /// The QUIC DATAGRAM frame's payload is an HTTP/3 Datagram
    /// (RFC 9297 §2.1): the Quarter Stream ID — this session's ID divided
    /// by four — as a variable-length integer, then the payload
    /// unchanged. draft-ietf-webtrans-http3 adds no framing of its own, so
    /// there is no context ID and no length: the datagram *is* the
    /// remainder of the frame.
    ///
    /// **A stream and a datagram name the same session differently**, and
    /// the asymmetry is RFC 9297's rather than ours: a stream header
    /// carries the session's full stream ID after the `0x41` signal, a
    /// datagram carries it shifted right by two. Getting that shift wrong
    /// addresses the datagram to a session that is not this one.
    ///
    /// # Errors
    ///
    /// [`DatagramsUnavailable`] when the session cannot send at all, and
    /// [`DatagramTooLarge`] when this particular payload does not fit —
    /// the second carries the budget that
    /// [`max_datagram_size`](Self::max_datagram_size) would have reported.
    /// The first has two variants and both are reachable from here: the
    /// peer's SETTINGS are checked on this line, and the connection's own
    /// answer arrives from quinn.
    pub fn send_datagram(&self, payload: Bytes) -> Result<(), Error> {
        if !self.shared.peer_datagrams {
            return Err(Error::new(
                ErrorKind::Unsupported,
                DatagramsUnavailable::NotAnnouncedByPeer,
            ));
        }
        // The size check is the only thing this crate can answer better
        // than quinn: it knows the header it is about to add. Whether the
        // connection carries datagrams at all is quinn's answer and is
        // deliberately left to it — a second check here would be a branch
        // no test could reach, because `max_datagram_size` being `None`
        // and `send_datagram` answering `UnsupportedByPeer` are the same
        // connection saying the same thing.
        if let Some(budget) = self.max_datagram_size()
            && payload.len() > budget
        {
            return Err(Error::new(
                ErrorKind::Body,
                DatagramTooLarge {
                    payload: payload.len(),
                    budget,
                },
            ));
        }

        let quarter = self.quarter_stream_id();
        let mut frame = Vec::with_capacity(varint_len(quarter) + payload.len());
        put_varint(&mut frame, quarter);
        frame.extend_from_slice(&payload);
        self.shared
            .conn
            .send_datagram(Bytes::from(frame))
            .map_err(send_datagram_error)
    }

    /// Wait for the next datagram addressed to this session.
    ///
    /// What comes back is the payload alone: the Quarter Stream ID has
    /// been read off and checked. Nothing is spawned — the returned future
    /// is the only thing reading datagrams, so a caller that stops calling
    /// this stops receiving, and quinn's own receive buffer drops the
    /// oldest when it fills.
    ///
    /// # A sibling's datagram is handed over, not dropped
    ///
    /// There is one datagram queue per QUIC connection and it is quinn's,
    /// so whichever session is being polled reads *everything* — including
    /// what belongs to a session opened by
    /// [`open_session`](Self::open_session). Until v0.4 this method
    /// discarded anything that was not its own, which was exactly right
    /// while a connection could hold only one session and is silent data
    /// loss now. So a foreign datagram is parked for its owner and its
    /// owner is woken; see `Shared::hand_over` for what happens when the
    /// owner is not listening, which is still the discard, and still
    /// RFC 9297 §2.1's *"SHALL either drop that datagram silently or
    /// buffer it temporarily"*.
    ///
    /// # What it silently drops, and why silence is right
    ///
    /// A datagram whose Quarter Stream ID names **no** session on this
    /// connection, and one too short to carry a Quarter Stream ID at all,
    /// are discarded and the wait continues — the RFC's sentence above,
    /// and an error here would be inventing a failure it forbids.
    ///
    /// The one thing it does not do is the RFC's other arm: an ID above
    /// `2^60 - 1` is illegal and "MUST be treated as an HTTP/3 connection
    /// error of type H3_DATAGRAM_ERROR". Such an ID cannot equal any
    /// session's, so it is dropped rather than escalated; closing the
    /// connection from here is recorded as not done in
    /// `docs/v04-w2-datagrams.md` rather than half-done.
    pub async fn recv_datagram(&self) -> Result<Bytes, Error> {
        let quarter = self.quarter_stream_id();
        loop {
            // A sibling may have read one for us while we were away.
            if let Some(payload) = self.shared.collect(quarter) {
                return Ok(payload);
            }
            // `read_datagram` is held across polls rather than rebuilt on
            // each one, and that is load-bearing: quinn's future registers
            // on a `Notify` when it is polled and **deregisters when it is
            // dropped**, so a version that made a fresh one per poll would
            // return `Pending` with nothing left to wake it.
            let mut read = std::pin::pin!(self.shared.conn.read_datagram());
            let frame = poll_fn(|cx| {
                // Registered before the read is polled, so a sibling that
                // hands one over between the two has somewhere to wake.
                if self.shared.wait_for_a_handover(quarter, cx) {
                    return Poll::Ready(None);
                }
                read.as_mut().poll(cx).map(Some)
            })
            .await;
            let Some(frame) = frame else {
                // A sibling parked one for us. Round again to collect it,
                // rather than reaching into the map twice in one pass.
                continue;
            };
            let frame = frame.map_err(|e| Error::new(ErrorKind::Connect, e))?;
            let Some((id, header)) = get_varint(&frame) else {
                continue;
            };
            if id == quarter {
                return Ok(frame.slice(header..));
            }
            self.shared.hand_over(id, frame.slice(header..));
        }
    }

    /// This session's Quarter Stream ID — RFC 9297 §2.1's "the value of
    /// the client-initiated bidirectional stream that this datagram is
    /// associated with divided by four".
    ///
    /// A shift rather than a division because it is two bits of stream
    /// type, and exact rather than lossy because a client-initiated
    /// bidirectional stream ID is a multiple of four by construction —
    /// which the CONNECT stream always is.
    fn quarter_stream_id(&self) -> u64 {
        self.id.0 >> 2
    }
}

/// WebTransport runs over HTTP/3, which is always TLS.
///
/// Asked before anything is built or sent, by both entry points, because
/// the answer does not depend on a connection — see [`NotHttps`].
fn check_https(uri: &http::Uri) -> Result<(), Error> {
    if uri.scheme_str() == Some("https") {
        return Ok(());
    }
    Err(Error::new(
        ErrorKind::Unsupported,
        NotHttps {
            scheme: uri.scheme_str().unwrap_or("(no scheme)").to_string(),
        },
    ))
}

/// Send the extended CONNECT and make a [`Session`] out of what comes back.
///
/// The half of [`Session::connect`] that [`Session::open_session`] repeats
/// verbatim: everything above it — the h3 client, the SETTINGS wait, the
/// gate — is per **connection** and happens once.
async fn establish(slot: Slot, uri: &http::Uri) -> Result<Session, Error> {
    // Cloned rather than borrowed: `send_request` takes `&mut self`, and
    // `SendRequest` is `Clone` exactly so that more than one request can be
    // in flight on one connection. `h3` counts the clones and closes the
    // connection when the last drops, which is why `Shared` keeps one
    // that is never used to send.
    let mut send = slot.shared().send.clone();

    // `Protocol` in the request's extensions is the whole of the
    // extended-CONNECT mechanism from this side: `h3`'s
    // `Pseudo::request` reads `ext.get::<Protocol>()` **only when the
    // method is CONNECT**, so the method and the extension are one
    // fact expressed twice and neither is decoration.
    let req = http::Request::builder()
        .method(http::Method::CONNECT)
        .uri(uri.clone())
        .extension(h3::ext::Protocol::WEB_TRANSPORT)
        .body(())
        .map_err(|e| Error::new(ErrorKind::Connect, e))?;

    let mut stream = send.send_request(req).await.map_err(stream_error)?;
    // Deliberately no `finish()`: the CONNECT stream stays open for
    // the life of the session, and finishing it is how the draft says
    // "the session is over".
    let resp = stream.recv_response().await.map_err(stream_error)?;
    if !resp.status().is_success() {
        return Err(Error::new(
            ErrorKind::Connect,
            SessionRefused {
                status: resp.status(),
            },
        ));
    }

    // The full stream ID, not `StreamId::index()`. draft §4.2 says the
    // Session ID *is* the Stream ID of the CONNECT stream, and the two
    // differ by two bits of type: `h3`'s own `From<StreamId> for
    // SessionId` uses `index()`, which is a fact about that crate's
    // server side rather than one this client can copy.
    let id = SessionId(stream.id().into_inner());
    // RFC 9000 §2.1: the two halves of a bidirectional stream are
    // independent, and `h3` hands them over as such. The session needs
    // them apart rather than together, because closing and being closed
    // are two things that happen at times nobody coordinates.
    let (writer, reader) = stream.split();

    // Before the handle exists, so that a sibling already inside
    // `recv_datagram` can hand this session a datagram from the first one
    // that arrives rather than from the first one after it asks.
    slot.shared().register(id.0 >> 2);

    Ok(Session {
        shared: slot.keep(),
        id,
        connect_send: Mutex::new(Some(writer)),
        connect_recv: Mutex::new(CloseWatch::new(reader)),
    })
}

/// One session's place in the peer's `SETTINGS_WT_MAX_SESSIONS` budget,
/// taken before the CONNECT goes out.
///
/// It exists so that the count is decided in **one** place under a
/// compare-and-exchange rather than in a check followed later by an
/// increment: two callers racing [`Session::open_session`] on the last free
/// slot would otherwise both see it free. Dropping it without
/// [`keep`](Self::keep) gives the slot back, which is what happens when the
/// CONNECT it was taken for never arrives.
struct Slot(Option<Arc<Shared>>);

impl Slot {
    fn shared(&self) -> &Arc<Shared> {
        self.0
            .as_ref()
            .expect("a slot holds its connection until kept")
    }

    /// Turn the reservation into a real session's share of the connection.
    /// The count is not decremented here; [`Session`]'s `Drop` does that.
    fn keep(mut self) -> Arc<Shared> {
        self.0.take().expect("a slot is kept at most once")
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        if let Some(shared) = &self.0 {
            shared.open.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

impl Shared {
    /// Take a slot without asking the peer's limit — the first session's
    /// path. See [`Session::open_session`] for why the first is exempt.
    fn claim(self: &Arc<Self>) -> Slot {
        self.open.fetch_add(1, Ordering::Relaxed);
        Slot(Some(self.clone()))
    }

    /// Take a slot if the peer's limit has one left.
    fn reserve(self: &Arc<Self>) -> Result<Slot, Error> {
        let mut open = self.open.load(Ordering::Relaxed);
        loop {
            if open >= self.max_sessions {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    TooManySessions {
                        limit: self.max_sessions,
                        open,
                    },
                ));
            }
            match self.open.compare_exchange_weak(
                open,
                open + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Ok(Slot(Some(self.clone()))),
                Err(actual) => open = actual,
            }
        }
    }

    /// Take the datagram a sibling read on this session's behalf, if there
    /// is one, and forget the waker with it — the caller is awake.
    fn collect(&self, quarter: u64) -> Option<Bytes> {
        let mut parked = self
            .parked
            .lock()
            .expect("no panic can be held while this lock is taken");
        let waiting = parked.get_mut(&quarter)?;
        waiting.waker = None;
        waiting.held.take()
    }

    /// Register `cx` as the thing to wake when a sibling reads a datagram
    /// for `quarter`, and say whether one is already there.
    ///
    /// Registering is what makes a session *listening*, and
    /// [`hand_over`](Self::hand_over) drops a datagram for a session that
    /// is not — which is RFC 9297 §2.1's own answer for an ID naming a
    /// stream the receiver does not know, applied to one it knows and
    /// nobody is reading.
    fn wait_for_a_handover(&self, quarter: u64, cx: &Context<'_>) -> bool {
        let mut parked = self
            .parked
            .lock()
            .expect("no panic can be held while this lock is taken");
        // `entry`, because a session registered at establishment and
        // removed at `Drop` is always here — this is a `get_mut` that
        // cannot be `None` rather than a second registration point.
        let waiting = parked.entry(quarter).or_default();
        if waiting.held.is_some() {
            return true;
        }
        match &waiting.waker {
            Some(known) if known.will_wake(cx.waker()) => {}
            _ => waiting.waker = Some(cx.waker().clone()),
        }
        false
    }

    /// Give a datagram to the session it is addressed to, and drop it if no
    /// session on this connection has that ID.
    ///
    /// `get_mut`, not `entry`, and that is the whole of RFC 9297 §2.1's
    /// *"a Quarter Stream ID naming a stream the receiver does not know"*:
    /// the map's keys **are** this connection's live sessions, put there by
    /// `establish` and taken out by [`Session`]'s `Drop`, so an ID that is
    /// not one of them is dropped rather than stored — which also means a
    /// peer cannot grow this map by inventing IDs.
    ///
    /// **At most one datagram is held per session**, and the rest of a
    /// burst is dropped. That is the right shape for a transport whose
    /// whole promise is that a datagram may be lost: a queue here would be
    /// a reliability this layer does not have and cannot honestly offer,
    /// and an unbounded one would let a session that stopped reading
    /// consume memory a peer chooses the size of.
    fn hand_over(&self, quarter: u64, payload: Bytes) {
        let mut parked = self
            .parked
            .lock()
            .expect("no panic can be held while this lock is taken");
        let Some(waiting) = parked.get_mut(&quarter) else {
            return;
        };
        if waiting.held.is_none() {
            waiting.held = Some(payload);
        }
        // Woken if anyone is inside `recv_datagram`; collected on the next
        // call if not. The two are the same code path — see `collect`.
        if let Some(waker) = waiting.waker.take() {
            waker.wake();
        }
    }

    /// Record that a session with this Quarter Stream ID exists on this
    /// connection, which is what makes [`hand_over`](Self::hand_over) able
    /// to tell *not this session* from *no session at all*.
    fn register(&self, quarter: u64) {
        self.parked
            .lock()
            .expect("no panic can be held while this lock is taken")
            .entry(quarter)
            .or_default();
    }
}

/// One session's side of the hand-off in `Shared::hand_over`.
#[derive(Default)]
struct Parked {
    /// A datagram a sibling read for this session and has not collected.
    held: Option<Bytes>,
    /// The waker of whoever is inside [`Session::recv_datagram`], or
    /// `None` when nobody is.
    waker: Option<Waker>,
}

/// The signal value that begins a client-initiated bidirectional
/// WebTransport stream — draft-ietf-webtrans-http3 §4.2, and the same
/// number `h3` knows as `FrameType::WEBTRANSPORT_BI_STREAM`.
const WEBTRANSPORT_STREAM: u64 = 0x41;

/// The capsule type of `CLOSE_WEBTRANSPORT_SESSION`,
/// draft-ietf-webtrans-http3 §5.
///
/// Two implementations sharing no code with each other or with this one
/// agree on the number and on what follows it, and both were read before
/// any of this was written: `wtransport-proto` 0.7.2's
/// `CAPSULE_TYPE_CLOSE_WEBTRANSPORT_SESSION` and `web-transport-proto`
/// 0.6.0's `CLOSE_WEBTRANSPORT_SESSION_TYPE`.
const CLOSE_WEBTRANSPORT_SESSION: u64 = 0x2843;

/// Build the one capsule this crate writes.
///
/// RFC 9297 §3 gives the outer two fields — a capsule type and a length,
/// both QUIC variable-length integers — and draft-ietf-webtrans-http3 §5
/// gives the payload: a 32-bit application error code, then the reason as
/// UTF-8. Nothing else; there is no version, no flags and no padding.
///
/// **`h3` 0.0.8 contains no capsule code at all**, which is why this is
/// here. Measured rather than assumed: `grep -rn capsule` over that crate
/// finds one doc comment on the `H3_DATAGRAM_ERROR` code and nothing else,
/// `h3-datagram` 0.0.2 finds nothing, and `h3-webtransport` 0.1.2 — the
/// crate whose name promises it — finds nothing either. What `h3` *does*
/// supply is the layer above: `RequestStream::send_data` writes the DATA
/// frame that RFC 9297 §3.2 says capsules travel in.
///
/// The one crate in the ecosystem that would supply this is
/// `web-transport-proto` 0.6.0, whose encoder is correct — and it is **48
/// crates**, ten of them `url` and the ICU stack this workspace spent a
/// whole task removing from `http-ng-proto`, against this crate's 49 in
/// total. `docs/v04-w2-capsules.md` §3.
fn close_capsule(error_code: u32, reason: &str) -> Vec<u8> {
    let payload = 4 + reason.len();
    let mut capsule = Vec::with_capacity(
        varint_len(CLOSE_WEBTRANSPORT_SESSION) + varint_len(payload as u64) + payload,
    );
    put_varint(&mut capsule, CLOSE_WEBTRANSPORT_SESSION);
    put_varint(&mut capsule, payload as u64);
    // Big-endian, because RFC 9000 §16's varints are and draft §5's fixed
    // 32-bit field is: network byte order everywhere on this wire.
    capsule.extend_from_slice(&error_code.to_be_bytes());
    capsule.extend_from_slice(reason.as_bytes());
    capsule
}

/// What one capsule at the front of `buf` turned out to be.
#[derive(Debug, PartialEq, Eq)]
enum Taken {
    /// A `CLOSE_WEBTRANSPORT_SESSION` capsule: the session is over.
    Close(SessionClose),
    /// A capsule of some other type, consumed and thrown away — RFC 9297
    /// §3.2's *"An endpoint that receives a capsule with an unknown
    /// capsule type MUST silently skip over that capsule."*
    Skipped,
    /// Nothing is wrong; there are simply not enough bytes yet.
    More,
    /// A close capsule that could not be honoured.
    Bad(BadCloseCapsule),
}

/// Take the capsule at the front of `buf`, if a whole one is there.
///
/// The length field is what makes skipping possible at all: an unknown
/// capsule is *skipped over*, not guessed at, and that only works because
/// RFC 9297 §3 puts its length in front of it. It is also what makes this
/// function consume exactly one capsule — a DATA frame may carry several,
/// and a capsule may straddle two DATA frames, so neither a frame boundary
/// nor the end of the buffer is a capsule boundary.
fn take_capsule(buf: &mut Vec<u8>) -> Taken {
    let Some((kind, kind_len)) = get_varint(buf) else {
        return Taken::More;
    };
    let Some((length, length_len)) = get_varint(&buf[kind_len..]) else {
        return Taken::More;
    };
    let start = kind_len + length_len;
    let Some(end) = usize::try_from(length)
        .ok()
        .and_then(|l| start.checked_add(l))
    else {
        // A length no `usize` can hold cannot be waited for either, and
        // the stream will end long before it arrives.
        return Taken::More;
    };
    if buf.len() < end {
        return Taken::More;
    }
    let taken = if kind == CLOSE_WEBTRANSPORT_SESSION {
        read_close(&buf[start..end])
    } else {
        Taken::Skipped
    };
    buf.drain(..end);
    taken
}

/// Read a `CLOSE_WEBTRANSPORT_SESSION` capsule's payload.
fn read_close(payload: &[u8]) -> Taken {
    let Some((code, reason)) = payload.split_at_checked(4) else {
        return Taken::Bad(BadCloseCapsule::NoErrorCode {
            payload: payload.len(),
        });
    };
    if reason.len() > BadCloseCapsule::MAX_REASON {
        return Taken::Bad(BadCloseCapsule::ReasonTooLong { len: reason.len() });
    }
    let Ok(reason) = std::str::from_utf8(reason) else {
        return Taken::Bad(BadCloseCapsule::ReasonNotUtf8);
    };
    Taken::Close(SessionClose {
        code: u32::from_be_bytes(code.try_into().expect("split_at_checked(4) gave four")),
        reason: reason.to_owned(),
    })
}

/// The receive half of the CONNECT stream, and the answer once it has one.
struct CloseWatch {
    stream: ConnectRecv,
    /// Bytes read past the end of the last complete capsule. A capsule can
    /// straddle two DATA frames, so what a frame ends with is not
    /// necessarily what a capsule ends with.
    buf: Vec<u8>,
    /// The session's end, once known. Remembered rather than re-read: see
    /// [`Session::closed`].
    ended: Option<Result<SessionClose, Error>>,
}

impl CloseWatch {
    fn new(stream: ConnectRecv) -> Self {
        Self {
            stream,
            buf: Vec::new(),
            ended: None,
        }
    }

    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<Result<SessionClose, Error>> {
        if let Some(ended) = &self.ended {
            return Poll::Ready(ended.clone());
        }
        let Poll::Ready(ended) = self.poll_stream(cx) else {
            return Poll::Pending;
        };
        self.ended = Some(ended.clone());
        Poll::Ready(ended)
    }

    fn poll_stream(&mut self, cx: &mut Context<'_>) -> Poll<Result<SessionClose, Error>> {
        loop {
            match take_capsule(&mut self.buf) {
                Taken::Close(close) => return Poll::Ready(Ok(close)),
                Taken::Bad(bad) => return Poll::Ready(Err(Error::new(ErrorKind::Body, bad))),
                Taken::Skipped => continue,
                Taken::More => {}
            }
            match self.stream.poll_recv_data(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Ok(Some(mut data))) => {
                    while data.has_remaining() {
                        let chunk = data.chunk();
                        self.buf.extend_from_slice(chunk);
                        let read = chunk.len();
                        data.advance(read);
                    }
                }
                // The stream ended. At a capsule boundary that is draft §5's
                // clean close with zeroes; part way through one it is not a
                // close at all, because what the peer meant to say is gone
                // with the rest of the capsule.
                Poll::Ready(Ok(None)) => {
                    return Poll::Ready(if self.buf.is_empty() {
                        Ok(SessionClose::ENDED_WITHOUT_A_CAPSULE)
                    } else {
                        Err(Error::new(
                            ErrorKind::Body,
                            BadCloseCapsule::Truncated {
                                have: self.buf.len(),
                            },
                        ))
                    });
                }
                // A reset CONNECT stream or a lost connection: the session
                // is over and the peer never said so. This is the arm the
                // whole method exists to keep apart from the two above.
                Poll::Ready(Err(e)) => {
                    return Poll::Ready(Err(Error::new(
                        ErrorKind::Body,
                        std::io::Error::other(e.to_string()),
                    )));
                }
            }
        }
    }
}

/// Await the peer's SETTINGS and refuse unless they announce WebTransport.
///
/// The wait is over `ConnectionInner::poll_control` rather than over the
/// driver's `poll_close`, and that is not a shortcut: `settings()` answers
/// with `Settings::default()` before the frame has arrived, and every flag
/// in that default is `false` — so "the peer has not answered yet" and
/// "the peer said no" are the same value, and only the *arrival* of the
/// frame tells them apart. `poll_control` resolving to `Frame::Settings`
/// is that arrival; `h3` has already enforced by then that it was the
/// first frame on the control stream (RFC 9114 §6.2.1) and has already
/// stored it, so the read below is of the peer's real answer.
///
/// Returns `PeerSettings`: the two things this crate reads off that frame
/// and keeps for the connection's life, neither of which is a condition on
/// the gate.
async fn settings_announce_webtransport(
    inner: &mut ConnectionInner<h3_quinn::Connection, Bytes>,
    send: &h3::client::SendRequest<h3_quinn::OpenStreams, Bytes>,
) -> Result<PeerSettings, Error> {
    // The frame is **kept**, where until v0.4 it was matched with a `_`.
    // See [`PeerSettings::max_sessions`].
    let frame = match poll_fn(|cx| inner.poll_control(cx)).await {
        Ok(Frame::Settings(frame)) => frame,
        // Unreachable rather than impossible, and typed rather than
        // `unwrap`ed: `h3` turns any other first frame into
        // `H3_MISSING_SETTINGS` on the line above, so this arm exists for
        // the version of `h3` that stops doing so.
        Ok(other) => {
            return Err(Error::new(
                ErrorKind::Connect,
                std::io::Error::other(format!("first control frame was {other:?}, not SETTINGS")),
            ));
        }
        Err(e) => return Err(connect_error(e)),
    };

    let settings = send.settings();
    let announced = NotSupportedByPeer {
        webtransport: settings.enable_webtransport(),
        extended_connect: settings.enable_extended_connect(),
    };
    if !announced.webtransport || !announced.extended_connect {
        return Err(Error::new(ErrorKind::Unsupported, announced));
    }
    Ok(PeerSettings {
        datagrams: settings.enable_datagram(),
        max_sessions: frame.get(SettingId::WEBTRANSPORT_MAX_SESSIONS).unwrap_or(0),
    })
}

/// What this crate keeps out of the peer's SETTINGS frame, and where each
/// half comes from.
///
/// # Two sources for two values, and the second is the finding
///
/// `datagrams` is read through `h3::config::Settings`, which has a getter
/// for it. `max_sessions` is read off the **frame**, because that struct
/// has no getter for `max_webtransport_sessions` — its field is
/// `pub(crate)` — and `docs/v04-w2-webtransport.md` §3(c) recorded that as
/// *"the peer's `max_webtransport_sessions` cannot be read"*.
///
/// **That was true of `h3::config::Settings` and false of `h3`.** The
/// SETTINGS frame this function already awaits *is*
/// `h3::proto::frame::Settings`, whose `get` is `pub` and whose
/// `SettingId::WEBTRANSPORT_MAX_SESSIONS` is a `pub const`, all under the
/// `i-implement-a-third-party-backend` feature this crate already takes for
/// the two facts one paragraph up. The value was arriving, being parsed and
/// being thrown away by the `_` in this function's own match.
///
/// The default when the setting is absent is `0`, which is the draft's own
/// default and means *no sessions* — see [`TooManySessions`].
///
/// Neither value is a condition on the establishment gate. A server may
/// honestly announce WebTransport and not datagrams, `h3`'s own server
/// builder can be configured into exactly that state, and a caller that
/// only ever opens streams would be refused a session that works; the same
/// argument, one setting over, is why a limit of zero does not refuse the
/// first session. What they decide instead is
/// [`Session::max_datagram_size`] and [`Session::open_session`].
struct PeerSettings {
    /// `SETTINGS_H3_DATAGRAM`.
    datagrams: bool,
    /// `SETTINGS_WT_MAX_SESSIONS`.
    max_sessions: u64,
}

/// QUIC's variable-length integer encoding, RFC 9000 §16.
///
/// Written out rather than borrowed from `h3::proto::varint`, because the
/// two bytes this crate puts in front of every stream are the whole of its
/// wire format and a dependency on another crate's private-by-intent
/// module is a worse place for them than fifteen lines here.
///
/// Values of 2^62 and above have no encoding. There is no such value to
/// encode: `WEBTRANSPORT_STREAM` is a constant and a QUIC stream ID is
/// itself a variable-length integer, so the branch is a `debug_assert`'s
/// worth of impossibility rather than an error the caller could act on —
/// it saturates to the largest encodable value, which cannot be mistaken
/// for a valid ID.
fn put_varint(buf: &mut Vec<u8>, v: u64) {
    if v < (1 << 6) {
        buf.push(v as u8);
    } else if v < (1 << 14) {
        buf.extend_from_slice(&((v as u16) | (0b01 << 14)).to_be_bytes());
    } else if v < (1 << 30) {
        buf.extend_from_slice(&((v as u32) | (0b10 << 30)).to_be_bytes());
    } else {
        buf.extend_from_slice(&(v.min((1 << 62) - 1) | (0b11 << 62)).to_be_bytes());
    }
}

/// The number of bytes [`put_varint`] will write for `v`.
///
/// A second statement of the same branch points, which is why the test
/// beside `varints_match_rfc_9000_a1` checks the two agree over the same
/// corpus rather than checking this one alone: a datagram whose header
/// length is computed here and written there would put the payload at the
/// wrong offset if they ever disagreed.
fn varint_len(v: u64) -> usize {
    if v < (1 << 6) {
        1
    } else if v < (1 << 14) {
        2
    } else if v < (1 << 30) {
        4
    } else {
        8
    }
}

/// Read one QUIC variable-length integer, and say how many bytes it took.
///
/// `None` when the buffer is empty or ends inside the integer — which for
/// a datagram means a frame too short to be an HTTP Datagram at all. The
/// caller decides what that means; see [`Session::recv_datagram`].
fn get_varint(buf: &[u8]) -> Option<(u64, usize)> {
    let first = *buf.first()?;
    let len = 1usize << (first >> 6);
    if buf.len() < len {
        return None;
    }
    let mut v = u64::from(first & 0x3f);
    for b in &buf[1..len] {
        v = (v << 8) | u64::from(*b);
    }
    Some((v, len))
}

fn send_datagram_error(e: quinn::SendDatagramError) -> Error {
    match e {
        // quinn's own `max_datagram_size` conflates these two into a
        // `None`, so this crate conflates them into one variant rather
        // than making the error and the budget disagree about how many
        // answers there are.
        quinn::SendDatagramError::UnsupportedByPeer | quinn::SendDatagramError::Disabled => {
            Error::new(
                ErrorKind::Unsupported,
                DatagramsUnavailable::NotOnTheConnection,
            )
        }
        // Reachable only as a race against the path MTU estimate: the size
        // was checked against `max_datagram_size` a few instructions ago.
        quinn::SendDatagramError::TooLarge => Error::new(ErrorKind::Body, e),
        quinn::SendDatagramError::ConnectionLost(_) => Error::new(ErrorKind::Connect, e),
    }
}

fn connect_error(e: h3::error::ConnectionError) -> Error {
    Error::new(ErrorKind::Connect, std::io::Error::other(e.to_string()))
}

fn stream_error(e: h3::error::StreamError) -> Error {
    Error::new(ErrorKind::Connect, std::io::Error::other(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 9000 §A.1's own examples, plus the two boundaries between
    /// lengths. The 0x41 in the first row is this crate's only constant:
    /// it is 65, which is one past the largest 1-byte value, so a varint
    /// encoder that took the short branch for it would put a single byte
    /// on the wire and every WebTransport stream this crate opens would be
    /// unreadable.
    /// RFC 9000 §A.1's examples, the boundaries between encoded lengths,
    /// and the one constant this crate has of its own.
    const CASES: &[(u64, &[u8])] = &[
        (0x41, &[0x40, 0x41]),
        (0, &[0x00]),
        (37, &[0x25]),
        (63, &[0x3f]),
        (64, &[0x40, 0x40]),
        (15293, &[0x7b, 0xbd]),
        (16383, &[0x7f, 0xff]),
        (16384, &[0x80, 0x00, 0x40, 0x00]),
        (494_878_333, &[0x9d, 0x7f, 0x3e, 0x7d]),
        (1_073_741_823, &[0xbf, 0xff, 0xff, 0xff]),
        (
            1_073_741_824,
            &[0xc0, 0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0x00],
        ),
        (
            151_288_809_941_952_652,
            &[0xc2, 0x19, 0x7c, 0x5e, 0xff, 0x14, 0xe8, 0x8c],
        ),
    ];

    #[test]
    fn varints_match_rfc_9000_a1() {
        for (value, expected) in CASES {
            let mut buf = Vec::new();
            put_varint(&mut buf, *value);
            assert_eq!(&buf[..], *expected, "encoding {value}");
        }
    }

    /// The two must agree, because a datagram's payload begins where the
    /// header ends: [`varint_len`] decides where
    /// [`Session::recv_datagram`]'s budget arithmetic thinks that is, and
    /// [`put_varint`] decides where it actually is. A disagreement is a
    /// datagram whose payload is short by a byte or carries one of the
    /// header's, and neither end would report an error.
    #[test]
    fn varint_len_agrees_with_the_encoder() {
        for (value, expected) in CASES {
            assert_eq!(varint_len(*value), expected.len(), "length of {value}");
        }
    }

    /// The capsule this crate writes, byte for byte, against vectors
    /// written by somebody else.
    ///
    /// The first row is `wtransport-proto` 0.7.2's own unit test for
    /// `CloseWebTransportSession` — `Frame::new_data(vec![104, 67, 4, 0, 0,
    /// 0, 0u8])`, which it asserts parses to error code 0 and an empty
    /// reason. The other three were **produced** by `web-transport-proto`
    /// 0.6.0's encoder and copied out of its output, which is the
    /// technique `http-ng-proto`'s 96-pair URI corpus uses: measured
    /// first, pinned second. Neither crate is in this workspace's graph
    /// (`web-transport-proto` alone is 48 crates, ten of them `url` and
    /// ICU), and neither shares any code with `h3` or with this file.
    ///
    /// What the rows are for: `0x68 0x43` is the two-byte QUIC varint for
    /// capsule type `0x2843`, the byte after it is the Capsule Length, and
    /// the four after **that** are draft §5's application error code, big
    /// endian, with the reason's UTF-8 behind them.
    #[test]
    fn close_capsules_match_two_other_implementations() {
        const VECTORS: &[(u32, &str, &[u8])] = &[
            (0, "", &[0x68, 0x43, 0x04, 0x00, 0x00, 0x00, 0x00]),
            (1, "x", &[0x68, 0x43, 0x05, 0x00, 0x00, 0x00, 0x01, 0x78]),
            (
                0x1234_5678,
                "so long, and thanks",
                &[
                    0x68, 0x43, 0x17, 0x12, 0x34, 0x56, 0x78, 0x73, 0x6f, 0x20, 0x6c, 0x6f, 0x6e,
                    0x67, 0x2c, 0x20, 0x61, 0x6e, 0x64, 0x20, 0x74, 0x68, 0x61, 0x6e, 0x6b, 0x73,
                ],
            ),
            (
                u32::MAX,
                "the whole thing",
                &[
                    0x68, 0x43, 0x13, 0xff, 0xff, 0xff, 0xff, 0x74, 0x68, 0x65, 0x20, 0x77, 0x68,
                    0x6f, 0x6c, 0x65, 0x20, 0x74, 0x68, 0x69, 0x6e, 0x67,
                ],
            ),
        ];
        for (code, reason, expected) in VECTORS {
            assert_eq!(&close_capsule(*code, reason), expected, "{code}/{reason:?}");
        }
    }

    /// What the encoder writes, the decoder reads back — including the
    /// lengths that decide where the payload starts.
    #[test]
    fn take_capsule_reads_back_what_close_capsule_wrote() {
        for (code, reason) in [
            (0u32, ""),
            (1, "x"),
            (u32::MAX, "the whole thing"),
            (
                42,
                "a reason of sixty-four bytes, which is where a varint grows: ok!",
            ),
        ] {
            let mut buf = close_capsule(code, reason);
            let trailing = b"and the next capsule's first byte".to_vec();
            buf.extend_from_slice(&trailing);
            assert_eq!(
                take_capsule(&mut buf),
                Taken::Close(SessionClose {
                    code,
                    reason: reason.to_owned(),
                }),
                "capsule for {code}/{reason:?}"
            );
            // Exactly one capsule is consumed: what follows it is
            // untouched, which is what lets two capsules share a DATA
            // frame.
            assert_eq!(buf, trailing);
        }
    }

    /// Every prefix of a capsule is "not yet", never a capsule.
    ///
    /// A reader that guessed at a short buffer would report a close the
    /// peer had not finished writing — the same defect as
    /// `BadCloseCapsule::Truncated`, but silent.
    #[test]
    fn take_capsule_waits_for_a_whole_capsule() {
        let whole = close_capsule(7, "seven");
        for short in 0..whole.len() {
            let mut buf = whole[..short].to_vec();
            assert_eq!(take_capsule(&mut buf), Taken::More, "cut to {short}");
            assert_eq!(buf.len(), short, "nothing is consumed at {short}");
        }
    }

    /// An unknown capsule type is skipped over by its length, and what
    /// follows is read as its own capsule.
    ///
    /// RFC 9297 §3.2 requires the skip, and the length field is what makes
    /// it possible: a reader without one could only give up.
    #[test]
    fn take_capsule_skips_an_unknown_type_by_its_length() {
        // `DRAIN_WEBTRANSPORT_SESSION`, whose payload is empty in the
        // draft but is not here — a reader that ignored the length would
        // take the bytes after it for the drain's own.
        let mut buf = vec![0xc0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x78, 0xae, 0x05];
        buf.extend_from_slice(b"drain");
        buf.extend_from_slice(&close_capsule(3, "three"));
        assert_eq!(take_capsule(&mut buf), Taken::Skipped);
        assert_eq!(
            take_capsule(&mut buf),
            Taken::Close(SessionClose {
                code: 3,
                reason: "three".to_owned(),
            })
        );
        assert!(buf.is_empty());
    }

    /// The decoder reads back what the encoder wrote, says how much it
    /// consumed, and refuses a buffer that ends inside the integer.
    ///
    /// The `consumed` half is the one that matters on the receive side: it
    /// is the offset the payload starts at, so a decoder that read the
    /// right value and reported the wrong length would hand the caller a
    /// payload with header bytes still on the front.
    #[test]
    fn get_varint_reads_back_what_put_varint_wrote() {
        for (value, encoded) in CASES {
            let mut frame = encoded.to_vec();
            frame.extend_from_slice(b"payload");
            assert_eq!(
                get_varint(&frame),
                Some((*value, encoded.len())),
                "decoding {value}"
            );
            assert_eq!(&frame[encoded.len()..], b"payload");
            // Every prefix that stops inside the integer is a refusal, not
            // a smaller number — a datagram truncated to fewer bytes than
            // its own header claims is not an HTTP Datagram at all.
            for short in 0..encoded.len() {
                assert_eq!(
                    get_varint(&encoded[..short]),
                    None,
                    "{value} cut to {short}"
                );
            }
        }
    }
}
