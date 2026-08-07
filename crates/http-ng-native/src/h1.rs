//! HTTP/1 exchange on hyper, driven all the way to the response without a
//! single `spawn`.
//!
//! # Why this is the vertical's technical crux
//!
//! `hyper::client::conn::http1::handshake` needs neither an executor nor a
//! timer — on its own it just writes the request and reads the response
//! through `hyper::rt::{Read, Write}`. But HTTP/1 has a `Connection` — a
//! future that someone has to poll, or bytes won't move in either
//! direction (see `hyper::client::conn::http1::Connection`'s doc comment:
//! "in most cases, this needs to be spawned on an executor"). We
//! deliberately don't spawn (see the crate's doc comment and
//! `http-ng-rt-pair-check`), so this file polls `Connection` **by hand,
//! alongside** reading the response — first inside [`exchange`] (until
//! headers arrive), then inside [`NativeBody::poll_frame`] (until the
//! body is fully read). `tests/h1.rs` checks this not as an assertion but
//! as a fact: `works_on_a_bare_futures_executor_with_no_spawn` runs the
//! whole path on bare `futures_executor::block_on` — a runtime with no
//! way whatsoever to spawn a task. If this needed `spawn`, the test would
//! either fail to compile or hang.
//!
//! # Manual polling alongside — not a busy-spin on a real runtime
//!
//! "Polls by hand, alongside" sounds like the same thing as a busy-spin —
//! spinning a `poll` loop hoping to catch progress. It isn't, and the
//! question wasn't debated, it was measured (Task 12, review round 1): on
//! a bare executor (`futures_executor::block_on`, no reactor) CPU time
//! genuinely does equal wall time — the spin there is real, because
//! there's simply nothing that can wait for socket readiness other than
//! polling again. But that exact same [`exchange`]/[`NativeBody`] code,
//! run through a real reactor (tokio, smol), costs ~0 CPU for the same
//! wall time: neither `TokioIo` nor `FuturesIo` calls `wake_by_ref`
//! itself — they return `Pending` and rely on the reactor to wake the
//! task once the socket is actually ready, and `poll_fn` in [`exchange`]
//! and `poll_frame` in [`NativeBody`] simply AREN'T CALLED until they're
//! woken. The whole spin lives in the test helper `testing::BlockingIo`
//! (its `poll_would_block` honestly calls `cx.waker().wake_by_ref()`
//! because there's no reactor on bare `futures_executor::block_on` — see
//! its doc comment) and doesn't exist on any production path. Task 14
//! would otherwise have had to re-litigate this on two runtimes at
//! once — written down here once, where the next reader of this file
//! will see it.
//!
//! # What happens when `Connection` finishes first, or fails
//!
//! `Connection` can finish (`Ready(Ok(()))`) or fail (`Ready(Err(_))`) at
//! two different spots, and both are handled differently:
//!
//! - **Inside [`exchange`]**, before the response has arrived: if
//!   `Connection` fails, the exchange must fail too — without it,
//!   `send_request` will never get a response. If `Connection` completes
//!   SUCCESSFULLY before `send_request` does, that isn't treated as
//!   success right away: `exchange` stops polling the already-finished
//!   `Connection` (re-polling a completed `Future` isn't guaranteed to be
//!   safe for every implementation) and keeps polling `send_request`. By
//!   hyper's dispatcher invariant (`SendRequest::send_request` panics
//!   inside hyper itself if its channel is dropped without returning
//!   either success or an error — `dispatch dropped without returning
//!   error`), `Connection` can't reach `Dispatched::Shutdown` without
//!   closing that channel — so once `conn` has returned `Ready(Ok(()))`,
//!   `send` must either already be ready or become ready on the very next
//!   poll — there's no hang here, and if hyper's invariant were ever
//!   violated, `send.await` itself would panic with hyper's own panic,
//!   not hang silently.
//! - **Inside [`NativeBody::poll_frame`]**, after headers have already
//!   been handed to the caller: `Connection` behaves the same as before,
//!   except its job now is to finish writing the remaining body bytes
//!   into the `hyper::body::Incoming` channel. hyper's dispatcher only
//!   reaches `Dispatched::Shutdown` once the body channel is closed
//!   (filled to completion, or closed due to an error) — so once
//!   `conn.poll()` has returned `Ready` a single time, it's fine to just
//!   stop polling it (`self.conn = None`) and keep reading from
//!   `incoming`: it will either hand back the remaining frames already
//!   delivered, or `None`, or (on a break) an error — it never "silently
//!   goes quiet." This is exactly what makes possible the danger the
//!   brief warns about — "a body that silently stops yielding bytes
//!   because nobody is driving its `Connection` anymore" — the test
//!   `body_keeps_driving_the_connection_after_headers` catches a
//!   regression of exactly this: without the lines that poll `conn`
//!   inside `poll_frame`, the body's second chunk would never be read
//!   from the socket, and `incoming.poll_frame()` would return `Pending`
//!   forever (nobody writes into the channel anymore), which is what
//!   turns the mutation into a hanging test — that's why the test is
//!   wrapped in a watchdog with a ceiling (see `tests/h1.rs`), rather than
//!   left as a bare `block_on`.
//!
//! `exchange` returns a `Response<NativeBody>`: `SendRequest`, a local
//! variable of `exchange`, is dropped when the function returns, but
//! `Connection` is not — it moves INTO THE BODY (`NativeBody::conn`) and
//! lives exactly as long as the body lives, or until the body is fully
//! read. Dropping `SendRequest` early is safe, and even correct, for
//! v0.1: there's no connection pool here ("one request, one connection"),
//! so losing the ability to send a SECOND request over the same
//! `SendRequest` isn't a problem — it's a signal to hyper's dispatcher
//! that there won't be any new requests, so it drives the current
//! exchange to completion and closes, instead of waiting around for reuse
//! (keep-alive reuse is a future pool's concern, not this file's). If the
//! caller drops `NativeBody` before fully reading it, the `Connection`
//! inside is dropped along with it — that's the caller's deliberate
//! choice to cut the read short, not a silent loss of bytes: hyper
//! documents exactly that behavior for a dropped `Connection`.
//!
//! # `ErrorKind` through `hyper::Error`
//!
//! Task 10 (`body.rs`, module doc comment) proved with a real handshake
//! that an outgoing body error (`http_ng_core::Error`) survives the trip
//! through `hyper::Error` without losing `ErrorKind` — it's recovered via
//! `hyper::Error::source().downcast_ref()`, not lost in a `Display`
//! string. A naive version of this file (see the task's draft) wrapped
//! EVERY `hyper::Error` in `Error::new(fixed_kind, e)` directly — meaning
//! an outgoing body error (`ErrorKind::Body`) that reached `conn.poll()`
//! or `send.poll()` as a `hyper::Error` would be silently flattened into
//! `ErrorKind::Connect`, even though an `http_ng_core::Error` with the
//! right `ErrorKind` sits right there in that same error's `source()`.
//! [`from_hyper_error`] is the single conversion point for every site in
//! this file: it first tries to recover our `Error` from `source()`, and
//! only wraps the `hyper::Error` fresh under the given `fallback` if it
//! isn't there. The test
//! `exchange_recovers_error_kind_through_hyper_error_not_flattening_it`
//! below proves this with a real (if synthetic-IO) handshake — not just
//! by reading the code.
use crate::body::OutgoingBody;
use bytes::Bytes;
use http_body::{Body, Frame, SizeHint};
use http_ng_core::{Error, ErrorKind};
use hyper::client::conn::http1;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

