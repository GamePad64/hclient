//! A real HTTP/3 server on loopback that speaks WebTransport, for the
//! tests to talk to.
//!
//! Not a mock and not a recorded exchange: a `quinn` endpoint with an
//! `rcgen` certificate, `h3`'s own **server** on top of it, and — for the
//! WebTransport streams — a raw `quinn::Connection::accept_bi`, because
//! that is what a WebTransport stream *is* and `h3`'s client API is not
//! involved on either side of it.
//!
//! It runs on its own tokio runtime on its own thread, with `quinn`'s own
//! `runtime-tokio`, for the reason `crates/http-ng-h3/tests/server.rs`
//! gives: the client is the thing under test, and a server sharing the
//! client's plumbing would make a green run ambiguous between "both work"
//! and "both are wrong the same way".
//!
//! # It decodes what the crate encodes, and it does so independently
//!
//! The crate under test *writes* the stream header — a varint `0x41` and a
//! varint session ID. This file *reads* one, with its own decoder written
//! from RFC 9000 §16 rather than from the encoder. That is the whole point
//! of the arrangement: a mutation to either the constant, the session ID or
//! the encoding is a mismatch here rather than a matching pair of bugs.
#![cfg(not(target_family = "wasm"))]
#![allow(dead_code)]

use bytes::{Buf, Bytes};
use h3::ConnectionState as _;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

/// What the server announces and how it answers.
#[derive(Debug, Clone)]
pub struct Options {
    /// `SETTINGS_ENABLE_WEBTRANSPORT`.
    pub announce_webtransport: bool,
    /// `SETTINGS_ENABLE_CONNECT_PROTOCOL`.
    pub announce_extended_connect: bool,
    /// `SETTINGS_H3_DATAGRAM`. Separate from the two above because a
    /// server can honestly announce WebTransport and not datagrams, and
    /// the client is required to notice: RFC 9297 §2.1 forbids sending
    /// HTTP Datagrams to an endpoint that did not ask for them.
    pub announce_datagram: bool,
    /// Whether the server's QUIC endpoint offers datagrams at all —
    /// `quinn::TransportConfig::datagram_receive_buffer_size`, which is
    /// what puts (or leaves out) RFC 9221's `max_datagram_frame_size`
    /// transport parameter. A *different* switch from the one above, one
    /// layer down, and the client must tell the two apart.
    pub quic_datagrams: bool,
    /// Send two datagrams the client must discard before each echo: one
    /// too short to carry a Quarter Stream ID, and one carrying a Quarter
    /// Stream ID that is not the session's.
    pub noise_before_echo: bool,
    /// The status the extended CONNECT is answered with.
    pub answer: http::StatusCode,
    /// What the server does with the CONNECT stream once it has answered.
    pub after_response: AfterResponse,
}

/// What the fixture does with the CONNECT stream after the response.
///
/// The CONNECT stream is where the capsule protocol lives
/// (draft-ietf-webtrans-http3 §5, RFC 9297 §3), so every way a session can
/// end is a variant here — and the point of having them all is that a test
/// asserting "the session ended" would pass against every one of them,
/// where the feature under test is telling them apart.
#[derive(Debug, Clone)]
pub enum AfterResponse {
    /// Hold the stream, read whatever capsules the client sends, record
    /// them, and close this side once the client's half has ended.
    ///
    /// The closing is what makes the client's own `closed()` a **causal**
    /// signal that its capsule arrived: bytes on one QUIC stream are
    /// ordered, so a FIN that only happens after the capsule was read
    /// cannot be observed before it.
    ReadCapsules,
    /// Send a `CLOSE_WEBTRANSPORT_SESSION` capsule, then FIN.
    Close { code: u32, reason: String },
    /// The same capsule, cut in two and sent as two DATA frames.
    ///
    /// `at` is how many bytes of the capsule go in the first frame, so a
    /// value under three cuts inside the capsule's own header rather than
    /// inside its payload.
    CloseInTwoFrames {
        code: u32,
        reason: String,
        at: usize,
    },
    /// An unknown capsule type, then the close. RFC 9297 §3.2 requires a
    /// receiver to skip the first and act on the second.
    UnknownThenClose { code: u32, reason: String },
    /// Send these bytes as one DATA frame and then FIN. For the capsules
    /// no honest encoder would produce.
    Raw(Vec<u8>),
    /// FIN with no capsule at all — draft §5's *"semantically equivalent
    /// to … an error code of 0 and an empty error string"*.
    Fin,
    /// Reset the CONNECT stream. The session is over and the peer never
    /// said why: this is the "vanished" half of the distinction.
    ///
    /// Waits for [`Server::abandon_the_session`] first — see there.
    Reset,
    /// Close the whole QUIC connection out from under the session.
    ///
    /// Waits for [`Server::abandon_the_session`] first — see there.
    AbortConnection,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            announce_webtransport: true,
            announce_extended_connect: true,
            announce_datagram: true,
            quic_datagrams: true,
            noise_before_echo: false,
            answer: http::StatusCode::OK,
            after_response: AfterResponse::ReadCapsules,
        }
    }
}

