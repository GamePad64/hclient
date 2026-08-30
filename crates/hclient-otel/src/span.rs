//! The two fronts, behind one recorder, and the rule that ends a span
//! exactly once.
//!
//! `docs/otel-design.md` §9 lists four files and this is a fifth. The
//! reason it exists is that both fronts owe the same five operations —
//! open, record the head, record an error, record a body error, end — and
//! writing them twice in `lib.rs` would be two places for the *end exactly
//! once* rule to be got wrong. `attrs.rs` decides what an attribute is;
//! this decides who is told.

use crate::attrs;

/// A span in flight, whichever front made it.
///
/// **Ending is idempotent and `Drop` is the backstop, not the path.**
/// §4's rule is that the span closes on whichever comes first, the end of
/// the body or `Drop` — so every ending path goes through [`Self::end`],
/// which takes the front out of the option, and `Drop` finds nothing left
/// where the body ended properly. A caller who abandons a body must not
/// leave a span open, and a caller who reads one to the end must not have
/// its duration extended to their own lifetime.
#[derive(Debug)]
pub(crate) struct Recorder {
    front: Option<Front>,
}

#[derive(Debug)]
enum Front {
    #[cfg(feature = "tracing")]
    Tracing(tracing::Span),
    #[cfg(feature = "otel")]
    Otel(opentelemetry::Context),
    /// A build with neither feature on.
    ///
    /// **It exists so that the matches below stay exhaustive without a
    /// wildcard**, which is the same reason `hclient-tls`'s `NoTls` is a
    /// type rather than an absence. Two variants cfg'd away leave an
    /// uninhabited enum, and stable Rust will not accept `match
    /// &self.front { None => {} }` as complete for one — a reference to
    /// an uninhabited type is still a pattern the exhaustiveness checker
    /// insists on. A wildcard would be the alternative, and a wildcard is
    /// where a third front would go to be silently ignored.
    ///
    /// Nothing can construct it: `Instrumented` has no constructor
    /// without a feature, so a build that reaches this variant does not
    /// exist.
    #[cfg(not(any(feature = "tracing", feature = "otel")))]
    Nothing,
}

/// Which front a decorator was built with.
///
/// A field on `Instrumented` rather than a feature test, because the
/// constructor picks the front and a feature only decides which
/// constructors exist — see this crate's `Cargo.toml` for why that
/// distinction is load-bearing under Cargo's feature unification.
///
/// With neither feature on this enum has no variants, so `Instrumented`
/// has no constructor and cannot be built. That is deliberate: a
/// decorator with no front records nothing, and `compile_error!` here
/// would have taken `just features`' no-feature pass down with it.
/// Not `Clone`: `opentelemetry::global::BoxedTracer` is not, and a
/// decorator is built once and shared behind `Client`'s `Arc` rather than
/// copied.
#[derive(Debug)]
pub(crate) enum Choice {
    #[cfg(feature = "tracing")]
    Tracing,
    #[cfg(feature = "otel")]
    Otel(opentelemetry::global::BoxedTracer),
    /// [`Front::Nothing`]'s counterpart, for its reason. Nothing
    /// constructs it, which is the point rather than an oversight: with
    /// no front, `Instrumented` has no constructor either.
    #[cfg(not(any(feature = "tracing", feature = "otel")))]
    #[allow(dead_code, reason = "unconstructible by design — see above")]
    Nothing,
}

impl Recorder {
    /// Open the span, and hand back the context to inject where the front
    /// has one.
    ///
    /// The `otel` arm returns the **child's** context rather than the
    /// parent's, which is what makes the server's span a child of this
    /// client span rather than a sibling of it. The `tracing` arm returns
    /// nothing at all — `context.rs`'s module doc is why.
    pub(crate) fn open(
        choice: &Choice,
        req: &attrs::Request<'_>,
        extensions: &http::Extensions,
    ) -> Self {
        // Both are read only by a `#[cfg]`-ed arm, and with no front
        // compiled in there are no arms at all. One `let _` rather than a
        // `cfg_attr(expect(unused_variables))` on the signature: the
        // attribute form has to name a lint that is not fired in the
        // configurations that do use them, and an `expect` that does not
        // fire is itself a warning.
        let _ = (req, extensions);
        let front = match choice {
            #[cfg(feature = "tracing")]
            Choice::Tracing => Front::Tracing(tracing_front::open(req)),
            #[cfg(feature = "otel")]
            Choice::Otel(tracer) => Front::Otel(otel_front::open(
                tracer,
                req,
                crate::context::parent_of(extensions),
            )),
            #[cfg(not(any(feature = "tracing", feature = "otel")))]
            Choice::Nothing => Front::Nothing,
        };
        Self { front: Some(front) }
    }

    /// The context whose ids belong on the wire, where there is one.
    #[cfg(feature = "otel")]
    pub(crate) fn wire_context(&self) -> Option<&opentelemetry::Context> {
        match self.front.as_ref()? {
            Front::Otel(cx) => Some(cx),
            #[cfg(feature = "tracing")]
            Front::Tracing(_) => None,
        }
    }

