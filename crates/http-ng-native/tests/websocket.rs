//! The WebSocket seam on `http-ng-native`, watched from the other side of
//! a real socket.
//!
//! # The observer is a hand-written server, on purpose
//!
//! Every fixture here speaks WebSocket by hand: it parses the opening
//! handshake out of the raw request bytes, and it encodes and decodes
//! frames itself ([`Wire`]). Using `tungstenite`'s own server would have
//! been a quarter of the code and a much weaker witness — the client under
//! test is framed by `tungstenite`, and a fixture that shares its frame
//! codec cannot tell a correct frame from a consistently wrong one. What
//! is asserted here is opcodes, mask bits and payload bytes, which is the
//! layer RFC 6455 is written in.
//!
//! The one thing the fixture does borrow is
//! `tungstenite::handshake::derive_accept_key`, and that borrowing is
//! defused rather than assumed: `the_accept_key_derivation_matches_rfc_6455`
//! pins that function against the vector in RFC 6455 §1.3, so a client and
//! a server agreeing on a *wrong* accept key is a failure this file
//! notices.
//!
//! # Nothing here is asserted by a clock
//!
//! Three timing-based assertions in this workspace turned out to be flakes
//! and one was hiding a real defect, so each test below is arranged so
//! that the thing it measures is the only thing that could have let it
//! finish at all — `crates/http-ng-h3/tests/streaming.rs` is the shape.
//! In particular
//! `a_ping_is_answered_with_a_pong_which_is_what_releases_the_next_message`
//! has the server withhold its next message until the pong has arrived, so
//! a client that never answers a ping cannot receive it at any speed; and
//! `a_message_larger_than_the_socket_buffer_arrives_whole` releases the
//! server's reader only once the client's own `poll_flush` has actually
//! returned `Pending`, so the partial write it is about is a fact the test
//! establishes rather than one it hopes for. `BOUND` is a watchdog, never
//! a threshold: every failure it can produce is "this hung", and no test
//! passes because something happened quickly enough.
#![cfg(all(not(target_family = "wasm"), feature = "websocket"))]

use bytes::Bytes;
use futures_sink::Sink;
use futures_util::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use http_ng_core::ErrorKind;
use http_ng_core::RequestBody;
use http_ng_core::unversioned::{Message, Transport, WebSocketConnect};
use http_ng_dns::IpLiteralOnly;
use http_ng_native::Native;
use http_ng_rt_tokio::Tokio;
use http_ng_tls_rustls::Rustls;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;
use tungstenite::handshake::derive_accept_key;

/// Ceiling for anything that must not hang. Never a threshold: see the
/// module doc.
const BOUND: Duration = Duration::from_secs(30);

// ── opcodes, spelled out where they are asserted ────────────────────────

const OP_TEXT: u8 = 0x1;
const OP_BINARY: u8 = 0x2;
const OP_CLOSE: u8 = 0x8;
const OP_PING: u8 = 0x9;
const OP_PONG: u8 = 0xA;

// ── the fixture ─────────────────────────────────────────────────────────

/// A socket the fixture speaks HTTP and then RFC 6455 over, by hand.
struct Wire {
    sock: std::net::TcpStream,
    buf: Vec<u8>,
}

impl Wire {
    fn new(sock: std::net::TcpStream) -> Self {
        sock.set_read_timeout(Some(BOUND)).expect("read timeout");
        Self {
            sock,
            buf: Vec::new(),
        }
    }

    /// One read into the buffer. `false` on EOF or error, which every
    /// caller below treats as "the connection is over".
    fn fill(&mut self) -> bool {
        let mut chunk = [0u8; 16 * 1024];
        match self.sock.read(&mut chunk) {
            Ok(0) | Err(_) => false,
            Ok(n) => {
                self.buf.extend_from_slice(&chunk[..n]);
                true
            }
        }
    }

