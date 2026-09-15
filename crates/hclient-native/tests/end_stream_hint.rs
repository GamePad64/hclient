//! `NativeBody::is_end_stream` answers honestly, and the asymmetry in
//! `http_body`'s contract is why that matters.
//!
//! `http_body::Body::is_end_stream`'s own documentation makes the two
//! answers mean different amounts: *"Returns `true` when the end of
//! stream has been reached. An end of stream means that `poll_frame`
//! will return `None`. A return value of `false` **does not** guarantee
//! that a value will be returned from `poll_frame`."*
//!
//! So `true` is a **promise** and `false` is a hint. A body that answers
//! `true` while frames remain is telling a consumer it may stop reading,
//! and a consumer entitled to believe it truncates the response.
//!
//! Replacing the whole method with `true` left **all 583 tests of this
//! crate green**. Nothing in this workspace catches it because nothing
//! here acts on the answer: every in-workspace caller — `hclient-core`'s
//! hooks wrappers and its erasure, `hclient`'s `ClientBody` and
//! `Deadline` — forwards it to its own inner body, and
//! `http_body_util::collect` ignores the hint entirely and drains
//! `poll_frame` until `None`. hyper *does* branch on it, in
//! `proto/h1/dispatch.rs`, but on the **request** body it writes rather
//! than the response body it hands back.
//!
//! That makes the mutation invisible in this workspace and visible to a
//! consumer outside it, which is exactly the case a public
//! `http_body::Body` impl has to be right about: `NativeBody` is what
//! `Transport::execute` returns, so anyone writing against this crate
//! gets one and may trust its `true`.
#![cfg(not(target_family = "wasm"))]

use hclient_core::body::RequestBody;
use hclient_core::transport::Transport;
use hclient_dns::IpLiteralOnly;
use hclient_native::Native;
use hclient_rt_tokio::Tokio;
use hclient_tls::NoTls;
use http_body::Body;
use http_body_util::BodyExt;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::time::Duration;

const BOUND: Duration = Duration::from_secs(10);

/// A server that answers with a body of `n` bytes under an exact
/// `Content-Length`.
fn server(body: &'static str) -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    std::thread::spawn(move || {
        for conn in l.incoming() {
            let Ok(mut s) = conn else { break };
            let mut head = Vec::new();
            let mut b = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                match s.read(&mut b) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => head.push(b[0]),
                }
            }
            let _ = s.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
            let _ = s.flush();
        }
    });
    addr
}

/// **A body with bytes still to come must not claim the stream ended.**
///
/// **Only the `false` half is asserted, and that is the contract rather
/// than laziness.** `false` promises nothing, so a body answering it for
/// ever is conforming and there is no "must eventually say `true`" to
/// pin; `true` is the promise, and the only way to be wrong about it is
/// to say it early. Draining afterwards is what makes the first
/// assertion mean something: it proves the five bytes really were
/// outstanding when the body said they were.
#[tokio::test(flavor = "multi_thread")]
async fn a_body_with_frames_left_does_not_claim_the_stream_ended() {
    let addr = server("hello");
    let t = Native::new(Tokio, NoTls, IpLiteralOnly).without_pool();
    let req = http::Request::get(format!("http://127.0.0.1:{}/x", addr.port()))
        .body(RequestBody::Empty)
        .expect("a well-formed request");

    let resp = tokio::time::timeout(BOUND, t.execute(req))
        .await
        .expect("must not hang")
        .expect("the exchange completes");
    assert_eq!(resp.status(), 200);

    let body = resp.into_body();
    assert!(
        !body.is_end_stream(),
        "five bytes are still to come, and `true` here tells a consumer it \
         may stop reading — `http_body` makes that answer a promise"
    );

    let collected = tokio::time::timeout(BOUND, body.collect())
        .await
        .expect("the body must not hang")
        .expect("the body arrives")
        .to_bytes();
    assert_eq!(&collected[..], b"hello", "and the bytes really were there");
}
