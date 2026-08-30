//! OpenTelemetry for `hclient`: a span per request, `traceparent` and
//! `baggage` on the wire, and the result reaching whatever collector the
//! application already runs.
//!
//! ```
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # #[cfg(feature = "otel")] {
//! # let transport = hclient_mock::MockTransport::new(); // any `SendTransport`
//! let client = hclient::Client::builder(hclient_otel::Instrumented::otel(transport)).build()?;
//! # let _ = client;
//! # }
//! # Ok(())
//! # }
//! ```
//!
//! # Why a transport decorator and not a hook
//!
//! The obvious home for this is the `Hooks` seam, and it cannot do the job
//! for a reason that is structural rather than a missing feature:
//! `fn on(&self, event: &Event<'_>)` takes an immutable event, `&self` and
//! returns nothing, so **nothing reachable from a hook can put a header
//! into an outgoing request.** That is what the seam is for — a backend
//! announces what happened without a hook being able to change what
//! happens — and widening it would make every backend's emission site a
//! place where a caller's code rewrites a request mid-flight.
//!
//! Two smaller facts point the same way, both measured in this tree.
//! **There is no request-start event**: `Event`'s variants are the life of
//! a *connection*, the arrival of a head, and octets moving, and a span
//! needs a beginning. And **`Hooks` is not universal**: `fn hooks` is
//! declared on four backends of the six that could carry it, so even a
//! mutating hook would have reached two thirds of them.
//!
//! `Transport::execute` gives both halves away already. The request
//! arrives **by value**, so a decorator may edit its headers; `Self::Body`
//! is an associated type, so a decorator may wrap the response body — and
//! wrapping the body is what makes the duration right rather than the time
//! to first byte. `docs/otel-design.md` has the whole argument.
//!
//! `hclient::Client` names no type parameters, so **nothing leaks
//! downward**: `Client::builder(Instrumented::otel(t))` is one line at the
//! call site and every signature below it is unchanged. That is the
//! erasure paying for itself a second time, after `hc --backend`.
//!
//! # This crate does not own a pipeline
//!
//! Spans go to `opentelemetry::global`'s tracer provider, or to a
//! `tracing` subscriber. The SDK, the sampler, the propagator and the OTLP
//! exporter are the application's to configure — a library that decides
//! where a process's telemetry is shipped has drawn the boundary in the
//! wrong place. A process that installs no propagator gets no
//! `traceparent`, which is correct and is why the tests here install one
//! exactly as an application would.
//!
//! **And a bootstrap loop that has to be named.** If the OTLP exporter
//! makes its own requests through an instrumented `Client`, exporting
//! produces spans which produce exports. The rule is already written in
//! this workspace for DNS — *a resolver's client is not the user's
//! client* — and it applies verbatim: **the exporter's client must be a
//! plain `Client`, built on the bare transport.** It is said at
//! [`Instrumented::otel`] as well as here, because the constructor is
//! where somebody is about to get it wrong.
//!
//! # What it does not set, and why
//!
//! `network.peer.address` and `network.peer.port` are `Recommended` and
//! are **not set**. They live in the `Connected` hook event, and a
//! decorator would have to be the hook as well to see them — which is a
//! capability that varies by backend, since `fn hooks` exists on four of
//! six. This workspace's rule is that an attribute whose value would be a
//! guess is omitted. Both `Connected` and `Head` carry a
//! `hclient_core::unversioned::RequestId` now, so a caller who installs a
//! hook of their own can join it to a span on a key; the crate does not
//! decide that for them.
//!
//! Metrics are a separate surface and are not here — the same data, and
//! the duration they want is the one this crate already fixes: to the end
//! of the body.
#![forbid(unsafe_code)]

pub mod attrs;
mod body;
pub mod context;
mod span;

pub use body::SpanBody;
#[cfg(feature = "otel")]
pub use context::OtelContext;
pub use context::PropagateWhen;