    /// The request head, up to and including the blank line. Every request
    /// this file's client sends is a bodyless `GET`, so the head is the
    /// whole request.
    fn head(&mut self) -> Option<String> {
        loop {
            if let Some(i) = self.buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&self.buf[..i + 4]).into_owned();
                self.buf.drain(..i + 4);
                return Some(head);
            }
            if !self.fill() {
                return None;
            }
        }
    }

    /// One frame: `(opcode, was_masked, payload)`, unmasked.
    ///
    /// Fragmentation is not handled, and does not need to be: a
    /// `tungstenite` client sends one frame per message. A continuation
    /// opcode would surface as opcode `0` and fail whichever assertion is
    /// looking at it, rather than being quietly reassembled.
    fn frame(&mut self) -> Option<(u8, bool, Vec<u8>)> {
        loop {
            if let Some(f) = self.parse_frame() {
                return Some(f);
            }
            if !self.fill() {
                return None;
            }
        }
    }

    fn parse_frame(&mut self) -> Option<(u8, bool, Vec<u8>)> {
        if self.buf.len() < 2 {
            return None;
        }
        let opcode = self.buf[0] & 0x0f;
        let masked = self.buf[1] & 0x80 != 0;
        let short = usize::from(self.buf[1] & 0x7f);
        let (len, mut at) = match short {
            126 => {
                if self.buf.len() < 4 {
                    return None;
                }
                (
                    usize::from(u16::from_be_bytes([self.buf[2], self.buf[3]])),
                    4,
                )
            }
            127 => {
                if self.buf.len() < 10 {
                    return None;
                }
                let mut n = [0u8; 8];
                n.copy_from_slice(&self.buf[2..10]);
                (u64::from_be_bytes(n) as usize, 10)
            }
            n => (n, 2),
        };
        let mut mask = [0u8; 4];
        if masked {
            if self.buf.len() < at + 4 {
                return None;
            }
            mask.copy_from_slice(&self.buf[at..at + 4]);
            at += 4;
        }
        if self.buf.len() < at + len {
            return None;
        }
        let mut payload = self.buf[at..at + len].to_vec();
        if masked {
            for (i, b) in payload.iter_mut().enumerate() {
                *b ^= mask[i % 4];
            }
        }
        self.buf.drain(..at + len);
        Some((opcode, masked, payload))
    }

    /// A server-to-client frame, which RFC 6455 §5.1 forbids to be masked.
    fn frame_bytes(opcode: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![0x80 | opcode];
        match payload.len() {
            n if n < 126 => out.push(n as u8),
            n if n <= usize::from(u16::MAX) => {
                out.push(126);
                out.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                out.push(127);
                out.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        out.extend_from_slice(payload);
        out
    }

    fn send(&mut self, opcode: u8, payload: &[u8]) -> bool {
        self.sock
            .write_all(&Wire::frame_bytes(opcode, payload))
            .is_ok()
    }

    fn send_raw(&mut self, bytes: &[u8]) -> bool {
        self.sock.write_all(bytes).is_ok()
    }
}

/// A one-line header lookup over a raw request head.
fn header<'h>(head: &'h str, name: &str) -> Option<&'h str> {
    head.lines()
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case(name))
        .map(|(_, v)| v.trim())
}

/// The `101` a correct server sends back.
fn accept_101(key: &str) -> String {
    format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Accept: {}\r\n\r\n",
        derive_accept_key(key.as_bytes())
    )
}

/// Binds a loopback port and runs `f` on every connection, on a thread of
/// its own. The counter is how many connections were accepted, which is
/// the only claim any test here makes about the pool.
fn serve<F>(f: F) -> (SocketAddr, Arc<AtomicUsize>)
where
    F: Fn(Wire) + Send + Sync + 'static,
{
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&accepted);
    let f = Arc::new(f);
    std::thread::spawn(move || {
        for sock in listener.incoming() {
            let Ok(sock) = sock else { continue };
            counter.fetch_add(1, Ordering::SeqCst);
            let f = Arc::clone(&f);
            std::thread::spawn(move || f(Wire::new(sock)));
        }
    });
    (addr, accepted)
}