    /// The response head arrived.
    pub(crate) fn head(&self, head: &attrs::Head) {
        let _ = head;
        match &self.front {
            None => {}
            #[cfg(feature = "tracing")]
            Some(Front::Tracing(span)) => tracing_front::head(span, head),
            #[cfg(feature = "otel")]
            Some(Front::Otel(cx)) => otel_front::head(cx, head),
            #[cfg(not(any(feature = "tracing", feature = "otel")))]
            Some(Front::Nothing) => {}
        }
    }

    /// The exchange failed, with or without a head having arrived.
    ///
    /// One method for both, because `error.type` and the `Error` status
    /// are one decision: a body that fails after a `200` is still an
    /// exchange that ended in an error, and a span saying `Unset` over a
    /// truncated body would be describing the head rather than the
    /// exchange.
    pub(crate) fn failed(&self, error_type: &'static str) {
        let _ = error_type;
        match &self.front {
            None => {}
            #[cfg(feature = "tracing")]
            Some(Front::Tracing(span)) => tracing_front::failed(span, error_type),
            #[cfg(feature = "otel")]
            Some(Front::Otel(cx)) => otel_front::failed(cx, error_type),
            #[cfg(not(any(feature = "tracing", feature = "otel")))]
            Some(Front::Nothing) => {}
        }
    }

    /// Close it. Idempotent, and every path ends here.
    pub(crate) fn end(&mut self) {
        match self.front.take() {
            None => {}
            #[cfg(feature = "tracing")]
            Some(Front::Tracing(span)) => drop(span),
            #[cfg(feature = "otel")]
            Some(Front::Otel(cx)) => {
                use opentelemetry::trace::TraceContextExt;
                cx.span().end();
            }
            #[cfg(not(any(feature = "tracing", feature = "otel")))]
            Some(Front::Nothing) => {}
        }
    }
}

impl Drop for Recorder {
    /// The backstop. A `Recorder` reaching here with its front still in
    /// place is a request whose body was dropped before it ended, which is
    /// an ordinary thing for a caller to do and must not leave a span
    /// open.
    ///
    /// **The mutation that empties this body survives the whole suite, and
    /// it is kept anyway — which is a claim that needs its measurement
    /// beside it.** Against the SDK the tests run on, both fronts close
    /// themselves: `opentelemetry_sdk::trace::Span` has an `impl Drop`
    /// that ends and exports, and dropping a `tracing::Span` decrements
    /// the registry's count and fires `on_close`. So no test here can
    /// distinguish the two.
    ///
    /// What decides it is whose promise it is.
    /// `opentelemetry::trace::Span` is a **trait with no `Drop`
    /// requirement**, and the API crate — checked, 0.32.0 — carries no
    /// `impl Drop` on any span type at all; the one that exists belongs to
    /// the SDK. A provider whose spans do not self-end is a conforming
    /// provider, and this crate hands its spans to
    /// `opentelemetry::global`, which is whatever the application
    /// installed. Leaving the close to the SDK's convenience would make
    /// the promise the SDK's rather than ours.
    ///
    /// The killable half of the same rule is one file over:
    /// `SpanBody::poll_frame` calling `end` when the body runs out is what
    /// `the_span_closes_at_the_end_of_the_body_and_not_at_the_head` pins,
    /// and removing it fails that test on both fronts.
    fn drop(&mut self) {
        self.end();
    }
}

#[cfg(feature = "tracing")]
mod tracing_front {
    use crate::attrs;
    use tracing::field::Empty;

    /// One span, ten literal names.
    ///
    /// **`tracing`'s span name is part of a `static` `Metadata`**, so it
    /// must be a constant expression — `attrs::span_name`'s
    /// `&'static str` is not one, measured. Ten expansions of one field
    /// list is what buys the span name §5a asks for; the alternative is a
    /// fixed name plus `otel.name`, which is the `tracing-opentelemetry`
    /// convention and leaves a plain `tracing` user reading a span called
    /// something other than the method.
    ///
    /// `otel.kind` and `otel.status_code` are that bridge's own field
    /// names and are set regardless, because they are the only way a
    /// `tracing` span can say `CLIENT` and `Error` at all. **Executed
    /// rather than read**: a scratch consumer outside this workspace
    /// running `tracing-opentelemetry` 0.33 over this front exports a
    /// span named `GET` with `kind = Client`, parented on the caller's
    /// own span and sharing its trace — so the bridge really does give
    /// OTel for nothing, and `otel.kind` is consumed rather than passed
    /// through as an attribute.
    ///
    /// **Every number is recorded as `i64`, and that is not cosmetic.**
    /// The same measurement found that `tracing-opentelemetry` maps a
    /// `u16` or a `u64` field to a **string** and only an `i64` to an
    /// integer — probed directly, four fields, at creation and after it.
    /// `server.port`, `http.response.status_code` and
    /// `http.request.resend_count` are `int` in the OTel registry, so
    /// recording them in their natural width would put quoted numbers in
    /// front of every collector that groups on them. A plain `tracing`
    /// subscriber cannot tell the two apart, so the widening costs
    /// nobody anything.
    macro_rules! open_span {
        ($name:literal) => {
            tracing::span!(
                target: "hclient",
                tracing::Level::INFO,
                $name,
                otel.kind = "client",
                otel.status_code = Empty,
                http.request.method = Empty,
                http.request.method_original = Empty,
                url.full = Empty,
                server.address = Empty,
                server.port = Empty,
                user_agent.original = Empty,
                http.request.resend_count = Empty,
                hclient.hop = Empty,
                hclient.resend = Empty,
                http.response.status_code = Empty,
                network.protocol.version = Empty,
                error.type = Empty,
            )
        };
    }

