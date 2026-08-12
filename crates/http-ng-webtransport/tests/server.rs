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

use bytes::Bytes;
use h3::ConnectionState as _;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

/// What the server announces and how it answers.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// `SETTINGS_ENABLE_WEBTRANSPORT`.
    pub announce_webtransport: bool,
    /// `SETTINGS_ENABLE_CONNECT_PROTOCOL`.
    pub announce_extended_connect: bool,
    /// The status the extended CONNECT is answered with.
    pub answer: http::StatusCode,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            announce_webtransport: true,
            announce_extended_connect: true,
            answer: http::StatusCode::OK,
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
}

#[derive(Debug, Default)]
struct State {
    requests: Mutex<Vec<SeenRequest>>,
    streams: Mutex<Vec<SeenStream>>,
    client_settings: Mutex<Option<SeenClientSettings>>,
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

    /// Every WebTransport stream the server has read to its end so far.
    pub fn streams(&self) -> Vec<SeenStream> {
        self.state.streams.lock().unwrap().clone()
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
    let cfg = quinn::ServerConfig::with_crypto(Arc::new(quic_tls));

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
        .enable_datagram(true)
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

    // Held, not dropped: the session lives exactly as long as the CONNECT
    // stream, and `quinn::SendStream::drop` calls `finish()`.
    let _connect = stream;

    while let Ok((send, recv)) = quic.accept_bi().await {
        let state = state.clone();
        tokio::spawn(async move {
            echo_webtransport_stream(send, recv, state).await;
        });
    }
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
