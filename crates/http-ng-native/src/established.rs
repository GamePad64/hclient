//! A connection that has finished handshaking, whichever protocol it
//! speaks — and the two vocabulary types both protocols share.
//!
//! [`crate::connect`] makes connections; this module is about *using* one
//! that already exists. It holds no protocol logic of its own: every
//! function here is a two-line match onto [`crate::h1`] or
//! [`crate::http2`], and it exists so that exactly one place in the crate
//! knows there is more than one protocol.
//!
//! Three things live here rather than in either protocol module, because
//! each of them is shared and none of them belongs to one side:
//!
//! - [`Established`] — what [`crate::pool`] stores. The pool is keyed on
//!   [`crate::pool::Protocol`] among other things, so a bucket only ever
//!   holds one variant; the enum is what lets one `Pool<I>` hold both
//!   kinds without the key and the value being able to disagree about
//!   which.
//! - [`Failed`] — the split a retry may act on, defined once so that
//!   `Native::execute` reads one verdict regardless of protocol.
//! - [`NativeBody`] — the transport's `Transport::Body`. It is one type
//!   with a private enum inside rather than two types, because
//!   `Transport::Body` is a single associated type and because the name is
//!   public API that predates HTTP/2.
//!
//! **Nothing here is boxed behind `dyn`**, and that is the same
//! load-bearing property `h1.rs`'s module doc spells out: an enum of two
//! concrete bodies still lets auto traits through, so
//! `NativeBody<NativeIo<Tokio, Rustls>>` stays `Send` with the `http2`
//! feature on exactly as it was with it off (`tests/shape.rs`).
use crate::body::OutgoingBody;
use crate::pool::CheckIn;
use bytes::Bytes;
use http_body::{Body, Frame, SizeHint};
use http_ng_core::Error;
use http_ng_core::unversioned::{ConnectionId, Hooks};
use hyper::rt::{Read, Write};
use std::pin::Pin;
use std::task::{Context, Poll};

/// A handshaken connection, ready for a request.
///
/// The HTTP/2 variant is boxed, and for the reason `Failed::NotSent`'s
/// request is: `h2::client::Connection` is roughly twice the size of
/// hyper's HTTP/1 pair, and an enum sized to its largest variant would
/// make every idle HTTP/1 connection in the pool pay for it (clippy's
/// `large_enum_variant`). It boxes a **concrete** type, so nothing is
/// erased — auto traits still pass through, which is the property
/// `h1.rs`'s module doc is about.
///
/// The lint then fires the other way — with the box in place HTTP/1 is
/// the large variant — and that is the imbalance we want: the HTTP/1 path
/// is the one every build has and the one that must not allocate, so it
/// is the variant that sets the size. Suppressed only where the lint can
/// fire at all: without the feature there is a single variant and an
/// `expect` would go unfulfilled.
#[cfg_attr(
    feature = "http2",
    expect(
        clippy::large_enum_variant,
        reason = "HTTP/1 is deliberately the unboxed variant; see above"
    )
)]
pub(crate) enum Established<I>
where
    I: Read + Write + Unpin,
{
    H1(crate::h1::Established<I>),
    #[cfg(feature = "http2")]
    H2(Box<crate::http2::Established<I>>),
}

impl<I> Established<I>
where
    I: Read + Write + Unpin,
{
    /// Which connection this is, for
    /// [`Hooks`](http_ng_core::unversioned::Hooks).
    ///
    /// Stored on the protocol's own `Established` and read back through a
    /// two-line match here — the shape this module's doc describes —
    /// rather than kept in a side table beside the pool: the id has to
    /// survive a check-in and a checkout to be worth anything, and the
    /// only thing that survives those is the connection itself.
    pub(crate) fn id(&self) -> ConnectionId {
        match self {
            Established::H1(e) => e.id,
            #[cfg(feature = "http2")]
            Established::H2(e) => e.id,
        }
    }
}

impl<I> std::fmt::Debug for Established<I>
where
    I: Read + Write + Unpin,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Established::H1(e) => e.fmt(f),
            #[cfg(feature = "http2")]
            Established::H2(e) => e.fmt(f),
        }
    }
}