use hclient_core::unversioned::{BoxSendExchange, SendTransport, Transport};
use hclient_core::{Capabilities, Error, RequestBody};
use span::{Choice, Recorder};
use std::future::Future;

/// A [`Transport`] that opens a span for every request it forwards.
///
/// # The front is chosen here, not by a feature
///
/// [`Instrumented::otel`] records an OpenTelemetry span and injects;
/// [`Instrumented::tracing`] records a `tracing` span and does not.
/// Enabling both features adds a constructor and changes nothing an
/// existing call does — which is the point, because Cargo unifies
/// features across a graph and a feature that decided *what a built
/// decorator does* would make a neighbour's build a floor on this one's
/// behaviour. Two spans per request is worse than either.
///
/// # The error type is `hclient_core::Error`, and nothing is lost
///
/// `Instrumented` classifies through `T::to_error` before it records,
/// because `error.type` must be a low-cardinality value and `ErrorKind` is
/// the only such value there is — a `Display` rendering of a DNS failure
/// carries a hostname. Having classified it, handing back the wrapper
/// rather than the backend's own error costs nothing: `Client` calls
/// `to_error` next, and its default sees an error that is already `Error`
/// and passes it through unwrapped. What it does require is
/// `T::Error: Send + Sync`, which is `to_error`'s own where-clause and
/// which every transport that can back a `Client` satisfies already.
pub struct Instrumented<T> {
    inner: T,
    choice: Choice,
    propagate: context::Filter,
}

impl<T> std::fmt::Debug for Instrumented<T>
where
    T: std::fmt::Debug,
{
    /// Hand-written because the propagation filter is a boxed closure and
    /// has nothing to print. Saying whether one is installed is the fact a
    /// reader of a `Debug` line wants — *is this hop going to carry the
    /// context* — where the closure's identity is not.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Instrumented")
            .field("inner", &self.inner)
            .field("front", &self.choice)
            .field("propagate_when", &self.propagate.is_some())
            .finish()
    }
}

#[cfg(feature = "otel")]
impl<T> Instrumented<T> {
    /// The instrumentation scope this crate reports itself under.
    ///
    /// The crate name, which is what a scope names: *who produced this
    /// span*, not *what it is about*.
    pub const SCOPE: &'static str = "hclient-otel";

    /// Record an OpenTelemetry span through the global tracer provider,
    /// and inject `traceparent` and `baggage` with the global propagator.
    ///
    /// **Not for the exporter's own client.** If the OTLP exporter makes
    /// its requests through a `Client` built on this, exporting produces
    /// spans which produce exports. Build the exporter's client on the
    /// bare transport — the same rule this workspace already writes for
    /// DNS, where *a resolver's client is not the user's client*.
    ///
    /// A **provider**, not a `Tracer` handed in, and that is a deliberate
    /// narrowing of `docs/otel-design.md` §7's *"or to a `Tracer` handed
    /// in"*. A tracer taken by value is a type parameter, and a type
    /// parameter on `Instrumented` is one every consumer that names the
    /// type has to carry — which is exactly the ceremony erasing `Client`
    /// removed. Handing in the *scope* covers the case that motivated it
    /// (see [`with_scope`](Self::with_scope)); handing in a whole tracer
    /// is the application setting a global provider, which it is doing
    /// anyway.
    #[must_use]
    pub fn otel(inner: T) -> Self {
        Self::with_scope(inner, Self::SCOPE)
    }

    /// [`otel`](Self::otel) under a scope name of the caller's choosing,
    /// for a library that wants its own requests attributable to itself
    /// rather than to this crate.
    #[must_use]
    pub fn with_scope(inner: T, scope: impl Into<std::borrow::Cow<'static, str>>) -> Self {
        Self {
            inner,
            choice: Choice::Otel(opentelemetry::global::tracer(scope)),
            propagate: None,
        }
    }

