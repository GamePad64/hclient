//! What this transport actually does with a request body's trailers —
//! measured on the wire, on both protocols, because
//! `Capabilities::request_trailers` is the declaration those two
//! behaviours have to be true of.
//!
//! # Three behaviours under one field, and only one of them was wrong
//!
//! The plan was to make `hclient-native` enforce a `false` the way
//! `hclient-h3` does, with a typed `RequestTrailersNotSent`, **unless the
//! h1 path turned out to send them too**. It does.
//! `sends_request_trailers_on_http1_when_the_caller_declares_them` below
//! has a raw socket read `0\r\ngrpc-status: 0\r\n\r\n` off the wire from
//! an ordinary `Native` over plaintext HTTP/1.1 — no `http2` feature
//! involved — and `sends_request_trailers_on_http2_without_any_
//! declaration` has an `h2::server` decode the same field off a HEADERS
//! frame with no `Trailer:` header anywhere in the request. So the field
//! has two carriers, the declaration is `true` (Appendix C), and "make
//! native refuse what h3 refuses" would have deleted a working feature
//! rather than closed a gap.
//!
//! What was wrong is the third behaviour, and it was the silence rather
//! than the drop. hyper sends request trailers only for fields the
//! request **declared** in a `Trailer:` header (`proto/h1/encode.rs`'s
//! `Kind::Chunked(Some(..))` arm, reached from `role.rs`'s client encoder
//! when `TRAILER` is present) — that is RFC 9110 §6.6.2's `Trailer` field
//! doing its job, and it is a sensible rule for a hop-by-hop-sensitive
//! feature: an intermediary that must decide whether to buffer needs to
//! know before the body starts. An **undeclared** field was logged at
//! `debug!`, dropped, and the request completed with a `200`. It is
//! `hclient_native::UndeclaredRequestTrailers` now, and the three tests
//! that pin it are the two shapes that must raise it (no header at all,
//! and a header naming another field) and the one that must not (an
//! empty trailers frame, which loses nothing because there is nothing to
//! lose).
//!
//! # Where the error lands, measured rather than assumed
//!
//! `undeclared_request_trailers_on_http1_are_a_typed_error_naming_the_
//! field` asserts what the server saw as well as what the caller got,
//! because the two together are the decision: the guard fires when the
//! trailers frame arrives, which is the first moment the fact exists, and
//! by then the message is part-written. What the refusal buys is the
//! last-chunk marker — the request is aborted rather than finished
//! without the caller's data — and the test asserts the absence of that
//! marker, not merely the presence of the error.
//!
//! # The HTTP/1 tests are `http://`, on purpose
//!
//! Plaintext, so no ALPN and therefore HTTP/1.1 with certainty — the
//! measurement is about the h1 encoder, and a test that could have
//! negotiated h2 under `--all-features` would be measuring whichever
//! protocol it happened to get. The HTTP/2 half is in its own module at
//! the bottom, behind the feature, with a TLS stub of its own for the
//! reason `tests/stream_reset.rs` gives for carrying one.
#![cfg(not(target_family = "wasm"))]

use hclient::Client;
use hclient_dns_system::SystemDns;
use hclient_native::{Native, UndeclaredRequestTrailers};
use hclient_rt_tokio::Tokio;
use hclient_tls_rustls::Rustls;
use std::error::Error as _;
use std::io::{Read, Write};
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;
use std::time::Duration;

const BOUND: Duration = Duration::from_secs(30);

/// One data frame, then a trailers frame.
///
/// `field` is the trailer's name — `grpc-status` in most tests, because
/// that is the field Appendix B's gRPC case is actually about — or `None`
/// for an **empty** trailers frame, which `http_body::Frame::trailers_ref`
/// still answers `Some` for and which must not be mistaken for an
/// undeclared one.
struct DataThenTrailers {
    polled: u8,
    field: Option<&'static str>,
    /// Yield `Pending` once between the data frame and the trailers frame
    /// — see [`DataThenTrailers::pending_before_trailers`].
    pend: bool,
}

impl DataThenTrailers {
    fn new(field: Option<&'static str>) -> Self {
        Self {
            polled: 0,
            field,
            pend: false,
        }
    }

