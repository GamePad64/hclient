//! Adapter from `http_ng_core::RequestBody` to the `http_body::Body`
//! expected by `hyper::client::conn::http1::handshake<T, B>`.
//!
//! # On `Send`, and why `type Error` is `http_ng_core::Error`, not `BoxError`
//!
//! `handshake` requires `B::Error: Into<Box<dyn StdError + Send + Sync>>`
//! and `B::Data: Send`. The first version of this file read that as "our
//! `Error` doesn't fit — the crate itself needs a `Box<dyn Error + Send +
//! Sync>`." That was already wrong at the time it was written:
//! `http_ng_core::Error` holds an `Arc<dyn std::error::Error + Send +
//! Sync + 'static>` (the core's amendment-C1, not a bare `Arc<dyn
//! Error>`) and itself implements `Error + Send + Sync + 'static`. The
//! standard library's blanket impl (`impl<E: Error + Send + Sync + 'a>
//! From<E> for Box<dyn Error + Send + Sync + 'a>`) closes the required
//! bound without a single line of our own code — `assert_bound` in the tests below checks
//! this directly on `OutgoingBody`, not on a bare `http_ng_core::Error`,
//! because `<OutgoingBody as Body>::Error` is exactly what actually gets
//! substituted into `handshake`.
//!
//! Which means `BoxError` isn't needed in this file at all: wrapping
//! `Error` in `Box<dyn StdError + Send + Sync>` here would mean losing
//! `ErrorKind` right at the transport's entry point — exactly the defect
//! (B2 of vertical 1's final review) that `Transport::to_error` exists in
//! the core to prevent. hyper's `Send` bound is real and confirmed
//! (`hyper::proto::h1::dispatch::Dispatcher` requires `Bs::Error:
//! Into<Box<dyn StdError + Send + Sync>>` in its where-clause), but it's
//! already satisfied by `Error` as-is; there's nowhere to get a `BoxError`
//! from, and nowhere to put one.
//!
//! # Does `ErrorKind` survive the trip through `hyper::Error`?
//!
//! Yes — without stringification, proven by the test
//! `streaming_bodys_error_kind_survives_hyper_error_source` below, not
//! just by reading the source. The route (hyper 1.11.0,
//! `src/proto/h1/dispatch.rs`):
//!
//! 1. `Dispatcher::poll_write` calls `body.poll_frame(cx)`. Our
//!    `poll_frame` for `RequestBody::Streaming` hands back `Err(e)` with
//!    `e: http_ng_core::Error` as-is — this impl's `Self::Error` is
//!    EXACTLY `http_ng_core::Error`, no intermediate box.
//! 2. The hook `crate::Error::new_user_body(e)` calls `.with(e)`, where
//!    `.with<C: Into<Cause>>` — `Cause = Box<dyn StdError + Send +
//!    Sync>`. `e.into()` uses the same standard-library blanket impl:
//!    `Box::new(e)` as `Box<dyn Error + Send + Sync>`, the concrete type
//!    isn't lost behind the vtable — this is boxing into a `dyn`, not
//!    serializing to a string.
//! 3. `hyper::Error::source()` returns `Some(&**cause as &(dyn StdError +
//!    'static))`. `downcast_ref::<http_ng_core::Error>()` on that trait
//!    object — the standard invariant method of `dyn Error + 'static`
//!    (doesn't need `Any`, available on any `Error` type since 1.0) —
//!    successfully recovers the original value, `ErrorKind` included.
//!
//! So the correct route for Task 12/13, when `SendRequest::
//! send_request` returns `Err(hyper::Error)` because the body failed:
//! `err.source().and_then(|s| s.downcast_ref::<http_ng_core::Error>())`
//! BEFORE wrapping the error through `Transport::to_error` — `to_error`'s
//! default only knows how to recognize "this is already our `Error`" when
//! `Self::Error` itself is `http_ng_core::Error`, and `hyper::Error` is a
//! foreign type carrying our `Error` inside its `source()`, not as
//! itself. Pulling it out from there is the connector/driver's job, not
//! this file's: this file only proves there's something to pull out.
//!
//! # No `RequestBody` variant silently turns into an empty body
//!
//! `Streaming` isn't a buffer, so it's forwarded as a stream rather than
//! collected into memory or dropped (which used to be a defect in
//! vertical 1's `wasi` transport: a streaming body silently became an
//! empty request while streaming support was claimed). `Rewindable` has
//! its factory called and the result is processed through the SAME path
//! as any other `RequestBody` (recursively, via
//! [`Inner::from_request_body`]), not a partial match that only accepts
//! `Full` and collapses everything else to `None`: the factory is legally
//! allowed to return `RequestBody::Streaming` (see `RequestBody::
//! Rewindable`'s doc comment in `http-ng-core`), and such a result must
//! stay a stream, not turn into an empty body just because it isn't
//! `Full`. The `rewindable_*` tests below are mutation-checked: reverting
//! to the partial match kills exactly them, not just the new `Streaming`
//! test.
//!
//! # `Inner`/`OutgoingBody` are no longer dead code outside tests
//!
//! Before Task 12, nothing in the crate built `OutgoingBody` outside this
//! file's `#[cfg(test)] mod tests`, so `#![cfg_attr(not(test),
//! expect(dead_code, ..))]` used to sit here. With Task 12, `h1::exchange`
//! genuinely takes an `http::Request<OutgoingBody>` (not just in tests),
//! and `testing::empty_body`/`testing::exchange_for_test` in `lib.rs` also
//! build and pass it in an ordinary, non-test build — `dead_code` no
//! longer triggers outside tests, and an `expect` with no matching
//! trigger would itself become a warning
//! (`unfulfilled_lint_expectations`, discovered when Task 12 wired this
//! up). The attribute is removed, not narrowed: there was nothing left to
//! narrow it to — no path through this file still lived as dead code
//! outside tests.
use bytes::Bytes;
use http_body::{Body, Frame, SizeHint};
use http_ng_core::{Error, RequestBody};
use std::pin::Pin;
use std::task::{Context, Poll};