    /// Restrict which hops are given the trace context.
    ///
    /// **Read `context::PropagateWhen`'s doc before setting one**, because
    /// what this is for is a hazard rather than a preference: baggage is
    /// caller-invented key/value pairs, tenant identifiers routinely end
    /// up in it, and a decorator sees each redirect hop as a separate
    /// `execute` with no memory of where the chain started. `Client` can
    /// strip `Authorization` across an origin and this cannot, so a
    /// redirect to a third party carries the context there unless this
    /// says otherwise. The default is to inject everywhere, as every SDK
    /// does.
    ///
    /// Only on the `otel` front, because it is the only one that injects
    /// — a setter that existed on the other would be the *silently
    /// ignored setting* defect this workspace has closed four times.
    #[must_use]
    pub fn propagate_when<F>(mut self, allow: F) -> Self
    where
        F: Fn(&http::Uri) -> bool + Send + Sync + 'static, // send-bound-exception: amendment-C12
    {
        self.propagate = Some(std::sync::Arc::new(allow));
        self
    }
}

#[cfg(feature = "tracing")]
impl<T> Instrumented<T> {
    /// Record a `tracing` span, which is what most applications already
    /// have — and with `tracing-opentelemetry` installed it becomes an
    /// OpenTelemetry span for nothing.
    ///
    /// **It emits and does not inject**, and that is structural rather
    /// than unfinished: a `tracing` span's identity is a
    /// `tracing::span::Id` handed out by whatever subscriber is
    /// installed, meaningful in one process and nowhere else, so there is
    /// no trace-id for a propagator to write. [`crate::context`]'s module
    /// doc has the whole of it, including the measurement showing that
    /// injecting `Context::current()` here would name the **caller's**
    /// span rather than this one — a server span that comes out a sibling
    /// of the client span instead of its child. Use [`otel`](Self::otel)
    /// if the requests have to be joinable at the server.
    ///
    /// The span's name is the method, `otel.kind` is `client` and
    /// `otel.status_code` is `ERROR` where §5a says the exchange failed —
    /// which is `tracing-opentelemetry`'s own vocabulary, and the only
    /// way a `tracing` span can say either of those things.
    #[must_use]
    pub fn tracing(inner: T) -> Self {
        Self {
            inner,
            choice: Choice::Tracing,
            propagate: None,
        }
    }
}

impl<T> Instrumented<T> {
    /// The transport underneath.
    pub fn get_ref(&self) -> &T {
        &self.inner
    }

    /// Open the span and, where the front has ids to put on the wire, put
    /// them there.
    ///
    /// Sequenced rather than inlined at both call sites because
    /// [`Transport::execute`] and [`SendTransport::execute_send`] must
    /// have **separate** async bodies — the first awaits `T::execute`,
    /// whose RPITIT future has no name, and the second awaits
    /// `T::execute_send`, whose `Send` does — and only the synchronous
    /// halves can be shared. Doing the same work twice is how the
    /// `resend_count` mapping would come to differ between them.
    fn begin(&self, mut req: http::Request<RequestBody>) -> (Recorder, http::Request<RequestBody>) {
        let recorder = {
            // Scoped: `attrs::Request` borrows the request, and the
            // injection below needs it mutably.
            let a = attrs::Request::of(&req);
            Recorder::open(&self.choice, &a, req.extensions())
        };
        self.inject_into(&recorder, &mut req);
        (recorder, req)
    }

    /// Put the context on the wire, where there is one and where this hop
    /// is allowed it.
    ///
    /// A pair of `#[cfg]`-ed methods rather than a `#[cfg]`-ed block
    /// inside [`begin`](Self::begin), because the block form leaves `mut
    /// req` unused in a build without the `otel` feature — and silencing
    /// that with an `allow` is how the day comes when the `mut` is
    /// genuinely unnecessary and nothing says so.
    #[cfg(feature = "otel")]
    fn inject_into(&self, recorder: &Recorder, req: &mut http::Request<RequestBody>) {
        if let Some(cx) = recorder.wire_context()
            && context::allowed(&self.propagate, req.uri())
        {
            let cx = cx.clone();
            context::inject(&cx, req.headers_mut());
        }
    }

