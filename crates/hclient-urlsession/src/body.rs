//! The response body, read out of the delegate's queue.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::Bytes;
use hclient_core::{Error, ErrorKind};
use http_body::{Body, Frame};

use crate::delegate::{Chunk, Shared};
use crate::session::UrlSessionError;

/// A streaming response body.
///
/// It holds the task so that **dropping the body cancels the transfer**,
/// which is `Transport::execute`'s own contract and what
/// `Capabilities::cancel_on_drop` promises — see [`Cancelling`].
#[derive(Debug)]
pub struct UrlSessionBody {
    shared: Arc<Shared>,
    /// `None` once the delegate has said the task ended, so a body polled
    /// past its end answers `None` rather than waiting for a queue that
    /// nobody will push to again.
    live: bool,
    _cancel: Cancelling,
}

/// Cancels the task when dropped.
///
/// A separate type rather than a `Drop` on the body, because the head and
/// the body are handed over at different moments and both have to be able
/// to hold it: a caller who drops the *response* before reading a byte
/// must stop the transfer just as one who drops it halfway does.
#[derive(Debug)]
pub(crate) struct Cancelling(pub(crate) objc2::rc::Retained<objc2_foundation::NSURLSessionTask>);

impl Drop for Cancelling {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

impl UrlSessionBody {
    pub(crate) fn new(shared: Arc<Shared>, cancel: Cancelling) -> Self {
        Self {
            shared,
            live: true,
            _cancel: cancel,
        }
    }
}

impl Body for UrlSessionBody {
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Error>>> {
        let this = self.get_mut();
        if !this.live {
            return Poll::Ready(None);
        }
        match this.shared.poll_next(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Chunk::Data(b)) => Poll::Ready(Some(Ok(Frame::data(b)))),
            Poll::Ready(Chunk::End(None)) => {
                this.live = false;
                Poll::Ready(None)
            }
            Poll::Ready(Chunk::End(Some(msg))) => {
                this.live = false;
                Poll::Ready(Some(Err(Error::new(ErrorKind::Body, UrlSessionError(msg)))))
            }
            // A second head cannot happen — `didReceiveResponse:` is
            // called once per task — and if it ever did, treating it as
            // data would put header bytes into a body. Ending is the
            // honest answer to something this code cannot represent.
            Poll::Ready(Chunk::Head(..)) => {
                this.live = false;
                Poll::Ready(Some(Err(Error::new(
                    ErrorKind::Body,
                    UrlSessionError("a second response head arrived on one task".into()),
                ))))
            }
        }
    }
}
