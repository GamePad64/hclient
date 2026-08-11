//! What [`SelectedBody`] passes through, and what it would silently
//! swallow if it did not.
//!
//! Three claims, none of which a request against a real server can make:
//! two of them are about `http_body::Body`'s **defaulted** methods, which a
//! wrapper that forgot to delegate still compiles and still delivers every
//! byte for, and the third is about an error type that a `Box<dyn Error>`
//! would have flattened while the tests went on passing.
#![cfg(not(target_family = "wasm"))]

use bytes::Bytes;
use http_body::{Body, Frame, SizeHint};
use http_ng_core::{Error, ErrorKind};
use http_ng_select::SelectedBody;
use std::pin::Pin;
use std::task::{Context, Poll};

/// A body whose three answers the test writes down.
///
/// Not a real member body: `NativeBody` and `H3Body` answer from a socket,
/// and a test that had to arrange a `Content-Length` on the wire to check
/// that `size_hint` was delegated would be measuring hyper rather than this
/// wrapper.
struct Scripted {
    frame: Option<Result<Bytes, Error>>,
    is_end: bool,
    exact: Option<u64>,
}

impl Body for Scripted {
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Error>>> {
        Poll::Ready(self.frame.take().map(|r| r.map(Frame::data)))
    }

    fn is_end_stream(&self) -> bool {
        self.is_end
    }

    fn size_hint(&self) -> SizeHint {
        match self.exact {
            Some(n) => SizeHint::with_exact(n),
            None => SizeHint::default(),
        }
    }
}

fn scripted(is_end: bool, exact: Option<u64>) -> Scripted {
    Scripted {
        frame: Some(Ok(Bytes::from_static(b"payload"))),
        is_end,
        exact,
    }
}

/// `is_end_stream` defaults to `false` and `size_hint` defaults to
/// "unknown", so a wrapper that delegates neither is still correct enough
/// to pass every end-to-end test in this crate: the bytes all arrive.
///
/// What it costs is a caller that reads the hint to size a buffer and gets
/// nothing, and one that stops on `is_end_stream` and polls a finished body
/// again. Both are asserted through both variants, because a delegation
/// written once and copied is exactly the kind that ends up with the same
/// arm twice.
#[test]
fn both_variants_report_the_members_own_end_of_stream_and_size_hint() {
    for is_end in [false, true] {
        let tcp: SelectedBody<Scripted, Scripted> = SelectedBody::Tcp(scripted(is_end, Some(7)));
        let quic: SelectedBody<Scripted, Scripted> = SelectedBody::Quic(scripted(is_end, Some(9)));

        assert_eq!(tcp.is_end_stream(), is_end);
        assert_eq!(quic.is_end_stream(), is_end);
        assert_eq!(tcp.size_hint().exact(), Some(7));
        assert_eq!(quic.size_hint().exact(), Some(9));
    }
}

/// A body error keeps the category the member gave it.
///
/// This is what `http_body_util::Either` could not do: its `type Error =
/// Box<dyn std::error::Error + Send + Sync>`, so `ErrorKind::Timeout(Phase::
/// BetweenBytes)` — which is what `http-ng-native`'s idle bound produces
/// from inside a body — would arrive at the caller as a string-shaped thing
/// with no kind to ask for. It is `Transport::to_error`'s finding (a
/// backend's whole classification discarded one seam up) with the body in
/// place of the head.
#[tokio::test]
async fn a_body_error_arrives_with_its_kind_intact_from_either_variant() {
    use http_body_util::BodyExt;

    for quic in [false, true] {
        let failing = Scripted {
            frame: Some(Err(Error::new(
                ErrorKind::Timeout(http_ng_core::Phase::BetweenBytes),
                std::io::Error::other("the peer went quiet"),
            ))),
            is_end: false,
            exact: None,
        };
        let body: SelectedBody<Scripted, Scripted> = if quic {
            SelectedBody::Quic(failing)
        } else {
            SelectedBody::Tcp(failing)
        };

        let err = body.collect().await.expect_err("the frame is an error");
        assert_eq!(
            *err.kind(),
            ErrorKind::Timeout(http_ng_core::Phase::BetweenBytes),
            "the member's classification survived the wrapper (quic: {quic})"
        );
    }
}

/// And the frames themselves come from the variant that holds them —
/// the control for the two tests above, which would both pass for a
/// wrapper that yielded nothing at all.
#[tokio::test]
async fn both_variants_yield_their_own_frames() {
    use http_body_util::BodyExt;

    let tcp: SelectedBody<Scripted, Scripted> = SelectedBody::Tcp(scripted(false, None));
    let quic: SelectedBody<Scripted, Scripted> = SelectedBody::Quic(scripted(false, None));

    for body in [tcp, quic] {
        let bytes = body.collect().await.expect("a complete body").to_bytes();
        assert_eq!(&bytes[..], b"payload");
    }
}
