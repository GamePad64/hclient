//! `deflate` and `zstd` end to end, against a real server on loopback.
//!
//! `tests/compression.rs` is the same shape for gzip and brotli and says
//! why the observer is outside the client; this file exists beside it
//! rather than inside it so that neither coding's wire test drags the
//! other's feature into its gate. What is different here is what each
//! coding brings that the first two did not:
//!
//! - **`deflate` has two wire formats under one token.** RFC 9110
//!   §8.4.1.2 specifies zlib and its own Note records that a long tail of
//!   servers sends raw. So the headline test is a **pair**: the same
//!   plaintext, the same `Content-Encoding: deflate`, two different byte
//!   streams, both arriving as text. Either half alone would pass for a
//!   client that had simply guessed the right one.
//!
//! - **`zstd`'s encoder here is libzstd itself**, through the `zstd` 0.13
//!   dev-dependency, and the decoder is `ruzstd`. They share not one line,
//!   which is the point — `tests/compression.rs` notes that its
//!   `flate2`-produced gzip streams deliberately are *not* independent of
//!   the decoder, and leaves that to `compression_capability.rs`'s GNU
//!   gzip. Here the independence is in this file.
#![cfg(all(
    any(feature = "deflate", feature = "zstd"),
    not(target_family = "wasm")
))]

use hclient::Client;
use hclient_dns_system::SystemDns;
use hclient_native::Native;
use hclient_rt_tokio::Tokio;
use hclient_tls_rustls::Rustls;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

fn transport() -> Native<Tokio, Rustls, SystemDns<Tokio>> {
    Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio))
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

/// Long enough to compress well and to cross frame boundaries on the way
/// in — a decoder that only ever meets a whole stream in one buffer is not
/// being tested as a streaming decoder.
fn plaintext() -> String {
    (0..4000)
        .map(|i| format!("line {i}: the quick brown fox jumps over the lazy dog\n"))
        .collect()
}

type Seen = Arc<Mutex<Vec<String>>>;

/// Answers every request with `body` under `Content-Encoding: encoding`,
/// written in eight pieces with a breath between them.
fn server(body: Vec<u8>, encoding: &'static str) -> (std::net::SocketAddr, Seen) {
    serve(body, encoding, true)
}

/// `whole == false` sends only the first two thirds of `body` — **inside
/// a chunked body that ends cleanly**, so that HTTP is complete and only
/// the compressed stream inside it is not.
///
/// That framing is the whole point and `tests/compression.rs` names the
/// trap it avoids: under `Content-Length` the transport reports the short
/// read itself, the error arrives as `ErrorKind::Body`, and the decoder's
/// end-of-stream check is never reached — so the test would pass for a
/// decoder that had none. Written the wrong way first, and the two
/// truncation tests below passed against `ErrorKind::Body` before this
/// comment existed.
fn serve(body: Vec<u8>, encoding: &'static str, whole: bool) -> (std::net::SocketAddr, Seen) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&seen);
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let mut buf = [0u8; 4096];
            let n = s.read(&mut buf).unwrap_or(0);
            recorder
                .lock()
                .expect("seen")
                .push(String::from_utf8_lossy(&buf[..n]).into_owned());

            let send: &[u8] = if whole {
                &body
            } else {
                &body[..body.len() * 2 / 3]
            };
            let framing = if whole {
                format!("Content-Length: {}\r\n", send.len())
            } else {
                "Transfer-Encoding: chunked\r\n".to_owned()
            };
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Encoding: {encoding}\r\n\
                 {framing}\r\n"
            );
            if s.write_all(head.as_bytes()).is_err() {
                continue;
            }
            let mut broken = false;
            for piece in send.chunks(send.len().div_ceil(8).max(1)) {
                let framed = if whole {
                    piece.to_vec()
                } else {
                    let mut v = format!("{:x}\r\n", piece.len()).into_bytes();
                    v.extend_from_slice(piece);
                    v.extend_from_slice(b"\r\n");
                    v
                };
                if s.write_all(&framed).is_err() || s.flush().is_err() {
                    broken = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            // The terminating zero-length chunk IS sent: as far as HTTP is
            // concerned this body ended normally.
            if !whole && !broken {
                let _ = s.write_all(b"0\r\n\r\n");
                let _ = s.flush();
            }
        }
    });
    (addr, seen)
}

/// Fetches `/` and hands back the decoded text, or the error.
fn fetch(addr: std::net::SocketAddr) -> Result<String, hclient_core::Error> {
    let c = Client::builder(transport()).build().expect("supported");
    rt().block_on(async {
        let body = c
            .get(&format!("http://{addr}/"))
            .send()
            .await?
            .collect()
            .await?;
        body.text()
    })
}

