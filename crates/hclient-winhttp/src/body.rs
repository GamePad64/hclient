//! The response body: one read in flight at a time, driven by the caller.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::Bytes;
use hclient_core::{Error, ErrorKind};

use crate::session::WinHttpError;
use crate::sys::{Connect, Event, Exchange, Request, Session};

/// Where the body is between two of WinHTTP's completions.
#[derive(Debug, Clone, Copy)]
enum State {
    /// Nothing in flight; the next poll asks for bytes.
    Idle,
    /// A buffer is loaned to WinHTTP; the next poll waits for it back.
    Reading,
    /// The body ended, or failed. Either way there is nothing more.
    Done,
}

/// The response body.
///
/// # Nothing is spawned, and that is what makes cancellation honest
///
/// The only thing that ever calls `WinHttpReadData` is `poll_frame`, so a
/// caller who stops polling stops the transfer — there is no background
/// pump filling a queue behind them. Dropping this closes the request
/// handle, which is WinHTTP's own cancellation, and is why
/// [`CancelSupport::Supported`](hclient_core::CancelSupport::Supported)
/// is an honest claim here rather than a hopeful one.
///
/// # Why the handles live here rather than in the transport
///
/// A WinHTTP request handle is the child of a connect handle, which is
/// the child of the session's. The session is shared and is an
/// [`Arc`]; the other two belong to this exchange alone and are dropped
/// with it, in declaration order — request, then connect — so a child is
/// never outlived by nothing.
#[derive(Debug)]
pub struct WinHttpBody {
    request: Request,
    _connect: Connect,
    _session: Arc<Session>,
    ex: Arc<Exchange>,
    state: State,
}

impl WinHttpBody {
    pub(crate) fn new(
        request: Request,
        connect: Connect,
        session: Arc<Session>,
        ex: Arc<Exchange>,
    ) -> Self {
        Self {
            request,
            _connect: connect,
            _session: session,
            ex,
            state: State::Idle,
        }
    }
}

/// A body-phase failure, in the shape the rest of the crate uses.
fn body_error(e: WinHttpError) -> Error {
    Error::new(ErrorKind::Body, e)
}

impl http_body::Body for WinHttpBody {
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, Error>>> {
        let this = self.get_mut();
        loop {
            match this.state {
                State::Done => return Poll::Ready(None),
                State::Idle => {
                    if let Err(e) = this.request.read(&this.ex) {
                        this.state = State::Done;
                        return Poll::Ready(Some(Err(body_error(WinHttpError::Call {
                            call: "WinHttpReadData",
                            source: e,
                        }))));
                    }
                    this.state = State::Reading;
                }
                State::Reading => match this.ex.poll_next(cx) {
                    Poll::Pending => return Poll::Pending,
                    // WinHTTP's own end-of-body: a completed read of
                    // nothing. There is no separate "finished" callback.
                    Poll::Ready(Event::ReadComplete(0)) => {
                        this.state = State::Done;
                        return Poll::Ready(None);
                    }
                    Poll::Ready(Event::ReadComplete(n)) => {
                        this.state = State::Idle;
                        let bytes = this.ex.take_read(n);
                        return Poll::Ready(Some(Ok(http_body::Frame::data(bytes))));
                    }
                    Poll::Ready(Event::Failed(code)) => {
                        this.state = State::Done;
                        return Poll::Ready(Some(Err(body_error(WinHttpError::Request(
                            crate::sys::Win32Error(code),
                        )))));
                    }
                    Poll::Ready(Event::SecureFailure(flags)) => {
                        this.state = State::Done;
                        return Poll::Ready(Some(Err(Error::new(
                            ErrorKind::Tls,
                            WinHttpError::Tls(flags),
                        ))));
                    }
                    // A completion for a call this body never made. It
                    // is reported rather than ignored: a silently
                    // truncated body is the failure mode this workspace
                    // refuses everywhere else.
                    Poll::Ready(other) => {
                        this.state = State::Done;
                        return Poll::Ready(Some(Err(body_error(WinHttpError::OutOfOrder {
                            got: event_name(&other),
                            expected: "READ_COMPLETE",
                        }))));
                    }
                },
            }
        }
    }
}

/// WinHTTP's own name for a completion, for an error a reader can match
/// against the documentation.
pub(crate) fn event_name(e: &Event) -> &'static str {
    match e {
        Event::SendComplete => "SENDREQUEST_COMPLETE",
        Event::HeadersAvailable => "HEADERS_AVAILABLE",
        Event::ReadComplete(_) => "READ_COMPLETE",
        Event::Failed(_) => "REQUEST_ERROR",
        Event::SecureFailure(_) => "SECURE_FAILURE",
    }
}