/// The transport under test. `IpLiteralOnly` rather than a system
/// resolver: every address here is a loopback literal, and a DNS backend
/// would be a second thing that could fail.
fn native() -> Native<Tokio, Rustls, IpLiteralOnly> {
    Native::new(Tokio, Rustls::with_webpki_roots(), IpLiteralOnly)
}

fn open(uri: &str) -> http::Request<()> {
    http::Request::builder().uri(uri).body(()).unwrap()
}

// ── the tests ───────────────────────────────────────────────────────────

/// The oracle the fixture's `101` rests on, pinned against RFC 6455 §1.3's
/// own worked example rather than against the client that consumes it.
///
/// Without this, `a_101_whose_accept_key_is_wrong_is_refused` would prove
/// only that the two sides agree, which is exactly what a shared mistake
/// looks like.
#[test]
fn the_accept_key_derivation_matches_rfc_6455() {
    assert_eq!(
        derive_accept_key(b"dGhlIHNhbXBsZSBub25jZQ=="),
        "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
    );
}

/// The happy path, and the control for everything else: a real handshake,
/// a message each way, and the wire-level facts RFC 6455 §4.1 and §5.1
/// require of a client.
#[tokio::test]
async fn a_websocket_opens_and_carries_messages_both_ways() {
    let (tx, rx) = mpsc::channel::<(String, u8, bool, Vec<u8>)>();
    let (addr, _) = serve(move |mut w| {
        let Some(head) = w.head() else { return };
        let Some(key) = header(&head, "sec-websocket-key").map(str::to_owned) else {
            return;
        };
        if !w.send_raw(accept_101(&key).as_bytes()) {
            return;
        }
        let Some((opcode, masked, payload)) = w.frame() else {
            return;
        };
        let _ = tx.send((head, opcode, masked, payload));
        w.send(OP_TEXT, b"from the server");
    });

    let mut ws = tokio::time::timeout(
        BOUND,
        native().websocket(open(&format!("ws://{addr}/chat"))),
    )
    .await
    .expect("the handshake must not hang")
    .expect("the handshake must succeed");

    tokio::time::timeout(BOUND, ws.send(Message::Text("from the client".into())))
        .await
        .expect("sending must not hang")
        .expect("sending must succeed");

    let got = tokio::time::timeout(BOUND, ws.next())
        .await
        .expect("receiving must not hang")
        .expect("the stream must not have ended")
        .expect("the message must not be an error");
    assert_eq!(got, Message::Text("from the server".into()));

    let (head, opcode, masked, payload) = rx.recv_timeout(BOUND).expect("the server's report");

    // RFC 6455 §4.1: the opening handshake, as it reached the wire.
    assert!(
        head.starts_with("GET /chat HTTP/1.1\r\n"),
        "the request line must be an origin-form GET, and was {:?}",
        head.lines().next()
    );
    assert_eq!(
        header(&head, "upgrade")
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("websocket")
    );
    assert!(
        header(&head, "connection").is_some_and(|v| v.eq_ignore_ascii_case("upgrade")),
        "Connection: Upgrade, and was {:?}",
        header(&head, "connection")
    );
    assert_eq!(header(&head, "sec-websocket-version"), Some("13"));
    assert_eq!(header(&head, "host"), Some(addr.to_string().as_str()));
    // The key is a base64-encoded 16-byte nonce, so 24 characters ending
    // in the padding two bytes short of a multiple of three produce.
    let key = header(&head, "sec-websocket-key").expect("a key must be sent");
    assert!(
        key.len() == 24 && key.ends_with("=="),
        "Sec-WebSocket-Key must be 16 random bytes in base64, and was {key:?}"
    );

    // RFC 6455 §5.1: every frame a client sends MUST be masked.
    assert_eq!(opcode, OP_TEXT);
    assert!(masked, "a client's frames must be masked");
    assert_eq!(payload, b"from the client");
}

