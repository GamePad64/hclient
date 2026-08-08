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
//! # Nothing here is boxed behind `dyn`, and that is load-bearing
//!
//! (There is one `Box` in this file, on [`Failed::NotSent`]'s request, and
//! it is not the kind this section is about: it boxes a *concrete* type to
//! keep an enum small, and a concrete type in a box still lets auto traits
//! through. The kind that matters is `Box<dyn _>`, which does not.)
//!
//! [`NativeBody`] used to store its `Connection` as a `Pin<Box<dyn
//! Future>>` — "the only place in the vertical where we box", as this
//! comment then said, "and not to erase `Send`". It erased `Send` all the
//! same: `Box<dyn Future>` is never `Send`, so `NativeBody` never was
//! either, and a caller could not put a response into `tokio::spawn`.
//! Connection reuse (v0.2 W2) turned that from a wart into a blocker —
//! a pool holding boxed connections would have made [`crate::Native`]
//! itself neither `Send` nor `Sync`, since `Arc<Mutex<T>>` needs `T: Send`
//! as much as `Rc<RefCell<T>>` does. Measured on the commit before this
//! one: `Native<Tokio, Rustls, SystemDns<Tokio>>` was `Send + Sync`,
//! so that would have been a regression of a property that already
//! existed.
//!
//! So the concrete `hyper::client::conn::http1::Connection<I,
//! OutgoingBody>` is stored instead — nameable, `Unpin`, and, being a
//! concrete type, transparent to auto traits. `Send` is then inferred
//! rather than declared, exactly as `Transport`'s own doc requires ("No
//! `poll_ready`, no `&mut self`, no `Send`: Send-ness is inferred by
//! auto-traits"), and it is inferred *conditionally*: `NativeBody<I>` is
//! `Send` when `I` is, and not when `I` holds an `Rc` — which is what
//! keeps `connect.rs`'s `FakeStream` probes compiling.
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
//!   `try_send_request` will never get a response. If `Connection`
//!   completes SUCCESSFULLY before the request does, that isn't treated as
//!   success right away: `exchange` stops polling the already-finished
//!   `Connection` (re-polling a completed `Future` isn't guaranteed to be
//!   safe for every implementation) and keeps polling the request — but
//!   only for as long as that can lead anywhere, which is not always.
//!
//!   An earlier version of this paragraph said it always could: "hyper's
//!   dispatcher can't reach `Dispatched::Shutdown` without closing the
//!   request channel, so the request must be ready or become ready on the
//!   very next poll — there's no hang here". That was true while every
//!   connection was fresh, and connection reuse made it false. A pooled
//!   connection whose server closed it while nobody was polling reaches
//!   `Dispatched::Shutdown` through `poll_read_keep_alive`'s "found EOF on
//!   idle connection" with our request still sitting in the channel:
//!   `close_read` makes `can_write_head()` false, so the dispatcher never
//!   picks it up, and the callback that would resolve it is inside the
//!   very `Connection` value this function is still holding. Nothing would
//!   ever wake that future. So `conn_done` with the request still pending
//!   returns a typed error instead — see the `Poll::Pending if conn_done`
//!   arm below, and this file's
//!   `a_connection_that_ends_with_the_request_queued_fails_instead_of_hanging`,
//!   which reproduces exactly this ordering, poll by poll, on a scripted
//!   connection with no clock in it.
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
//! `exchange` returns a `Response<NativeBody>`: `Connection` is not
//! dropped when the function returns — it moves INTO THE BODY
//! (`NativeBody::conn`) and lives exactly as long as the body lives, or
//! until the body is fully read. If the caller drops `NativeBody` before
//! fully reading it, the `Connection` inside is dropped along with it —
//! that's the caller's deliberate choice to cut the read short, not a
//! silent loss of bytes: hyper documents exactly that behavior for a
//! dropped `Connection`, and `Transport::execute`'s cancellation contract
//! (v0.2 W1) requires precisely this.
//!
//! # Who keeps `SendRequest`, and why the answer changed
//!
//! Before connection reuse, `exchange` dropped `SendRequest` on the way
//! out, and that was correct: with one request per connection there was no
//! second request to send, and dropping the sender tells hyper's
//! dispatcher so, which makes it finish the current exchange and close
//! rather than wait around for a reuse that will never come.
//!
//! That is still exactly what happens when reuse is off — [`exchange`]
//! drops the sender at the end when it is given no [`CheckIn`], so the
//! no-pool path behaves as it always did. When reuse is on, the sender
//! travels into the body alongside the connection and the two are handed
//! back to the pool together, because a `Connection` without its
//! `SendRequest` is a connection nothing can ever send on again.
//!
//! **Only a body that ended cleanly is handed back.** The check-in happens
//! at exactly one place — `incoming` returning `Ready(None)` — and nowhere
//! else. Not in `Drop`, which is what makes v0.2 W1's rule structural
//! rather than remembered: a cancelled exchange, or one whose body the
//! caller abandoned half-read, leaves a connection in a protocol state
//! nobody can describe, and there is no code path here that could return
//! one to the pool even by mistake.
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
//! or the request's poll as a `hyper::Error` would be silently flattened
//! into `ErrorKind::Connect`, even though an `http_ng_core::Error` with the
//! right `ErrorKind` sits right there in that same error's `source()`.
//! [`from_hyper_error`] is the single conversion point for every site in
//! this file: it first tries to recover our `Error` from `source()`, and
//! only wraps the `hyper::Error` fresh under the given `fallback` if it
//! isn't there. The test
//! `exchange_recovers_error_kind_through_hyper_error_not_flattening_it`
//! below proves this with a real (if synthetic-IO) handshake — not just
//! by reading the code.
use crate::body::OutgoingBody;
use crate::pool::{Pool, PoolKey};
use bytes::Bytes;
use http_body::{Body, Frame, SizeHint};
use http_ng_core::{Error, ErrorKind};
use hyper::client::conn::http1;
use hyper::rt::{Read, Write};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

