//! `Response::lines` at the `Client` level.
//!
//! The splitter itself is `hclient-proto`'s and is tested there against
//! chunk boundaries and a property test. What is checked here is the half
//! that has a body: the bound, the ordering of an error against the lines
//! that were already whole, and what becomes of a final line with no
//! terminator.

// `hclient::mock` lives behind the `test-util` feature (see `mock.rs`);
// without this line `cargo test -p hclient` with no flags fails with
// E0432 instead of compiling down to nothing.
#![cfg(feature = "test-util")]

use bytes::Bytes;
use hclient::Client;
use hclient::lines::LineStream;
use hclient_core::{Error, ErrorKind};
use std::error::Error as _;

/// A response whose body arrives as `frames`, in order.
fn client_serving(frames: Vec<Bytes>) -> Client {
    let m = hclient::mock::MockTransport::new();
    m.push_response_bytes(http::Response::builder().status(200).body(frames).unwrap());
    Client::builder(m).build().unwrap()
}

fn lines_of(frames: Vec<Bytes>) -> Vec<Result<Bytes, Error>> {
    let c = client_serving(frames);
    futures_executor::block_on(async move {
        let resp = c.get("https://a/log").send().await.unwrap();
        let mut stream = resp.lines();
        let mut out = Vec::new();
        while let Some(line) = stream.next().await {
            out.push(line);
        }
        out
    })
}

fn ok_lines(frames: Vec<Bytes>) -> Vec<String> {
    lines_of(frames)
        .into_iter()
        .map(|l| String::from_utf8(l.unwrap().to_vec()).unwrap())
        .collect()
}

fn one(body: &'static str) -> Vec<Bytes> {
    vec![Bytes::from_static(body.as_bytes())]
}

#[test]
fn a_body_is_split_on_all_three_terminators() {
    assert_eq!(ok_lines(one("a\nb\r\nc\rd\n")), ["a", "b", "c", "d"]);
}