    pub(super) fn open(req: &attrs::Request<'_>) -> tracing::Span {
        let span = match req.method {
            "GET" => open_span!("GET"),
            "HEAD" => open_span!("HEAD"),
            "POST" => open_span!("POST"),
            "PUT" => open_span!("PUT"),
            "DELETE" => open_span!("DELETE"),
            "CONNECT" => open_span!("CONNECT"),
            "OPTIONS" => open_span!("OPTIONS"),
            "TRACE" => open_span!("TRACE"),
            "PATCH" => open_span!("PATCH"),
            _ => open_span!("_OTHER"),
        };
        span.record("http.request.method", req.method);
        span.record("url.full", req.url_full.as_str());
        if let Some(original) = req.method_original {
            span.record("http.request.method_original", original);
        }
        if let Some(address) = req.server_address {
            span.record("server.address", address);
        }
        if let Some(port) = req.server_port {
            span.record("server.port", i64::from(port));
        }
        if let Some(ua) = req.user_agent {
            span.record("user_agent.original", ua);
        }
        if let Some(n) = req.resend_count {
            span.record("http.request.resend_count", i64::from(n));
        }
        if let Some(a) = req.attempt {
            span.record("hclient.hop", i64::from(a.hop));
            span.record("hclient.resend", i64::from(a.resend));
        }
        span
    }

    pub(super) fn head(span: &tracing::Span, head: &attrs::Head) {
        span.record("http.response.status_code", i64::from(head.status.as_u16()));
        if let Some(v) = head.version {
            span.record("network.protocol.version", v);
        }
        if let Some(e) = &head.error_type {
            span.record("error.type", e.as_str());
            span.record("otel.status_code", "ERROR");
        }
    }

    pub(super) fn failed(span: &tracing::Span, error_type: &'static str) {
        span.record("error.type", error_type);
        span.record("otel.status_code", "ERROR");
    }
}

#[cfg(feature = "otel")]
mod otel_front {
    use crate::attrs;
    use opentelemetry::trace::{SpanBuilder, SpanKind, Status, TraceContextExt, Tracer};
    use opentelemetry::{Context, KeyValue, Value};

    pub(super) fn open(
        tracer: &opentelemetry::global::BoxedTracer,
        req: &attrs::Request<'_>,
        parent: Context,
    ) -> Context {
        let mut kv = Vec::with_capacity(9);
        kv.push(KeyValue::new("http.request.method", req.method));
        kv.push(KeyValue::new("url.full", Value::from(req.url_full.clone())));
        if let Some(original) = req.method_original {
            kv.push(KeyValue::new(
                "http.request.method_original",
                original.to_owned(),
            ));
        }
        if let Some(address) = req.server_address {
            kv.push(KeyValue::new("server.address", address.to_owned()));
        }
        if let Some(port) = req.server_port {
            kv.push(KeyValue::new("server.port", i64::from(port)));
        }
        if let Some(ua) = req.user_agent {
            kv.push(KeyValue::new("user_agent.original", ua.to_owned()));
        }
        if let Some(n) = req.resend_count {
            kv.push(KeyValue::new("http.request.resend_count", i64::from(n)));
        }
        if let Some(a) = req.attempt {
            kv.push(KeyValue::new("hclient.hop", i64::from(a.hop)));
            kv.push(KeyValue::new("hclient.resend", i64::from(a.resend)));
        }

        // The span's name is the normalised method and nothing else —
        // §5a, and `attrs::Request` keeps one field for the two because
        // they are one value.
        let builder = SpanBuilder::from_name(req.method)
            .with_kind(SpanKind::Client)
            .with_attributes(kv);
        let span = tracer.build_with_context(builder, &parent);
        parent.with_span(span)
    }

    pub(super) fn head(cx: &Context, head: &attrs::Head) {
        let span = cx.span();
        span.set_attribute(KeyValue::new(
            "http.response.status_code",
            i64::from(head.status.as_u16()),
        ));
        if let Some(v) = head.version {
            span.set_attribute(KeyValue::new("network.protocol.version", v));
        }
        if let Some(e) = &head.error_type {
            span.set_attribute(KeyValue::new("error.type", e.clone()));
            // `Status::error` takes a description and the specification
            // asks for none here: the status code is already an attribute,
            // and a description repeating it is a second place to be wrong
            // about one fact.
            span.set_status(Status::error(""));
        }
    }

    pub(super) fn failed(cx: &Context, error_type: &'static str) {
        let span = cx.span();
        span.set_attribute(KeyValue::new("error.type", error_type));
        span.set_status(Status::error(""));
    }
}