/// A buffered body or a forwarded stream — decided once, in
/// [`OutgoingBody::from_request_body`], not on every `poll_frame`.
enum Inner {
    /// `None` — the body is already honestly empty (`RequestBody::Empty`,
    /// an empty `Full`, or a `Rewindable`/factory result that collapsed to
    /// the same).
    Buffered(Option<Bytes>),
    /// `Unpin + Send` — the same bounds the core's `RequestBody::
    /// Streaming` carries (amendment-C2, same place), just carried across
    /// the crate boundary: the adapter wraps a foreign stream rather than
    /// producing its own `!Unpin`/`!Send` type, so keeping the bounds
    /// costs nothing.
    Streaming(Box<dyn Body<Data = Bytes, Error = Error> + Unpin + Send>), // send-bound-exception: amendment-C2
}

impl Inner {
    fn from_request_body(body: RequestBody) -> Self {
        match body {
            RequestBody::Empty => Inner::Buffered(None),
            RequestBody::Full(b) if b.is_empty() => Inner::Buffered(None),
            RequestBody::Full(b) => Inner::Buffered(Some(b)),
            // The factory can legally return anything, including another
            // `Rewindable` — the same conversion path unpacks that too,
            // rather than a partial match that only knows about `Full`.
            RequestBody::Rewindable(f) => Inner::from_request_body(f()),
            RequestBody::Streaming(s) => Inner::Streaming(s),
        }
    }
}

/// Request body for `hyper::client::conn::http1::handshake`.
///
/// `type Error = http_ng_core::Error` — see the module doc comment for why
/// this isn't `Box<dyn StdError + Send + Sync>`.
///
/// `pub`, not `pub(crate)` as it was before Task 12: the `body` module
/// itself is still private (`mod body;` in `lib.rs`, no `pub`), but as of
/// Task 12 `h1::exchange` isn't the only thing inside the crate that
/// builds an `OutgoingBody` anymore; `testing::empty_body`/`testing::
/// exchange_for_test` need to carry it across the crate boundary into
/// `tests/h1.rs` exactly the way `testing` already carries `h1::
/// NativeBody` via `pub use`. A private `mod body` still keeps external
/// crates from seeing the path `crate::body::OutgoingBody` directly —
/// only via the re-export in `testing` (see `lib.rs`) — so the crate's
/// actual public API surface doesn't grow.
#[derive(Debug)]
pub struct OutgoingBody {
    inner: Inner,
}

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Inner::Buffered(b) => f.debug_tuple("Buffered").field(b).finish(),
            Inner::Streaming(_) => f.write_str("Streaming(..)"),
        }
    }
}

impl OutgoingBody {
    pub(crate) fn from_request_body(body: RequestBody) -> Self {
        Self {
            inner: Inner::from_request_body(body),
        }
    }
}

