//! A `tower::Service` as a transport, so a server can be tested in
//! process.
//!
//! # What this is for
//!
//! `axum::Router` is a `tower::Service<http::Request<axum::body::Body>>`.
//! [`AppTransport`] makes one into a [`Transport`], so a test drives the
//! **real** `hclient::Client` — redirects, the cookie jar, the cache,
//! retries, decompression, `.json()` — against the **real** router, with
//! no socket, no port and nothing spawned.
//!
//! It is httpx's `ASGITransport` in Rust, and it exists here rather than
//! anywhere else for a structural reason: a client with no transport seam
//! cannot have one. `reqwest` has no `pub trait Connect`, which is why
//! testing an axum app against it means binding a port.
//!
//! What `tower::ServiceExt::oneshot` already gives you is a **service
//! call**, not a client: the `http::Request` is assembled by hand, no
//! redirect is followed, no cookie is stored, nothing is decompressed and
//! there is no `.json()`. The difference is the whole of `hclient::Client`.
//!
//! ```no_run
//! # async fn f(app: impl tower_service::Service<
//! #     http::Request<hclient_tower::app::OutgoingBody>,
//! #     Response = http::Response<hclient_tower::app::OutgoingBody>,
//! #     Error = std::convert::Infallible,
//! #     Future: Send,
//! # > + Clone + Send + Sync + 'static) -> Result<(), Box<dyn std::error::Error>> {
//! let t = hclient_tower::app::AppTransport::new("testserver", app);
//! let client = hclient::Client::builder(t).build()?;
//! let body = client.get("http://testserver/health").send().await?.collect().await?;
//! # let _ = body; Ok(()) }
//! ```
//!
//! # The authority is named, and a request naming another is refused
//!
//! An in-process service has no origin, and a client needs an absolute
//! URI — without one there is nowhere to resolve a redirect's
//! `Location: /other` against. httpx solves it with a synthetic
//! `http://testserver`, and so does this.
//!
//! **What this adds is the refusal.** A test that accidentally names a
//! real host would otherwise be answered by the local router — a passing
//! test about a server it never reached. So the authority is given at
//! construction and any other is [`WrongAuthority`], by name.

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

use crate::error::{BodyFailure, WrongAuthority};
use bytes::Bytes;
use hclient_core::Capabilities;
use hclient_core::unversioned::Transport;
use hclient_core::{Error, ErrorKind, RequestBody};
use http_body::{Body, Frame, SizeHint};

/// [`RequestBody`] as an `http_body::Body`, which is what a server-side
/// service expects.
///
/// `RequestBody` is an enum with a factory arm and is not itself a body;
/// this is the thin view that makes it one. It lives here rather than in
/// `hclient-core` because this is the only consumer — the rule that put
/// the browser body's pump back into `hclient-fetch`.
pub struct OutgoingBody(Inner);

/// A streaming body has no `Debug`, so this reports the shape rather than
/// deriving — the same trade `FromFn` makes one crate over.
impl core::fmt::Debug for OutgoingBody {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match &self.0 {
            Inner::Empty => f.write_str("OutgoingBody::Empty"),
            Inner::Full(b) => write!(
                f,
                "OutgoingBody::Full({} bytes)",
                b.as_ref().map_or(0, Bytes::len)
            ),
            Inner::Streaming(_) => f.write_str("OutgoingBody::Streaming(..)"),
        }
    }
}

enum Inner {
    Empty,
    /// One frame, then the end.
    Full(Option<Bytes>),
    Streaming(Box<dyn Body<Data = Bytes, Error = Error> + Unpin + Send>), // send-bound-exception: amendment-C2
}

impl OutgoingBody {
    /// Turns a [`RequestBody`] into a body a service can read.
    ///
    /// **Through `RequestBody::reduce` rather than by matching the enum**,
    /// and the reason is the API freeze working from outside: `RequestBody`
    /// is `#[non_exhaustive]`, so a match here needs a wildcard arm and a
    /// wildcard arm is where a new variant goes to be silently mishandled.
    /// `Reduced` is exhaustive and owned by the crate that would add one,
    /// and it already carries the factory arm's depth bound, so this gets
    /// the loop protection for free instead of writing a second one.
    ///
    /// # Errors
    ///
    /// A `Rewindable` whose factory returns another `Rewindable` past
    /// `MAX_REWIND_DEPTH`.
    pub fn new(body: RequestBody) -> Result<Self, Error> {
        Ok(Self(
            match body.reduce().map_err(|e| Error::new(ErrorKind::Body, e))? {
                hclient_core::Reduced::Empty => Inner::Empty,
                hclient_core::Reduced::Bytes(b) => Inner::Full(Some(b)),
                hclient_core::Reduced::Streaming(b) => Inner::Streaming(b),
            },
        ))
    }
}

impl Body for OutgoingBody {
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Error>>> {
        match &mut self.0 {
            Inner::Empty => Poll::Ready(None),
            Inner::Full(b) => Poll::Ready(b.take().map(|b| Ok(Frame::data(b)))),
            Inner::Streaming(b) => Pin::new(b).poll_frame(cx),
        }
    }

    fn is_end_stream(&self) -> bool {
        match &self.0 {
            Inner::Empty => true,
            Inner::Full(b) => b.is_none(),
            Inner::Streaming(b) => b.is_end_stream(),
        }
    }

    fn size_hint(&self) -> SizeHint {
        match &self.0 {
            Inner::Empty => SizeHint::with_exact(0),
            Inner::Full(Some(b)) => SizeHint::with_exact(b.len() as u64),
            Inner::Full(None) => SizeHint::with_exact(0),
            Inner::Streaming(b) => b.size_hint(),
        }
    }
}

