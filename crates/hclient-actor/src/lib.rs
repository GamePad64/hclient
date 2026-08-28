//! A `Send` transport in front of one that is not.
//!
//! # The problem this exists for
//!
//! `hclient::Client` boxes its transport `Send + Sync`, so a transport that
//! cannot cross a thread cannot back one — and `Native<Embassy, ..>` is
//! exactly that, because `embassy_net::Stack<'d>` is `&'d RefCell<Inner>`
//! and `embassy-net` carries no `unsafe impl Send` anywhere, by design.
//! The consequence is not "no `Send`": it is no cookie jar, no redirects,
//! no response cache and no digest auth on that target, because all of
//! them live in `Client`.
//!
//! # What crosses, and why that is the whole design
//!
//! **A value, never a stream.** The obvious repair — proxying the socket —
//! makes every `poll_read` a round trip and, since a `&mut [u8]` cannot be
//! lent across a channel, costs an owned buffer and a copy per read, on
//! the target with the least RAM. This actor sits one layer up, at
//! `Transport::execute`: an `http::Request<RequestBody>` goes in (already
//! `Send`) and an `http::Response<Bytes>` comes out. One channel per
//! request rather than per read, and no JS-handle-shaped problem at all —
//! the type crossing the boundary holds nothing of the inner transport.
//!
//! It is the same shape `hclient-fetch`'s `body::pump` uses for the
//! browser, one level higher.
//!
//! # What it costs, stated where a caller meets it
//!
//! **Streaming.** The response is collected before it crosses, so a body
//! larger than [`Limits::max_response`] is a typed error rather than a
//! stream. On a device that is the trade to think about twice: streaming
//! is exactly what you keep when a response is bigger than RAM. The bound
//! is a **run-time** value, not a constant, because the right number is a
//! fact about the device.
//!
//! **One task.** The driver is a future the caller spawns — one, not one
//! per connection — so nothing here needs a compile-time task pool.
//!
//! # Cancellation
//!
//! Dropping the future `execute` returned drops the reply channel, the
//! driver notices, and the inner exchange is dropped where it lives. That
//! is `Transport::execute`'s contract, and a spawned task does not honour
//! it by accident: `tests/cancel.rs` is the pair that pins it.

#![forbid(unsafe_code)]

use bytes::Bytes;
use futures_channel::{mpsc, oneshot};
use futures_util::StreamExt as _;
use hclient_core::unversioned::{BoxSendExchange, SendTransport, Transport};
use hclient_core::{Capabilities, Error, ErrorKind, RequestBody};

/// How much of a response the boundary will carry.
///
/// A field rather than a constant: the ceiling that is right for a device
/// with 64 KiB of RAM is wrong for one with 8 MiB, and only the caller
/// knows which they have.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct Limits {
    /// Refuse a response body larger than this, in bytes.
    pub max_response: usize,
    /// How many requests may be in flight before `execute` waits.
    ///
    /// One is the honest default for a single-threaded device: the driver
    /// serves one exchange at a time, so a deeper queue buys latency
    /// hiding at the cost of memory that device does not have.
    pub in_flight: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_response: 64 * 1024,
            in_flight: 1,
        }
    }
}

impl Limits {
    /// The ceiling on a collected response body.
    #[must_use]
    pub fn max_response(mut self, bytes: usize) -> Self {
        self.max_response = bytes;
        self
    }

    /// How many requests may be queued for the driver.
    #[must_use]
    pub fn in_flight(mut self, n: usize) -> Self {
        self.in_flight = n.max(1);
        self
    }
}

/// A response that outgrew [`Limits::max_response`].
#[derive(Debug, thiserror::Error)]
#[error("the response body exceeded the actor's {limit}-byte boundary limit")]
#[non_exhaustive]
pub struct ResponseTooLarge {
    /// The bound that was exceeded.
    pub limit: usize,
}

