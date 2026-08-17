//! `multipart/form-data` on the wire, read back by a parser that shares no
//! code with the encoder.
//!
//! An encoder checked against its own view of itself is green for every
//! self-consistent mistake it can make — a delimiter missing its leading
//! CRLF, a `Content-Disposition` the encoder would also mis-read. So the
//! observer here is twofold: a server that records the exact bytes of the
//! request, head included, and `multer` reading those bytes back into
//! parts. `multer` is the role `url` plays for `uri.rs` — an incumbent
//! implementation used as an oracle, in dev-dependencies only.
//!
//! Two things about that oracle, both established by running it rather
//! than by reading its documentation. It takes the boundary from the
//! caller, so this file takes it out of the recorded `Content-Type` and a
//! header that disagreed with the body would fail to parse rather than
//! quietly matching. And it does **not** percent-decode `name` or
//! `filename`, so the assertions below are about the escaped forms — which
//! is what goes on the wire and what a browser sends.
#![cfg(not(target_family = "wasm"))]

use bytes::Bytes;
use http_ng::multipart::{Form, Part};
use http_ng::{Client, RequestBody};
use http_ng_dns_system::SystemDns;
use http_ng_native::Native;
use http_ng_rt_tokio::Tokio;
use http_ng_tls_rustls::Rustls;
use std::io::{Read, Write};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().expect("tokio runtime")
}

fn client() -> Client<Native<Tokio, Rustls, SystemDns<Tokio>>> {
    Client::builder(Native::new(
        Tokio,
        Rustls::with_webpki_roots(),
        SystemDns::new(Tokio),
    ))
    .build()
    .expect("build")
}

/// One request as the server saw it: the head verbatim, and the body after
/// whatever framing carried it.
#[derive(Clone, Debug)]
struct Seen {
    head: String,
    body: Vec<u8>,
}

impl Seen {
    /// The boundary the head declared, unquoted.
    ///
    /// Read off the wire rather than out of the client, so the parse below
    /// is a check that the header and the body agree.
    fn boundary(&self) -> String {
        let v = self.header("content-type").expect("a Content-Type");
        let raw = v.split("boundary=").nth(1).expect("a boundary parameter");
        raw.trim().trim_matches('"').to_owned()
    }

    fn header(&self, name: &str) -> Option<String> {
        self.head
            .lines()
            .skip(1)
            .find(|l| {
                l.to_ascii_lowercase()
                    .starts_with(&format!("{name}:").to_ascii_lowercase())
            })
            .map(|l| {
                l.split_once(':')
                    .expect("a header line")
                    .1
                    .trim()
                    .to_owned()
            })
    }
}

/// A part as `multer` read it back.
#[derive(Debug, PartialEq, Eq)]
struct Read1 {
    name: Option<String>,
    file_name: Option<String>,
    content_type: Option<String>,
    body: Vec<u8>,
}

/// Feeds `multer` the recorded bytes in one chunk.
///
/// A hand-rolled `Stream` rather than `futures-util`'s `once`: `futures-
/// core` is already a dependency of this crate and the whole of what is
/// needed here is ten lines, which is the same call `http-ng-proto`'s
/// `encode` module makes about `base64`.
struct Once(Option<Bytes>);

impl futures_core::Stream for Once {
    type Item = Result<Bytes, std::io::Error>;
    fn poll_next(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(self.0.take().map(Ok))
    }
}

async fn parse(seen: &Seen) -> Vec<Read1> {
    let mut m = multer::Multipart::new(Once(Some(Bytes::from(seen.body.clone()))), seen.boundary());
    let mut out = Vec::new();
    while let Some(mut f) = m.next_field().await.expect("a well-formed part") {
        let name = f.name().map(str::to_owned);
        let file_name = f.file_name().map(str::to_owned);
        let content_type = f.content_type().map(ToString::to_string);
        let mut body = Vec::new();
        while let Some(c) = f.chunk().await.expect("a chunk") {
            body.extend_from_slice(&c);
        }
        out.push(Read1 {
            name,
            file_name,
            content_type,
            body,
        });
    }
    out
}

