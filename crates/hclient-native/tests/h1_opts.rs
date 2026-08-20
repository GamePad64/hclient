//! `Native::h1_opts`, against servers that send more head than a client
//! should accept.
//!
//! **A response head is the one part of a response a client must buffer
//! whole before it can act on any of it**, so it is the one part a hostile
//! server can make expensive without ever sending a body. That is what
//! these two bounds are for, and it is why each test is a *pair*: the same
//! server, once under the default and once under a bound, so that "the
//! bound fired" is distinguishable from "this server was always going to
//! fail".
#![cfg(not(target_family = "wasm"))]

use hclient_core::RequestBody;
use hclient_core::unversioned::Transport;
use hclient_dns::IpLiteralOnly;
use hclient_native::{H1Opts, MaxBufSizeTooSmall, Native};
use hclient_rt_tokio::Tokio;
use hclient_tls::NoTls;
use std::io::{Read, Write};
use std::time::Duration;

const BOUND: Duration = Duration::from_secs(5);

/// A server answering with `count` header fields, each `size` bytes of
/// value.
fn server(count: usize, size: usize) -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = l.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let mut buf = [0u8; 4096];
            let _ = s.read(&mut buf);
            let mut head =
                String::from("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n");
            for i in 0..count {
                head.push_str(&format!("x-pad-{i}: {}\r\n", "v".repeat(size)));
            }
            head.push_str("\r\n");
            let _ = s.write_all(head.as_bytes());
            let _ = s.write_all(b"hi");
            let _ = s.flush();
        }
    });
    port
}

fn fetch(port: u16, opts: H1Opts) -> Result<u16, hclient_core::Error> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let t = Native::new(Tokio, NoTls, IpLiteralOnly)
        .h1_opts(opts)
        .expect("the value is above hyper's minimum");
    rt.block_on(async {
        tokio::time::timeout(
            BOUND,
            t.execute(
                http::Request::builder()
                    .uri(format!("http://127.0.0.1:{port}/x"))
                    .body(RequestBody::Empty)
                    .expect("request"),
            ),
        )
        .await
        .expect("must not hang")
    })
    .map(|r| r.status().as_u16())
}

/// **The header count.** hyper's default is 100 and it is not a soft one:
/// a response with more is an error rather than a truncation, which is the
/// right shape — a truncated head is a response a caller would act on.
#[test]
fn the_header_count_a_caller_sets_is_the_one_enforced() {
    // 150 fields, plus the three the fixture always writes.
    let port = server(150, 8);

    // `ErrorKind::Connect`, and it is worth naming why: hyper reports a
    // head it cannot parse as the **connection** failing, since nothing
    // usable ever came off it — there is no response to attach a `Body`
    // error to. The same classification `Native::run` gives every other
    // hyper head failure, and not a judgement this test makes.
    assert_eq!(
        fetch(port, H1Opts::default())
            .map(|_| ())
            .unwrap_err()
            .kind(),
        &hclient_core::ErrorKind::Connect,
        "the control: 153 fields is past hyper's default of 100"
    );
    assert_eq!(
        fetch(
            port,
            H1Opts {
                max_headers: Some(300),
                ..H1Opts::default()
            }
        )
        .expect("300 is room for 153"),
        200
    );
}

/// **The head's size in bytes**, which the count alone does not bound: a
/// server can send one field with a megabyte of value and stay under any
/// count.
///
/// The pair is the assertion. The same server passes under hyper's ~400 KB
/// default and fails under 8192 — so what is measured is the bound rather
/// than the fixture.
#[test]
fn the_head_size_a_caller_sets_is_the_one_enforced() {
    // Two fields of 16 KiB: comfortably inside any count, comfortably
    // past a 8192-byte buffer, comfortably inside 400 KB.
    let port = server(2, 16 * 1024);

    assert_eq!(
        fetch(port, H1Opts::default()).expect("32 KiB of head is nothing to the default"),
        200
    );
    assert_eq!(
        fetch(
            port,
            H1Opts {
                max_buf_size: Some(8192),
                ..H1Opts::default()
            }
        )
        .map(|_| ())
        .unwrap_err()
        .kind(),
        &hclient_core::ErrorKind::Connect,
        "a head that will not fit the buffer is an error, not a short head"
    );
}

/// **A value hyper would panic on is refused by name**, at the setter,
/// where the caller wrote it.
///
/// This is the whole reason `h1_opts` returns a `Result` where `h2_opts`
/// does not: a `SETTINGS` frame is written by this crate and there is
/// nobody to say no, but `max_buf_size` is handed to hyper, which
/// `assert!`s. A caller's number reaching a `panic!` inside a connect is
/// not a refusal they can act on.
#[test]
fn a_buffer_below_hypers_minimum_is_refused_rather_than_panicking() {
    let t = Native::new(Tokio, NoTls, IpLiteralOnly);
    let err = t
        .h1_opts(H1Opts {
            max_buf_size: Some(4096),
            ..H1Opts::default()
        })
        .map(|_| ())
        .expect_err("hyper's minimum is 8192");
    assert_eq!(*err.kind(), hclient_core::ErrorKind::Unsupported, "{err:?}");
    assert_eq!(
        std::error::Error::source(&err).and_then(|s| s.downcast_ref::<MaxBufSizeTooSmall>()),
        Some(&MaxBufSizeTooSmall { asked: 4096 }),
        "the refusal names the number the caller wrote: {err:?}"
    );

    // The boundary itself is accepted — a check written as `<=` would pass
    // every other assertion here.
    assert!(
        Native::new(Tokio, NoTls, IpLiteralOnly)
            .h1_opts(H1Opts {
                max_buf_size: Some(8192),
                ..H1Opts::default()
            })
            .is_ok()
    );
}