    /// Pend once before the trailers, which is what decides **how much of
    /// the request has already reached the server** when the guard fires.
    ///
    /// hyper buffers the head and each chunk and flushes them in
    /// `Dispatcher::poll_loop`, which calls `poll_write` and then
    /// `poll_flush`. A body that answers `Ready` for every frame is
    /// drained inside one `poll_write`, so an error on the trailers frame
    /// aborts the connection with the head still in the write buffer and
    /// **nothing at all** on the wire. A body that pends first gets a
    /// flush in between — the shape of any real streaming body, since a
    /// gRPC client computes `grpc-status` after doing work — and there
    /// the abort leaves a truncated message behind. Both are measured
    /// below, because "the request has already gone" is true of one and
    /// not the other, and the error type says exactly that rather than
    /// picking the reassuring half.
    fn pending_before_trailers(mut self) -> Self {
        self.pend = true;
        self
    }
}

impl http_body::Body for DataThenTrailers {
    type Data = bytes::Bytes;
    type Error = hclient_core::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<bytes::Bytes>, hclient_core::Error>>> {
        if self.pend && self.polled == 1 {
            self.pend = false;
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        self.polled += 1;
        let field = self.field;
        Poll::Ready(match self.polled {
            1 => Some(Ok(http_body::Frame::data(bytes::Bytes::from_static(
                b"AAAA",
            )))),
            2 => {
                let mut m = http::HeaderMap::new();
                if let Some(f) = field {
                    m.insert(f, http::HeaderValue::from_static("0"));
                }
                Some(Ok(http_body::Frame::trailers(m)))
            }
            _ => None,
        })
    }

    fn is_end_stream(&self) -> bool {
        self.polled >= 2 && !self.pend
    }
}

/// Accepts one connection and reads until the chunked message is complete
/// **or the client gives up on it**, then answers `200`.
///
/// **Read in a loop, not once**, and the loop's exit condition is the
/// message's own framing rather than a length or a timer: the trailers are
/// written after the last-chunk marker, so a single `read` — the shape
/// every other capturing fixture in this crate uses — can return the head
/// and the data and miss precisely the bytes these tests are about. The
/// terminator of a chunked body with trailers is `\r\n0\r\n`, the trailer
/// lines, and a final CRLF; without trailers it is `\r\n0\r\n\r\n`. Both
/// end in `\r\n\r\n` after a last-chunk marker, so waiting for both facts
/// is the same condition for the two cases and needs no clock.
///
/// A request the client **aborts** never satisfies that condition, and
/// that is the point rather than a hazard: the loop then ends on the
/// client's own `FIN` (`read` returning `0`), and what it hands back is
/// exactly the prefix that reached the server. The `200` written
/// afterwards goes into a socket nobody is reading, which costs nothing
/// and keeps one fixture for both cases.
fn spawn_capturing_h1_server() -> (std::net::SocketAddr, std::sync::mpsc::Receiver<Vec<u8>>) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        if let Ok((mut s, _)) = l.accept() {
            let mut acc = Vec::new();
            let mut b = [0u8; 4096];
            while let Ok(n) = s.read(&mut b) {
                if n == 0 {
                    break;
                }
                acc.extend_from_slice(&b[..n]);
                if acc.windows(5).any(|w| w == b"\r\n0\r\n") && acc.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let _ = tx.send(acc);
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
        }
    });
    (addr, rx)
}

/// One exchange against that server: `declare` is the `Trailer:` header to
/// send (or none) and `frames` is the request body. Returns what the
/// caller got and what the server saw.
async fn exchange(
    declare: Option<&str>,
    frames: DataThenTrailers,
) -> (Result<u16, hclient_core::Error>, String) {
    let (addr, rx) = spawn_capturing_h1_server();
    let t = Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio));
    let c = Client::builder(t).build().unwrap();
    let body = hclient_core::RequestBody::Streaming(Box::new(frames));
    let mut req = c.post(&format!("http://{addr}/"));
    if let Some(d) = declare {
        req = req.header("trailer", d);
    }
    let outcome = tokio::time::timeout(BOUND, req.body(body).send())
        .await
        .expect("must not hang")
        .map(|r| r.status().as_u16());
    let raw = rx
        .recv_timeout(BOUND)
        .expect("server must have seen a connection");
    (outcome, String::from_utf8_lossy(&raw).into_owned())
}

