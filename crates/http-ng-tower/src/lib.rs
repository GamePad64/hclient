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
//! **The boxed future is `!Send`, and today that cannot be fixed here.**
//! Erasing into `dyn Future` drops auto-traits (amendment C1), so `Send`
//! would have to be declared on the box — and declaring it means proving
//! the inner `execute` future is `Send`, which on stable Rust needs return
//! type notation (`T: Transport<execute(..): Send>`). That is still
//! unstable: rust-lang/rust#109417.
//!
//! What it costs: this service cannot be `tokio::spawn`ed or handed to
//! anything else that requires a `Send` future — `axum` handlers, most
//! multithreaded executors. What it does NOT cost: the `tower-http`
//! middleware stack itself, which bounds `S: Service` and never requires
//! `Send` of its own accord, so `DecompressionLayer` and its neighbours
//! compose here today.
//!
//! **The alternative was measured and rejected.** Requiring `+ Send` on
//! `Transport::execute` in the core would make this a one-line change, but
//! it is not a one-line change there: `http-ng-native::execute` captures
//! `&self`, so the bound propagates as `Sync` through the TLS, DNS and
//! runtime seams and every backend built on them, and it contradicts the
//! core's own rule that it declares no `Send` bounds at all — the rule that
//! exists because this property has broken twice through type erasure.
//! Trading a stable-Rust gap for a permanent seam constraint is the wrong
//! way round, particularly when the gap closes on its own.
//!
//! When #109417 lands, the fix is one bound in this file. Nothing in the
//! core or in any backend has to move, which is why the wait is the right
//! call rather than a resigned one.
#![forbid(unsafe_code)]

use http_ng_core::unversioned::Transport;
use http_ng_core::{Capabilities, Error, RequestBody};
use std::future::Future;
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
    T: Transport + 'static,
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
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Error>>>>;

    /// Always ready.
    ///
    /// `Transport` has no notion of backpressure — no permit to acquire, and
    /// nothing to wait for — so there is nothing to be not-ready about, and
    /// reporting `Pending` here would invent a signal the layer below cannot
    /// produce. `http-ng-native` has had a connection pool since v0.2 W2, and
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
            // the point of that hook: dropping it here would repeat branch
            // finding B2, where forty lines sorting `ErrorCode` variants
            // into `ErrorKind`s were discarded one layer up and every
            // `is_*` predicate answered `false`.
            match inner.execute(req).await {
                Ok(resp) => Ok(resp),
                Err(e) => Err(inner.to_error(e)),
            }
        })
    }
}

/// A [`tower_service::Service`] wearing the seam's clothes — the return
/// journey, so a tower stack can sit *underneath* [`http_ng::Client`]
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
/// **The client's type does not change shape** — `Client<T>` was always
/// generic over its transport, so this is a different `T`, not a different
/// `Client`. Nothing in the facade moves.
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
    /// `Capabilities::none()` to avoid thinking is worse than wrong: it
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
    E: std::error::Error + 'static,
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
            std::future::poll_fn(|cx| svc.poll_ready(cx)).await?;
            svc.call(req).await
        }
    }

    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }
}
