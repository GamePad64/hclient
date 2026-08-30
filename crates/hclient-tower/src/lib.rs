//! [`Transport`] as a [`tower_service::Service`], so that the `tower-http`
//! middleware stack — decompression, tracing, retries, timeouts — applies
//! to this client without any of it being reimplemented here.
//!
//! # The impedance mismatch, and the one thing it costs
//!
//! `Transport::execute` is `fn(&self, ..) -> impl Future`: the future
//! borrows the transport. `Service::Future` is an associated type with no
//! lifetime parameter, so it cannot borrow `self` at all. The bridge is an
//! owned handle inside the future — the transport lives in an `Arc`, cloned
//! per call — and a boxed future, because an RPITIT's type cannot be named
//! to declare it as an associated type.
//!
//! **The boxed future is `Send`**, so this service goes wherever a tower
//! service goes — `tokio::spawn`, `axum` handlers, a multithreaded
//! executor — and the `tower-http` middleware stack, which never required
//! `Send` of its own accord, composes as it always did.
//!
//! **This module said the opposite for two verticals, and the record of
//! why is worth more than the fix.** It read that declaring `Send` on the
//! box meant proving `Transport::execute`'s RPITIT `Send`, which needs
//! return type notation — still unstable, rust-lang/rust#109417 — and
//! concluded with *when #109417 lands, the fix is one bound in this file*.
//!
//! Two things were wrong with that, and neither was the unstable feature.
//!
//! **A bound is only worth having if something satisfies it.**
//! `hclient-native`'s own future was `!Send` at the time, from a single
//! `Box<dyn Stream<..>>` in its connector that discarded every shipped
//! resolver's `Send`. The promised one-line fix would have compiled and
//! then excluded the one transport anybody reaches for.
//!
//! **And RTN was never the only way to name a bound.** The seams a
//! transport awaits carry associated futures now, and
//! `hclient_core::unversioned::SendTransport` is a separate trait whose
//! impl may carry bounds `Transport` does not — so `T: SendTransport` says
//! what `T: Transport<execute(..): Send>` would have said, on stable, and
//! excludes nobody from the seam. RTN was measured on nightly before that
//! route was taken: it works, and across a crate boundary it ICEs.
//!
//! What it costs is that a transport which cannot promise `Send` cannot be
//! adapted here. `hclient-dns-doh`-resolving transports are that case, and
//! they keep `Transport` itself.
//!
//! # Bounding concurrency
//!
//! Without a limit, in-flight requests are unbounded. The limit is
//! `tower`'s, applied between the two adapters, and this crate deliberately
//! neither re-exports nor re-implements it — taking `tower::limit` as a
//! normal dependency would put `tokio` (`sync`) and `tokio-util` in the
//! graph of every consumer for a layer they may never use:
//!
//! ```text
//! ServiceTransport::new(ConcurrencyLimit::new(TransportService::new(t), 8), caps)
//! ```
//!
//! That it actually limits is not assumed: `tests/concurrency.rs` drives
//! three requests through a limit of two and checks the third never
//! reaches the transport until a permit frees. It works because
//! `ServiceTransport::execute` drives `poll_ready` to completion on the
//! clone it is about to call, which is where `ConcurrencyLimit` reserves
//! its permit — remove that drive and the layer panics rather than
//! silently overshooting.
//!
//! **What it does not bound: sockets.** The permit lives in
//! `ConcurrencyLimit`'s response future and is dropped when that future
//! completes — at the response HEAD. The body streams on afterwards,
//! holding its connection, outside the limit, so with a limit of N there
//! can be more than N connections open. Measured, in
//! `the_permit_is_released_at_the_response_head_so_bodies_are_not_bounded`.
//! A limit that covered bodies too would have to carry its permit into the
//! response body — the shape `hclient-wasi`'s `Body` and `hclient::
//! Deadline` both use — and that is a layer this crate would have to own
//! rather than borrow; it is not written yet, and the design document
//! records the gap rather than implying it away.
#![forbid(unsafe_code)]

pub mod app;
mod error;

// `WrongAuthority` keeps the root it already had; `BodyFailure` is
// private and reaches a caller only through `Error::source`.
pub use error::WrongAuthority;

use hclient_core::unversioned::Transport;
use hclient_core::{Capabilities, Error, RequestBody};
use std::error::Error as StdError;
use std::future::Future;
use std::future::poll_fn;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

/// A [`Transport`] wearing tower's clothes.
///
/// Cloning is cheap and shares one transport: `Arc` internally, which is
/// what `Service::call`'s `&mut self` plus a `'static` future require.
#[derive(Debug)]
pub struct TransportService<T> {
    inner: Arc<T>,
}