/// One request as the server saw it, in the terms the wire has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeenRequest {
    pub method: http::Method,
    pub uri: http::Uri,
    /// The `:protocol` pseudo-header, as `h3`'s server decoded it — `None`
    /// when the request carried none.
    ///
    /// A `String` rather than `h3::ext::Protocol` so that a test asserting
    /// on it is asserting on the value that crossed the wire and not on a
    /// type both sides happen to share.
    pub protocol: Option<String>,
    /// The QUIC stream ID the request arrived on. This is what the session
    /// ID on every WebTransport stream must equal.
    pub stream_id: u64,
}

/// One WebTransport datagram as the server saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeenDatagram {
    /// The first varint in the QUIC DATAGRAM frame — RFC 9297 §2.1's
    /// Quarter Stream ID, which for a WebTransport datagram is the
    /// session's CONNECT stream ID divided by four.
    pub quarter_stream_id: u64,
    /// Everything after it.
    pub payload: Vec<u8>,
}

/// One capsule as the server read it off the CONNECT stream.
///
/// Raw on purpose: the type and the length are decoded with this file's own
/// varint reader, and everything after them is handed to the test
/// **unparsed**. So a test asserting on a close capsule spells out
/// draft-ietf-webtrans-http3 §5's payload — four big-endian bytes of error
/// code, then UTF-8 — byte by byte, rather than through a decoder that
/// could be wrong in the same direction as the encoder under test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeenCapsule {
    /// RFC 9297 §3's Capsule Type. `0x2843` is
    /// `CLOSE_WEBTRANSPORT_SESSION`.
    pub kind: u64,
    /// The Capsule Value, exactly as many bytes as the Capsule Length said.
    pub payload: Vec<u8>,
}

/// One WebTransport stream as the server saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeenStream {
    /// The first varint on the stream — `0x41` for a client-initiated
    /// bidirectional WebTransport stream.
    pub signal: u64,
    /// The second varint: the session ID.
    pub session_id: u64,
    /// Everything after the header, read to the end of the stream.
    pub payload: Vec<u8>,
}

/// What the *client's* SETTINGS frame announced, read out of `h3`'s own
/// server-side connection state.
///
/// Here because nothing else can see it: a client's own settings leave no
/// other trace, and without this the line that sends them could be deleted
/// without a single test noticing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeenClientSettings {
    pub extended_connect: bool,
    pub webtransport: bool,
    /// `SETTINGS_H3_DATAGRAM`, as the client sent it. Here for the same
    /// reason as the other two: nothing on the wire forces a client to
    /// announce it, so without this the line that does could be deleted
    /// and every other test would stay green.
    pub datagram: bool,
}

