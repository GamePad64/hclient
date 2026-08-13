//! WebTransport sessions over this workspace's HTTP/3.
//!
//! ```ignore
//! // `conn` is a `quinn::Connection` that negotiated ALPN `h3` — see
//! // "Where the connection comes from" below, which is the part that is
//! // missing rather than the part that is here.
//! let session = http_ng_webtransport::Session::connect(conn, &uri).await?;
//! let (mut send, mut recv) = session.open_bi().await?;
//! send.write_all(b"ping").await?;
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
//! What that costs is named rather than discovered: a `GOAWAY` arriving
//! later is not observed, and neither is the peer closing the session by
//! ending the CONNECT stream. Both are recorded as not-done in
//! `docs/v04-w2-webtransport.md` §6, with what each would need.
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
//! [`put_varint`], and the datagram header is the same two lines.
//!
//! # What is deliberately not here
//!
//! - **The capsule protocol** — `CLOSE_WEBTRANSPORT_SESSION` and
//!   `DRAIN_WEBTRANSPORT_SESSION`. Dropping a [`Session`] ends it the
//!   other way the draft allows, by closing the CONNECT stream, which is
//!   what `quinn::SendStream::drop` does anyway; a `close()` method that
//!   did no more than `drop` would be a second name for one behaviour.
//! - **Server-initiated streams.** A server-opened *unidirectional*
//!   WebTransport stream is not merely unimplemented here, it is
//!   unreachable: `h3`'s client driver classifies it as
//!   `AcceptedRecvStream::WebTransportUni` and then discards it, because
//!   the arm that keeps it is guarded by `enable_webtransport`, which
//!   `h3` 0.0.8's **client** builder has no setter for. See
//!   `docs/v04-w2-webtransport.md` §3.
#![forbid(unsafe_code)]

use bytes::Bytes;
use h3::ConnectionState as _;
use h3::connection::ConnectionInner;
use h3::proto::frame::Frame;
use http_ng_core::{Error, ErrorKind};
use std::future::poll_fn;

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

/// An open WebTransport session: a multiplexer over one QUIC connection.
///
/// # What it holds, and why each of the three is not dead weight
///
/// A `Session` owns three things it never polls again, and dropping any of
/// them would end the session or the connection under it:
///
/// - the **CONNECT request stream**, because the session lives exactly as
///   long as that stream (draft §5) and `quinn::SendStream::drop` calls
///   `finish()`, so letting it go says "session over";
/// - the h3 **`SendRequest`**, because `h3` counts them and closes the
///   connection with `H3_NO_ERROR` when the last one drops;
/// - the h3 **connection driver**, because it owns the *control* stream,
///   and a control stream that ends is `H3_CLOSED_CRITICAL_STREAM` — a
///   connection error, not a stream one.
///
/// The last of the three is the one worth knowing about: the driver is
/// held to keep a stream open, not to be polled. See the crate doc.
pub struct Session {
    /// The raw QUIC connection. WebTransport streams are QUIC streams with
    /// a header, opened beside h3's rather than through it — which is why
    /// this is here and `h3` is not asked to open them.
    conn: quinn::Connection,
    id: SessionId,
    /// What the peer's SETTINGS said about `SETTINGS_H3_DATAGRAM`, read
    /// once at establishment because that is when the frame arrives and
    /// SETTINGS cannot be changed afterwards. It is not a third flag on
    /// the gate — see [`Session::max_datagram_size`].
    peer_datagrams: bool,
    // The three anchors. Named with a leading underscore because they are
    // never read: their whole job is to not be dropped. See the type doc.
    _connect: h3::client::RequestStream<h3_quinn::BidiStream<Bytes>, Bytes>,
    _send: h3::client::SendRequest<h3_quinn::OpenStreams, Bytes>,
    _driver: h3::client::Connection<h3_quinn::Connection, Bytes>,
}