/// The single conversion point from `hyper::Error` to `http_ng_core::Error`
/// for this file — see the module doc comment for why flattening
/// everything into `fallback` would regress the property Task 10 proved.
fn from_hyper_error(e: hyper::Error, fallback: ErrorKind) -> Error {
    match std::error::Error::source(&e).and_then(|s| s.downcast_ref::<Error>()) {
        Some(inner) => inner.clone(),
        None => Error::new(fallback, e),
    }
}

/// A connection that has completed its HTTP/1 handshake: the two halves
/// hyper hands back, kept together.
///
/// They are only useful as a pair. `SendRequest` without `Connection` has
/// nothing driving it, and `Connection` without `SendRequest` is a socket
/// with no way to put a request on it — which is also why the pool stores
/// this type rather than either half.
pub(crate) struct Established<I>
where
    I: Read + Write + Unpin,
{
    sender: http1::SendRequest<OutgoingBody>,
    conn: http1::Connection<I, OutgoingBody>,
}

impl<I> std::fmt::Debug for Established<I>
where
    I: Read + Write + Unpin,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Established")
            .field("closed", &self.sender.is_closed())
            .finish()
    }
}

/// Everything [`exchange`] needs in order to hand a connection back once
/// the response body has ended cleanly — or `None` at the call site, which
/// is how "do not reuse this connection" is said.
pub(crate) struct CheckIn<I>
where
    I: Read + Write + Unpin,
{
    pool: Pool<I>,
    key: PoolKey,
    expires_at: Duration,
}

impl<I> CheckIn<I>
where
    I: Read + Write + Unpin,
{
    pub(crate) fn new(pool: Pool<I>, key: PoolKey, expires_at: Duration) -> Self {
        Self {
            pool,
            key,
            expires_at,
        }
    }
}