#[derive(Debug, Default)]
struct State {
    requests: Mutex<Vec<SeenRequest>>,
    streams: Mutex<Vec<SeenStream>>,
    datagrams: Mutex<Vec<SeenDatagram>>,
    client_settings: Mutex<Option<SeenClientSettings>>,
    capsules: Mutex<Vec<SeenCapsule>>,
    /// Released by [`Server::abandon_the_session`].
    abandon: tokio::sync::Notify,
    /// Whether the client's half of the CONNECT stream ended with a FIN.
    ///
    /// Recorded because draft §5 asks for the FIN *as well as* the capsule,
    /// and because the two are separate lines in the client: a close that
    /// wrote the capsule and left the stream open would satisfy every
    /// assertion about capsule bytes.
    client_fin: Mutex<bool>,
}

#[derive(Debug)]
pub struct Server {
    pub addr: SocketAddr,
    pub cert_der: rustls::pki_types::CertificateDer<'static>,
    state: Arc<State>,
}

impl Server {
    /// Every request the server has resolved so far.
    pub fn requests(&self) -> Vec<SeenRequest> {
        self.state.requests.lock().unwrap().clone()
    }

    /// Let the fixture destroy the session, for the two `AfterResponse`
    /// variants that do.
    ///
    /// A gate rather than a delay, and it is there for a reason found by
    /// running it: `quinn::SendStream::reset` and `Connection::close` both
    /// abandon data that is written but not yet delivered, so a fixture
    /// that reset the CONNECT stream immediately after answering could
    /// destroy the **response** — and the client then fails in
    /// `Session::connect`, before the thing under test exists. Called after
    /// `Session::connect` has returned, this makes the order causal rather
    /// than lucky.
    pub fn abandon_the_session(&self) {
        // `notify_one` rather than `notify_waiters`: the permit is stored,
        // so this works whether or not the fixture is already waiting.
        self.state.abandon.notify_one();
    }

    /// Every capsule the server has read off the CONNECT stream.
    pub fn capsules(&self) -> Vec<SeenCapsule> {
        self.state.capsules.lock().unwrap().clone()
    }

    /// Whether the client ended its half of the CONNECT stream with a FIN.
    pub fn client_fin(&self) -> bool {
        *self.state.client_fin.lock().unwrap()
    }

    /// Every WebTransport stream the server has read to its end so far.
    pub fn streams(&self) -> Vec<SeenStream> {
        self.state.streams.lock().unwrap().clone()
    }

    /// Every datagram the server has read so far.
    pub fn datagrams(&self) -> Vec<SeenDatagram> {
        self.state.datagrams.lock().unwrap().clone()
    }