/// Pulls the transport's own error type out from under
/// `hclient_core::Error`, the way a caller would.
fn undeclared(e: &hclient_core::Error) -> &UndeclaredRequestTrailers {
    assert_eq!(
        e.kind(),
        &hclient_core::ErrorKind::Body,
        "the request body is what failed, and `Unsupported` is \
         `hclient-h3`'s answer for a transport that cannot send trailers \
         at all — this one can. Got: {e}"
    );
    e.source()
        .and_then(|s| s.downcast_ref::<UndeclaredRequestTrailers>())
        .unwrap_or_else(|| panic!("not an `UndeclaredRequestTrailers`: {e}"))
}

/// **The measurement Appendix B asked for, and the reason
/// `request_trailers` is `true`.** The trailer field reaches the server
/// over plain HTTP/1.1.
#[tokio::test]
async fn sends_request_trailers_on_http1_when_the_caller_declares_them() {
    let (outcome, text) = exchange(
        Some("grpc-status"),
        DataThenTrailers::new(Some("grpc-status")),
    )
    .await;
    assert_eq!(
        outcome.expect("a declared trailer is an ordinary request"),
        200
    );
    assert!(
        text.to_lowercase().contains("transfer-encoding: chunked"),
        "trailers are only expressible under chunked framing, got:\n{text}"
    );
    assert!(
        text.contains("0\r\ngrpc-status: 0\r\n\r\n"),
        "the trailer field must arrive after the last-chunk marker — this \
         is the whole measurement, and it is what `request_trailers: \
         false` used to contradict. Got:\n{text}"
    );
}

/// `Trailer:` is a comma-separated list of case-insensitive field names,
/// and the guard has to read it the way hyper's encoder does or it will
/// refuse a request hyper would have sent.
///
/// Both halves in one header, because both are the same parse and either
/// alone would leave the other unpinned: a guard that took the whole
/// header value as one name would not find `grpc-status` in
/// `X-Checksum, Grpc-Status`, and one that compared raw strings would not
/// match the capitalised spelling.
#[tokio::test]
async fn a_comma_separated_and_differently_cased_declaration_still_declares() {
    let (outcome, text) = exchange(
        Some("X-Checksum, Grpc-Status"),
        DataThenTrailers::new(Some("grpc-status")),
    )
    .await;
    assert_eq!(
        outcome.expect("the field is declared, in the second position and in another case"),
        200
    );
    assert!(
        text.contains("0\r\ngrpc-status: 0\r\n\r\n"),
        "and it goes out, so the guard agreed with the encoder rather \
         than merely staying quiet. Got:\n{text}"
    );
}

/// The defect Appendix C decided to kill: a caller who attached trailers
/// and omitted `Trailer:` used to get a `200` with the data gone and
/// nothing said.
///
/// Three claims, and the third is the one that says where the error is
/// raised. The caller gets a typed error; the error names the field it
/// could not send; and the server never received a last-chunk marker —
/// the message was **aborted** rather than completed without the
/// trailers, which is the whole difference a guard at the frame can make
/// on a request that is already part-written.
#[tokio::test]
async fn undeclared_request_trailers_on_http1_are_a_typed_error_naming_the_field() {
    let (outcome, text) = exchange(
        None,
        DataThenTrailers::new(Some("grpc-status")).pending_before_trailers(),
    )
    .await;
    let e = outcome.expect_err("the trailers cannot be sent and saying so is the point");
    assert_eq!(
        undeclared(&e).fields(),
        [http::HeaderName::from_static("grpc-status")],
        "the error has to name the field, or a caller cannot write the \
         `Trailer:` header that fixes it"
    );
    assert!(
        !text.to_lowercase().contains("grpc-status"),
        "hyper sends only DECLARED trailer fields; an undeclared one must \
         not appear on the wire, or this test is measuring something \
         else. Got:\n{text}"
    );
    assert!(
        text.contains("4\r\nAAAA\r\n"),
        "the head and the data chunk before the trailers really did reach \
         the server — this body pends between them, so hyper flushed. \
         That is the half of `UndeclaredRequestTrailers`'s message a \
         caller must not be allowed to disbelieve. Got:\n{text}"
    );
    assert!(
        !text.contains("\r\n0\r\n"),
        "and the request must be left UNTERMINATED: a last-chunk marker \
         would mean the server had been handed a well-formed message with \
         the caller's trailers silently missing, which is exactly the \
         outcome this error exists to prevent. Got:\n{text}"
    );
}

