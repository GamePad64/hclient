//! Response decompression end to end, against a real server on loopback —
//! v0.2 W5.
//!
//! **The observer is outside the client.** A `std::net::TcpListener` in a
//! thread puts a genuine gzip or brotli stream on the wire, under a
//! genuine `Content-Encoding`, and records the request bytes it received;
//! the assertions are on the PLAINTEXT that comes back out of
//! `collect().text()` and on what the server saw. A test that only checked
//! `Accept-Encoding` was sent would pass for a client that asked politely
//! and then handed the caller a compressed blob.
//!
//! Everything here goes through `hclient-native` over a real socket rather
//! than through a mock, for the same reason `tests/deadline.rs` does: the
//! property is that hyper's framing, the socket's chunk boundaries and the
//! decoder agree, and a mock body cannot be wrong about that in any
//! interesting way. `tests/compression_capability.rs` is the mock half —
//! the gate, which is about `Capabilities` and needs no wire at all — and
//! it is also where the gzip stream comes from an INDEPENDENT
//! implementation (GNU gzip), which this file's `flate2`-produced streams
//! deliberately do not.
//!
//! The whole-file gate is `two_runtimes.rs`'s and `deadline.rs`'s: on
//! `wasm32-*` there is no `TcpListener` and the dev-dependencies this file
//! needs are target-gated in `Cargo.toml`. The feature gate is this task's
//! own — without the decoders compiled in there is nothing here to check.
#![cfg(all(feature = "gzip", feature = "brotli", not(target_family = "wasm")))]

use hclient::Client;
use hclient_dns_system::SystemDns;
use hclient_native::Native;
use hclient_rt_tokio::Tokio;
use hclient_tls_rustls::Rustls;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

type NativeTransport = Native<Tokio, Rustls, SystemDns<Tokio>>;

/// `with_webpki_roots`, like `deadline.rs` and `two_runtimes.rs`: no
/// handshake happens on these plain-HTTP servers, but `Native::new` still
/// needs a concrete `TlsConnect`, and this one does not touch the system
/// trust store.
fn transport() -> NativeTransport {
    Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio))
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

/// Long enough that the compressed form is many times smaller and arrives
/// in more than one frame — a decoder that only ever meets a whole stream
/// in one buffer is not being tested as a streaming decoder.
fn plaintext() -> String {
    (0..4000)
        .map(|i| format!("line {i}: the quick brown fox jumps over the lazy dog\n"))
        .collect()
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    e.write_all(data).expect("gzip");
    e.finish().expect("gzip")
}

fn brotli(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut w = brotli::CompressorWriter::new(&mut out, 4096, 9, 22);
        w.write_all(data).expect("brotli");
    }
    out
}

/// Every request line + header block the server received, in order.
type Seen = Arc<Mutex<Vec<String>>>;

/// A server that answers `/` with `body` under `Content-Encoding: encoding`
/// and `/redirect` with a 302 to `/`, recording every request it read.
///
/// The body is written in eight pieces with a breath between them, so the
/// compressed stream really does cross frame boundaries on the way in
/// rather than arriving as one buffer by luck of the MTU.
fn server(body: Vec<u8>, encoding: &'static str) -> (std::net::SocketAddr, Seen) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&seen);
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let mut buf = [0u8; 4096];
            let n = s.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).into_owned();
            let redirect = req.starts_with("GET /redirect");
            recorder.lock().expect("seen").push(req);

            if redirect {
                let _ =
                    s.write_all(b"HTTP/1.1 302 Found\r\nLocation: /\r\nContent-Length: 0\r\n\r\n");
                let _ = s.flush();
                continue;
            }

            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Encoding: {}\r\n\
                 Content-Length: {}\r\n\r\n",
                encoding,
                body.len()
            );
            if s.write_all(head.as_bytes()).is_err() {
                continue;
            }
            for piece in body.chunks(body.len().div_ceil(8)) {
                if s.write_all(piece).is_err() || s.flush().is_err() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
    });
    (addr, seen)
}

/// The headline test: a real gzip body off a real socket, asserted on the
/// plaintext.
#[test]
fn a_gzip_body_from_a_real_server_arrives_as_plaintext() {
    let want = plaintext();
    let coded = gzip(want.as_bytes());
    assert!(
        coded.len() * 4 < want.len(),
        "the fixture must actually be compressed, or this test would pass \
         for a client that decoded nothing: {} vs {}",
        coded.len(),
        want.len()
    );
    let (addr, seen) = server(coded.clone(), "gzip");
    let c = Client::builder(transport()).build().expect("supported");

    let got = rt().block_on(async {
        c.get(&format!("http://{addr}/"))
            .send()
            .await
            .expect("responds")
            .collect()
            .await
            .expect("decodes")
    });

    assert_eq!(got.text().expect("utf-8"), want);
    // And the wire really did carry the compressed form — otherwise the
    // assertion above would be satisfied by a server that never compressed
    // and a client that never decoded.
    let req = &seen.lock().expect("seen")[0];
    // `contains("gzip")` and not `contains("accept-encoding: gzip")`: the
    // header names every coding this build can reverse, in preference
    // order, and gzip stopped being first the day `zstd` and `deflate`
    // arrived. What this asserts is that the coding decoded was asked
    // for — the position in the list is `decompress.rs`'s decision and is
    // pinned there.
    assert!(
        req.to_ascii_lowercase().contains("gzip"),
        "the client must have asked for the coding it decoded: {req}"
    );
    // The two headers that stopped being true.
    assert!(
        got.headers().get(http::header::CONTENT_ENCODING).is_none(),
        "the body handed over is not encoded any more"
    );
    assert!(
        got.headers().get(http::header::CONTENT_LENGTH).is_none(),
        "`Content-Length` counted the {} bytes on the wire, not the {} the \
         caller can read",
        coded.len(),
        want.len()
    );
}