    /// The `tracing` front has no trace-id to write — `context`'s module
    /// doc says why that is structural rather than unfinished.
    #[cfg(not(feature = "otel"))]
    fn inject_into(&self, _recorder: &Recorder, _req: &mut http::Request<RequestBody>) {}

    /// Record the outcome, and hand the span to the body where there is
    /// one.
    fn finish<B>(
        &self,
        mut recorder: Recorder,
        outcome: Result<http::Response<B>, T::Error>,
    ) -> Result<http::Response<SpanBody<B>>, Error>
    where
        T: Transport,
        T::Error: Send + Sync, // send-bound-exception: amendment-C1
    {
        match outcome {
            Ok(res) => {
                let head = attrs::Head::of(&res, self.inner.capabilities().version_reported);
                recorder.head(&head);
                // The span is NOT ended here. §4: it ends at the end of
                // the body, or when the body is dropped.
                Ok(res.map(|b| SpanBody::new(b, recorder)))
            }
            Err(e) => {
                let err = self.inner.to_error(e);
                recorder.failed(attrs::error_type(err.kind()));
                recorder.end();
                Err(err)
            }
        }
    }
}

impl<T> Transport for Instrumented<T>
where
    T: Transport,
    T::Error: Send + Sync, // send-bound-exception: amendment-C1
    // `SpanBody` projects with `Pin::new(&mut inner)` rather than a
    // projection macro or an `unsafe` this workspace forbids, so the
    // wrapped body has to be `Unpin`. It is the same bound
    // `hclient::Limited` and the rest of the `ClientBody` chain already
    // carry, which is what makes it free: a body that is not `Unpin`
    // could not reach `Client` through any of them either.
    T::Body: Unpin,
{
    type Body = SpanBody<T::Body>;
    /// See the type's doc: the classification is made here, and handing
    /// back the already-classified `Error` is what keeps `Client` from
    /// having to make it a second time.
    type Error = Error;

    fn execute(
        &self,
        req: http::Request<RequestBody>,
    ) -> impl Future<Output = Result<http::Response<Self::Body>, Error>> {
        let (recorder, req) = self.begin(req);
        async move {
            let outcome = self.inner.execute(req).await;
            self.finish(recorder, outcome)
        }
    }

    fn capabilities(&self) -> &Capabilities {
        // Verbatim. Wrapping a body changes nothing a `Capabilities`
        // describes — not the framing, not the redirect owner, not
        // cancellation, which this decorator passes through by holding
        // nothing that outlives a dropped future.
        self.inner.capabilities()
    }
}

impl<T> SendTransport for Instrumented<T>
where
    // `Sync` because the boxed future holds `&self` across the await, so
    // `&Instrumented<T>` has to cross a thread with it. It is a bound on
    // this impl and not on the seam, which is the whole of C16's shape.
    T: SendTransport + Sync, // send-bound-exception: amendment-C16
    T::Error: Send + Sync,   // send-bound-exception: amendment-C1
    T::Body: Send + Unpin,   // send-bound-exception: amendment-C16
{
    /// The one line every backend writes, and for the reason every backend
    /// writes it: at a concrete type `Send` is **inferred**, and proof is
    /// only ever owed by generic code. What is generic here is `T`, and
    /// the bounds above are the whole of what that costs — ordinary trait
    /// bounds, on the impl rather than on the seam.
    fn execute_send(
        &self,
        req: http::Request<RequestBody>,
    ) -> BoxSendExchange<'_, Self::Body, Self::Error> {
        let (recorder, req) = self.begin(req);
        Box::pin(async move {
            let outcome = self.inner.execute_send(req).await;
            self.finish(recorder, outcome)
        })
    }
}