/// A server that records whole requests and answers with what `reply`
/// says, given the index of the request and the server's own port.
///
/// It decodes `Transfer-Encoding: chunked` itself, because that framing is
/// half of what is under test: a fixture that only understood
/// `Content-Length` would hang on exactly the request this module exists
/// to send.
///
/// Every answer carries `Connection: close` and the socket is closed after
/// it — RFC 9112 §9.6 makes that a MUST for a server that closes, and
/// omitting it is what left `http-ng-select`'s Alt-Svc fixture racing a FIN
/// once `TCP_NODELAY` stopped padding the gap.
fn serve(
    reply: impl Fn(usize, u16) -> String + Send + 'static,
) -> (std::net::SocketAddr, Arc<Mutex<Vec<Seen>>>) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    let seen = Arc::new(Mutex::new(Vec::<Seen>::new()));
    let log = Arc::clone(&seen);
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let Some(rec) = read_request(&mut s) else {
                continue;
            };
            let n = {
                let mut g = log.lock().expect("log");
                g.push(rec);
                g.len() - 1
            };
            let _ = s.write_all(reply(n, addr.port()).as_bytes());
            let _ = s.flush();
        }
    });
    (addr, seen)
}

/// Reads one request: head, then a body framed by `Content-Length` or by
/// `Transfer-Encoding: chunked`.
fn read_request(s: &mut std::net::TcpStream) -> Option<Seen> {
    let mut buf = Vec::new();
    let mut b = [0u8; 4096];
    let head_end = loop {
        match s.read(&mut b) {
            Ok(0) | Err(_) => return None,
            Ok(n) => buf.extend_from_slice(&b[..n]),
        }
        if let Some(i) = find(&buf, b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let lower = head.to_ascii_lowercase();
    let mut body = buf.split_off(head_end);

    if lower.contains("transfer-encoding: chunked") {
        loop {
            // Grow until the terminating zero-length chunk has arrived,
            // then decode the whole thing at once. Sufficient here and
            // deliberately not a streaming decoder: this fixture's job is
            // to record bytes, not to be an HTTP server.
            if let Some(d) = dechunk(&body) {
                return Some(Seen { head, body: d });
            }
            match s.read(&mut b) {
                Ok(0) | Err(_) => return None,
                Ok(n) => body.extend_from_slice(&b[..n]),
            }
        }
    }

    let want: usize = lower
        .split("content-length:")
        .nth(1)
        .and_then(|r| r.split("\r\n").next())
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    while body.len() < want {
        match s.read(&mut b) {
            Ok(0) | Err(_) => return None,
            Ok(n) => body.extend_from_slice(&b[..n]),
        }
    }
    body.truncate(want);
    Some(Seen { head, body })
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// RFC 9112 §7.1, enough of it for a client's own request bodies: no chunk
/// extensions and no trailer section, both of which this client never
/// sends here. `None` means "not complete yet".
fn dechunk(raw: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 0usize;
    loop {
        let nl = find(&raw[i..], b"\r\n")? + i;
        let n = usize::from_str_radix(std::str::from_utf8(&raw[i..nl]).ok()?.trim(), 16).ok()?;
        i = nl + 2;
        if n == 0 {
            // The zero chunk's own CRLF, and no trailers.
            return (raw.len() >= i + 2).then_some(out);
        }
        if raw.len() < i + n + 2 {
            return None;
        }
        out.extend_from_slice(&raw[i..i + n]);
        i += n + 2;
    }
}

const OK: &str = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

/// A stream of chunks whose `size_hint` is told rather than worked out —
/// so one test can send a part that knows its length and another can send
/// one that does not, with everything else identical.
struct Chunks {
    left: Vec<Bytes>,
    declared: Option<u64>,
}

impl http_body::Body for Chunks {
    type Data = Bytes;
    type Error = http_ng::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
        Poll::Ready(
            (!self.left.is_empty()).then(|| Ok(http_body::Frame::data(self.left.remove(0)))),
        )
    }
    fn size_hint(&self) -> http_body::SizeHint {
        match self.declared {
            Some(n) => http_body::SizeHint::with_exact(n),
            None => http_body::SizeHint::default(),
        }
    }
}

fn streaming(chunks: &[&'static [u8]], declared: Option<u64>) -> RequestBody {
    RequestBody::Streaming(Box::new(Chunks {
        left: chunks.iter().map(|c| Bytes::from_static(c)).collect(),
        declared,
    }))
}

/// **A form of buffered parts arrives with a `Content-Length`, and parses
/// into exactly the parts that were put in.**
///
/// Both halves in one test because they are the same decision: the parts
/// resolved to bytes, so the encoder knows the total and hyper writes it.
#[test]
fn a_buffered_form_declares_its_length_and_parses_back_into_its_parts() {
    let (addr, seen) = serve(|_, _| OK.to_owned());
    rt().block_on(async move {
        let c = client();
        c.post(&format!("http://127.0.0.1:{}/upload", addr.port()))
            .multipart(
                Form::new()
                    .part(Part::text("title", "a holiday photo"))
                    .part(
                        Part::bytes("file", &b"\x89PNG\r\n\x1a\n"[..])
                            .file_name("beach.png")
                            .mime("image/png"),
                    )
                    .part(Part::text("tag", "one"))
                    .part(Part::text("tag", "two")),
            )
            .send()
            .await
            .expect("send");
    });
    let s = seen.lock().expect("log")[0].clone();
    assert!(
        s.header("content-type")
            .expect("a Content-Type")
            .starts_with("multipart/form-data; boundary=----http-ng-"),
        "{}",
        s.head
    );
    assert_eq!(
        s.header("content-length"),
        Some(s.body.len().to_string()),
        "the length the encoder computed is the length that arrived: {}",
        s.head
    );
    assert_eq!(
        s.header("transfer-encoding"),
        None,
        "and it is not chunked: {}",
        s.head
    );

    let got = rt().block_on(parse(&s));
    assert_eq!(
        got,
        vec![
            Read1 {
                name: Some("title".into()),
                file_name: None,
                content_type: None,
                body: b"a holiday photo".to_vec(),
            },
            Read1 {
                name: Some("file".into()),
                file_name: Some("beach.png".into()),
                content_type: Some("image/png".into()),
                body: b"\x89PNG\r\n\x1a\n".to_vec(),
            },
            // Two parts with one name, in the order given: RFC 7578 §5.3
            // makes that order the value, and a receiver reading them into
            // a list gets whichever one this encoder wrote first.
            Read1 {
                name: Some("tag".into()),
                file_name: None,
                content_type: None,
                body: b"one".to_vec(),
            },
            Read1 {
                name: Some("tag".into()),
                file_name: None,
                content_type: None,
                body: b"two".to_vec(),
            },
        ]
    );
}

/// **A stream that will not say how long it is makes the request chunked,
/// and one that will gets a `Content-Length`** — the same request in every
/// other respect, which is what makes this about the stream's `size_hint`
/// rather than about the presence of a stream.
///
/// The pair is one test because either alone reads as an accident: a body
/// that were always chunked would pass the first, and one that always
/// declared a length would pass the second.
#[test]
fn a_streams_own_size_hint_decides_content_length_against_chunked() {
    for (declared, want_chunked) in [(None, true), (Some(4u64), false)] {
        let (addr, seen) = serve(|_, _| OK.to_owned());
        rt().block_on(async move {
            let c = client();
            c.post(&format!("http://127.0.0.1:{}/upload", addr.port()))
                .multipart(
                    Form::new().part(Part::text("a", "1")).part(
                        Part::new("f", streaming(&[b"ab", b"cd"], declared))
                            .file_name("s.bin")
                            .mime("application/octet-stream"),
                    ),
                )
                .send()
                .await
                .expect("send");
        });
        let s = seen.lock().expect("log")[0].clone();
        assert_eq!(
            s.header("transfer-encoding").as_deref() == Some("chunked"),
            want_chunked,
            "declared = {declared:?}: {}",
            s.head
        );
        assert_eq!(
            s.header("content-length").is_none(),
            want_chunked,
            "declared = {declared:?}: {}",
            s.head
        );
        // The body is the same either way, which is what says the framing
        // decision changed the framing and nothing else.
        let got = rt().block_on(parse(&s));
        assert_eq!(got.len(), 2, "declared = {declared:?}");
        assert_eq!(got[1].body, b"abcd", "declared = {declared:?}");
        assert_eq!(got[1].file_name.as_deref(), Some("s.bin"));
    }
}

/// **A `Content-Type` set by the caller is a build error whichever order
/// the two calls were written in, and nothing is sent.**
///
/// The header carries the boundary, so a caller's value cannot describe
/// the body — deferring to it, which is what `form` and `json` do, would
/// put an unparseable request on the wire. The order-independence is the
/// half worth pinning: the header is written at `send()` for exactly this,
/// and a check that lived in `multipart()` would catch only the first row.
///
/// The control is every other test in this file, which sends.
#[test]
fn a_caller_set_content_type_is_refused_in_either_order_and_nothing_is_sent() {
    let (addr, seen) = serve(|_, _| OK.to_owned());
    rt().block_on(async move {
        let c = client();
        let url = format!("http://127.0.0.1:{}/upload", addr.port());
        for before in [true, false] {
            let b = c.post(&url);
            let b = if before {
                b.header("content-type", "multipart/form-data; boundary=mine")
                    .multipart(Form::new().part(Part::text("a", "1")))
            } else {
                b.multipart(Form::new().part(Part::text("a", "1")))
                    .header("content-type", "multipart/form-data; boundary=mine")
            };
            let err = b.send().await.expect_err("a caller-set Content-Type");
            assert!(
                std::error::Error::source(&err)
                    .and_then(|s| s.downcast_ref::<http_ng::ContentTypeIsNotOursToKeep>())
                    .is_some(),
                "before = {before}: {err:?}"
            );
        }
    });
    assert!(
        seen.lock().expect("log").is_empty(),
        "a build error must not reach the network"
    );
}

/// **Replacing the body clears the multipart mark**, so the boundary
/// header does not outlive the body it described.
///
/// Without this the request would go out as a form body labelled
/// `multipart/form-data` — the exact corruption the refusal above exists
/// to prevent, arriving by the other door.
#[test]
fn a_body_set_after_a_form_takes_its_content_type_with_it() {
    let (addr, seen) = serve(|_, _| OK.to_owned());
    rt().block_on(async move {
        let c = client();
        c.post(&format!("http://127.0.0.1:{}/x", addr.port()))
            .multipart(Form::new().part(Part::text("a", "1")))
            .form([("b", "2")])
            .send()
            .await
            .expect("send");
    });
    let s = seen.lock().expect("log")[0].clone();
    assert_eq!(
        s.header("content-type").as_deref(),
        Some("application/x-www-form-urlencoded"),
        "{}",
        s.head
    );
    assert_eq!(s.body, b"b=2", "{:?}", String::from_utf8_lossy(&s.body));
}

/// **A CR LF in a file name does not become a header**, which is the
/// reason the escape exists at all and not merely an interoperability
/// nicety.
///
/// Asserted from the parser's side: one part, its `filename` carrying the
/// escaped text, and no `x-injected` field anywhere in what the part
/// declared. A raw CR LF would have ended the `Content-Disposition` line
/// and the rest would have been read as further part headers.
#[test]
fn a_crlf_in_a_file_name_cannot_inject_a_part_header() {
    let (addr, seen) = serve(|_, _| OK.to_owned());
    rt().block_on(async move {
        let c = client();
        c.post(&format!("http://127.0.0.1:{}/x", addr.port()))
            .multipart(
                Form::new()
                    .part(Part::text("f", "v").file_name("a\r\nX-Injected: 1\r\n\r\nowned.txt")),
            )
            .send()
            .await
            .expect("send");
    });
    let s = seen.lock().expect("log")[0].clone();
    let got = rt().block_on(parse(&s));
    assert_eq!(got.len(), 1, "one part, not two: {got:?}");
    assert_eq!(
        got[0].file_name.as_deref(),
        Some("a%0D%0AX-Injected: 1%0D%0A%0D%0Aowned.txt"),
        "the escaped text is the file name, not a header"
    );
    assert_eq!(got[0].body, b"v");
    assert!(
        !String::from_utf8_lossy(&s.body)
            .to_ascii_lowercase()
            .contains("\r\nx-injected:"),
        "no injected header line reached the wire: {:?}",
        String::from_utf8_lossy(&s.body)
    );
}

/// A non-ASCII file name goes out as UTF-8 and comes back as itself —
/// RFC 7578 §5.1.2's *"MAY be represented directly"*, with §4.2's MUST NOT
/// against `filename*` asserted on the same bytes.
#[test]
fn a_non_ascii_file_name_round_trips_without_a_filename_star() {
    let (addr, seen) = serve(|_, _| OK.to_owned());
    rt().block_on(async move {
        let c = client();
        c.post(&format!("http://127.0.0.1:{}/x", addr.port()))
            .multipart(Form::new().part(Part::text("f", "v").file_name("naïve 日.txt")))
            .send()
            .await
            .expect("send");
    });
    let s = seen.lock().expect("log")[0].clone();
    assert!(
        !String::from_utf8_lossy(&s.body).contains("filename*"),
        "RFC 7578 §4.2 MUST NOT"
    );
    let got = rt().block_on(parse(&s));
    assert_eq!(got[0].file_name.as_deref(), Some("naïve 日.txt"));
}

/// **A buffered form survives a 307 and arrives twice, byte for byte** —
/// which is the `Rewindable` half of the module's table doing its job
/// through the whole client rather than in a unit test.
///
/// `307`, not `302`: RFC 9110 §15.4.8 keeps the method and the body, which
/// is the redirect a POST of a form actually meets.
#[test]
fn a_buffered_form_is_replayed_across_a_307() {
    let (addr, seen) = serve(|n, port| {
        if n == 0 {
            format!(
                "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://127.0.0.1:{port}/second\r\n\
                 Content-Length: 0\r\nConnection: close\r\n\r\n"
            )
        } else {
            OK.to_owned()
        }
    });
    rt().block_on(async move {
        let c = client();
        c.post(&format!("http://127.0.0.1:{}/first", addr.port()))
            .multipart(
                Form::new()
                    .part(Part::text("a", "1"))
                    .part(Part::bytes("f", &b"payload"[..]).file_name("f.bin")),
            )
            .send()
            .await
            .expect("send");
    });
    let log = seen.lock().expect("log").clone();
    assert_eq!(log.len(), 2, "the second hop must have been sent");
    assert!(log[0].head.starts_with("POST /first "), "{}", log[0].head);
    assert!(log[1].head.starts_with("POST /second "), "{}", log[1].head);
    assert_eq!(
        log[0].body, log[1].body,
        "the replay is the same bytes, not a fresh encoding"
    );
    assert_eq!(
        log[0].header("content-type"),
        log[1].header("content-type"),
        "including the boundary, or the header and the body would disagree"
    );
    assert_eq!(rt().block_on(parse(&log[1])).len(), 2);
}

/// **A form with a stream in it is not replayed**, and the `307` itself
/// is what the caller gets.
///
/// The pair with the test above is the decision: the same redirect, the
/// same two parts, and only the part body's kind differs — so what
/// changes is the consequence of `RetryKind::Impossible` and nothing
/// else.
///
/// The `307` reaching the caller is `next_hop`'s own rule, not this
/// module's — *"more honest to return the 3xx as-is than to send an empty
/// body where one is expected"* — and it is asserted here because a
/// caller uploading a file is the first person likely to meet it.
#[test]
fn a_streaming_form_is_not_replayed_across_a_307() {
    let (addr, seen) = serve(|n, port| {
        if n == 0 {
            format!(
                "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://127.0.0.1:{port}/second\r\n\
                 Content-Length: 0\r\nConnection: close\r\n\r\n"
            )
        } else {
            OK.to_owned()
        }
    });
    let status = rt().block_on(async move {
        let c = client();
        c.post(&format!("http://127.0.0.1:{}/first", addr.port()))
            .multipart(
                Form::new()
                    .part(Part::text("a", "1"))
                    .part(Part::new("f", streaming(&[b"payload"], None)).file_name("f.bin")),
            )
            .send()
            .await
            .expect("the 3xx is the answer, not an error")
            .status()
    });
    let log = seen.lock().expect("log").clone();
    assert_eq!(
        log.len(),
        1,
        "nothing may be re-sent for a body that cannot be replayed"
    );
    assert_eq!(
        status,
        http::StatusCode::TEMPORARY_REDIRECT,
        "and the redirect itself is handed back rather than swallowed"
    );
    assert!(
        log[0]
            .header("transfer-encoding")
            .as_deref()
            .is_some_and(|v| v == "chunked"),
        "the one request that did go out was the streaming one: {}",
        log[0].head
    );
}

/// **An empty part in a chunked request does not end the body early.**
///
/// The risk is specific to this framing: a zero-length chunk *is* the
/// terminator in RFC 9112 §7.1, so an encoder that handed hyper an empty
/// data frame would be one `debug_assert!` away from truncating the
/// request at the empty part. The encoder skips empty pieces as *frames*
/// while still writing their head and delimiter, and this is where that
/// shows.
///
/// The empty part is in the middle rather than at the end, and the form is
/// chunked, because both are what make the truncation reachable at all: a
/// trailing empty part would lose nothing visible.
#[test]
fn an_empty_part_in_a_chunked_form_does_not_truncate_it() {
    let (addr, seen) = serve(|_, _| OK.to_owned());
    rt().block_on(async move {
        let c = client();
        c.post(&format!("http://127.0.0.1:{}/x", addr.port()))
            .multipart(
                Form::new()
                    .part(Part::text("before", "1"))
                    .part(Part::new("empty", RequestBody::Empty))
                    // Unknown length, so the whole request is chunked.
                    .part(Part::new("after", streaming(&[b"tail"], None))),
            )
            .send()
            .await
            .expect("send");
    });
    let s = seen.lock().expect("log")[0].clone();
    assert_eq!(s.header("transfer-encoding").as_deref(), Some("chunked"));
    let got = rt().block_on(parse(&s));
    assert_eq!(
        got.iter().map(|p| p.name.clone()).collect::<Vec<_>>(),
        vec![
            Some("before".into()),
            Some("empty".into()),
            Some("after".into())
        ],
        "all three parts, in order: {got:?}"
    );
    assert_eq!(
        got[1].body, b"",
        "and the empty one is empty rather than absent"
    );
    assert_eq!(got[2].body, b"tail");
}

/// **Two requests never share a boundary**, read off the wire rather than
/// off the type — the security argument in the module's documentation is
/// about what goes out, so this is where it has to be checked.
#[test]
fn two_requests_carry_two_boundaries() {
    let (addr, seen) = serve(|_, _| OK.to_owned());
    rt().block_on(async move {
        let c = client();
        let url = format!("http://127.0.0.1:{}/x", addr.port());
        for _ in 0..2 {
            c.post(&url)
                .multipart(Form::new().part(Part::text("a", "1")))
                .send()
                .await
                .expect("send");
        }
    });
    let log = seen.lock().expect("log").clone();
    assert_ne!(log[0].boundary(), log[1].boundary());
    assert!(
        String::from_utf8_lossy(&log[0].body).contains(&log[0].boundary()),
        "and each body carries its own"
    );
}
