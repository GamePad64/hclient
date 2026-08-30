//! Where the trace context comes from, and where it is allowed to go.
//!
//! # Propagation belongs to the `otel` front, and that is structural
//!
//! `docs/otel-design.md` §7 offers two fronts and implies both do the
//! whole job. They cannot, and the half that separates them is this one.
//!
//! `traceparent` is a trace-id and a span-id in W3C's spelling. A
//! `tracing` span has neither: its identity is a `tracing::span::Id`
//! handed out by whatever subscriber is installed, meaningful inside one
//! process and nowhere else. So under the `tracing` front alone there is
//! no value for a propagator to write, and this crate emits no header.
//!
//! **The tempting repair is worse than the absence.** With the `otel`
//! feature also compiled in, the `tracing` front could inject
//! `Context::current()` — and under `tracing-opentelemetry`, which is the
//! whole reason anybody picks that front, `Context::current()` is *empty*:
//! that bridge keeps the OTel span in the tracing span's extensions and
//! never pushes it onto the `Context` stack. The propagator would then
//! write nothing at all, on a request that looked instrumented. A header
//! that is silently absent is this workspace's *capability that lies* one
//! layer down, and §6's own rule — *naming it as absent is better than
//! implying it exists* — is what settles it.
//!
//! What would close it is `tracing_opentelemetry::OpenTelemetrySpanExt`,
//! which can read the OTel context of a `tracing` span. Taking it would
//! mean this crate choosing the caller's bridge crate and its version for
//! them, and it is a third feature rather than a change to these two. Not
//! built; recorded here so the next reader does not re-derive it.

use std::sync::Arc;

/// Which URIs may be given the trace context.
///
/// **Baggage crosses an origin and this decorator cannot stop it**, which
/// is `docs/otel-design.md` §6 and is worth reading before setting one.
/// `Client` strips `Authorization` on a cross-origin redirect because it
/// has `Follow::strip_sensitive` and knows where the chain started; a
/// decorator sees each hop as a separate `execute` and has no memory of
/// the first origin. So a redirect to a third party carries the trace
/// context and the baggage there, and the allow-list is what a caller has
/// instead.
///
/// A named alias rather than the bound written at each use site, which is
/// amendment C12's own rule: `cargo fmt` reflows a long signature and
/// carries a trailing marker comment away with it, so the bound lives on
/// a line fmt has no reason to touch.
pub type PropagateWhen = dyn Fn(&http::Uri) -> bool + Send + Sync; // send-bound-exception: amendment-C12

/// The allow-list as the decorator holds it. `None` means *everywhere*,
/// which is what every SDK does and what §6 proposes.
pub(crate) type Filter = Option<Arc<PropagateWhen>>;

/// Whether this URI may be given the context.
///
/// Gated with its only caller: without the `otel` front nothing injects,
/// so a filter has nothing to filter and an ungated helper here would be
/// dead code in exactly the build that has no use for it.
#[cfg(feature = "otel")]
pub(crate) fn allowed(filter: &Filter, uri: &http::Uri) -> bool {
    filter.as_ref().is_none_or(|f| f(uri))
}

#[cfg(feature = "otel")]
mod otel {
    use opentelemetry::Context;
    use opentelemetry::propagation::Injector;

    /// A caller's own trace context, for one request.
    ///
    /// **The extension is read first and the ambient context is the
    /// fallback**, said once, here, because that is the rule and it is
    /// invisible from either call site. Ambient works and it works for a
    /// reason rather than by luck: this client does not spawn the request
    /// future, so `execute` is polled on the caller's own task and
    /// `Context::current()` is theirs. The extension is for the caller who
    /// has themselves crossed a task or a channel boundary, at which point
    /// the ambient context is somebody else's.
    ///
    /// An extension rather than a `Client` setting, for the reason
    /// `AllowEarlyData` and `ClientIdentity` are extensions: a trace
    /// context is a property of *a request*, not of the client that sends
    /// it. Reach it with `hclient::RequestBuilder::extension` — not an
    /// intra-doc link, because `hclient` is a dev-dependency here and a
    /// dev-dependency resolves in a doctest and not in a doc build.
    ///
    /// ```
    /// # use hclient_otel::OtelContext;
    /// # fn f(client: &hclient::Client, cx: opentelemetry::Context) {
    /// let req = client.get("https://example.test/").extension(OtelContext(cx));
    /// # let _ = req;
    /// # }
    /// ```
    #[derive(Clone, Debug)]
    pub struct OtelContext(pub Context);

    /// The context this request should hang off: the extension where the
    /// caller set one, the ambient context otherwise.
    pub(crate) fn parent_of(extensions: &http::Extensions) -> Context {
        match extensions.get::<OtelContext>() {
            Some(explicit) => explicit.0.clone(),
            None => Context::current(),
        }
    }

    /// `HeaderMap` as an [`Injector`].
    ///
    /// `opentelemetry-http` has one of these and is not worth taking for
    /// it — §8's measurement, and this workspace's rule for a dependency
    /// is whether a wrong answer would be *silent*. A header injector
    /// fails loudly: a malformed value cannot become a `HeaderValue` and
    /// the header is simply not there.
    struct HeaderInjector<'a>(&'a mut http::HeaderMap);

    impl Injector for HeaderInjector<'_> {
        fn set(&mut self, key: &str, value: String) {
            let Ok(name) = http::HeaderName::from_bytes(key.as_bytes()) else {
                return;
            };
            let Ok(value) = http::HeaderValue::from_str(&value) else {
                return;
            };
            self.0.insert(name, value);
        }
    }

    /// Write whatever the application's propagator writes.
    ///
    /// **The propagator is the application's and not this crate's**, which
    /// is §7's boundary: a library that decides a process's wire format
    /// for trace context has drawn it in the wrong place. The default
    /// global propagator is a no-op, so a process that installs none gets
    /// no headers — which is correct, and is why the tests here install
    /// `opentelemetry_sdk::propagation::TraceContextPropagator` exactly as
    /// an application would.
    ///
    /// One call covers `traceparent` and `baggage` both, because they live
    /// in the same `Context` and a composite propagator is one object.
    pub(crate) fn inject(cx: &Context, headers: &mut http::HeaderMap) {
        opentelemetry::global::get_text_map_propagator(|propagator| {
            propagator.inject_context(cx, &mut HeaderInjector(headers));
        });
    }
}

#[cfg(feature = "otel")]
pub use otel::OtelContext;
#[cfg(feature = "otel")]
pub(crate) use otel::{inject, parent_of};

#[cfg(all(test, feature = "otel"))]
mod tests {
    use super::*;

    #[test]
    fn no_filter_means_everywhere() {
        let uri: http::Uri = "https://anything.test/".parse().unwrap();
        assert!(allowed(&None, &uri));
    }

    #[test]
    fn a_filter_is_asked_about_the_uri_of_the_hop() {
        let filter: Filter = Some(Arc::new(|u: &http::Uri| u.host() == Some("ours.test")));
        assert!(allowed(&filter, &"https://ours.test/a".parse().unwrap()));
        assert!(!allowed(
            &filter,
            &"https://third-party.test/a".parse().unwrap()
        ));
    }
}