#[cfg(feature = "deflate")]
fn zlib_wrapped(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    e.write_all(data).expect("zlib");
    e.finish().expect("zlib")
}

#[cfg(feature = "deflate")]
fn raw_deflate(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
    e.write_all(data).expect("raw deflate");
    e.finish().expect("raw deflate")
}

/// **Both spellings of one token**, which is the whole of the `deflate`
/// design: RFC 9110 §8.4.1.2 says zlib, its own Note says a good many
/// servers send raw, and the client decides from the first two bytes
/// rather than from a failure.
///
/// Asserted as a pair in one test on purpose. A client that always assumed
/// zlib passes the first half; one that always assumed raw passes the
/// second; only a client that looks passes both. The premise — that the
/// two encoders really did produce different bytes for the same input — is
/// asserted too, or this would be one test run twice.
#[cfg(feature = "deflate")]
#[test]
fn both_wire_formats_of_deflate_arrive_as_the_same_plaintext() {
    let want = plaintext();
    let zlib = zlib_wrapped(want.as_bytes());
    let raw = raw_deflate(want.as_bytes());
    assert_ne!(
        zlib, raw,
        "the two encoders must differ, or this test is one test"
    );
    assert_eq!(
        &zlib[2..zlib.len() - 4],
        &raw[..],
        "and they must differ only by RFC 1950's two-byte header and its \
         four-byte Adler-32 trailer — the same RFC 1951 stream in between, \
         which is what makes the sniff a two-byte question"
    );

    for (what, coded) in [("zlib-wrapped", zlib), ("raw", raw)] {
        assert!(
            coded.len() * 4 < want.len(),
            "{what}: the fixture must actually be compressed"
        );
        let (addr, seen) = server(coded, "deflate");
        assert_eq!(fetch(addr).expect(what), want, "{what}");
        let req = &seen.lock().expect("seen")[0];
        assert!(
            req.to_ascii_lowercase().contains("deflate"),
            "{what}: the client must have asked for the coding it decoded: {req}"
        );
    }
}

/// A `deflate` body cut short is an error, not a shorter document —
/// zlib's Adler-32 trailer for one form, RFC 1951 §3.2.3's `BFINAL` for
/// the other, so both are checked.
#[cfg(feature = "deflate")]
#[test]
fn a_deflate_stream_cut_short_is_an_error() {
    for (what, coded) in [
        ("zlib-wrapped", zlib_wrapped(plaintext().as_bytes())),
        ("raw", raw_deflate(plaintext().as_bytes())),
    ] {
        let (addr, _) = serve(coded, "deflate", false);
        let err = fetch(addr).expect_err(what);
        assert_eq!(
            *err.kind(),
            hclient_core::ErrorKind::Decode,
            "{what}: the HTTP body ended cleanly, so this must be the \
             decoder's objection and not the transport's: {err:?}"
        );
    }
}

#[cfg(feature = "zstd")]
fn zstd_encode(data: &[u8], checksum: bool) -> Vec<u8> {
    let mut e = zstd::stream::write::Encoder::new(Vec::new(), 19).expect("encoder");
    e.include_checksum(checksum).expect("checksum flag");
    e.write_all(data).expect("zstd");
    e.finish().expect("zstd")
}

/// libzstd encodes, `ruzstd` decodes. Two implementations sharing no code
/// agreeing on the wire is the strongest form this file can take, and it
/// is the reason the `zstd` crate is a dev-dependency at all — see
/// `Cargo.toml`, where the C-and-a-build-script objection that keeps it
/// out of `[dependencies]` is answered for this side of the line.
#[cfg(feature = "zstd")]
#[test]
fn a_zstd_body_from_the_reference_encoder_arrives_as_plaintext() {
    let want = plaintext();
    let coded = zstd_encode(want.as_bytes(), true);
    assert!(
        coded.len() * 4 < want.len(),
        "the fixture must be compressed"
    );
    let (addr, seen) = server(coded, "zstd");
    assert_eq!(fetch(addr).expect("decodes"), want);
    let req = &seen.lock().expect("seen")[0];
    assert!(
        req.to_ascii_lowercase().contains("zstd"),
        "the client must have asked for the coding it decoded: {req}"
    );
}

/// **Concatenated frames are one body**, RFC 8878 §3.1 — and neither
/// `ruzstd` entry point crosses a frame boundary on its own, so this is
/// checked rather than inherited. A decoder that stopped at the first
/// frame's end would hand the caller the first half and call it complete,
/// which is the failure this asserts against by comparing to the WHOLE
/// plaintext.
#[cfg(feature = "zstd")]
#[test]
fn concatenated_zstd_frames_are_one_body() {
    let first = plaintext();
    let second: String = (0..2000).map(|i| format!("second {i}\n")).collect();
    let mut coded = zstd_encode(first.as_bytes(), true);
    coded.extend_from_slice(&zstd_encode(second.as_bytes(), false));
    let (addr, _) = server(coded, "zstd");
    assert_eq!(fetch(addr).expect("decodes"), format!("{first}{second}"));
}