/// The other half of *how much has already gone*, and it is the reason
/// the error says "may" rather than "has".
///
/// The same undeclared trailer from a body that never pends: hyper drains
/// it inside a single `poll_write` and the abort takes the head with it,
/// so **not one byte** reaches the server. Nothing about the guard
/// changes between this test and the one above — what changes is the
/// caller's body — and an error message that promised either outcome
/// would be wrong for one of them.
///
/// A failure here is informative rather than alarming: it would mean
/// hyper flushes earlier than it did in 1.11, which makes the error's
/// wording more true rather than less.
#[tokio::test]
async fn a_body_that_never_pends_is_refused_before_any_of_it_is_flushed() {
    let (outcome, text) = exchange(None, DataThenTrailers::new(Some("grpc-status"))).await;
    outcome.expect_err("the same refusal, for the same reason");
    assert!(
        text.is_empty(),
        "the head was still in hyper's write buffer when the trailers \
         frame failed the body, so the server saw a connection and no \
         request at all. Got:\n{text}"
    );
}

/// The same error for a `Trailer:` that names a **different** field,
/// because hyper's encoder compares names rather than checking that the
/// header exists — and a guard that only checked for the header's
/// presence would let this one through to be dropped in silence.
#[tokio::test]
async fn a_trailer_header_naming_another_field_is_the_same_error() {
    let (outcome, text) = exchange(
        Some("x-checksum"),
        DataThenTrailers::new(Some("grpc-status")),
    )
    .await;
    let e = outcome.expect_err("declaring one field does not license another");
    assert_eq!(
        undeclared(&e).fields(),
        [http::HeaderName::from_static("grpc-status")],
        "the field named must be the one that could not be sent, not the \
         one that was declared"
    );
    assert!(!text.to_lowercase().contains("grpc-status"), "got:\n{text}");
}

/// An **empty** trailers frame is not an undeclared trailer.
///
/// `Frame::trailers_ref` answers `Some(&HeaderMap)` for an empty map, so a
/// guard written on "is this a trailers frame" rather than on the names in
/// it would fail a body that lost nothing: an empty map puts no field on
/// the wire whether it was declared or not. The request completes, and the
/// last-chunk marker is there to prove the message was finished normally.
#[tokio::test]
async fn an_empty_trailers_frame_is_not_an_undeclared_trailer() {
    let (outcome, text) = exchange(None, DataThenTrailers::new(None)).await;
    assert_eq!(
        outcome.expect("an empty trailers frame costs the caller nothing"),
        200
    );
    assert!(
        text.ends_with("0\r\n\r\n"),
        "the body must be terminated normally, got:\n{text}"
    );
}

/// The HTTP/2 half of the declaration.
///
/// It is a separate module rather than a separate file so that the two
/// halves of one capability are read together, and it carries its own TLS
/// stub for the reason `tests/stream_reset.rs` states: ALPN is the only
/// route to HTTP/2 here and `TlsConnect` is what reports it. The hazard of
/// a stub is that a mis-wired one would silently measure HTTP/1.1
/// instead; here the server speaks nothing but HTTP/2, so a client that
/// sent an HTTP/1.1 request would get no answer at all.
#[cfg(feature = "http2")]
mod over_http2 {
    use super::{BOUND, DataThenTrailers};
    use hclient::Client;
    use hclient_dns_system::SystemDns;
    use hclient_native::Native;
    use hclient_rt_tokio::Tokio;
    use hclient_tls::{TlsConfigId, TlsConnect, TlsIdentity, TlsInfo, TlsRequest};
    use std::sync::{Arc, Mutex};

    /// A `TlsConnect` that encrypts nothing and reports `h2` — the same
    /// stub, for the same reason, as `tests/http2.rs`'s `FakeTls`.
    #[derive(Clone)]
    struct H2Alpn(TlsConfigId);