impl<T> Clone for TransportService<T> {
    /// Hand-written rather than derived: `#[derive(Clone)]` would demand
    /// `T: Clone`, which is not needed — the `Arc` is what is cloned.
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<T> TransportService<T> {
    pub fn new(transport: T) -> Self {
        Self {
            inner: Arc::new(transport),
        }
    }

    /// The wrapped transport's capabilities.
    ///
    /// Exposed deliberately, and it is the reason this adapter is not the
    /// whole story for middleware. A tower layer knows nothing about the
    /// backend underneath it: `tower_http`'s `DecompressionLayer`, for
    /// instance, inserts `Accept-Encoding` and decodes the response body —
    /// correct over a transport that does neither, and corrupting over one
    /// that already decompressed, which the browser's `fetch` always does
    /// and cannot be told not to. A layer that can be applied wrongly needs
    /// a guard that reads this before wrapping.
    pub fn capabilities(&self) -> &Capabilities
    where
        T: Transport,
    {
        self.inner.capabilities()
    }

    /// The transport itself, for the code paths that still want it
    /// directly.
    pub fn get_ref(&self) -> &T {
        &self.inner
    }
}

impl<T> tower_service::Service<http::Request<RequestBody>> for TransportService<T>
where
    T: hclient_core::unversioned::SendTransport + Send + Sync + 'static, // send-bound-exception: amendment-C16
    // The ONLY declared bound in this crate, and not its choice:
    // `Transport::to_error`'s own where-clause requires it, because
    // `Error::new` stores its source as `Arc<dyn Error + Send + Sync>` —
    // erasure into a `dyn Trait` never lets auto-traits through, so this
    // has to be written or the backend's classification cannot be carried
    // at all.
    //
    // Three neighbours were written here first, out of habit from ordinary
    // tower code, and all three were unnecessary: `T: Send`, `T: Sync` and
    // `T::Body: Send`. The boxed future is not `Send` (module doc), so
    // nothing downstream needed any of them. `no-declared-send` caught all
    // three — which is what it is for, and worth recording, because the
    // habit that produced them will produce them again.
    T::Error: Send + Sync + 'static, // send-bound-exception: amendment-C1
{
    type Response = http::Response<T::Body>;
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Error>> + Send>>; // send-bound-exception: amendment-C16

    /// Always ready.
    ///
    /// `Transport` has no notion of backpressure — no permit to acquire, and
    /// nothing to wait for — so there is nothing to be not-ready about, and
    /// reporting `Pending` here would invent a signal the layer below cannot
    /// produce. `hclient-native` has had a connection pool since v0.2 W2, and
    /// it does not change this: it never makes a caller wait for a
    /// connection — a checkout that finds nothing dials — so there is still
    /// no state in which this service could honestly say "not yet".
    /// A tower user accustomed to `poll_ready` gating a bounded resource
    /// should read this as "this service imposes no limit", not as "the
    /// limit is always satisfied".
    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: http::Request<RequestBody>) -> Self::Future {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            // `to_error`, not a fresh wrap. The backend's classification is
            // the point of that hook: dropping it here discards a
            // backend's whole taxonomy one layer up, leaving every `is_*`
            // predicate answering `false`.
            match hclient_core::unversioned::SendTransport::execute_send(&*inner, req).await {
                Ok(resp) => Ok(resp),
                Err(e) => Err(inner.to_error(e)),
            }
        })
    }
}

/// A [`tower_service::Service`] wearing the seam's clothes — the return
/// journey, so a tower stack can sit *underneath* `hclient::Client`
/// rather than replacing it.
///
/// Without this, a `tower-http` layer could only be applied on top of the
/// transport, which meant giving up everything `Client` does: the redirect
/// stage, `base_url` resolution, timeout merging, the capability check. The
/// two adapters together close the loop:
///
/// ```text
/// Native -> TransportService -> [tower layers] -> ServiceTransport -> Client
/// ```
///
/// **The client's type does not change shape**, and it is now the strongest
/// version of that claim: `Client` names no transport type at all, so a
/// tower stack behind it is not even a different type parameter. Nothing in
/// the facade moves. When this line was written `Client<T>` was generic and
/// the claim was *this is a different `T`* — erasure made it *there is no
/// `T`*.
///
/// # Readiness, and what cloning costs
///
/// `Service::call` needs `&mut self` and `Transport::execute` has `&self`,
/// so each call clones the service and drives `poll_ready` on the clone —
/// the same thing `tower::ServiceExt::oneshot` does. Readiness is therefore
/// not cached between calls.
///
/// For the standard layers this is correct rather than merely acceptable:
/// their backpressure state is shared through an `Arc` (a concurrency
/// limit's semaphore, a rate limiter's clock), so a clone observes the same
/// limit. A layer that keeps its budget in a plain field, unshared, would
/// hand every clone a fresh budget and enforce nothing. That is a property
/// of such a layer, not of this adapter, but it is the failure mode to look
/// for if a limit stops limiting.
#[derive(Debug, Clone)]
pub struct ServiceTransport<S> {
    inner: S,
    capabilities: Capabilities,
}