type ConnFuture = Pin<Box<dyn Future<Output = hyper::Result<()>>>>;

/// The single conversion point from `hyper::Error` to `http_ng_core::Error`
/// for this file — see the module doc comment for why flattening
/// everything into `fallback` would regress the property Task 10 proved.
fn from_hyper_error(e: hyper::Error, fallback: ErrorKind) -> Error {
    match std::error::Error::source(&e).and_then(|s| s.downcast_ref::<Error>()) {
        Some(inner) => inner.clone(),
        None => Error::new(fallback, e),
    }
}

/// A response body that **polls the connection itself**.
///
/// Without this, the connection would stop moving as soon as headers
/// arrived: hyper requires someone to drive `Connection`, and we
/// deliberately don't spawn — that would require `Send + 'static`, and
/// single-threaded runtimes would end up shut out (see the crate's doc
/// comment and `http-ng-rt-pair-check`).
pub struct NativeBody {
    incoming: hyper::body::Incoming,
    /// `None` — `Connection` has already finished (successfully or with
    /// an error, recorded below); from then on `incoming` can just be
    /// drained without it, see the module doc comment for why that isn't
    /// a silent hang. `Box<dyn Future>`, not a concrete type — the only
    /// place in the vertical where we box, and not to erase `Send` (no
    /// `Send` bound is declared anywhere) but simply to be able to store
    /// `Connection` as a struct field at all: it has no name outside
    /// `http1`.
    conn: Option<ConnFuture>,
}