/// The service's response body, with its error mapped to this
/// workspace's.
///
/// **The transport is the boundary, so this is where the conversion
/// belongs.** `BoxedTransport`'s blanket impl requires a response body
/// whose error is `Into<hclient_core::Error>`, and a server-side body's
/// is not: `http_body_util::Full`'s is `Infallible` and `axum::body::Body`'s
/// is `axum::Error`. Without this an `axum::Router` could be a
/// `Transport` and still not back a `Client`, which is the whole point.
///
/// `B: Unpin` rather than a pin projection, which every body this is
/// meant for satisfies and which keeps `unsafe` out of a crate that has
/// none.
#[derive(Debug)]
pub struct IncomingBody<B>(B);

impl<B> Body for IncomingBody<B>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>, // send-bound-exception: amendment-C1
{
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Error>>> {
        Pin::new(&mut self.0).poll_frame(cx).map(|o| {
            o.map(|r| {
                r.map_err(|e| {
                    // `Error::new` wants a sized source; a `Box<dyn
                    // Error>` is not one, so the text is carried
                    // instead. The alternative — a `Box<Box<dyn ..>>`
                    // — would type-check and read as a mistake.
                    Error::new(ErrorKind::Body, BodyFailure(e.into().to_string()))
                })
            })
        })
    }

    fn is_end_stream(&self) -> bool {
        self.0.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.0.size_hint()
    }
}

impl core::fmt::Display for BodyFailure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BodyFailure {}

impl core::fmt::Display for WrongAuthority {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "this app transport serves `{}`; the request named `{}`",
            self.expected, self.actual
        )
    }
}

impl std::error::Error for WrongAuthority {}

/// A `tower::Service` as a [`Transport`], for testing a server in
/// process. See the module doc.
#[derive(Debug, Clone)]
pub struct AppTransport<S> {
    inner: S,
    authority: String,
    capabilities: Capabilities,
}

impl<S> AppTransport<S> {
    /// Serves `authority` with `svc`.
    ///
    /// `authority` is the host — and port, if the test uses one — that
    /// requests must name. Anything else is refused rather than served.
    pub fn new(authority: impl Into<String>, svc: S) -> Self {
        Self {
            inner: svc,
            authority: authority.into(),
            // **Every claim here is about this transport and none is
            // aspirational, because a test will believe it.** No TLS
            // handshake happens, no proxy is consulted, no connection is
            // reused because none exists, and the client follows
            // redirects itself since the service answers one request.
            // `full_duplex` is `false`: a `tower::Service` takes the
            // whole request and then answers, so a caller streaming a
            // body cannot read a response while still writing.
            capabilities: Capabilities::default(),
        }
    }

    /// The service underneath, for a test that also wants to call it
    /// directly.
    pub fn service(&self) -> &S {
        &self.inner
    }
}

impl<S, B, E> Transport for AppTransport<S>
where
    S: tower_service::Service<http::Request<OutgoingBody>, Response = http::Response<B>, Error = E>
        + Clone,
    B: http_body::Body<Data = Bytes> + Unpin,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>, // send-bound-exception: amendment-C1
    E: std::error::Error + Send + Sync + 'static,             // send-bound-exception: amendment-C1
{
    type Body = IncomingBody<B>;
    type Error = Error;

    fn execute(
        &self,
        req: http::Request<RequestBody>,
    ) -> impl Future<Output = Result<http::Response<IncomingBody<B>>, Error>> {
        let mut svc = self.inner.clone();
        let expected = self.authority.clone();
        async move {
            let actual = req.uri().authority().map(http::uri::Authority::as_str);
            if actual != Some(expected.as_str()) {
                // Refused before the service sees it: a test that named a
                // real host must not be answered by the local router.
                return Err(Error::new(
                    ErrorKind::Connect,
                    WrongAuthority {
                        expected,
                        actual: actual.unwrap_or("<none>").to_owned(),
                    },
                ));
            }
            let (parts, body) = req.into_parts();
            let req = http::Request::from_parts(parts, OutgoingBody::new(body)?);

            core::future::poll_fn(|cx| svc.poll_ready(cx))
                .await
                .map_err(|e| Error::new(ErrorKind::Connect, e))?;
            svc.call(req)
                .await
                .map(|resp| resp.map(IncomingBody))
                .map_err(|e| Error::new(ErrorKind::Other, e))
        }
    }

    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    fn to_error(&self, e: Self::Error) -> Error {
        e
    }
}

/// The `Send` half, which every backend in this workspace owes.
///
/// Its body at a concrete type is `Box::pin(self.execute(req))` — `Send`
/// is *inferred* here rather than proven, which is the asymmetry the
/// whole seam design rests on: proof is owed only by generic code.
impl<S, B, E> hclient_core::unversioned::SendTransport for AppTransport<S>
where
    S: tower_service::Service<http::Request<OutgoingBody>, Response = http::Response<B>, Error = E>
        + Clone
        + Send // send-bound-exception: amendment-C16
        + Sync // send-bound-exception: amendment-C16
        + 'static, // send-bound-exception: amendment-C16
    S::Future: Send + 'static, // send-bound-exception: amendment-C16
    B: http_body::Body<Data = Bytes> + Unpin + Send + 'static, // send-bound-exception: amendment-C16
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,  // send-bound-exception: amendment-C1
    E: std::error::Error + Send + Sync + 'static, // send-bound-exception: amendment-C16
{
    fn execute_send(
        &self,
        req: http::Request<RequestBody>,
    ) -> Pin<Box<dyn Future<Output = Result<http::Response<IncomingBody<B>>, Error>> + Send + '_>> // send-bound-exception: amendment-C16
    {
        Box::pin(<Self as Transport>::execute(self, req))
    }
}