/// `read_buf`: a server may put its first frames in the same flight as the
/// `101`, and hyper will already have read them by the time the upgrade is
/// taken apart.
///
/// The fixture sends the `101` and the frame in **one** `write_all` and
/// then never writes again — so a client that dropped
/// `hyper::client::conn::http1::Parts::read_buf` has no second chance to
/// receive this message, and the test hangs rather than merely failing.
#[tokio::test]
async fn the_first_frame_may_arrive_in_the_same_flight_as_the_101() {
    let (addr, _) = serve(move |mut w| {
        let Some(head) = w.head() else { return };
        let Some(key) = header(&head, "sec-websocket-key") else {
            return;
        };
        let mut flight = accept_101(key).into_bytes();
        flight.extend_from_slice(&Wire::frame_bytes(OP_TEXT, b"in the same flight"));
        w.send_raw(&flight);
        // Deliberately nothing else, ever: this message is either in
        // `read_buf` or it is lost.
        loop {
            if !w.fill() {
                return;
            }
        }
    });

    let mut ws = tokio::time::timeout(BOUND, native().websocket(open(&format!("ws://{addr}/"))))
        .await
        .expect("the handshake must not hang")
        .expect("the handshake must succeed");

    let got = tokio::time::timeout(BOUND, ws.next())
        .await
        .expect(
            "the frame the server sent in the same flight as the 101 was never delivered: \
             hyper had already read it off the socket, so it can only have come from \
             Parts::read_buf",
        )
        .expect("the stream must not have ended")
        .expect("the message must not be an error");
    assert_eq!(got, Message::Text("in the same flight".into()));
}