    impl TlsIdentity for H2Alpn {
        fn config_id(&self) -> TlsConfigId {
            self.0
        }
    }

    impl TlsConnect for H2Alpn {
        type Stream<S>
            = S
        where
            S: hyper::rt::Read + hyper::rt::Write + Unpin;

        fn reports_alpn(&self) -> bool {
            true
        }

        async fn connect<S>(
            &self,
            io: S,
            _req: TlsRequest<'_>,
        ) -> Result<(S, TlsInfo), hclient_core::Error>
        where
            S: hyper::rt::Read + hyper::rt::Write + Unpin,
        {
            Ok((
                io,
                TlsInfo {
                    alpn: Some(b"h2".to_vec()),
                    ..Default::default()
                },
            ))
        }
    }

    /// One HTTP/2 connection: reads the request body, then the trailers
    /// h2 delivers as a second HEADERS frame, records them and answers
    /// `200`.
    async fn serve(
        tcp: tokio::net::TcpStream,
        seen: Arc<Mutex<Vec<Option<http::HeaderMap>>>>,
    ) -> Result<(), h2::Error> {
        let mut conn = h2::server::handshake(tcp).await?;
        while let Some(accepted) = conn.accept().await {
            let (req, mut respond) = accepted?;
            let seen = Arc::clone(&seen);
            tokio::spawn(async move {
                let (_, mut body) = req.into_parts();
                while let Some(chunk) = body.data().await {
                    let Ok(chunk) = chunk else { return };
                    let _ = body.flow_control().release_capacity(chunk.len());
                }
                let trailers = body.trailers().await.ok().flatten();
                seen.lock().unwrap().push(trailers);
                let response = http::Response::builder().status(200).body(()).unwrap();
                if let Ok(mut send) = respond.send_response(response, false) {
                    let _ = send.send_data(bytes::Bytes::from_static(b"ok"), true);
                }
            });
        }
        Ok(())
    }

    fn spawn_h2_server() -> (
        std::net::SocketAddr,
        Arc<Mutex<Vec<Option<http::HeaderMap>>>>,
    ) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let seen: Arc<Mutex<Vec<Option<http::HeaderMap>>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_for_thread = Arc::clone(&seen);
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                while let Ok((tcp, _)) = listener.accept().await {
                    let seen = Arc::clone(&seen_for_thread);
                    tokio::spawn(async move {
                        let _ = serve(tcp, seen).await;
                    });
                }
            });
        });
        (addr, seen)
    }

    /// **The second carrier, and the one with no condition on it.** RFC
    /// 9113 §8.1 puts a trailer section in a HEADERS frame and asks for
    /// no `Trailer:` header at all — which is why a gRPC client sends
    /// `grpc-status` without declaring it, and why the HTTP/1.1
    /// declaration this transport now insists on is a fact about that
    /// protocol rather than about this crate.
    #[tokio::test]
    async fn sends_request_trailers_on_http2_without_any_declaration() {
        let (addr, seen) = spawn_h2_server();
        let t = Native::new(
            Tokio,
            H2Alpn(TlsConfigId::new_unique()),
            SystemDns::new(Tokio),
        );
        let c = Client::builder(t).build().unwrap();
        let body = hclient_core::RequestBody::Streaming(Box::new(DataThenTrailers::new(Some(
            "grpc-status",
        ))));
        let resp =
            tokio::time::timeout(BOUND, c.post(&format!("https://{addr}/")).body(body).send())
                .await
                .expect("must not hang")
                .expect("HTTP/2 needs no `Trailer:` and must not be refused one");
        assert_eq!(resp.status(), 200);
        assert_eq!(
            resp.version(),
            http::Version::HTTP_2,
            "the stub is what chooses the protocol, so the protocol is \
             asserted rather than assumed"
        );

        let trailers = seen.lock().unwrap().clone();
        assert_eq!(trailers.len(), 1, "exactly one request reached the server");
        let m = trailers[0]
            .clone()
            .expect("the server must have decoded a trailer section");
        assert_eq!(
            m.get("grpc-status").map(|v| v.as_bytes()),
            Some(&b"0"[..]),
            "the field the caller attached, decoded by `h2` from the \
             second HEADERS frame"
        );
    }
}