    /// Wait until `n` datagrams have been read, or give up.
    ///
    /// Unlike [`Server::wait_for_streams`] this one has no causal
    /// alternative: a datagram is not acknowledged, so nothing the client
    /// can observe proves the server saw one. It is used only where the
    /// test's own claim is about arrival, and a `false` is that claim
    /// failing.
    pub async fn wait_for_datagrams(&self, n: usize, within: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + within;
        while std::time::Instant::now() < deadline {
            if self.state.datagrams.lock().unwrap().len() >= n {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        false
    }

    /// The client's SETTINGS, or `None` if none had arrived when the
    /// request was resolved.
    pub fn client_settings(&self) -> Option<SeenClientSettings> {
        *self.state.client_settings.lock().unwrap()
    }

    /// Wait until `n` streams have been read, or give up.
    ///
    /// A poll rather than a sleep: the tests that use it assert on the
    /// *content* afterwards, so a `false` here is a failure to observe and
    /// not a slow machine's fault line.
    pub async fn wait_for_streams(&self, n: usize, within: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + within;
        while std::time::Instant::now() < deadline {
            if self.state.streams.lock().unwrap().len() >= n {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        false
    }
}

pub fn start(opts: Options) -> Server {
    // All three names, as in `http-ng-h3`'s fixture: the tests dial the
    // literal `127.0.0.1` so that resolution is not a second thing under
    // test, and rcgen turns an IP-shaped SAN into an IP SAN, which is what
    // rustls checks a literal against.
    let cert = rcgen::generate_simple_self_signed(vec![
        "localhost".into(),
        "127.0.0.1".into(),
        "::1".into(),
    ])
    .expect("rcgen can always make a self-signed cert");
    let cert_der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::try_from(cert.signing_key.serialize_der())
        .expect("the key was made with the cert");

    let mut tls = rustls::ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_no_client_auth()
        .with_single_cert(vec![cert_der.clone()], key_der)
        .expect("the cert and key were made together");
    tls.alpn_protocols = vec![b"h3".to_vec()];

    let quic_tls = quinn::crypto::rustls::QuicServerConfig::try_from(tls)
        .expect("TLS 1.3 with a ring provider always has the initial suite");
    let mut cfg = quinn::ServerConfig::with_crypto(Arc::new(quic_tls));
    if !opts.quic_datagrams {
        // `None` is what makes quinn leave `max_datagram_frame_size` out of
        // the transport parameters, which is RFC 9221 §3's way of saying
        // "do not send me datagrams". The default is `Some(..)`, so a
        // connection made against this fixture carries datagrams unless a
        // test asks otherwise.
        let mut transport = quinn::TransportConfig::default();
        transport.datagram_receive_buffer_size(None);
        cfg.transport_config(Arc::new(transport));
    }

    let state = Arc::new(State::default());
    let for_thread = state.clone();
    let (tx, rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let endpoint = quinn::Endpoint::server(cfg, "127.0.0.1:0".parse().unwrap())
                .expect("a v4 loopback bind");
            tx.send(endpoint.local_addr().unwrap()).unwrap();
            while let Some(incoming) = endpoint.accept().await {
                let state = for_thread.clone();
                let opts = opts.clone();
                tokio::spawn(async move {
                    let Ok(conn) = incoming.await else { return };
                    serve(conn, opts, state).await;
                });
            }
        });
    });

    Server {
        addr: rx.recv().expect("the server thread binds or panics"),
        cert_der,
        state,
    }
}

async fn serve(conn: quinn::Connection, opts: Options, state: Arc<State>) {
    // The h3 layer and the raw connection side by side, deliberately: the
    // CONNECT is an HTTP/3 request and every WebTransport stream after it
    // is not. `h3`'s server would decode the `0x41` frame type itself, and
    // reaching the stream through it needs the same
    // `i-implement-a-third-party-backend` door the client uses — so the
    // fixture takes the streams from `quinn` instead, which is both less
    // machinery and a more independent reading.
    let quic = conn.clone();
    let Ok(mut h3) = h3::server::builder()
        .enable_webtransport(opts.announce_webtransport)
        .enable_extended_connect(opts.announce_extended_connect)
        .enable_datagram(opts.announce_datagram)
        .max_webtransport_sessions(1)
        .build::<h3_quinn::Connection, Bytes>(h3_quinn::Connection::new(conn))
        .await
    else {
        return;
    };

    // Exactly one *resolvable* request is accepted through h3, and then
    // the h3 connection is never polled again — which is what leaves
    // `quic.accept_bi()` below as the only reader of incoming bidi
    // streams. `h3_quinn::Connection`'s accept stream is lazy, so nothing
    // of ours is stuck inside it.
    //
    // The loop is not defensive programming: `tests/webtransport.rs`
    // deliberately opens and resets one bidirectional stream ahead of the
    // session, so that the CONNECT does not land on stream 0 where every
    // wrong session ID is also the right one. That stream arrives here
    // first — QUIC opens the lower-numbered streams implicitly — and
    // resolves to an error, which is a *stream* error and not a connection
    // one.
    let (req, mut stream) = loop {
        let Ok(Some(resolver)) = h3.accept().await else {
            return;
        };
        match resolver.resolve_request().await {
            Ok(pair) => break pair,
            Err(_) => continue,
        }
    };
    // Read at the moment the request resolved, which is after the client's
    // control stream has been read — the same `h3` shared state the client
    // half of this exchange reads the *server's* settings from.
    *state.client_settings.lock().unwrap() = Some(SeenClientSettings {
        extended_connect: h3.settings().enable_extended_connect(),
        webtransport: h3.settings().enable_webtransport(),
        datagram: h3.settings().enable_datagram(),
    });
    state.requests.lock().unwrap().push(SeenRequest {
        method: req.method().clone(),
        uri: req.uri().clone(),
        protocol: req
            .extensions()
            .get::<h3::ext::Protocol>()
            .map(|p| p.as_str().to_string()),
        stream_id: stream.id().into_inner(),
    });

    let resp = http::Response::builder()
        .status(opts.answer)
        .body(())
        .expect("a status and no body always builds");
    if stream.send_response(resp).await.is_err() {
        return;
    }

    // The session ID, which is what every WebTransport stream and every
    // WebTransport datagram on this connection must name.
    let session_id = stream.id().into_inner();

    // The CONNECT stream is *moved* into its own task rather than held and
    // ignored. That task is what keeps the session alive — it owns the
    // stream, and `quinn::SendStream::drop` finishes it — and it is also
    // the whole of the capsule protocol on this side.
    tokio::spawn(connect_stream(
        stream,
        quic.clone(),
        opts.clone(),
        state.clone(),
    ));

    tokio::spawn(echo_datagrams(
        quic.clone(),
        session_id,
        opts.clone(),
        state.clone(),
    ));

    while let Ok((send, recv)) = quic.accept_bi().await {
        let state = state.clone();
        tokio::spawn(async move {
            echo_webtransport_stream(send, recv, state).await;
        });
    }
}

/// The capsule protocol, on the server's side of the CONNECT stream.
///
/// # It encodes with `h3`'s varint and decodes with its own
///
/// The same arrangement as [`echo_datagrams`], and for the same reason: the
/// capsule headers it writes come from `h3::proto::varint::VarInt`, a third
/// implementation that is neither the crate under test's encoder nor this
/// file's decoder, and what it reads is decoded by [`VarintReader`], written
/// from RFC 9000 §16. The capsule *payload* it never decodes at all — see
/// [`SeenCapsule`].
async fn connect_stream(
    mut stream: h3::server::RequestStream<h3_quinn::BidiStream<Bytes>, Bytes>,
    quic: quinn::Connection,
    opts: Options,
    state: Arc<State>,
) {
    match opts.after_response.clone() {
        AfterResponse::ReadCapsules => {}
        AfterResponse::Close { code, reason } => {
            let _ = stream.send_data(close_capsule(code, &reason)).await;
            // Dropping the stream is the FIN, which is draft §5's second
            // half of a clean close.
            return;
        }
        AfterResponse::CloseInTwoFrames { code, reason, at } => {
            let whole = close_capsule(code, &reason);
            let _ = stream.send_data(whole.slice(..at)).await;
            let _ = stream.send_data(whole.slice(at..)).await;
            return;
        }
        AfterResponse::UnknownThenClose { code, reason } => {
            // `DRAIN_WEBTRANSPORT_SESSION`, the draft's other capsule and
            // the one this client deliberately does not act on. Its
            // payload is empty, so a receiver that ignored the length
            // field would still get the next capsule right — which is why
            // the payload here is not empty.
            let _ = stream.send_data(capsule(0x78ae, b"drain")).await;
            let _ = stream.send_data(close_capsule(code, &reason)).await;
            return;
        }
        AfterResponse::Raw(bytes) => {
            let _ = stream.send_data(Bytes::from(bytes)).await;
            return;
        }
        AfterResponse::Fin => return,
        AfterResponse::Reset => {
            state.abandon.notified().await;
            stream.stop_stream(h3::error::Code::H3_NO_ERROR);
            // Held so the reset is not immediately followed by the whole
            // connection going away, which would be a different fact.
            std::future::pending::<()>().await;
            return;
        }
        AfterResponse::AbortConnection => {
            state.abandon.notified().await;
            quic.close(0u32.into(), b"gone");
            return;
        }
    }

    let mut buf = Vec::new();
    loop {
        match stream.recv_data().await {
            Ok(Some(mut data)) => {
                while data.has_remaining() {
                    let chunk = data.chunk();
                    buf.extend_from_slice(chunk);
                    let read = chunk.len();
                    data.advance(read);
                }
                // Recorded as they are decoded, and before the FIN below,
                // so a client that has observed this side closing has
                // causally already had its capsule recorded.
                while let Some(seen) = take_capsule(&mut buf) {
                    state.capsules.lock().unwrap().push(seen);
                }
            }
            // The client's half ended. Record it and let the stream drop,
            // which FINs this half in turn.
            Ok(None) => {
                *state.client_fin.lock().unwrap() = true;
                return;
            }
            Err(_) => return,
        }
    }
}

/// RFC 9297 §3's framing: a capsule type and a length, both QUIC
/// variable-length integers, then the value.
fn capsule(kind: u64, payload: &[u8]) -> Bytes {
    use h3::proto::varint::VarInt;
    let mut buf = Vec::new();
    VarInt::from_u64(kind)
        .expect("a capsule type is a varint")
        .encode(&mut buf);
    VarInt::from_u64(payload.len() as u64)
        .expect("a test payload is short")
        .encode(&mut buf);
    buf.extend_from_slice(payload);
    Bytes::from(buf)
}

/// A `CLOSE_WEBTRANSPORT_SESSION` capsule: draft §5's 32-bit application
/// error code, big-endian, then the reason as UTF-8.
fn close_capsule(code: u32, reason: &str) -> Bytes {
    let mut payload = code.to_be_bytes().to_vec();
    payload.extend_from_slice(reason.as_bytes());
    capsule(0x2843, &payload)
}

/// Take one whole capsule off the front of `buf`, if there is one.
fn take_capsule(buf: &mut Vec<u8>) -> Option<SeenCapsule> {
    let mut reader = VarintReader {
        buf: buf.clone(),
        at: 0,
    };
    let kind = reader.try_decode()?;
    let length = reader.try_decode()? as usize;
    let start = reader.at;
    if buf.len() < start + length {
        return None;
    }
    let payload = buf[start..start + length].to_vec();
    buf.drain(..start + length);
    Some(SeenCapsule { kind, payload })
}

/// Read every datagram on the connection, record it, and echo it back.
///
/// # It encodes with `h3`'s varint and decodes with its own
///
/// The Quarter Stream ID it writes comes from `h3::proto::varint::VarInt`
/// — a third implementation, neither the crate under test's encoder nor
/// this file's decoder — so a client that reads the header wrongly gets a
/// mismatch here rather than a matching pair of bugs. What it reads is
/// decoded by [`VarintReader`], written from RFC 9000 §16.
///
/// Note what is *not* here: no check that the client announced
/// `SETTINGS_H3_DATAGRAM`. This side is raw `quinn`, so nothing at the
/// HTTP/3 layer gates it, which is precisely why the client's own
/// announcement has to be asserted from [`SeenClientSettings`] instead of
/// being inferred from a datagram arriving.
async fn echo_datagrams(
    conn: quinn::Connection,
    session_id: u64,
    opts: Options,
    state: Arc<State>,
) {
    let quarter = session_id >> 2;
    while let Ok(frame) = conn.read_datagram().await {
        let mut reader = VarintReader {
            buf: frame.to_vec(),
            at: 0,
        };
        let Some(quarter_stream_id) = reader.try_decode() else {
            continue;
        };
        let payload = reader.drain();
        state.datagrams.lock().unwrap().push(SeenDatagram {
            quarter_stream_id,
            payload: payload.clone(),
        });
        if opts.noise_before_echo {
            // Too short to carry a Quarter Stream ID at all.
            let _ = conn.send_datagram(Bytes::new());
            // A well-formed HTTP Datagram for a session that is not this
            // one. `+ 1` rather than an arbitrary number so that the
            // header is the same length as the real one and the two
            // differ in value alone.
            let _ = conn.send_datagram(datagram(quarter + 1, b"stray"));
        }
        let _ = conn.send_datagram(datagram(quarter, &payload));
    }
}

/// An HTTP/3 datagram: RFC 9297 §2.1's Quarter Stream ID varint, then the
/// payload. Encoded with `h3`'s `VarInt`, deliberately — see
/// [`echo_datagrams`].
fn datagram(quarter_stream_id: u64, payload: &[u8]) -> Bytes {
    use h3::proto::varint::VarInt;
    let mut frame = Vec::new();
    VarInt::from_u64(quarter_stream_id)
        .expect("a quarter stream id is a varint by construction")
        .encode(&mut frame);
    frame.extend_from_slice(payload);
    Bytes::from(frame)
}

/// Read one WebTransport stream's header and payload, record both, and
/// echo the payload back.
async fn echo_webtransport_stream(
    mut send: quinn::SendStream,
    mut recv: quinn::RecvStream,
    state: Arc<State>,
) {
    let mut reader = VarintReader::default();
    let Some(signal) = reader.next(&mut recv).await else {
        return;
    };
    let Some(session_id) = reader.next(&mut recv).await else {
        return;
    };
    let mut payload = reader.drain();
    // Read to the end of the stream — the client finishes it, so this is a
    // FIN and not a length.
    while let Ok(Some(chunk)) = recv.read_chunk(64 * 1024, true).await {
        payload.extend_from_slice(&chunk.bytes);
    }
    // **Recorded before the echo is written, and that ordering is the
    // test's clock.** A caller that has read the echo has, causally,
    // already had its stream recorded here — so no test in this suite
    // waits on a duration to observe one.
    state.streams.lock().unwrap().push(SeenStream {
        signal,
        session_id,
        payload: payload.clone(),
    });
    let _ = send.write_all(&payload).await;
    let _ = send.finish();
    let _ = send.stopped().await;
}

/// A QUIC variable-length integer decoder over a `quinn::RecvStream`.
///
/// Written from RFC 9000 §16 rather than from the encoder it checks. It
/// buffers, because a varint can straddle a read and because whatever is
/// read past the header is the payload's first bytes and must not be lost.
#[derive(Debug, Default)]
struct VarintReader {
    buf: Vec<u8>,
    at: usize,
}

impl VarintReader {
    async fn next(&mut self, recv: &mut quinn::RecvStream) -> Option<u64> {
        loop {
            if let Some(v) = self.try_decode() {
                return Some(v);
            }
            let chunk = recv.read_chunk(64 * 1024, true).await.ok()??;
            self.buf.extend_from_slice(&chunk.bytes);
        }
    }