/// A failed exchange, split by the one distinction a retry may act on.
///
/// The split is not our judgement about what looks safe to resend: it is
/// hyper's. `SendRequest::try_send_request` hands the request object back
/// when — and only when — the error happened before a single byte of it
/// was serialized onto the connection ("it is safe to tell the user that
/// the request was completely canceled", `hyper::client::dispatch`). So
/// [`Failed::NotSent`] carries the original request rather than a flag
/// saying it would be fine to rebuild one, and the retry in
/// `Native::execute` needs neither a clone of the request nor a rewindable
/// body: the body it resends is the untouched original, still at its first
/// byte, because nothing ever polled it.
pub(crate) enum Failed {
    /// The request never reached the wire, and here it is back.
    ///
    /// `Box`ed only because `http::Request<OutgoingBody>` is an order of
    /// magnitude larger than an `Error`, and this enum is returned from
    /// every exchange, successful or not (clippy's `large_enum_variant`).
    /// Nothing is erased by it: the type inside is concrete, so auto traits
    /// still pass through — see the module doc on why that matters here.
    NotSent {
        error: Error,
        request: Box<http::Request<OutgoingBody>>,
    },
    /// Anything else: some or all of the request is on the wire, and
    /// resending it would not be a retry but a second request.
    Sent(Error),
}

impl Failed {
    pub(crate) fn into_error(self) -> Error {
        match self {
            Failed::NotSent { error, .. } | Failed::Sent(error) => error,
        }
    }
}

/// A pooled connection that turned out to be finished at the last moment
/// before its request was handed to hyper — the server closed it while
/// nothing was polling it, and the checkout poll one instant earlier had
/// not seen it yet.
#[derive(Debug, thiserror::Error)]
#[error("the pooled connection was closed before the request was sent")]
struct ConnectionWentAwayBeforeTheRequest;

/// The residual race the pool cannot close: the connection ended between
/// the request being handed to hyper and hyper writing it. Named rather
/// than folded into a generic connect error, because a caller reading it
/// should be able to tell it apart from a connect that never happened.
#[derive(Debug, thiserror::Error)]
#[error("the connection ended while the request was still queued on it")]
struct ConnectionEndedWithTheRequestQueued;

/// A response body that **polls the connection itself**.
///
/// Without this, the connection would stop moving as soon as headers
/// arrived: hyper requires someone to drive `Connection`, and we
/// deliberately don't spawn — that would require `Send + 'static`, and
/// single-threaded runtimes would end up shut out (see the crate's doc
/// comment and `http-ng-rt-pair-check`).
///
/// Generic over the IO type rather than boxing it — see the module doc
/// comment's section on why nothing here is boxed.
pub struct NativeBody<I>
where
    I: Read + Write + Unpin,
{
    incoming: hyper::body::Incoming,
    /// `None` — `Connection` has already finished (successfully or with
    /// an error, reported at the time); from then on `incoming` can just
    /// be drained without it, see the module doc comment for why that
    /// isn't a silent hang.
    conn: Option<http1::Connection<I, OutgoingBody>>,
    /// `None` — this connection will not be reused, whether because reuse
    /// is off, because the connection has already finished, or because
    /// something went wrong. See the module doc comment: this field going
    /// to `None` is the only way a connection is kept out of the pool, and
    /// it is deliberately easier to lose reuse than to gain it.
    reuse: Option<Reuse<I>>,
}

/// The half of [`CheckIn`] that only exists once the request has been
/// sent: the sender that has to travel back to the pool with its
/// connection.
struct Reuse<I>
where
    I: Read + Write + Unpin,
{
    checkin: CheckIn<I>,
    sender: http1::SendRequest<OutgoingBody>,
}

impl<I> std::fmt::Debug for NativeBody<I>
where
    I: Read + Write + Unpin,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeBody")
            .field("still_driving_connection", &self.conn.is_some())
            .field("may_be_reused", &self.reuse.is_some())
            .finish()
    }
}

impl<I> NativeBody<I>
where
    I: Read + Write + Unpin,
{
    /// The one and only check-in. Called when `incoming` has reported a
    /// clean end of body, and from nowhere else — see the module doc.
    fn hand_back_to_pool(&mut self) {
        let (Some(conn), Some(reuse)) = (self.conn.take(), self.reuse.take()) else {
            return;
        };
        // There is deliberately no second check here — no
        // `sender.is_closed()`, which an earlier draft had. A response that
        // ends the connection (`Connection: close`, or anything else hyper
        // reads as final) makes the `Connection` future complete, and
        // `poll_frame` above clears `reuse` the moment it does, so such a
        // connection never reaches this function at all
        // (`tests/pool.rs`'s
        // `a_connection_the_server_asked_to_close_is_not_reused`). Mutation
        // testing found the extra check unkillable, which is the honest
        // signal that it was answering a question already answered: two
        // places deciding whether a connection is usable is how they come
        // to disagree.
        reuse.checkin.pool.put(
            reuse.checkin.key,
            Established {
                sender: reuse.sender,
                conn,
            },
            reuse.checkin.expires_at,
        );
    }
}