// Hand-written: none of the three anchors is `Debug`, and requiring it of
// them would mean asking `h3` for an impl in order to print a field whose
// whole purpose is to exist. What a reader wants from a `Session` is which
// session it is and where it goes, which is what this prints.
impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("id", &self.id)
            .field("remote", &self.conn.remote_address())
            .finish_non_exhaustive()
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
        if uri.scheme_str() != Some("https") {
            return Err(Error::new(
                ErrorKind::Unsupported,
                NotHttps {
                    scheme: uri.scheme_str().unwrap_or("(no scheme)").to_string(),
                },
            ));
        }
        // No authority check follows, and that is a fact about `http::Uri`
        // rather than an omission — see [`NotHttps`].

        let (mut driver, mut send) = h3::client::builder()
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

        let peer_datagrams = settings_announce_webtransport(&mut driver.inner, &send).await?;

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

        Ok(Self {
            conn,
            // The full stream ID, not `StreamId::index()`. draft §4.2 says
            // the Session ID *is* the Stream ID of the CONNECT stream, and
            // the two differ by two bits of type: `h3`'s own
            // `From<StreamId> for SessionId` uses `index()`, which is a
            // fact about that crate's server side rather than one this
            // client can copy.
            id: SessionId(stream.id().into_inner()),
            peer_datagrams,
            _connect: stream,
            _send: send,
            _driver: driver,
        })
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
        if !self.peer_datagrams {
            return None;
        }
        self.conn
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
        if !self.peer_datagrams {
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
        self.conn
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
    /// # What it silently drops, and why silence is right
    ///
    /// A datagram whose Quarter Stream ID is not this session's, and one
    /// too short to carry a Quarter Stream ID at all, are discarded and
    /// the wait continues. RFC 9297 §2.1 says a receiver "SHALL either
    /// drop that datagram silently or buffer it temporarily" when the ID
    /// names a stream it does not know, so an error here would be
    /// inventing a failure the RFC forbids — and this crate holds one
    /// session per connection, so *not this session* is the only
    /// unknown-stream case that exists.
    ///
    /// The one thing it does not do is the RFC's other arm: an ID above
    /// `2^60 - 1` is illegal and "MUST be treated as an HTTP/3 connection
    /// error of type H3_DATAGRAM_ERROR". Such an ID cannot equal this
    /// session's, so it is dropped rather than escalated; closing the
    /// connection from here is recorded as not done in
    /// `docs/v04-w2-datagrams.md` rather than half-done.
    pub async fn recv_datagram(&self) -> Result<Bytes, Error> {
        let quarter = self.quarter_stream_id();
        loop {
            let frame = self
                .conn
                .read_datagram()
                .await
                .map_err(|e| Error::new(ErrorKind::Connect, e))?;
            let Some((id, header)) = get_varint(&frame) else {
                continue;
            };
            if id != quarter {
                continue;
            }
            return Ok(frame.slice(header..));
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

/// The signal value that begins a client-initiated bidirectional
/// WebTransport stream — draft-ietf-webtrans-http3 §4.2, and the same
/// number `h3` knows as `FrameType::WEBTRANSPORT_BI_STREAM`.
const WEBTRANSPORT_STREAM: u64 = 0x41;

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
/// Returns what the same frame said about `SETTINGS_H3_DATAGRAM`, which is
/// read here because this is the only moment it is readable and because
/// the answer is fixed for the connection's life. It is deliberately
/// **not** a third condition on the gate: a server may honestly announce
/// WebTransport and not datagrams, `h3`'s own server builder can be
/// configured into exactly that state, and a caller that only ever opens
/// streams would be refused a session that works. What it does instead is
/// decide [`Session::max_datagram_size`].
async fn settings_announce_webtransport(
    inner: &mut ConnectionInner<h3_quinn::Connection, Bytes>,
    send: &h3::client::SendRequest<h3_quinn::OpenStreams, Bytes>,
) -> Result<bool, Error> {
    match poll_fn(|cx| inner.poll_control(cx)).await {
        Ok(Frame::Settings(_)) => {}
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
    }

    let settings = send.settings();
    let announced = NotSupportedByPeer {
        webtransport: settings.enable_webtransport(),
        extended_connect: settings.enable_extended_connect(),
    };
    if announced.webtransport && announced.extended_connect {
        Ok(settings.enable_datagram())
    } else {
        Err(Error::new(ErrorKind::Unsupported, announced))
    }
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