impl std::fmt::Debug for NativeBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeBody")
            .field("still_driving_connection", &self.conn.is_some())
            .finish()
    }
}

impl Body for NativeBody {
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Error>>> {
        let this = &mut *self;
        // Move the connection first — otherwise no new data will arrive
        // in the `incoming` channel (see the module doc comment).
        if let Some(conn) = this.conn.as_mut() {
            match conn.as_mut().poll(cx) {
                Poll::Ready(Ok(())) => {
                    this.conn = None;
                }
                Poll::Ready(Err(e)) => {
                    this.conn = None;
                    return Poll::Ready(Some(Err(from_hyper_error(e, ErrorKind::Body))));
                }
                Poll::Pending => {}
            }
        }
        match Pin::new(&mut this.incoming).poll_frame(cx) {
            Poll::Ready(Some(Ok(f))) => Poll::Ready(Some(Ok(f))),
            Poll::Ready(Some(Err(e))) => {
                Poll::Ready(Some(Err(from_hyper_error(e, ErrorKind::Body))))
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.incoming.is_end_stream()
    }

    /// Forwards `hyper::body::Incoming`'s real hint (e.g. from
    /// `Content-Length`) instead of the default "unknown": we already have
    /// it for free, no reason to throw it away.
    fn size_hint(&self) -> SizeHint {
        self.incoming.size_hint()
    }
}

/// One request over one connection. There's no pool in v0.1.
pub(crate) async fn exchange<I>(
    io: I,
    req: http::Request<OutgoingBody>,
) -> Result<http::Response<NativeBody>, Error>
where
    I: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
{
    let (mut sender, conn) = http1::handshake::<I, OutgoingBody>(io)
        .await
        .map_err(|e| from_hyper_error(e, ErrorKind::Connect))?;

    // Drive the connection and the request **together**, without spawn.
    let mut conn = Box::pin(conn);
    let mut send = Box::pin(sender.send_request(req));
    // `conn` isn't polled again once it has returned `Ready`: per
    // `Future`'s contract, re-polling an already-completed future isn't
    // guaranteed to be safe for every concrete implementation. See the
    // module doc comment for what happens to `send` if `Connection`
    // finishes before it does.
    let mut conn_done = false;

    let resp = std::future::poll_fn(|cx| {
        if !conn_done {
            match conn.as_mut().poll(cx) {
                Poll::Ready(Ok(())) => conn_done = true,
                Poll::Ready(Err(e)) => {
                    conn_done = true;
                    return Poll::Ready(Err(from_hyper_error(e, ErrorKind::Connect)));
                }
                Poll::Pending => {}
            }
        }
        match send.as_mut().poll(cx) {
            Poll::Ready(Ok(r)) => Poll::Ready(Ok(r)),
            Poll::Ready(Err(e)) => Poll::Ready(Err(from_hyper_error(e, ErrorKind::Connect))),
            Poll::Pending => Poll::Pending,
        }
    })
    .await?;

    let (parts, incoming) = resp.into_parts();
    Ok(http::Response::from_parts(
        parts,
        NativeBody {
            incoming,
            // `conn_done` isn't reused here as-is: if `Connection` had
            // already finished, `conn_done` would be `true`, but `send`
            // has just returned `Ready(Ok(_))` too — meaning, per the
            // analysis in the module doc comment, the exchange is still
            // correct, there's simply nothing left to poll, and
            // `NativeBody` must learn that for itself, not inherit the
            // state from `exchange`.
            conn: if conn_done {
                None
            } else {
                Some(conn as ConnFuture)
            },
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_ng_core::RequestBody;
    use std::io;

    /// Polls a future EXACTLY once and panics on `Pending` — the same
    /// choice and the same reasoning as `http-ng-native::body::tests::
    /// poll_once`: the whole path below (handshake, sending headers with
    /// no body, the outgoing body failing, delivering the error into
    /// `send_request`) runs synchronously on `SinkIo`, so `Pending` here
    /// means a broken test assumption, not "wait some more" — and it's
    /// caught immediately, not by hanging.
    fn poll_once<F: Future>(fut: Pin<&mut F>) -> F::Output {
        let mut cx = Context::from_waker(std::task::Waker::noop());
        match fut.poll(&mut cx) {
            Poll::Ready(v) => v,
            Poll::Pending => panic!("SinkIo must not return Pending anywhere on this path"),
        }
    }

    /// A sink: writes into the void, always answers reads with `Pending`
    /// — see `http-ng-native::body::tests::SinkIo` for the detailed
    /// breakdown of why `Pending` rather than an immediate EOF (a fresh
    /// connection starts in `KA::Busy`, and an immediate EOF there reads
    /// as "unexpected end of a busy connection," not as the REQUEST
    /// BODY's failure, which is what's being tested here).
    #[derive(Default)]
    struct SinkIo;

    impl hyper::rt::Read for SinkIo {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: hyper::rt::ReadBufCursor<'_>,
        ) -> Poll<io::Result<()>> {
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

    /// A single-pass stream of one error — the same technique as
    /// `body::tests::OneShotStream::error`, just duplicated here: this
    /// crate's files have no shared test module, and duplicating a small
    /// fixture is cheaper than exporting it for a single consumer.
    struct OneShotErr(Option<Error>);
    impl Body for OneShotErr {
        type Data = Bytes;
        type Error = Error;
        fn poll_frame(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Error>>> {
            Poll::Ready(self.0.take().map(Err))
        }
        fn is_end_stream(&self) -> bool {
            self.0.is_none()
        }
    }

    /// Proves the property from the module doc comment, rather than just
    /// asserting it: a failed outgoing body carries `ErrorKind::Body`, and
    /// that `ErrorKind` must survive the path from
    /// `OutgoingBody::poll_frame` through hyper
    /// (`new_user_body`/`.with()`) to the `hyper::Error` that comes back
    /// out of `conn`/`send`, through `from_hyper_error`. Mutation check:
    /// reverting `from_hyper_error` to the brief's `Error::new(fallback,
    /// e)` with no `downcast` attempt turns this test red immediately —
    /// `err.kind()` would become `ErrorKind::Connect`, not
    /// `ErrorKind::Body`.
    #[test]
    fn exchange_recovers_error_kind_through_hyper_error_not_flattening_it() {
        let original = Error::new(ErrorKind::Body, io::Error::other("stream broke"));

        let body = OutgoingBody::from_request_body(RequestBody::Streaming(Box::new(OneShotErr(
            Some(original.clone()),
        ))));
        let req = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", "example.invalid")
            .body(body)
            .unwrap();

        let fut = exchange(SinkIo, req);
        let mut fut = std::pin::pin!(fut);
        let err = match poll_once(fut.as_mut()) {
            Ok(_) => panic!("a failed outgoing body must fail exchange"),
            Err(e) => e,
        };

        assert_eq!(
            err.kind(),
            &ErrorKind::Body,
            "ErrorKind must survive the path through hyper::Error, not flatten \
             into ErrorKind::Connect: {err}"
        );
    }
}