/// The driver went away, or was never spawned.
///
/// Its own error rather than a generic one, because the two causes a
/// caller can act on are both structural: the future [`actor`] handed back
/// was never spawned, or the executor it was spawned on has stopped.
#[derive(Debug, thiserror::Error)]
#[error("the transport actor is not running — was the future returned by `actor()` spawned?")]
#[non_exhaustive]
pub struct ActorGone;

type Reply = Result<http::Response<Bytes>, Error>;

struct Job {
    req: http::Request<RequestBody>,
    reply: oneshot::Sender<Reply>,
}

/// The `Send` half: hand this to `hclient::Client`.
///
/// `Clone` shares the channel, so several handles feed one driver.
#[derive(Debug, Clone)]
pub struct Handle {
    tx: mpsc::Sender<Job>,
    caps: Capabilities,
}

/// A body that is already in memory — what the boundary carries.
#[derive(Debug)]
pub struct ActorBody(Option<Bytes>);

impl http_body::Body for ActorBody {
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Bytes>, Error>>> {
        std::task::Poll::Ready(self.0.take().map(|b| Ok(http_body::Frame::data(b))))
    }

    fn is_end_stream(&self) -> bool {
        self.0.is_none()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        match &self.0 {
            Some(b) => http_body::SizeHint::with_exact(b.len() as u64),
            None => http_body::SizeHint::with_exact(0),
        }
    }
}

/// Split a `!Send` transport into a `Send` handle and the driver that owns
/// it.
///
/// The driver is a future: spawn it wherever the transport belongs — an
/// `embassy_executor::Spawner`, a `spawn_local`, anything — and keep the
/// handle. It resolves when every handle has been dropped, so it does not
/// have to be cancelled by hand.
///
/// **The capabilities are copied out of the inner transport at this
/// point**, with the two the boundary changes overridden below: a
/// collected body is neither streamed nor full-duplex, and claiming
/// otherwise would deadlock a caller who believed it.
pub fn actor<T>(inner: T, limits: Limits) -> (Handle, impl Future<Output = ()>)
where
    T: Transport,
    T::Body: http_body::Body<Data = Bytes>,
    <T::Body as http_body::Body>::Error: std::error::Error + Send + Sync + 'static, // send-bound-exception: amendment-C1
    // The **error** type must cross, even though the transport does not:
    // `Transport::to_error` asks for it, and an error is a value like any
    // other. This is not a `Send` demand on the transport — that is the
    // whole point of the boundary — and `Native<Embassy, ..>`'s error is
    // `hclient_core::Error`, which satisfies it.
    T::Error: Send + Sync + 'static, // send-bound-exception: amendment-C1
{
    let mut caps = inner.capabilities().clone();
    // The boundary decides these two whatever the transport said, and the
    // direction is the seam's own rule: an over-claimed `full_duplex`
    // deadlocks a caller and an under-claimed one costs a buffered copy —
    // which is what this actor already is.
    caps.streaming_request_body = false;
    caps.full_duplex = false;

    let (tx, rx) = mpsc::channel(limits.in_flight);
    (Handle { tx, caps }, drive(inner, rx, limits))
}

async fn drive<T>(inner: T, mut rx: mpsc::Receiver<Job>, limits: Limits)
where
    T: Transport,
    T::Body: http_body::Body<Data = Bytes>,
    <T::Body as http_body::Body>::Error: std::error::Error + Send + Sync + 'static, // send-bound-exception: amendment-C1
    // The **error** type must cross, even though the transport does not:
    // `Transport::to_error` asks for it, and an error is a value like any
    // other. This is not a `Send` demand on the transport — that is the
    // whole point of the boundary — and `Native<Embassy, ..>`'s error is
    // `hclient_core::Error`, which satisfies it.
    T::Error: Send + Sync + 'static, // send-bound-exception: amendment-C1
{
    while let Some(Job { req, reply }) = rx.next().await {
        // Racing the work against the reply channel closing is what keeps
        // `Transport::execute`'s drop-is-cancellation contract: a caller
        // who dropped the returned future has dropped the receiver, and
        // the exchange must stop rather than run to completion behind
        // them. Nothing about a spawned driver provides that by itself.
        let mut reply = reply;
        let settled = {
            let work = one(&inner, req, limits);
            let cancelled = reply.cancellation();
            futures_util::pin_mut!(work, cancelled);
            match futures_util::future::select(work, cancelled).await {
                futures_util::future::Either::Left((out, _)) => Some(out),
                // Nobody is listening. `work` is dropped as this scope
                // ends, and with it whatever the inner transport held.
                futures_util::future::Either::Right(((), _)) => None,
            }
        };
        if let Some(out) = settled {
            let _ = reply.send(out);
        }
    }
}