/// A failed exchange, split by the one distinction a retry may act on.
///
/// The split is not our judgement about what looks safe to resend. On
/// HTTP/1 it is hyper's: `SendRequest::try_send_request` hands the request
/// object back when — and only when — the error happened before a single
/// byte of it was serialized onto the connection ("it is safe to tell the
/// user that the request was completely canceled",
/// `hyper::client::dispatch`). So [`Failed::NotSent`] carries the original
/// request rather than a flag saying it would be fine to rebuild one, and
/// the retry in `Native::execute` needs neither a clone of the request nor
/// a rewindable body: the body it resends is the untouched original, still
/// at its first byte, because nothing ever polled it.
///
/// On HTTP/2 there is no such hand-back — see [`crate::http2::exchange`]'s
/// doc comment for where the line falls instead, and why the difference
/// only narrows what may be retried rather than widening it.
pub(crate) enum Failed {
    /// The request never reached the wire, and here it is back.
    ///
    /// `Box`ed only because `http::Request<OutgoingBody>` is an order of
    /// magnitude larger than an `Error`, and this enum is returned from
    /// every exchange, successful or not (clippy's `large_enum_variant`).
    /// Nothing is erased by it: the type inside is concrete, so auto traits
    /// still pass through — see `h1.rs`'s module doc on why that matters.
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

/// Whether a connection taken out of the pool is still worth a request —
/// see each protocol's `is_reusable` for the contract, which is the same
/// on both: exactly one poll, and it never suspends.
pub(crate) async fn is_reusable<I>(est: &mut Established<I>) -> bool
where
    I: Read + Write + Unpin + 'static,
{
    match est {
        Established::H1(e) => crate::h1::is_reusable(e).await,
        #[cfg(feature = "http2")]
        Established::H2(e) => crate::http2::is_reusable(e).await,
    }
}

/// One request over one connection.
///
/// **The request is put into the shape its protocol needs here, and
/// nowhere else** — origin-form with a `Host:` for HTTP/1, the absolute
/// URI and no `Host:` of ours for HTTP/2, which builds `:scheme`,
/// `:authority` and `:path` out of it.
///
/// That this happens inside the same `match` that picks the exchange is
/// the point, and it was not free. An earlier version had
/// `Native::execute` rewrite the request from the *pool key*'s protocol
/// while this function dispatched on the *connection*'s: two sources for
/// one fact. Mutation testing found them disagreeing — check a connection
/// in under the wrong protocol and the next request goes out as HTTP/2
/// carrying an origin-form URI, which has neither a scheme nor an
/// authority for h2 to build pseudo-headers from. Reading the protocol off
/// the connection about to be spoken on cannot disagree with itself.
///
/// `canonical` is the URI the request arrived with. It is needed because
/// the HTTP/1 rewrite has to be undone when hyper hands the request back
/// unsent: `Native::execute` may try again on a connection that speaks the
/// other protocol, and an origin-form URI would be unusable there.
pub(crate) async fn exchange<I, H>(
    est: Established<I>,
    mut req: http::Request<OutgoingBody>,
    checkin: Option<CheckIn<I>>,
    canonical: &http::Uri,
    hooks: H,
) -> Result<http::Response<NativeBody<I, H>>, Failed>
where
    I: Read + Write + Unpin + 'static,
    H: Hooks,
{
    let id = est.id();
    match est {
        Established::H1(e) => {
            let rewritten = Rewritten::for_http1(&mut req);
            match crate::h1::exchange(e, req, checkin, hooks, id).await {
                Ok(r) => Ok(r.map(|b| NativeBody {
                    inner: Inner::H1(b),
                })),
                Err(Failed::NotSent { error, mut request }) => {
                    rewritten.undo(&mut request, canonical);
                    Err(Failed::NotSent { error, request })
                }
                Err(other) => Err(other),
            }
        }
        #[cfg(feature = "http2")]
        Established::H2(e) => crate::http2::exchange(*e, req, checkin).await.map(|r| {
            r.map(|b| NativeBody {
                inner: Inner::H2(Box::new(b)),
            })
        }),
    }
}

/// What [`Rewritten::for_http1`] changed about a request, and how to
/// change it back.
///
/// **Why anything has to be undone at all.** `Native::execute` may make
/// more than one attempt with the same request object — that is what
/// [`Failed::NotSent`] is for — and the attempts need not agree on the
/// protocol. Rewriting in place and never undoing it would leave an
/// HTTP/2 attempt holding a URI with no authority in it.
///
/// The trailer guard is here for exactly that reason and not because it
/// is a rewrite: it is armed on the HTTP/1 branch and would otherwise
/// still be armed on an HTTP/2 retry, where `Trailer:` means nothing and
/// trailers go out regardless. Pairing it with the URI in one value is
/// what makes forgetting the second half impossible rather than
/// unlikely.
struct Rewritten {
    /// [`Rewritten::for_http1`] inserted `Host:` because the caller
    /// had not set one. Undoing removes exactly the header we added and no
    /// other: removing a caller's own `Host:` would lose a deliberate
    /// override, and keeping ours would hand h2 a `Host:` that can
    /// disagree with `:authority` (they differ whenever the URI names the
    /// scheme's default port explicitly).
    host_inserted: bool,
}

impl Rewritten {
    /// Everything the HTTP/1 path needs done to a request before hyper
    /// sees it: the URI rewritten into origin-form (hyper's HTTP/1 client
    /// requires exactly that, not absolute-form), `Host:` set if the
    /// caller didn't set it themselves, and the body's trailer guard
    /// armed from `Trailer:`
    /// ([`crate::body::UndeclaredRequestTrailers`] for what that buys and
    /// what it costs).
    ///
    /// By the time this is called, `Native::key_parts` has succeeded —
    /// meaning its checks (`connect::host`, `connect::wants_tls`) passed,
    /// so `req.uri()` is guaranteed to carry a host and a supported
    /// (`http`/`https`) scheme; this function doesn't recheck them.
    fn for_http1(req: &mut http::Request<OutgoingBody>) -> Self {
        let uri = req.uri().clone();
        let https = uri.scheme_str() == Some("https");
        let default_port = if https { 443 } else { 80 };
        let port = uri.port_u16().unwrap_or(default_port);
        let host = uri.host().unwrap_or_default();

        let mut host_inserted = false;
        if !req.headers().contains_key(http::header::HOST) {
            let authority = if port == default_port {
                host.to_owned()
            } else {
                format!("{host}:{port}")
            };
            // A host that got this far (having passed DNS resolution, and
            // for `https` also having built a TLS SNI value) is, in
            // practice, always valid as a header value. If it somehow
            // isn't, the request goes out without `Host:`, and that's not
            // a silent loss: no server this crate talks to will accept an
            // HTTP/1.1 request without `Host:`, so the failure will be an
            // immediate, explicit protocol failure, not a silent no-op.
            if let Ok(v) = http::HeaderValue::from_str(&authority) {
                req.headers_mut().insert(http::header::HOST, v);
                host_inserted = true;
            }
        }
        let pq = uri
            .path_and_query()
            .map(|p| p.as_str())
            .unwrap_or("/")
            .to_owned();
        if let Ok(u) = pq.parse::<http::Uri>() {
            *req.uri_mut() = u;
        }
        // Read from the headers as they are about to go out, and in two
        // statements because the second borrows `req` mutably: the set
        // the guard enforces has to be the set hyper's encoder will parse
        // off this same request.
        let declared = crate::body::declared_trailer_names(req.headers());
        req.body_mut().require_declared_trailers(declared);
        Self { host_inserted }
    }