/// The `101` is recognised **by status**, and before the connection is
/// polled out.
///
/// A `200` with a body is a connection hyper is not finished with, so
/// `poll_without_shutdown` on it never returns `Ready` — which is why
/// moving the status check after the `into_parts` step is a hang rather
/// than a wrong answer, and why this test is the one that catches it.
#[tokio::test]
async fn a_200_is_an_error_rather_than_a_websocket() {
    let (addr, _) = serve(move |mut w| {
        let Some(_head) = w.head() else { return };
        w.send_raw(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
        loop {
            if !w.fill() {
                return;
            }
        }
    });

    let outcome = tokio::time::timeout(BOUND, native().websocket(open(&format!("ws://{addr}/"))))
        .await
        .expect(
            "a 200 must come back as an error rather than hang: the status has to be read \
             before the connection is driven to completion",
        );
    let Err(err) = outcome else {
        panic!("a 200 is not a WebSocket")
    };
    assert_eq!(err.kind(), &ErrorKind::Status);
    assert!(
        err.to_string().contains("rather than 101"),
        "the error must name what the server actually answered, and said {err}"
    );
}

/// RFC 6455 §4.1 step 5: a `101` whose `Sec-WebSocket-Accept` is not the
/// digest of the key this client sent is refused.
///
/// The fixture is otherwise a correct server — same `101`, same `Upgrade`
/// and `Connection` headers — so the only difference from
/// `a_websocket_opens_and_carries_messages_both_ways` is the one header
/// this test is about. The server also records whatever arrived after its
/// `101`, because "refused" has to mean the client sent no WebSocket
/// traffic, not merely that it returned an error.
#[tokio::test]
async fn a_101_whose_accept_key_is_wrong_is_refused() {
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let (addr, _) = serve(move |mut w| {
        let Some(_head) = w.head() else { return };
        w.send_raw(
            b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
              Sec-WebSocket-Accept: AAAAAAAAAAAAAAAAAAAAAAAAAAA=\r\n\r\n",
        );
        while w.fill() {}
        let _ = tx.send(std::mem::take(&mut w.buf));
    });

    let outcome = tokio::time::timeout(BOUND, native().websocket(open(&format!("ws://{addr}/"))))
        .await
        .expect("the handshake must not hang");
    let Err(err) = outcome else {
        panic!("a 101 with the wrong accept key is not a WebSocket")
    };
    assert_eq!(err.kind(), &ErrorKind::Status);
    assert!(
        err.to_string().contains("Sec-WebSocket-Accept"),
        "the error must name the header it rejected, and said {err}"
    );

    let after = rx.recv_timeout(BOUND).expect("the server's report");
    assert!(
        after.is_empty(),
        "a client that refused the handshake must not have spoken WebSocket on the socket, \
         and the server received {after:?}"
    );
}

/// RFC 6455 §5.5.2: a ping is answered with a pong, and the answer is this
/// endpoint's duty rather than the caller's.
///
/// Causal, not timed. The server sends a ping and then **reads until it
/// has a pong carrying the same payload**; only then does it send the
/// message the client is waiting for. So a client that never answers a
/// ping receives nothing at all, at any speed — and the client is only
/// ever `await`ing the next *message*, never flushing, which is the shape
/// that makes the answer the transport's job.
///
/// The second assertion is the other half: the ping is not surfaced to the
/// caller. `Message` has no `Ping` variant, so the only way it could be is
/// as something else.
#[tokio::test]
async fn a_ping_is_answered_with_a_pong_which_is_what_releases_the_next_message() {
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let (addr, _) = serve(move |mut w| {
        let Some(head) = w.head() else { return };
        let Some(key) = header(&head, "sec-websocket-key") else {
            return;
        };
        if !w.send_raw(accept_101(key).as_bytes()) {
            return;
        }
        if !w.send(OP_PING, b"are you there") {
            return;
        }
        // Nothing else goes out until the pong is in.
        loop {
            let Some((opcode, _masked, payload)) = w.frame() else {
                return;
            };
            if opcode == OP_PONG {
                let _ = tx.send(payload);
                break;
            }
        }
        w.send(OP_TEXT, b"released by the pong");
    });

    let mut ws = tokio::time::timeout(BOUND, native().websocket(open(&format!("ws://{addr}/"))))
        .await
        .expect("the handshake must not hang")
        .expect("the handshake must succeed");

    let got = tokio::time::timeout(BOUND, ws.next())
        .await
        .expect(
            "the server withholds this message until it has the pong, so a client that \
             never answered the ping can never receive it",
        )
        .expect("the stream must not have ended")
        .expect("the message must not be an error");
    assert_eq!(
        got,
        Message::Text("released by the pong".into()),
        "the ping must not be surfaced to the caller — the first message the caller sees \
         is the one the server sent after the pong"
    );

    let pong = rx.recv_timeout(BOUND).expect("the server's report");
    assert_eq!(
        pong, b"are you there",
        "RFC 6455 §5.5.3: a pong carries the ping's application data unchanged"
    );
}

/// A partial write is not lost between polls.
///
/// The message is far larger than any socket buffer, and the server does
/// not read a byte of it until the client's own `poll_flush` has actually
/// returned `Pending` — so the write really is cut in the middle, and
/// `assert!(blocked)` is what says the test exercised what it claims to.
/// `Shim`'s `write` returning `buf.len()` instead of the count
/// `poll_write` reported is the mutation: `FrameCodec::write_out_buffer`
/// would then drain bytes it never sent, and the server would be left
/// waiting for a frame that can no longer arrive.
#[tokio::test]
async fn a_message_larger_than_the_socket_buffer_arrives_whole() {
    const SIZE: usize = 8 * 1024 * 1024;
    let payload: Vec<u8> = (0..SIZE).map(|i| (i % 251) as u8).collect();

    let (release_tx, release_rx) = mpsc::channel::<()>();
    let (tx, rx) = mpsc::channel::<(u8, bool, Vec<u8>)>();
    let release_rx = std::sync::Mutex::new(release_rx);
    let (addr, _) = serve(move |mut w| {
        let Some(head) = w.head() else { return };
        let Some(key) = header(&head, "sec-websocket-key") else {
            return;
        };
        if !w.send_raw(accept_101(key).as_bytes()) {
            return;
        }
        // Not one byte is read until the client says its write blocked.
        if release_rx
            .lock()
            .expect("release channel")
            .recv_timeout(BOUND)
            .is_err()
        {
            return;
        }
        let Some(frame) = w.frame() else { return };
        let _ = tx.send(frame);
    });

    let mut ws = tokio::time::timeout(BOUND, native().websocket(open(&format!("ws://{addr}/"))))
        .await
        .expect("the handshake must not hang")
        .expect("the handshake must succeed");

    let sent = payload.clone();
    let blocked = tokio::time::timeout(BOUND, async move {
        let mut ws = std::pin::Pin::new(&mut ws);
        std::future::poll_fn(|cx| ws.as_mut().poll_ready(cx))
            .await
            .expect("ready");
        ws.as_mut()
            .start_send(Message::Binary(Bytes::from(sent)))
            .expect("start_send buffers, it does not write");
        let mut blocked = false;
        std::future::poll_fn(|cx| {
            let p = ws.as_mut().poll_flush(cx);
            if p.is_pending() && !blocked {
                blocked = true;
                // The server may start draining now — and only now.
                let _ = release_tx.send(());
            }
            p
        })
        .await
        .expect("the flush must finish once the server drains");
        blocked
    })
    .await
    .expect("the send must not hang");

    assert!(
        blocked,
        "the fixture did not produce a partial write at all, so it proves nothing about \
         one: 8 MiB was accepted by the socket with nobody reading it"
    );

    let (opcode, masked, arrived) = rx.recv_timeout(BOUND).expect("the server's report");
    assert_eq!(opcode, OP_BINARY);
    assert!(masked, "a client's frames must be masked");
    assert_eq!(
        arrived.len(),
        payload.len(),
        "the whole message must arrive, and {} of {} bytes did",
        arrived.len(),
        payload.len()
    );
    assert!(
        arrived == payload,
        "the message arrived with its length intact but its contents changed, which is \
         what a write that reported more bytes than it sent looks like"
    );
}

/// The close handshake: the peer's close reaches the caller as a
/// [`Message::Close`], the client's echo reaches the peer, and the stream
/// then ends.
///
/// Causal in the same way as the pong test: the server does not close the
/// TCP connection until the client's echoing close frame has arrived, and
/// the stream cannot end before that close.
#[tokio::test]
async fn a_close_from_the_peer_is_echoed_and_ends_the_stream() {
    let (tx, rx) = mpsc::channel::<(u8, Vec<u8>)>();
    let (addr, _) = serve(move |mut w| {
        let Some(head) = w.head() else { return };
        let Some(key) = header(&head, "sec-websocket-key") else {
            return;
        };
        if !w.send_raw(accept_101(key).as_bytes()) {
            return;
        }
        let mut close = 1000u16.to_be_bytes().to_vec();
        close.extend_from_slice(b"bye");
        if !w.send(OP_CLOSE, &close) {
            return;
        }
        loop {
            let Some((opcode, _masked, payload)) = w.frame() else {
                return;
            };
            if opcode == OP_CLOSE {
                let _ = tx.send((opcode, payload));
                break;
            }
        }
        // Only now: the client's stream can only end on this.
        let _ = w.sock.shutdown(std::net::Shutdown::Both);
    });

    let mut ws = tokio::time::timeout(BOUND, native().websocket(open(&format!("ws://{addr}/"))))
        .await
        .expect("the handshake must not hang")
        .expect("the handshake must succeed");

    let got = tokio::time::timeout(BOUND, ws.next())
        .await
        .expect("the close must not hang")
        .expect("the stream must not have ended before the close was delivered")
        .expect("the close must not be an error");
    match got {
        Message::Close(Some(frame)) => {
            assert_eq!(frame.code, 1000);
            assert_eq!(frame.reason, "bye");
        }
        other => panic!("the peer's close must reach the caller whole, and was {other:?}"),
    }

    assert!(
        tokio::time::timeout(BOUND, ws.next())
            .await
            .expect("the stream must end once the peer has closed")
            .is_none(),
        "a WebSocket whose peer has closed must end its stream"
    );

    let (opcode, payload) = rx.recv_timeout(BOUND).expect("the server's report");
    assert_eq!(opcode, OP_CLOSE);
    assert_eq!(
        payload,
        [1000u16.to_be_bytes().to_vec(), b"bye".to_vec()].concat(),
        "RFC 6455 §5.5.1: the echoed close carries the peer's own status code"
    );
}

/// The four headers the handshake owns are refused rather than
/// overwritten, because overwriting is dropping a header the caller set —
/// `WebSocketConnect::websocket`'s own rule, and the one that keeps this
/// seam from being where an `Authorization` silently does not go out.
#[tokio::test]
async fn a_request_that_sets_a_handshake_header_itself_is_refused() {
    let (addr, accepted) = serve(|mut w| while w.fill() {});

    for name in [
        "sec-websocket-key",
        "connection",
        "upgrade",
        "sec-websocket-version",
    ] {
        let req = http::Request::builder()
            .uri(format!("ws://{addr}/"))
            .header(name, "x")
            .body(())
            .unwrap();
        let outcome = tokio::time::timeout(BOUND, native().websocket(req))
            .await
            .expect("must not hang");
        let Err(err) = outcome else {
            panic!("{name} is the handshake's, and must be refused")
        };
        assert_eq!(err.kind(), &ErrorKind::Unsupported, "for {name}");
    }
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        0,
        "a request refused on its own headers must not have opened a socket"
    );
}