/// One exchange, collected. Runs where the transport lives.
async fn one<T>(inner: &T, req: http::Request<RequestBody>, limits: Limits) -> Reply
where
    T: Transport,
    T::Body: http_body::Body<Data = Bytes>,
    <T::Body as http_body::Body>::Error: std::error::Error + Send + Sync + 'static, // send-bound-exception: amendment-C1
    // The **error** type must cross, even though the transport does not:
    // `Transport::to_error` asks for it, and an error is a value like any
    // other. This is not a `Send` demand on the transport — that is the
    // whole point of the boundary — and `Native<Embassy, ..>`'s error is
    // `hclient_core::Error`, which satisfies it.
    T::Error: Send + Sync + 'static, // send-bound-exception: amendment-C1
{
    use http_body_util::BodyExt as _;

    let resp = inner.execute(req).await.map_err(|e| inner.to_error(e))?;
    let (parts, body) = resp.into_parts();

    // Bounded before it is collected rather than after: a `size_hint` a
    // server can lie about is not a bound, so this counts what arrives and
    // stops at the ceiling instead of allocating what it was promised.
    let mut collected = bytes::BytesMut::new();
    let mut body = std::pin::pin!(body);
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|e| Error::new(ErrorKind::Body, e))?;
        if let Ok(data) = frame.into_data() {
            if collected.len() + data.len() > limits.max_response {
                return Err(Error::new(
                    ErrorKind::Body,
                    ResponseTooLarge {
                        limit: limits.max_response,
                    },
                ));
            }
            collected.extend_from_slice(&data);
        }
        // Trailers are dropped: they cannot cross as part of an
        // `http::Response<Bytes>`, and inventing a header for them would
        // put a fact on the response the server did not send there.
    }
    Ok(http::Response::from_parts(parts, collected.freeze()))
}

impl Transport for Handle {
    type Body = ActorBody;
    type Error = Error;

    async fn execute(
        &self,
        req: http::Request<RequestBody>,
    ) -> Result<http::Response<Self::Body>, Self::Error> {
        let (reply, wait) = oneshot::channel();
        let mut tx = self.tx.clone();
        // `SinkExt::send` waits for room rather than failing, which is
        // what `Limits::in_flight` is: back-pressure, not a refusal.
        futures_util::SinkExt::send(&mut tx, Job { req, reply })
            .await
            .map_err(|_| Error::new(ErrorKind::Other, ActorGone))?;
        let (parts, body) = wait
            .await
            .map_err(|_| Error::new(ErrorKind::Other, ActorGone))??
            .into_parts();
        Ok(http::Response::from_parts(parts, ActorBody(Some(body))))
    }

    /// Identity: `Self::Error` is already `hclient_core::Error`, and the
    /// inner transport's own classification was applied on the far side of
    /// the channel. Without this the category of every error would be
    /// flattened to `Other` on the way out — the same reason
    /// `hclient-wasi` overrides it.
    fn to_error(&self, e: Self::Error) -> Error {
        e
    }

    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
}

impl SendTransport for Handle {
    fn execute_send(
        &self,
        req: http::Request<RequestBody>,
    ) -> BoxSendExchange<'_, Self::Body, Self::Error> {
        // `Send` is *inferred* here, not proved: at this concrete type the
        // future holds a channel sender and a oneshot receiver and nothing
        // of the transport behind them. That is the whole point of the
        // boundary.
        Box::pin(<Self as Transport>::execute(self, req))
    }
}