    fn try_decode(&mut self) -> Option<u64> {
        let rest = &self.buf[self.at..];
        let first = *rest.first()?;
        let len = 1usize << (first >> 6);
        if rest.len() < len {
            return None;
        }
        let mut v = u64::from(first & 0x3f);
        for b in &rest[1..len] {
            v = (v << 8) | u64::from(*b);
        }
        self.at += len;
        Some(v)
    }

    /// Everything read past the last decoded varint.
    fn drain(self) -> Vec<u8> {
        self.buf[self.at..].to_vec()
    }
}

/// A `quinn` client endpoint that trusts exactly this server's certificate.
///
/// The client half of the fixture, and deliberately not part of the crate
/// under test: `http-ng-webtransport` takes a `quinn::Connection` and has
/// no opinion about where it came from — see its crate doc, and
/// `docs/v04-w2-webtransport.md` §4 for the half that is missing.
pub async fn dial(server: &Server) -> quinn::Connection {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(server.cert_der.clone()).unwrap();
    let mut tls = rustls::ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_root_certificates(roots)
        .with_no_client_auth();
    tls.alpn_protocols = vec![b"h3".to_vec()];
    let quic_tls = quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap();
    let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(quic_tls)));
    // The endpoint must outlive the connection, and a test holding only the
    // `Session` would drop it. `Box::leak` for the length of one test
    // process is the smaller of the two evils against threading an extra
    // return value through every test.
    let endpoint: &'static quinn::Endpoint = Box::leak(Box::new(endpoint));
    endpoint
        .connect(server.addr, "127.0.0.1")
        .unwrap()
        .await
        .expect("the server is up and presents a certificate for this name")
}