/// The property the whole type exists for, pinned the way
/// `hclient-proto`'s `head.rs` pins its own: one byte per frame is the
/// worst boundary there is, and no line may come out cut.
#[test]
fn a_line_arriving_one_byte_at_a_time_still_comes_out_whole() {
    let raw = b"{\"n\":1}\r\n{\"n\":2}\n{\"n\":3}";
    let frames: Vec<Bytes> = raw.iter().map(|b| Bytes::copy_from_slice(&[*b])).collect();
    assert_eq!(
        ok_lines(frames),
        [r#"{"n":1}"#, r#"{"n":2}"#, r#"{"n":3}"#],
        "a frame boundary is not a line boundary, including inside a CRLF"
    );
}

/// Half the NDJSON writers in the world omit the trailing newline, so
/// dropping this record would lose a whole line with nothing said.
#[test]
fn a_final_line_with_no_terminator_is_yielded() {
    assert_eq!(ok_lines(one("a\nb")), ["a", "b"]);
}

/// The control for the test above: a body that *does* end in a terminator
/// must not gain a phantom empty line at the end. Without it, "yield the
/// tail" and "yield an empty last line" would be indistinguishable.
#[test]
fn a_body_ending_in_a_terminator_yields_no_phantom_line() {
    assert_eq!(ok_lines(one("a\nb\n")), ["a", "b"]);
}

#[test]
fn an_empty_line_in_the_middle_is_a_line() {
    assert_eq!(ok_lines(one("a\n\nb\n")), ["a", "", "b"]);
}

#[test]
fn an_empty_body_is_no_lines_at_all() {
    assert!(ok_lines(vec![]).is_empty());
}

#[test]
fn a_leading_byte_order_mark_is_not_part_of_the_first_line() {
    assert_eq!(
        ok_lines(one("\u{feff}first\nsecond\n")),
        ["first", "second"]
    );
}

/// A body error must reach the caller as an error rather than as a quiet
/// end — the rule `hclient-fetch`'s body tests already pin — and the lines
/// that were whole before it happened must arrive first, which is
/// `SseStream`'s ordering and its reason.
#[test]
fn a_body_error_arrives_after_the_whole_lines_and_ends_the_stream() {
    let m = hclient::mock::MockTransport::new();
    m.push_response_frames_then_error(
        http::Response::builder()
            .status(200)
            .body(vec!["a\nb\n", "c\n"])
            .unwrap(),
        Error::new(ErrorKind::Body, std::io::Error::other("the peer vanished")),
    );
    let c = Client::builder(m).build().unwrap();

    futures_executor::block_on(async move {
        let mut stream = c.get("https://a/log").send().await.unwrap().lines();
        for expected in ["a", "b", "c"] {
            let line = stream.next().await.expect("a line").expect("not an error");
            assert_eq!(&line[..], expected.as_bytes());
        }
        let err = stream
            .next()
            .await
            .expect("the failure must be reported")
            .expect_err("as an error, not as the end of the body");
        assert_eq!(
            *err.kind(),
            ErrorKind::Body,
            "the transport's own classification survives, it is not relabelled here"
        );
        assert!(
            stream.next().await.is_none(),
            "the error is handed back exactly once"
        );
    });
}

/// The sharper half of the test above: a run of bytes with no terminator
/// behind a failure is a line that was **cut off**, and yielding it would
/// make truncation indistinguishable from a short record.
#[test]
fn a_truncated_last_line_is_not_yielded_after_an_error() {
    let m = hclient::mock::MockTransport::new();
    m.push_response_frames_then_error(
        http::Response::builder()
            .status(200)
            .body(vec!["whole\ntrunca"])
            .unwrap(),
        Error::new(ErrorKind::Body, std::io::Error::other("the peer vanished")),
    );
    let c = Client::builder(m).build().unwrap();

    futures_executor::block_on(async move {
        let mut stream = c.get("https://a/log").send().await.unwrap().lines();
        assert_eq!(
            &stream.next().await.unwrap().unwrap()[..],
            b"whole",
            "the line that was complete before the failure is still a line"
        );
        assert!(stream.next().await.unwrap().is_err());
        assert!(
            stream.next().await.is_none(),
            "`trunca` was never a line and must not be handed over as one"
        );
    });
}

#[test]
fn a_line_longer_than_the_bound_is_a_typed_error() {
    let c = client_serving(one("aaaaaaaaaaaaaaaaaaaaaa"));
    futures_executor::block_on(async move {
        let resp = c.get("https://a/log").send().await.unwrap();
        let mut stream = LineStream::new(resp, 8);
        let err = stream
            .next()
            .await
            .expect("an answer")
            .expect_err("refused");
        assert_eq!(*err.kind(), ErrorKind::Decode);
        let too_long = err
            .source()
            .and_then(|s| s.downcast_ref::<hclient::error::LineTooLong>())
            .expect("the payload names both numbers");
        assert_eq!(too_long.limit, 8);
        assert!(too_long.seen > 8);
        assert!(stream.next().await.is_none(), "and it is terminal");
    });
}

/// The bound is on **one line**, not on the buffer — and those differ
/// exactly when several lines arrive in one frame. Written after the first
/// implementation checked `buffered_len()` before draining, where this
/// body of six-byte lines would have been refused at a bound of 8.
#[test]
fn many_short_lines_in_one_frame_do_not_trip_a_single_line_bound() {
    let c = client_serving(one("aaaaa\nbbbbb\nccccc\nddddd\n"));
    let got = futures_executor::block_on(async move {
        let resp = c.get("https://a/log").send().await.unwrap();
        let mut stream = LineStream::new(resp, 8);
        let mut out = Vec::new();
        while let Some(line) = stream.next().await {
            out.push(
                String::from_utf8(line.expect("no line here is over 8 bytes").to_vec()).unwrap(),
            );
        }
        out
    });
    assert_eq!(got, ["aaaaa", "bbbbb", "ccccc", "ddddd"]);
}

/// The lines that were whole before the bound fired are handed over first,
/// for the same reason a body error's are.
#[test]
fn the_lines_before_an_oversized_one_are_handed_over_first() {
    let c = client_serving(one("ok\nalso ok\nand then a very long one indeed"));
    futures_executor::block_on(async move {
        let resp = c.get("https://a/log").send().await.unwrap();
        let mut stream = LineStream::new(resp, 12);
        assert_eq!(&stream.next().await.unwrap().unwrap()[..], b"ok");
        assert_eq!(&stream.next().await.unwrap().unwrap()[..], b"also ok");
        assert!(stream.next().await.unwrap().is_err());
    });
}