impl Body for OutgoingBody {
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Error>>> {
        // `OutgoingBody` doesn't hold any `!Unpin` fields directly
        // (`Inner::Streaming` is already `Box<dyn .. + Unpin>`), so the
        // projection is a plain `get_mut`, no `pin_project` needed.
        match &mut self.get_mut().inner {
            Inner::Buffered(opt) => Poll::Ready(opt.take().map(|b| Ok(Frame::data(b)))),
            Inner::Streaming(s) => Pin::new(&mut **s).poll_frame(cx),
        }
    }

    fn is_end_stream(&self) -> bool {
        match &self.inner {
            Inner::Buffered(opt) => opt.is_none(),
            Inner::Streaming(s) => s.is_end_stream(),
        }
    }

    fn size_hint(&self) -> SizeHint {
        match &self.inner {
            Inner::Buffered(Some(b)) => SizeHint::with_exact(b.len() as u64),
            Inner::Buffered(None) => SizeHint::with_exact(0),
            // The `Rewindable` factory deliberately has no size_hint of
            // its own in `RequestBody` (see its doc comment in
            // `http-ng-core`); a forwarded `Streaming` reports whatever
            // the concrete stream knows — not necessarily exact.
            Inner::Streaming(s) => s.size_hint(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use http_ng_core::{ErrorKind, RequestBody};

    // `error_type_satisfies_hypers_send_sync_bound` used to live here, but
    // Task 13 (vertical 2) added `crates/http-ng-native/src` to the
    // `no-declared-send` CI scan (it now has a public item, `Native`, worth
    // protecting the same way `http-ng-core`/`http-ng` already are). Spec
    // amendment-C3 is explicit that a `B::Error: ... + Send + Sync` bound
    // like that one belongs in `tests/`, not `src/`, precisely so the guard
    // doesn't trip on its own assertion text. Relocated to
    // `tests/shape.rs`'s `outgoing_bodys_error_satisfies_hypers_send_sync_bound`
    // — same assertion, run against `crate::testing::OutgoingBody` from
    // outside the crate.

    #[test]
    fn full_body_yields_its_bytes_once() {
        let b = OutgoingBody::from_request_body(RequestBody::Full(bytes::Bytes::from_static(
            b"payload",
        )));
        let collected = futures_executor::block_on(b.collect()).unwrap().to_bytes();
        assert_eq!(&collected[..], b"payload");
    }

    #[test]
    fn empty_body_is_end_stream_immediately() {
        let b = OutgoingBody::from_request_body(RequestBody::Empty);
        assert!(http_body::Body::is_end_stream(&b));
    }

    #[test]
    fn size_hint_is_exact_for_buffered_bodies() {
        let b =
            OutgoingBody::from_request_body(RequestBody::Full(bytes::Bytes::from_static(b"1234")));
        assert_eq!(http_body::Body::size_hint(&b).exact(), Some(4));
    }

    /// An empty `Full` — the same honestly-empty case as `Empty`, not a
    /// separate silent special case: `Inner::Buffered(None)` for both.
    #[test]
    fn empty_full_body_is_end_stream_immediately() {
        let b = OutgoingBody::from_request_body(RequestBody::Full(bytes::Bytes::new()));
        assert!(http_body::Body::is_end_stream(&b));
        assert_eq!(http_body::Body::size_hint(&b).exact(), Some(0));
    }

    /// A `Rewindable` whose factory returns `Full` must go through as a
    /// plain buffer, not as an empty body. Mutation check: reverting
    /// `Inner::from_request_body` to the brief's partial match
    /// (`RequestBody::Full(b) if !b.is_empty() => Some(b), _ => None`)
    /// still leaves this test green (Full is exactly the variant that
    /// match catches). It's the next test, the `streaming`-factory one,
    /// that turns red.
    #[test]
    fn rewindable_body_yields_the_factorys_bytes() {
        let b = OutgoingBody::from_request_body(RequestBody::rewindable(|| {
            RequestBody::Full(bytes::Bytes::from_static(b"rewound"))
        }));
        let collected = futures_executor::block_on(b.collect()).unwrap().to_bytes();
        assert_eq!(&collected[..], b"rewound");
    }

    /// A `Rewindable` factory that legally returns `Streaming` (see
    /// `RequestBody::Rewindable`'s doc comment in `http-ng-core`) must
    /// stay a stream — not collapse into an empty body just because "it
    /// isn't `Full`." Mutation check: the brief's partial match
    /// (`RequestBody::Full(b) if !b.is_empty() => Some(b), _ => None`)
    /// yields `Buffered(None)` on this input, so `collect()` would return
    /// empty bytes, and this test expects specific content — switching to
    /// the partial match turns exactly this test red, not the previous
    /// one.
    #[test]
    fn rewindable_body_may_legally_produce_a_streaming_body() {
        let b = OutgoingBody::from_request_body(RequestBody::rewindable(|| {
            RequestBody::Streaming(Box::new(OneShotStream::data(b"streamed-via-factory")))
        }));
        let collected = futures_executor::block_on(b.collect()).unwrap().to_bytes();
        assert_eq!(&collected[..], b"streamed-via-factory");
    }

    /// A single-pass stream of one data frame, for tests. `Option`, not a
    /// ready-made queue: exactly one `Some -> None` transition is needed
    /// on `poll_frame`, the second call must return `Ready(None)`.
    struct OneShotStream(Option<Result<Frame<Bytes>, Error>>);

    impl OneShotStream {
        fn data(bytes: &'static [u8]) -> Self {
            Self(Some(Ok(Frame::data(Bytes::from_static(bytes)))))
        }
        fn error(e: Error) -> Self {
            Self(Some(Err(e)))
        }
    }

    impl Body for OneShotStream {
        type Data = Bytes;
        type Error = Error;
        fn poll_frame(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Error>>> {
            Poll::Ready(self.0.take())
        }
        fn is_end_stream(&self) -> bool {
            self.0.is_none()
        }
    }

    /// `Streaming` is forwarded as a stream: `poll_frame` hands back a
    /// frame directly, without buffering the whole thing into memory
    /// first. Mutation check: the brief's `RequestBody::Streaming(_) =>
    /// None` turns this test red immediately — `collect()` would see an
    /// empty body instead of `streamed`.
    #[test]
    fn streaming_body_forwards_frames_without_buffering() {
        let b = OutgoingBody::from_request_body(RequestBody::Streaming(Box::new(
            OneShotStream::data(b"streamed"),
        )));
        assert!(!http_body::Body::is_end_stream(&b));
        let collected = futures_executor::block_on(b.collect()).unwrap().to_bytes();
        assert_eq!(&collected[..], b"streamed");
    }

    /// A `Streaming` whose `poll_frame` yields an error must forward it
    /// as-is, `ErrorKind` included — not lose its category on the way
    /// through the adapter. Separate from `streaming_bodys_error_kind_
    /// survives_hyper_error_source`: this test checks the adapter itself,
    /// that one checks that the category survives `hyper::Error` too.
    #[test]
    fn streaming_bodys_error_kind_survives_the_adapter() {
        let source = std::io::Error::other("boom");
        let b = OutgoingBody::from_request_body(RequestBody::Streaming(Box::new(
            OneShotStream::error(Error::new(ErrorKind::Body, source)),
        )));
        let e = futures_executor::block_on(b.collect()).unwrap_err();
        assert_eq!(e.kind(), &ErrorKind::Body);
    }

    // --- Proof of the round trip through a real hyper handshake ---
    //
    // Everything below is a minimal `hyper::rt::Read + Write` (writes into
    // the void, reads as `Pending` — see `SinkIo::poll_read`'s doc
    // comment), plus one real `hyper::client::conn::http1::handshake` with
    // a real `SendRequest::send_request`. Polled by hand, `Waker::noop()`
    // (stable since 1.85 — the same MSRV floor the vertical already uses;
    // the technique is already used in `http-ng-tls::tests::poll_once`):
    // the whole path — handshake, writing headers, the body failing,
    // delivering the error into `send_request` — fits into one poll of
    // `conn`, then one poll of `send_request`, with no real executor and
    // no sleeping. The bound here isn't on iteration count, it's
    // structural: `poll_once` panics on `Pending` instead of hanging — and
    // since `SinkIo::poll_write`/`poll_flush`/`poll_shutdown` themselves
    // never return `Pending`, and the one `Pending` there is
    // (`poll_read`), per the analysis below, doesn't block progress,
    // `poll_once` on `conn`/`send_request` never panics.
    use std::io;

    fn poll_once<F: std::future::Future>(fut: std::pin::Pin<&mut F>) -> F::Output {
        let mut cx = Context::from_waker(std::task::Waker::noop());
        match fut.poll(&mut cx) {
            Poll::Ready(v) => v,
            Poll::Pending => panic!("SinkIo must not return Pending anywhere on this path"),
        }
    }

    /// A sink: accepts and discards any write; always answers reads with
    /// `Pending` (see `poll_read`'s doc comment for why not an immediate
    /// EOF). This test doesn't need the response's raw bytes — it only
    /// cares what happens to the request BODY's error.
    #[derive(Default)]
    struct SinkIo;

    impl hyper::rt::Read for SinkIo {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: hyper::rt::ReadBufCursor<'_>,
        ) -> Poll<io::Result<()>> {
            // `Pending`, not an immediate EOF — and here's why that
            // doesn't contradict "`SinkIo` never blocks" from the comment
            // above the module.
            //
            // A fresh client connection starts in `KA::Busy` (hyper
            // 1.11.0, `proto/h1/conn.rs`, `Conn::new`), meaning that
            // BEFORE the first write, `poll_read` (the dispatcher,
            // `proto/h1/dispatch.rs`) falls into the
            // `poll_read_keep_alive` -> `require_empty_read` branch:
            // there, ANY immediate EOF at this stage is interpreted as
            // "found an unexpected EOF on a busy connection" —
            // `crate::Error::new_incomplete()` — and that's exactly what
            // the first version of this test caught
            // (`hyper::Error(Canceled, hyper::Error(IncompleteMessage))`
            // instead of the body's error). A real socket here would
            // return `Pending` (no data yet, but the connection isn't
            // closed), not `Ready(EOF)` — `SinkIo` has to reproduce that
            // same distinction, not just "never truly blocks."
            //
            // `Pending` here doesn't block the round trip:
            // `Dispatcher::poll_loop` (hyper 1.11.0) calls `let _ =
            // self.poll_read(cx)?;` — `?` on a `Poll<Result<T, E>>>`
            // only propagates an `Err` outward, and returns `Pending` as
            // a VALUE (`Poll<T>`), which here is immediately discarded by
            // `let _ =`. Checked separately, outside this tree: a minimal
            // repro, `fn f() -> Poll<Result<u32,String>> { Poll::Pending
            // }`, called as `let _ = f()?;` inside a function returning
            // `Poll<Result<(), String>>>`, continues executing AFTER the
            // `?` — no short circuit. So `poll_write` (and with it, the
            // body's failure) is called in the SAME pass of `poll_loop` as
            // this `Pending` read, which the test below confirms by
            // getting the expected error without a single extra poll.
            Poll::Pending
        }
    }

    impl hyper::rt::Write for SinkIo {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Ready(Ok(buf.len()))
        }
        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[test]
    fn streaming_bodys_error_kind_survives_hyper_error_source() {
        let source = std::io::Error::other("stream broke");
        let original = Error::new(ErrorKind::Body, source);

        let body = OutgoingBody::from_request_body(RequestBody::Streaming(Box::new(
            OneShotStream::error(original.clone()),
        )));

        let req = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", "example.invalid")
            .body(body)
            .unwrap();

        let handshake = hyper::client::conn::http1::handshake::<_, OutgoingBody>(SinkIo);
        let mut handshake = std::pin::pin!(handshake);
        let (mut sender, conn) =
            poll_once(handshake.as_mut()).expect("handshake never blocks on SinkIo");

        // Queued synchronously inside `send_request` (before its first
        // `.await`), so the "poll conn first, then this future" order
        // below is guaranteed to see the request already queued.
        let send_fut = sender.send_request(req);
        let mut send_fut = std::pin::pin!(send_fut);

        let mut conn = std::pin::pin!(conn);
        // `conn` resolves in one poll in this scenario too: writing
        // headers, the body failing, and delivering the error into
        // `send_request`'s channel all happen synchronously inside one
        // `poll_write` — see the module doc comment.
        let _ = poll_once(conn.as_mut());

        let err = match poll_once(send_fut.as_mut()) {
            Ok(_) => panic!("a failed body must fail send_request"),
            Err(e) => e,
        };

        let recovered = std::error::Error::source(&err)
            .and_then(|s| s.downcast_ref::<Error>())
            .unwrap_or_else(|| {
                panic!("hyper::Error::source() must yield our http_ng_core::Error, got: {err:?}")
            });
        assert_eq!(
            recovered.kind(),
            &ErrorKind::Body,
            "ErrorKind must survive the path through hyper::Error::new_user_body/.with()"
        );
    }
}