/// `http://` and `https://` name the same two schemes as `ws://` and
/// `wss://`, so a caller holding an origin does not have to rewrite it;
/// anything else is a typed `Unsupported` rather than a silent `http`.
#[tokio::test]
async fn http_is_the_same_scheme_as_ws_and_anything_else_is_refused() {
    let (addr, _) = serve(move |mut w| {
        let Some(head) = w.head() else { return };
        let Some(key) = header(&head, "sec-websocket-key") else {
            return;
        };
        w.send_raw(accept_101(key).as_bytes());
        while w.fill() {}
    });

    tokio::time::timeout(BOUND, native().websocket(open(&format!("http://{addr}/"))))
        .await
        .expect("must not hang")
        .expect("http:// is ws://");

    let outcome = tokio::time::timeout(BOUND, native().websocket(open(&format!("ftp://{addr}/"))))
        .await
        .expect("must not hang");
    let Err(err) = outcome else {
        panic!("ftp is not a WebSocket scheme")
    };
    assert_eq!(err.kind(), &ErrorKind::Unsupported);
}

/// A WebSocket is opened on a connection of its own, and never one the
/// pool was holding.
///
/// The first assertion is the control — this fixture's connections *are*
/// pooled and reused, so the count after the upgrade is a difference the
/// observer measured rather than a number it cannot tell from any other.
#[tokio::test]
async fn a_websocket_never_takes_a_pooled_connection() {
    let (addr, accepted) = serve(move |mut w| {
        loop {
            let Some(head) = w.head() else { return };
            if header(&head, "sec-websocket-key").is_some() {
                let key = header(&head, "sec-websocket-key").expect("just checked");
                w.send_raw(accept_101(key).as_bytes());
                while w.fill() {}
                return;
            }
            if !w.send_raw(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok") {
                return;
            }
        }
    });

    // One transport, so that the pool the two GETs fill is the pool the
    // upgrade would have taken from. `Transport::execute` directly rather
    // than through `http_ng::Client`, because `Client` consumes the
    // transport and this test needs to keep asking the same one.
    let transport = native();
    for _ in 0..2 {
        let req = http::Request::builder()
            .uri(format!("http://{addr}/"))
            .body(RequestBody::Empty)
            .unwrap();
        let resp = tokio::time::timeout(BOUND, transport.execute(req))
            .await
            .expect("must not hang")
            .expect("the request must succeed");
        tokio::time::timeout(BOUND, resp.into_body().collect())
            .await
            .expect("must not hang")
            .expect("body");
    }
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        1,
        "the control: this fixture's connections are pooled and reused, or the count \
         below would say nothing about the upgrade"
    );

    let _ws = tokio::time::timeout(BOUND, transport.websocket(open(&format!("ws://{addr}/"))))
        .await
        .expect("must not hang")
        .expect("the handshake must succeed");
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        2,
        "the upgrade must have opened a socket of its own"
    );
}