    fn undo(self, req: &mut http::Request<OutgoingBody>, canonical: &http::Uri) {
        *req.uri_mut() = canonical.clone();
        if self.host_inserted {
            req.headers_mut().remove(http::header::HOST);
        }
        req.body_mut().allow_undeclared_trailers();
    }
}

/// The response body of [`crate::Native`], on either protocol.
///
/// It **polls its connection itself** — on both. That is the vertical's
/// technical crux and it is not a detail of one protocol: hyper's HTTP/1
/// `Connection` and h2's `Connection` are alike futures that somebody has
/// to drive, this transport deliberately has no `Spawn` to drive them
/// with, so the body does it from `poll_frame`. See `h1.rs`'s and
/// `http2.rs`'s module docs.
pub struct NativeBody<I, H = http_ng_core::unversioned::NoHooks>
where
    I: Read + Write + Unpin,
{
    inner: Inner<I, H>,
}

/// Boxed on the HTTP/2 side for the same reason [`Established`] is, with
/// the same non-consequence (a box around a concrete type is transparent
/// to auto traits) and the same suppression, for the same reason.
#[cfg_attr(
    feature = "http2",
    expect(
        clippy::large_enum_variant,
        reason = "HTTP/1 is deliberately the unboxed variant; see `Established`"
    )
)]
enum Inner<I, H>
where
    I: Read + Write + Unpin,
{
    H1(crate::h1::H1Body<I, H>),
    /// **No `H`, and that is a gap rather than a decision made twice.**
    /// The h2 body reports no [`Closed`](http_ng_core::unversioned::Closed)
    /// event: `Connected`, `Reused` and `Head` come from
    /// `Native::execute` and are protocol-agnostic, but the end of a
    /// connection is known inside the body, and h2's has three places it
    /// can arrive (the connection future, the response stream, the pump)
    /// where HTTP/1's has two. It is written down in
    /// `docs/v03-acceptance.md` rather than guessed at here.
    #[cfg(feature = "http2")]
    H2(Box<crate::http2::H2Body<I>>),
}

impl<I, H> NativeBody<I, H>
where
    I: Read + Write + Unpin,
{
    pub(crate) fn h1(b: crate::h1::H1Body<I, H>) -> Self {
        Self {
            inner: Inner::H1(b),
        }
    }
}

impl<I, H> std::fmt::Debug for NativeBody<I, H>
where
    I: Read + Write + Unpin,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.inner {
            Inner::H1(b) => b.fmt(f),
            #[cfg(feature = "http2")]
            Inner::H2(b) => b.fmt(f),
        }
    }
}

impl<I, H> Body for NativeBody<I, H>
where
    I: Read + Write + Unpin,
    H: Hooks + Unpin,
{
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Error>>> {
        match &mut self.inner {
            Inner::H1(b) => Pin::new(b).poll_frame(cx),
            #[cfg(feature = "http2")]
            Inner::H2(b) => Pin::new(b).poll_frame(cx),
        }
    }

    fn is_end_stream(&self) -> bool {
        match &self.inner {
            Inner::H1(b) => b.is_end_stream(),
            #[cfg(feature = "http2")]
            Inner::H2(b) => b.is_end_stream(),
        }
    }

    fn size_hint(&self) -> SizeHint {
        match &self.inner {
            Inner::H1(b) => b.size_hint(),
            #[cfg(feature = "http2")]
            Inner::H2(b) => b.size_hint(),
        }
    }
}