/// **The XXH64 content checksum is compared, which `ruzstd` does not do.**
///
/// The last four bytes of a frame carrying `Content_Checksum_flag` are
/// that checksum; flipping a bit there leaves every block structurally
/// perfect, so a decoder that only decodes hands the caller a document it
/// has evidence is wrong. The control is the test above, whose fixture
/// carries a checksum and is not corrupted.
#[cfg(feature = "zstd")]
#[test]
fn a_corrupted_zstd_checksum_is_an_error_and_not_a_document() {
    let want = plaintext();
    let mut coded = zstd_encode(want.as_bytes(), true);
    let last = coded.len() - 1;
    coded[last] ^= 0xff;
    let (addr, _) = serve(coded, "zstd", true);
    let err = fetch(addr).expect_err("the frame says its content hashes to something else");
    assert_eq!(*err.kind(), hclient_core::ErrorKind::Decode, "{err:?}");
}

/// A `zstd` body cut short is an error rather than a shorter document.
#[cfg(feature = "zstd")]
#[test]
fn a_zstd_stream_cut_short_is_an_error() {
    let (addr, _) = serve(zstd_encode(plaintext().as_bytes(), true), "zstd", false);
    let err = fetch(addr).expect_err("truncated");
    assert_eq!(*err.kind(), hclient_core::ErrorKind::Decode, "{err:?}");
}

/// **A frame declaring a window past 8 MB is refused**, RFC 8878
/// §3.1.1.1.2's recommended interoperability ceiling and the number Chrome
/// settled on — a narrowing of `ruzstd`'s own 100 MB default.
///
/// The header is written by hand rather than by the encoder, because the
/// quantity under test is a **declaration** and libzstd will not declare a
/// window larger than the content it was given. Six bytes, RFC 8878
/// §3.1.1.1: the magic number, a frame header descriptor of zero (no
/// content size, not single-segment, no checksum, no dictionary) and a
/// window descriptor whose exponent 14 means `1 << (10 + 14)` — 16 MiB,
/// twice the ceiling.
///
/// The control is the byte below it: exponent 13 is 8 MiB exactly, which
/// is admitted, so this asserts a bound rather than a refusal of anything
/// unusual.
#[cfg(feature = "zstd")]
#[test]
fn a_zstd_frame_declaring_a_window_past_the_ceiling_is_refused() {
    fn frame(exponent: u8) -> Vec<u8> {
        let mut v = vec![0x28, 0xb5, 0x2f, 0xfd, 0x00, exponent << 3];
        // One empty raw block with the last-block flag, so a frame that
        // IS admitted decodes to nothing instead of hanging on truncation.
        v.extend_from_slice(&[0x01, 0x00, 0x00]);
        v
    }
    let (addr, _) = serve(frame(13), "zstd", true);
    assert_eq!(
        fetch(addr).expect("8 MiB is the ceiling, not past it"),
        "",
        "the control: a window at exactly the ceiling decodes"
    );

    let (addr, _) = serve(frame(14), "zstd", true);
    let err = fetch(addr).expect_err("16 MiB is past the ceiling");
    assert_eq!(*err.kind(), hclient_core::ErrorKind::Decode, "{err:?}");
}

/// **Bytes after the end of the `deflate` stream are an error, not
/// litter.**
///
/// This test exists because the check was written as a mutation
/// **control** — "no well-formed body has trailing bytes, so nothing can
/// observe it" — and that reasoning was wrong in the way this project has
/// a rule about: a *server* can send them, so the behaviour is reachable,
/// and a reachable behaviour with no test is a gap rather than a control.
/// Silently discarding them would be this client deciding which of the
/// bytes a server sent are part of the document.
///
/// Both arrival shapes are covered, because they take different code
/// paths: in the same chunk as the stream's end, and in a later one.
#[cfg(feature = "deflate")]
#[test]
fn bytes_after_the_end_of_a_deflate_stream_are_an_error() {
    let want = plaintext();
    for (what, extra) in [("one trailing byte", 1usize), ("a trailing chunk", 4096)] {
        let mut coded = zlib_wrapped(want.as_bytes());
        coded.extend(std::iter::repeat_n(b'!', extra));
        let (addr, _) = serve(coded, "deflate", true);
        let err = fetch(addr).expect_err(what);
        assert_eq!(
            *err.kind(),
            hclient_core::ErrorKind::Decode,
            "{what}: {err:?}"
        );
    }
}