impl<I> Body for NativeBody<I>
where
    I: Read + Write + Unpin,
{
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
            match Pin::new(conn).poll(cx) {
                Poll::Ready(Ok(())) => {
                    this.conn = None;
                    this.reuse = None;
                }
                Poll::Ready(Err(e)) => {
                    this.conn = None;
                    this.reuse = None;
                    return Poll::Ready(Some(Err(from_hyper_error(e, ErrorKind::Body))));
                }
                Poll::Pending => {}
            }
        }
        match Pin::new(&mut this.incoming).poll_frame(cx) {
            Poll::Ready(Some(Ok(f))) => Poll::Ready(Some(Ok(f))),
            Poll::Ready(Some(Err(e))) => {
                this.reuse = None;
                Poll::Ready(Some(Err(from_hyper_error(e, ErrorKind::Body))))
            }
            Poll::Ready(None) => {
                this.hand_back_to_pool();
                Poll::Ready(None)
            }
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

/// The HTTP/1 handshake, split out from [`exchange`] because a pooled
/// connection has already had one and a fresh one has not.
pub(crate) async fn handshake<I>(io: I) -> Result<Established<I>, Error>
where
    I: Read + Write + Unpin + 'static,
{
    let (sender, conn) = http1::handshake::<I, OutgoingBody>(io)
        .await
        .map_err(|e| from_hyper_error(e, ErrorKind::Connect))?;
    Ok(Established { sender, conn })
}

/// Whether a connection taken out of the pool is still worth a request.
///
/// **Exactly one poll, and it never suspends.** Both halves of that matter.
/// The poll is what lets an idle connection notice anything at all: nothing
/// polls it between requests (see [`crate::pool`]'s module doc), so a
/// server that closed the socket an hour ago is discovered here, at the
/// first poll with a live waker, or not at all. That it does not suspend is
/// what keeps checkout from ever hanging: `poll_ready` answering `Pending`
/// is read as "not ready", the connection is dropped, and the caller tries
/// the next one or dials. A false negative costs one socket; waiting could
/// cost the whole request.
///
/// The order is not interchangeable. Polling the connection first is what
/// makes `poll_ready` meaningful at all: hyper's dispatcher signals that it
/// wants another request from inside its own poll (`Receiver::poll_recv`
/// calls `taker.want()` when the queue is empty), so asking `SendRequest`
/// whether it is ready before driving the connection would ask a question
/// nothing has had the chance to answer yet.
pub(crate) async fn is_reusable<I>(est: &mut Established<I>) -> bool
where
    I: Read + Write + Unpin + 'static,
{
    std::future::poll_fn(|cx| {
        if Pin::new(&mut est.conn).poll(cx).is_ready() {
            // The connection is over: the server closed it while it was
            // idle, or it failed. Either way there is nothing here to send
            // on.
            return Poll::Ready(false);
        }
        Poll::Ready(matches!(est.sender.poll_ready(cx), Poll::Ready(Ok(()))))
    })
    .await
}

/// One request over one connection — pooled or fresh, this function cannot
/// tell and does not need to.
pub(crate) async fn exchange<I>(
    est: Established<I>,
    req: http::Request<OutgoingBody>,
    checkin: Option<CheckIn<I>>,
) -> Result<http::Response<NativeBody<I>>, Failed>
where
    I: Read + Write + Unpin + 'static,
{
    let Established {
        mut sender,
        mut conn,
    } = est;

    // One last look at the connection while the request is still OURS.
    //
    // This is the difference between a retryable failure and a lost
    // request, and the reason is asymmetric: until `try_send_request` is
    // called, we still hold the request and can hand it back; once hyper
    // has it, hyper decides. And in this particular case hyper decides
    // "no" — a connection that ends gracefully with a request still queued
    // drops the callback, and `Callback::drop` reports `message: None`
    // (conservatively, because by then it can no longer tell whether the
    // request was written). So the one moment at which a connection that
    // died while nobody was polling it can be reported honestly as "not
    // sent" is here, before it is handed over.
    //
    // Exactly one poll, and it never suspends, for the same reasons as
    // [`is_reusable`]. On a fresh connection it is not wasted either: the
    // poll is what makes hyper's dispatcher ask for a request
    // (`taker.want()`), which is what `try_send_request` needs to see
    // below.
    let dead =
        std::future::poll_fn(|cx| Poll::Ready(Pin::new(&mut conn).poll(cx).is_ready())).await;
    if dead {
        return Err(Failed::NotSent {
            error: Error::new(ErrorKind::Connect, ConnectionWentAwayBeforeTheRequest),
            request: Box::new(req),
        });
    }

    // `conn` isn't polled again once it has returned `Ready`: per
    // `Future`'s contract, re-polling an already-completed future isn't
    // guaranteed to be safe for every concrete implementation. See the
    // module doc comment for what happens to the request if `Connection`
    // finishes before it does.
    let mut conn_done = false;

    // The scope is what lets `sender` be moved afterwards: the future
    // returned by `try_send_request` borrows it for as long as it lives.
    let sent = {
        // Drive the connection and the request **together**, without spawn.
        let mut send = Box::pin(sender.try_send_request(req));
        std::future::poll_fn(|cx| {
            if !conn_done {
                match Pin::new(&mut conn).poll(cx) {
                    Poll::Ready(Ok(())) => conn_done = true,
                    Poll::Ready(Err(e)) => {
                        conn_done = true;
                        return Poll::Ready(Err(Failed::Sent(from_hyper_error(
                            e,
                            ErrorKind::Connect,
                        ))));
                    }
                    Poll::Pending => {}
                }
            }
            match send.as_mut().poll(cx) {
                Poll::Ready(Ok(r)) => Poll::Ready(Ok(r)),
                Poll::Ready(Err(mut e)) => Poll::Ready(Err(match e.take_message() {
                    // hyper's own verdict, not ours — see `Failed`.
                    Some(request) => Failed::NotSent {
                        error: from_hyper_error(e.into_error(), ErrorKind::Connect),
                        request: Box::new(request),
                    },
                    None => Failed::Sent(from_hyper_error(e.into_error(), ErrorKind::Connect)),
                })),
                // The connection has finished and the request is still
                // waiting on a channel nobody will read: hyper's dispatcher
                // reached `Dispatched::Shutdown` with our request queued
                // behind a read side it had already closed. Nothing can
                // resolve this future any more — the callback that would is
                // inside the dispatcher we are holding, and it only fires
                // when that is dropped — so returning here is the
                // alternative to hanging. `Sent`, not `NotSent`: we no
                // longer own the request, so there is nothing to hand back,
                // whatever we may believe about how far it got.
                Poll::Pending if conn_done => Poll::Ready(Err(Failed::Sent(Error::new(
                    ErrorKind::Connect,
                    ConnectionEndedWithTheRequestQueued,
                )))),
                Poll::Pending => Poll::Pending,
            }
        })
        .await
    };
    let resp = sent?;

    let (parts, incoming) = resp.into_parts();
    // `conn_done` isn't reused as-is for the body: if `Connection` had
    // already finished, `conn_done` would be `true`, but the request has
    // just returned `Ready(Ok(_))` too — meaning, per the analysis in the
    // module doc comment, the exchange is still correct, there's simply
    // nothing left to poll. A finished connection is also not a connection
    // to pool, which is why `reuse` follows the same condition rather than
    // being decided separately: two conditions that must agree are a
    // standing invitation for them to stop agreeing.
    let (conn, reuse) = if conn_done {
        (None, None)
    } else {
        (
            Some(conn),
            // Reuse is off when there is no `CheckIn`, and then `sender` is
            // dropped right here rather than travelling into the body —
            // which is what tells hyper's dispatcher there will be no
            // second request, so it closes instead of waiting. That is
            // exactly what this function did before there was a pool.
            checkin.map(|checkin| Reuse { checkin, sender }),
        )
    };

    Ok(http::Response::from_parts(
        parts,
        NativeBody {
            incoming,
            conn,
            reuse,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_ng_core::RequestBody;
    use std::future::Future;
    use std::io;

    /// Polls a future EXACTLY once and panics on `Pending` — the same
    /// choice and the same reasoning as `http-ng-native::body::tests::
    /// poll_once`: the whole path below (handshake, sending headers with
    /// no body, the outgoing body failing, delivering the error into the
    /// request future) runs synchronously on `SinkIo`, so `Pending` here
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

    impl Read for SinkIo {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: hyper::rt::ReadBufCursor<'_>,
        ) -> Poll<io::Result<()>> {
            Poll::Pending
        }
    }

    impl Write for SinkIo {
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
    /// out of the connection or the request future, through
    /// `from_hyper_error`. Mutation check: reverting `from_hyper_error` to
    /// the brief's `Error::new(fallback, e)` with no `downcast` attempt
    /// turns this test red immediately — `err.kind()` would become
    /// `ErrorKind::Connect`, not `ErrorKind::Body`.
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

        let est = {
            let fut = handshake(SinkIo);
            let mut fut = std::pin::pin!(fut);
            poll_once(fut.as_mut()).expect("handshake over SinkIo must succeed")
        };
        let fut = exchange(est, req, None);
        let mut fut = std::pin::pin!(fut);
        let err = match poll_once(fut.as_mut()) {
            Ok(_) => panic!("a failed outgoing body must fail exchange"),
            Err(e) => e.into_error(),
        };

        assert_eq!(
            err.kind(),
            &ErrorKind::Body,
            "ErrorKind must survive the path through hyper::Error, not flatten \
             into ErrorKind::Connect: {err}"
        );
    }

    /// A scripted connection: reads only after something has been written
    /// to it, hands back a canned response, and then ends when the test
    /// says so.
    ///
    /// Shared with the test through an `Rc<RefCell<_>>` so the ending can
    /// be arranged *between* two exchanges — which is the only way to
    /// reach the state this file's last test is about, because it depends
    /// on hyper's keep-alive bookkeeping (`KA::Idle` versus the `KA::Busy`
    /// a fresh connection starts in) and not on the bytes.
    #[derive(Clone)]
    struct ScriptIo(std::rc::Rc<std::cell::RefCell<Script>>);

    struct Script {
        /// Bytes still owed to the client.
        to_read: Vec<u8>,
        /// Once `to_read` runs out: `None` — `Pending` for ever; `Some(n)`
        /// — `n` more `Pending`s and then EOF.
        eof_after: Option<usize>,
        wrote: bool,
    }

    impl ScriptIo {
        fn new(response: &[u8]) -> Self {
            Self(std::rc::Rc::new(std::cell::RefCell::new(Script {
                to_read: response.to_vec(),
                eof_after: None,
                wrote: false,
            })))
        }
        fn end_after(&self, pendings: usize) {
            self.0.borrow_mut().eof_after = Some(pendings);
        }
    }

    impl Read for ScriptIo {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            mut buf: hyper::rt::ReadBufCursor<'_>,
        ) -> Poll<io::Result<()>> {
            let mut s = self.0.borrow_mut();
            if !s.wrote {
                return Poll::Pending;
            }
            if !s.to_read.is_empty() {
                let n = buf.remaining().min(s.to_read.len());
                let chunk: Vec<u8> = s.to_read.drain(..n).collect();
                buf.put_slice(&chunk);
                return Poll::Ready(Ok(()));
            }
            match s.eof_after {
                None => Poll::Pending,
                Some(0) => Poll::Ready(Ok(())),
                Some(n) => {
                    s.eof_after = Some(n - 1);
                    Poll::Pending
                }
            }
        }
    }

    impl Write for ScriptIo {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            self.0.borrow_mut().wrote = true;
            Poll::Ready(Ok(buf.len()))
        }
        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    /// Polls until ready, with a ceiling. A noop waker is enough because
    /// `ScriptIo` makes progress on every poll it is given — nothing here
    /// waits on the outside world — and the ceiling turns "this future will
    /// never finish" into a failed assertion instead of a hung test.
    fn poll_to_completion<F: Future>(mut fut: Pin<&mut F>) -> F::Output {
        let mut cx = Context::from_waker(std::task::Waker::noop());
        for _ in 0..64 {
            if let Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
                return v;
            }
        }
        panic!("did not finish within 64 polls of a synchronous, scripted connection");
    }

    fn get_request() -> http::Request<OutgoingBody> {
        http::Request::builder()
            .uri("/")
            .header("host", "example.invalid")
            .body(OutgoingBody::from_request_body(RequestBody::Empty))
            .unwrap()
    }

    /// The residual race of connection reuse, and the one outcome it must
    /// not have.
    ///
    /// **Why the connection has to have served a request first, rather than
    /// being a fresh one with EOF arranged.** hyper answers an EOF
    /// differently depending on its own keep-alive state:
    /// `should_error_on_eof` is `!state.is_idle()`, and a connection that
    /// has never been used is `KA::Busy`, so an EOF there is an *error* —
    /// and an error is the one case hyper does hand the request back for
    /// (`TrySendError::message`), which is retryable and is a different
    /// test. Only a connection that completed an exchange is `KA::Idle`,
    /// and only there does EOF mean "closing gracefully": `close_read`,
    /// `can_write_head()` false, the queued request never picked up,
    /// `Dispatched::Shutdown`, `Connection` completing with `Ok` — and the
    /// request's callback stranded inside the very `Connection` this
    /// function is still holding.
    ///
    /// Three outcomes are then possible: a response (there is none —
    /// nothing was written), an error, or `Pending` for ever. Without the
    /// `Poll::Pending if conn_done` arm it is the third, and a request that
    /// never returns and never fails is worse than either of the others.
    /// This test would not fail on that mutation, it would run out of
    /// polls — which is why `poll_to_completion` has a ceiling instead of
    /// waiting.
    #[test]
    fn a_connection_that_ends_with_the_request_queued_fails_instead_of_hanging() {
        let io = ScriptIo::new(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
        let script = io.clone();

        // A real pool and a real check-in, because the sender has to
        // survive the first exchange for there to be a second one at all.
        let pool = crate::pool::Pool::new(Some(crate::PoolConfig::default()));
        let key = crate::pool::PoolKey::new(
            crate::pool::Security::Plaintext,
            "example.invalid",
            80,
            crate::pool::Protocol::Http11,
        );

        let est = {
            let fut = handshake(io);
            let mut fut = std::pin::pin!(fut);
            poll_to_completion(fut.as_mut()).expect("handshake must succeed")
        };
        let resp = {
            let fut = exchange(
                est,
                get_request(),
                Some(CheckIn::new(pool.clone(), key.clone(), Duration::MAX)),
            );
            let mut fut = std::pin::pin!(fut);
            match poll_to_completion(fut.as_mut()) {
                Ok(r) => r,
                Err(e) => panic!("the first exchange must succeed: {}", e.into_error()),
            }
        };
        // Draining the body to its clean end is what hands the connection
        // back — see this module's doc comment.
        let mut body = std::pin::pin!(resp.into_body());
        let mut cx = Context::from_waker(std::task::Waker::noop());
        for _ in 0..64 {
            if let Poll::Ready(None) = body.as_mut().poll_frame(&mut cx) {
                break;
            }
        }

        let est = pool
            .take(&key, Duration::ZERO)
            .expect("the connection must have been handed back");

        // From here the server is gone. One `Pending` covers the look
        // before the request is handed over; the poll after that is hyper's,
        // with the request already queued.
        script.end_after(1);

        let fut = exchange(est, get_request(), None);
        let mut fut = std::pin::pin!(fut);
        let err = match poll_to_completion(fut.as_mut()) {
            Ok(_) => panic!("nothing was ever written, so there is no response to have"),
            Err(e) => e,
        };
        assert!(
            matches!(err, Failed::Sent(_)),
            "hyper has the request and will not hand it back, so this is not retryable"
        );
        assert_eq!(*err.into_error().kind(), ErrorKind::Connect);
    }
}