/// The same, one coding over: brotli's decoder is an entirely different
/// crate with an entirely different end-of-stream rule, so it is checked
/// on its own rather than assumed to follow gzip.
#[test]
fn a_brotli_body_from_a_real_server_arrives_as_plaintext() {
    let want = plaintext();
    let coded = brotli(want.as_bytes());
    assert!(
        coded.len() * 4 < want.len(),
        "the fixture must be compressed"
    );
    let (addr, seen) = server(coded, "br");
    let c = Client::builder(transport()).build().expect("supported");

    let got = rt().block_on(async {
        c.get(&format!("http://{addr}/"))
            .send()
            .await
            .expect("responds")
            .collect()
            .await
            .expect("decodes")
    });

    assert_eq!(got.text().expect("utf-8"), want);
    let req = &seen.lock().expect("seen")[0];
    assert!(
        req.to_ascii_lowercase().contains("accept-encoding:")
            && req.to_ascii_lowercase().contains("br"),
        "the client must have asked for the coding it decoded: {req}"
    );
}

/// The negotiation survives a redirect, and so does the decoding.
///
/// `Accept-Encoding` is set once, before the first hop; the hops after it
/// inherit the headers (`stages::redirect::next_hop`). A client that set
/// the header per hop from a value computed per hop could get this right
/// too — what this test rules out is the arrangement where it is set on
/// the request the caller handed in and then lost, so the last hop, whose
/// response is the one that gets decoded, never asked for a coding.
#[test]
fn the_negotiation_and_the_decoding_both_survive_a_redirect() {
    let want = plaintext();
    let (addr, seen) = server(gzip(want.as_bytes()), "gzip");
    let c = Client::builder(transport()).build().expect("supported");

    let got = rt().block_on(async {
        c.get(&format!("http://{addr}/redirect"))
            .send()
            .await
            .expect("responds")
            .collect()
            .await
            .expect("decodes")
    });

    assert_eq!(got.text().expect("utf-8"), want);
    let seen = seen.lock().expect("seen");
    assert_eq!(seen.len(), 2, "one hop and the request it redirected to");
    for (i, req) in seen.iter().enumerate() {
        assert!(
            req.to_ascii_lowercase().contains("accept-encoding:")
                && req.to_ascii_lowercase().contains("gzip"),
            "hop {i} went out without the header: {req}"
        );
    }
}

/// A server that compresses nothing is left completely alone — the common
/// case, and the one a decoder wired in unconditionally would break.
#[test]
fn a_response_with_no_content_encoding_is_handed_through_untouched() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let mut b = [0u8; 2048];
            let _ = s.read(&mut b);
            let _ = s.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\n\r\nplain",
            );
        }
    });
    let c = Client::builder(transport()).build().expect("supported");

    let got = rt().block_on(async {
        c.get(&format!("http://{addr}/"))
            .send()
            .await
            .expect("responds")
            .collect()
            .await
            .expect("nothing to decode")
    });

    assert_eq!(got.text().expect("utf-8"), "plain");
    assert_eq!(
        got.headers()[http::header::CONTENT_LENGTH],
        "5",
        "an untouched response keeps every header it arrived with"
    );
}

/// A gzip stream cut off mid-transfer must not read as a complete, shorter
/// document. This is the failure a decoder without an end-of-stream check
/// gets wrong: the bytes that did arrive decode perfectly well, and the
/// caller would be handed a truncated body with no indication of it.
#[test]
fn a_stream_the_server_cuts_short_is_an_error_not_a_shorter_document() {
    let want = plaintext();
    let coded = gzip(want.as_bytes());
    let short = coded[..coded.len() / 2].to_vec();
    let (full_len, short_len) = (coded.len(), short.len());

    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let mut b = [0u8; 2048];
            let _ = s.read(&mut b);
            // Chunked, and the terminating zero-length chunk IS sent: the
            // body ends cleanly as far as HTTP is concerned, and only the
            // gzip stream inside it is incomplete. Under `Content-Length`
            // the transport would report the truncation itself and this
            // would prove nothing about the decoder.
            let _ = s.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\n\
                  Transfer-Encoding: chunked\r\n\r\n",
            );
            let _ = s.write_all(format!("{:x}\r\n", short.len()).as_bytes());
            let _ = s.write_all(&short);
            let _ = s.write_all(b"\r\n0\r\n\r\n");
            let _ = s.flush();
        }
    });
    let c = Client::builder(transport()).build().expect("supported");

    let err = rt().block_on(async {
        c.get(&format!("http://{addr}/"))
            .send()
            .await
            .expect("the head arrives fine")
            .collect()
            .await
            .expect_err("half of a gzip stream is not a document")
    });
    assert_eq!(
        *err.kind(),
        hclient::ErrorKind::Decode,
        "{} of {full_len} bytes arrived and the client called it a success: {err:?}",
        short_len
    );
}
