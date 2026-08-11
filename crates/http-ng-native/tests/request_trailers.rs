//! What this transport actually does with a request body's trailers —
//! measured on the wire, on both protocols, because
//! `Capabilities::request_trailers` reads `false` and the question
//! `docs/v04-design.md`'s Appendix B asks is whether that declaration has
//! one carrier or two.
//!
//! # The answer, and it re-opened the decision
//!
//! Appendix B proposed making `http-ng-native` enforce its `false` the way
//! `http-ng-h3` does (a typed `RequestTrailersNotSent`), **unless the h1
//! path turned out to send them too**. It does. `sends_request_trailers_on_
//! http1_when_the_caller_declares_them` below has a raw socket read
//! `0\r\ngrpc-status: 0\r\n\r\n` off the wire from an ordinary
//! `Native` over plaintext HTTP/1.1 — no `http2` feature involved. So the
//! field has two carriers, not one, and "make native refuse what h3
//! refuses" would have deleted a working HTTP/1.1 feature rather than
//! closed a gap.
//!
//! # The condition, which is RFC 9110's and not ours
//!
//! hyper sends request trailers only for fields the request **declared**
//! in a `Trailer:` header (`proto/h1/encode.rs`'s `Kind::Chunked(Some(..))`
//! arm, reached from `role.rs`'s client encoder when `TRAILER` is
//! present). That is RFC 9110 §6.6.1's `Trailer` field doing its job, and
//! it is a sensible rule for a hop-by-hop-sensitive feature: an
//! intermediary that must decide whether to buffer needs to know before
//! the body starts.
//!
//! It also means the **undeclared** case is a silent drop — hyper logs at
//! `debug!` and returns `None`, and the request completes successfully
//! with the trailers gone. `drops_undeclared_request_trailers_on_http1_
//! without_telling_anyone` pins that too, deliberately: it is the one
//! shape here that neither `true` nor `false` describes, and a future
//! change to the declaration has to answer for it rather than discover it.
//!
//! # Both tests are `http://`, on purpose
//!
//! Plaintext, so no ALPN and therefore HTTP/1.1 with certainty — the
//! measurement is about the h1 encoder, and a test that could have
//! negotiated h2 under `--all-features` would be measuring whichever
//! protocol it happened to get.

use http_ng::Client;
use http_ng_dns_system::SystemDns;
use http_ng_native::Native;
use http_ng_rt_tokio::Tokio;
use http_ng_tls_rustls::Rustls;
use std::io::{Read, Write};
use std::time::Duration;

const BOUND: Duration = Duration::from_secs(30);

/// One data frame, then a trailers frame. The trailer name is
/// `grpc-status` because that is the field Appendix B's gRPC case is
/// actually about.
struct DataThenTrailers(u8);

impl http_body::Body for DataThenTrailers {
    type Data = bytes::Bytes;
    type Error = http_ng_core::Error;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<bytes::Bytes>, http_ng_core::Error>>> {
        self.0 += 1;
        std::task::Poll::Ready(match self.0 {
            1 => Some(Ok(http_body::Frame::data(bytes::Bytes::from_static(
                b"AAAA",
            )))),
            2 => {
                let mut m = http::HeaderMap::new();
                m.insert("grpc-status", http::HeaderValue::from_static("0"));
                Some(Ok(http_body::Frame::trailers(m)))
            }
            _ => None,
        })
    }

    fn is_end_stream(&self) -> bool {
        self.0 >= 2
    }
}

/// Accepts one connection and reads until the chunked message is complete,
/// then answers `200`.
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

async fn send_with_trailers(declare: bool) -> String {
    let (addr, rx) = spawn_capturing_h1_server();
    let t = Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio));
    let c = Client::builder(t).build().unwrap();
    let body = http_ng_core::RequestBody::Streaming(Box::new(DataThenTrailers(0)));
    let mut req = c.post(&format!("http://{addr}/"));
    if declare {
        req = req.header("trailer", "grpc-status");
    }
    let resp = tokio::time::timeout(BOUND, req.body(body).send())
        .await
        .expect("must not hang")
        .expect("the request itself must succeed either way");
    assert_eq!(resp.status(), 200);
    let raw = rx
        .recv_timeout(BOUND)
        .expect("server must have seen a request");
    String::from_utf8_lossy(&raw).into_owned()
}

/// **The measurement Appendix B asked for.** `Capabilities::
/// request_trailers` is `false`, and on this path that `false` understates
/// the code: the trailer field reaches the server.
#[tokio::test]
async fn sends_request_trailers_on_http1_when_the_caller_declares_them() {
    let text = send_with_trailers(true).await;
    assert!(
        text.to_lowercase().contains("transfer-encoding: chunked"),
        "trailers are only expressible under chunked framing, got:\n{text}"
    );
    assert!(
        text.contains("0\r\ngrpc-status: 0\r\n\r\n"),
        "the trailer field must arrive after the last-chunk marker — this \
         is the whole measurement, and `request_trailers: false` is what it \
         contradicts. Got:\n{text}"
    );
}

/// The other half, and the reason the decision is re-opened rather than
/// simply reversed: with no `Trailer:` header the same body's trailers are
/// **dropped without a word**, and the request still succeeds.
///
/// This is neither what `request_trailers: false` promises (h3 raises a
/// typed error) nor what `true` would promise (they go out). Pinned so
/// that whoever changes the declaration has to decide about this case
/// rather than meet it by surprise.
#[tokio::test]
async fn drops_undeclared_request_trailers_on_http1_without_telling_anyone() {
    let text = send_with_trailers(false).await;
    assert!(
        text.to_lowercase().contains("transfer-encoding: chunked"),
        "still a streamed body, got:\n{text}"
    );
    assert!(
        !text.to_lowercase().contains("grpc-status"),
        "hyper sends only DECLARED trailer fields; an undeclared one must \
         not appear on the wire, or this test is measuring something else. \
         Got:\n{text}"
    );
    assert!(
        text.ends_with("0\r\n\r\n"),
        "the body must still be terminated correctly — a dropped trailer \
         frame must not also cost the last-chunk marker. Got:\n{text}"
    );
}