impl<S> ServiceTransport<S> {
    /// Wrap a service, declaring what the stack underneath it can do.
    ///
    /// **The capabilities are an argument because a `Service` has none, and
    /// this adapter must not invent them.** Take them from the transport at
    /// the bottom of the stack —
    /// `TransportService::capabilities()` — and adjust only for a layer
    /// that genuinely changes what the stack can do. Passing
    /// `Capabilities::default()` to avoid thinking is worse than wrong: it
    /// tells `Client::build` to reject configurations the stack supports
    /// perfectly well.
    ///
    /// A layer that changes behaviour and leaves these untouched produces
    /// exactly the defect this project has caught in four backends — a
    /// capability describing something other than what the code does.
    ///
    /// **`cancel_on_drop` is the field a tower stack is most likely to
    /// invalidate, and the easiest to carry over by accident.**
    /// `Transport::execute` requires that dropping its future stop the
    /// exchange, and this adapter passes the drop straight through — but a
    /// layer that hands the request to a worker task does not.
    /// `tower::buffer::Buffer` is the plain example: it spawns, the request
    /// is already in its channel, and dropping the future the adapter
    /// returns leaves that request to be sent and answered by somebody
    /// else. A stack with such a layer must report
    /// `CancelSupport::None` here even when the transport underneath
    /// reports `Supported` — that is what the "adjust for a layer that
    /// changes what the stack can do" sentence above means in the one case
    /// where the adjustment is downward.
    pub fn new(inner: S, capabilities: Capabilities) -> Self {
        Self {
            inner,
            capabilities,
        }
    }

    /// The wrapped service.
    pub fn get_ref(&self) -> &S {
        &self.inner
    }
}

impl<S, B, E> Transport for ServiceTransport<S>
where
    S: tower_service::Service<http::Request<RequestBody>, Response = http::Response<B>, Error = E>
        + Clone,
    B: http_body::Body<Data = bytes::Bytes>,
    E: StdError + 'static,
{
    type Body = B;
    type Error = E;

    fn execute(
        &self,
        req: http::Request<RequestBody>,
    ) -> impl Future<Output = Result<http::Response<B>, E>> {
        let mut svc = self.inner.clone();
        async move {
            // `poll_ready` to completion BEFORE `call`, which is tower's
            // contract and not a formality: a service is entitled to panic
            // if called unready, and the layers that matter here —
            // concurrency limits, rate limits — reserve their permit in
            // `poll_ready` and would otherwise hand out work they have no
            // budget for.
            poll_fn(|cx| svc.poll_ready(cx)).await?;
            svc.call(req).await
        }
    }

    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }
}

/// The `Send` half of the seam, forwarded from the tower service beneath.
///
/// The bounds are the service's own: a `tower::Service` whose `Future` and
/// `Response` cross a thread makes a `Transport` whose exchange does. They
/// are ordinary trait bounds rather than anything exotic, which is the
/// difference between this and naming an RPITIT — a consumer returning
/// `impl Transport` can add `+ SendTransport` to say so, and that is a
/// thing the language has always been able to write.
/// The three auto-trait bounds this crate asks of a tower service,
/// behind one short name so the marker sits on a line `cargo fmt` has no
/// reason to reflow — the rule amendment C12 records.
trait SendSyncStatic: Send + Sync + 'static {} // send-bound-exception: amendment-C16
impl<T: Send + Sync + 'static> SendSyncStatic for T {} // send-bound-exception: amendment-C16

impl<S, B, E> hclient_core::unversioned::SendTransport for ServiceTransport<S>
where
    S: tower_service::Service<http::Request<RequestBody>, Response = http::Response<B>, Error = E>
        + Clone
        + SendSyncStatic,
    S::Future: Send + 'static, // send-bound-exception: amendment-C16
    B: http_body::Body<Data = bytes::Bytes> + Send + 'static, // send-bound-exception: amendment-C16
    E: StdError + Send + 'static, // send-bound-exception: amendment-C16
{
    fn execute_send(
        &self,
        req: http::Request<RequestBody>,
    ) -> Pin<Box<dyn Future<Output = Result<http::Response<B>, E>> + Send + '_>> // send-bound-exception: amendment-C16
    {
        Box::pin(<Self as Transport>::execute(self, req))
    }
}
