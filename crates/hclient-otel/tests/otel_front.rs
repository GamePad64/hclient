//! The `otel` front, against `opentelemetry_sdk`'s in-memory exporter.
//!
//! **No collector and no socket.** `docs/otel-design.md` §10 names the
//! instrument and it is the whole reason this crate is testable: neither
//! front needs a pipeline, so a `SimpleSpanProcessor` writing into an
//! `InMemorySpanExporter` gives every assertion in §5a, and the
//! `TraceContextPropagator` a test installs is the same one an
//! application installs.
//!
//! **The globals are per test because nextest is.** Each test runs in its
//! own process, so `global::set_tracer_provider` here is a fact about one
//! test rather than a race between them — which is the same property
//! `AGENTS.md` records as the reason mutation testing is this project's
//! primary review technique.
#![cfg(feature = "otel")]

use hclient_core::unversioned::Transport;
use hclient_mock::MockTransport;
use hclient_otel::{Instrumented, OtelContext};
use http_body_util::BodyExt;
use opentelemetry::trace::{SpanKind, Status, TraceContextExt, TracerProvider};
use opentelemetry::{Context, Value};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SimpleSpanProcessor};

/// Install a provider that exports on `end`, and a real W3C propagator.
///
/// `SimpleSpanProcessor` and not a batch one, deliberately: it exports at
/// the moment the span ends, which is what turns *when does the span
/// close* from a clock question into a **causal** one — the count of
/// exported spans, read at a chosen point in the exchange. Three timing-
/// based assertions in this workspace have turned out to be flakes and
/// one of them was hiding a real defect.
fn recording() -> InMemorySpanExporter {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_span_processor(SimpleSpanProcessor::new(exporter.clone()))
        .build();
    opentelemetry::global::set_tracer_provider(provider);
    opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());
    exporter
}

fn finished(exporter: &InMemorySpanExporter) -> Vec<opentelemetry_sdk::trace::SpanData> {
    exporter.get_finished_spans().expect("in-memory exporter")
}

fn attr<'a>(span: &'a opentelemetry_sdk::trace::SpanData, key: &str) -> Option<&'a Value> {
    span.attributes
        .iter()
        .find(|kv| kv.key.as_str() == key)
        .map(|kv| &kv.value)
}

fn string_attr(span: &opentelemetry_sdk::trace::SpanData, key: &str) -> Option<String> {
    attr(span, key).map(|v| v.as_str().into_owned())
}

/// Drive one exchange.
///
/// **`futures_executor::block_on` is the obvious driver and it panics
/// here**, which is worth the four lines it takes to say:
/// `SimpleSpanProcessor::on_end` blocks on the exporter's own future, and
/// `futures_executor` refuses a nested `block_on` on one thread —
/// measured, 14 of 14 tests failing with *cannot execute `LocalPool`
/// executor from within another executor*. A tokio current-thread runtime
/// keeps no such guard. **No reactor**: nothing in this file opens a
/// socket, and the mock's timer is nobody's.
fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a runtime needs nothing here but a queue")
        .block_on(f)
}

fn get(uri: &str) -> http::Request<hclient_core::RequestBody> {
    http::Request::builder()
        .uri(uri)
        .body(hclient_core::RequestBody::Empty)
        .expect("test request")
}

// ── the attribute set ───────────────────────────────────────────────────

#[test]
fn a_request_carries_the_required_attributes_and_the_span_is_named_for_the_method() {
    let exporter = recording();
    let mock = MockTransport::new();
    mock.push_response(http::Response::builder().status(200).body("ok").unwrap());
    let t = Instrumented::otel(mock);

    block_on(async {
        let res = t
            .execute(
                http::Request::builder()
                    .method(http::Method::POST)
                    .uri("https://api.test:8443/v1/things?page=2")
                    .header(http::header::USER_AGENT, "hclient/0.1")
                    .body(hclient_core::RequestBody::Empty)
                    .unwrap(),
            )
            .await
            .expect("the mock answers");
        res.into_body().collect().await.expect("body");
    });

    let spans = finished(&exporter);
    assert_eq!(spans.len(), 1);
    let s = &spans[0];
    assert_eq!(
        s.name, "POST",
        "the span name is the method and nothing else"
    );
    assert_eq!(s.span_kind, SpanKind::Client);
    assert_eq!(
        string_attr(s, "http.request.method").as_deref(),
        Some("POST")
    );
    assert_eq!(
        string_attr(s, "url.full").as_deref(),
        Some("https://api.test:8443/v1/things?page=2")
    );
    assert_eq!(
        string_attr(s, "server.address").as_deref(),
        Some("api.test")
    );
    assert_eq!(attr(s, "server.port"), Some(&Value::I64(8443)));
    assert_eq!(
        string_attr(s, "user_agent.original").as_deref(),
        Some("hclient/0.1")
    );
    assert_eq!(attr(s, "http.response.status_code"), Some(&Value::I64(200)));
    assert_eq!(s.status, Status::Unset, "a 200 is not an error");
    // Not set, and §5 says why: they live in a `Hooks` event a decorator
    // cannot read, and an attribute whose value would be a guess is
    // omitted.
    assert_eq!(attr(s, "network.peer.address"), None);
    assert_eq!(attr(s, "network.peer.port"), None);
    // Never set, because this client normalises no method it was given.
    assert_eq!(attr(s, "http.request.method_original"), None);
    // Required only when the protocol is not HTTP.
    assert_eq!(attr(s, "network.protocol.name"), None);
}

#[test]
fn url_full_is_redacted() {
    // §10's third mutation: with the raw URI here, a request to
    // `https://u:p@host/` puts a password in a span.
    let exporter = recording();
    let mock = MockTransport::new();
    mock.push_response(http::Response::builder().status(200).body("ok").unwrap());
    let t = Instrumented::otel(mock);

    block_on(async {
        let res = t
            .execute(get("https://alice:hunter2@api.test/x"))
            .await
            .expect("the mock answers");
        res.into_body().collect().await.expect("body");
    });

    let spans = finished(&exporter);
    let url = string_attr(&spans[0], "url.full").expect("url.full is Required");
    assert_eq!(url, "https://REDACTED:REDACTED@api.test/x");
    assert!(!url.contains("hunter2"), "the password reached a collector");
    assert!(!url.contains("alice"));
}

#[test]
fn a_four_xx_is_an_error_and_names_the_status_as_the_error_type() {
    let exporter = recording();
    let mock = MockTransport::new();
    mock.push_response(
        http::Response::builder()
            .status(404)
            .body("not found")
            .unwrap(),
    );
    let t = Instrumented::otel(mock);

    block_on(async {
        let res = t.execute(get("https://api.test/x")).await.expect("answers");
        res.into_body().collect().await.expect("body");
    });

    let s = &finished(&exporter)[0];
    // A client span, where a server span would leave 4xx unset — and
    // deliberately not `hclient`'s own `error_for_status`, which leaves
    // the decision to the caller because a 404 is a normal answer.
    assert!(matches!(s.status, Status::Error { .. }));
    // The half `docs/otel-design.md` §5a's table does not have: the
    // specification's second arm for `error.type`, without which the
    // commonest error a client sees is the one nothing can group by.
    assert_eq!(string_attr(s, "error.type").as_deref(), Some("404"));
}

#[test]
fn a_transport_failure_reports_the_error_kind_and_still_closes_the_span() {
    let exporter = recording();
    let mock = MockTransport::new();
    mock.push_transport_error(hclient_core::Error::new(
        hclient_core::ErrorKind::Timeout(hclient_core::Phase::Connect),
        std::io::Error::from(std::io::ErrorKind::TimedOut),
    ));
    let t = Instrumented::otel(mock);

    block_on(async {
        t.execute(get("https://api.test/x"))
            .await
            .expect_err("the mock fails");
    });

    let spans = finished(&exporter);
    assert_eq!(spans.len(), 1, "a failed exchange still ends its span");
    assert!(matches!(spans[0].status, Status::Error { .. }));
    assert_eq!(
        string_attr(&spans[0], "error.type").as_deref(),
        Some("Timeout.Connect"),
        "the phase is the fact a dashboard is built on"
    );
    assert_eq!(attr(&spans[0], "http.response.status_code"), None);
}

#[test]
fn network_protocol_version_is_set_exactly_where_the_capability_says_it_is_meaningful() {
    // The biconditional `Head::version` already established one seam
    // over, read from the capability rather than from the response: a
    // backend that neither selects the protocol nor learns it leaves
    // `http`'s builder default on the response, and an attribute set from
    // that reports a browser's h2 traffic as HTTP/1.1.
    for reported in [false, true] {
        let exporter = recording();
        let mut caps = hclient_core::Capabilities::default();
        caps.version_reported = reported;
        let mock = MockTransport::new().with_capabilities(caps);
        mock.push_response(http::Response::builder().status(200).body("ok").unwrap());
        let t = Instrumented::otel(mock);

        block_on(async {
            let res = t.execute(get("https://api.test/x")).await.expect("answers");
            res.into_body().collect().await.expect("body");
        });

        let s = &finished(&exporter)[0];
        assert_eq!(
            string_attr(s, "network.protocol.version").as_deref(),
            reported.then_some("1.1"),
            "version_reported = {reported}"
        );
    }
}

// ── when the span closes: the pair neither half covers ──────────────────

#[test]
fn the_span_closes_at_the_end_of_the_body_and_not_at_the_head() {
    // §10's first mutation. The assertion is **causal**, not a duration:
    // a `SimpleSpanProcessor` exports at `end`, so counting exported
    // spans at a chosen point in the exchange says exactly when the span
    // closed. Mutating `finish` to end the span in `execute` makes the
    // first assertion fail.
    //
    // And the body is deliberately still alive at the last assertion,
    // which is what makes this test fail for a `SpanBody` that closes
    // only on `Drop` — the half its neighbour below would otherwise
    // cover for.
    let exporter = recording();
    let mock = MockTransport::new();
    mock.push_response_frames(
        http::Response::builder()
            .status(200)
            .body(vec!["first", "second"])
            .unwrap(),
    );
    let t = Instrumented::otel(mock);

    block_on(async {
        let res = t.execute(get("https://api.test/x")).await.expect("answers");
        assert_eq!(
            finished(&exporter).len(),
            0,
            "the head arrived and the span must still be open"
        );

        let mut body = res.into_body();
        while let Some(frame) = body.frame().await {
            frame.expect("a frame");
        }
        assert_eq!(
            finished(&exporter).len(),
            1,
            "the body ended, so the span must have closed — and `body` is still alive here"
        );
        drop(body);
    });
}

#[test]
fn a_body_dropped_before_it_ends_still_closes_the_span() {
    // The other half. A caller who reads a head and walks away must not
    // leave a span open for the life of the process, and `Drop` is the
    // backstop rather than the path.
    //
    // **No mutation of this crate kills this test, and that is worth
    // knowing rather than hiding**: emptying `Recorder`'s `Drop` leaves it
    // green, because `opentelemetry_sdk::trace::Span` ends itself on drop.
    // The property is real and the mechanism is currently the SDK's — see
    // `Recorder`'s `Drop` for why the crate keeps its own anyway, and this
    // is the test that would catch a front, or a provider, that does not
    // self-close.
    let exporter = recording();
    let mock = MockTransport::new();
    mock.push_response_frames(
        http::Response::builder()
            .status(200)
            .body(vec!["first", "second"])
            .unwrap(),
    );
    let t = Instrumented::otel(mock);

    block_on(async {
        let res = t.execute(get("https://api.test/x")).await.expect("answers");
        let mut body = res.into_body();
        body.frame().await.expect("one frame").expect("ok");
        assert_eq!(finished(&exporter).len(), 0, "one frame read of two");
        drop(body);
        assert_eq!(finished(&exporter).len(), 1, "the drop closed it");
    });
}

#[test]
fn a_body_that_fails_mid_stream_marks_the_span() {
    let exporter = recording();
    let mock = MockTransport::new();
    mock.push_response_frames_then_error(
        http::Response::builder()
            .status(200)
            .body(vec!["half"])
            .unwrap(),
        hclient_core::Error::new(
            hclient_core::ErrorKind::Body,
            std::io::Error::from(std::io::ErrorKind::UnexpectedEof),
        ),
    );
    let t = Instrumented::otel(mock);

    block_on(async {
        let res = t.execute(get("https://api.test/x")).await.expect("answers");
        let mut body = res.into_body();
        body.frame().await.expect("one frame").expect("ok");
        let err = body
            .frame()
            .await
            .expect("a second poll")
            .expect_err("fails");
        assert_eq!(err.kind(), &hclient_core::ErrorKind::Body);
    });

    let s = &finished(&exporter)[0];
    // A 200 whose body did not arrive is still an exchange that ended in
    // an error; a span saying `Unset` here would be describing the head
    // rather than the exchange.
    assert!(matches!(s.status, Status::Error { .. }));
    assert_eq!(string_attr(s, "error.type").as_deref(), Some("Body"));
    assert_eq!(attr(s, "http.response.status_code"), Some(&Value::I64(200)));
}

// ── the context: where it comes from and where it goes ──────────────────

#[test]
fn the_traceparent_names_this_span_rather_than_its_parent() {
    let exporter = recording();
    let mock = MockTransport::new();
    mock.push_response(http::Response::builder().status(200).body("ok").unwrap());
    let t = Instrumented::otel(mock);

    block_on(async {
        let res = t.execute(get("https://api.test/x")).await.expect("answers");
        res.into_body().collect().await.expect("body");
    });

    let s = &finished(&exporter)[0];
    let sent = t.get_ref().requests();
    let traceparent = sent[0]
        .headers
        .get("traceparent")
        .expect("a propagator was installed, so a header must have been written")
        .to_str()
        .expect("ascii");

    // `00-<trace-id>-<span-id>-<flags>`: the ids are **this** span's, not
    // the parent's, which is what makes the server's span a child of the
    // client span rather than a sibling of it.
    let parts: Vec<&str> = traceparent.split('-').collect();
    assert_eq!(parts.len(), 4, "{traceparent}");
    assert_eq!(parts[1], format!("{:032x}", s.span_context.trace_id()));
    assert_eq!(parts[2], format!("{:016x}", s.span_context.span_id()));
}

#[test]
fn the_extension_beats_the_ambient_context() {
    // §10's fourth mutation: preferring the ambient context gives a
    // caller who crossed a task boundary the wrong parent, which no
    // assertion about *a* span being emitted would catch.
    let exporter = recording();
    let tracer = opentelemetry::global::tracer_provider().tracer("test");
    let ambient = {
        use opentelemetry::trace::Tracer;
        Context::current().with_span(tracer.start("ambient"))
    };
    let explicit = {
        use opentelemetry::trace::Tracer;
        Context::current().with_span(tracer.start("explicit"))
    };
    let explicit_trace = explicit.span().span_context().trace_id();
    let ambient_trace = ambient.span().span_context().trace_id();
    assert_ne!(
        explicit_trace, ambient_trace,
        "the fixture is only discriminating if the two differ"
    );

    let mock = MockTransport::new();
    mock.push_response(http::Response::builder().status(200).body("ok").unwrap());
    let t = Instrumented::otel(mock);

    let _guard = ambient.attach();
    let mut req = get("https://api.test/x");
    req.extensions_mut().insert(OtelContext(explicit.clone()));

    block_on(async {
        let res = t.execute(req).await.expect("answers");
        res.into_body().collect().await.expect("body");
    });

    let ours = finished(&exporter)
        .into_iter()
        .find(|s| s.name == "GET")
        .expect("our span");
    assert_eq!(ours.span_context.trace_id(), explicit_trace);
    assert_eq!(
        ours.parent_span_id,
        explicit.span().span_context().span_id()
    );
}

#[test]
fn propagate_when_keeps_the_context_off_a_hop_it_refuses() {
    // §6: a decorator sees each hop as a separate `execute` and has no
    // memory of the first origin, so this is what a caller has instead of
    // `Client`'s cross-origin stripping.
    let exporter = recording();
    let mock = MockTransport::new();
    mock.push_response(http::Response::builder().status(200).body("ok").unwrap());
    mock.push_response(http::Response::builder().status(200).body("ok").unwrap());
    let t = Instrumented::otel(mock).propagate_when(|u| u.host() == Some("ours.test"));

    block_on(async {
        for uri in ["https://ours.test/x", "https://third-party.test/x"] {
            let res = t.execute(get(uri)).await.expect("answers");
            res.into_body().collect().await.expect("body");
        }
    });

    let sent = t.get_ref().requests();
    assert!(sent[0].headers.contains_key("traceparent"));
    assert!(
        !sent[1].headers.contains_key("traceparent"),
        "the refused hop must carry no context — baggage is where tenant identifiers live"
    );
    // The span is still recorded for the refused hop: not propagating is
    // not the same as not observing.
    assert_eq!(finished(&exporter).len(), 2);
}

#[test]
fn a_process_with_no_propagator_installed_writes_no_header() {
    // The boundary §7 draws: the propagator is the application's. The
    // default global one is a no-op, and this is what says this crate
    // does not quietly install one of its own.
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_span_processor(SimpleSpanProcessor::new(exporter.clone()))
        .build();
    opentelemetry::global::set_tracer_provider(provider);
    // Deliberately no `set_text_map_propagator`.

    let mock = MockTransport::new();
    mock.push_response(http::Response::builder().status(200).body("ok").unwrap());
    let t = Instrumented::otel(mock);
    block_on(async {
        let res = t.execute(get("https://api.test/x")).await.expect("answers");
        res.into_body().collect().await.expect("body");
    });

    assert_eq!(finished(&exporter).len(), 1, "the span is still recorded");
    assert!(
        !t.get_ref().requests()[0]
            .headers
            .contains_key("traceparent")
    );
}

// ── through a real `Client`, where the redirects are real ───────────────

#[test]
fn resend_count_is_hop_plus_resend_over_a_real_redirect_chain() {
    // §10's second mutation, and §5's headline: reading `resend` alone is
    // the mapping the field names invite, and it reports nothing at all
    // for the third hop of a redirect chain — which is exactly the case
    // the attribute exists for. Driven through a real `hclient::Client`
    // so the hops are the client's own rather than simulated.
    let exporter = recording();
    let mock = MockTransport::new();
    for to in ["https://api.test/two", "https://api.test/three"] {
        mock.push_response(
            http::Response::builder()
                .status(302)
                .header(http::header::LOCATION, to)
                .body("")
                .unwrap(),
        );
    }
    mock.push_response(http::Response::builder().status(200).body("done").unwrap());

    let client = hclient::Client::builder(Instrumented::otel(mock))
        .build()
        .expect("a decorator changes no capability");

    // An ambient span, so the three hops have something to be grouped
    // under. Without one each hop is the root of its own trace, which is
    // correct and is §12's answer: OTel's model is one client span per
    // request, a redirect is a *resend* rather than a child, and grouping
    // the chain is a thing a caller may want and the convention does not
    // describe. The caller here wants it, and gets it for free by having
    // a span of their own.
    let operation = {
        use opentelemetry::trace::Tracer;
        Context::current().with_span(
            opentelemetry::global::tracer_provider()
                .tracer("test")
                .start("operation"),
        )
    };
    let operation_trace = operation.span().span_context().trace_id();
    let operation_span = operation.span().span_context().span_id();
    let guard = operation.attach();

    block_on(async {
        let body = client
            .get("https://api.test/one")
            .send()
            .await
            .expect("the chain completes")
            .collect()
            .await
            .expect("body");
        assert_eq!(body.status(), 200);
    });
    drop(guard);

    let mut spans: Vec<_> = finished(&exporter)
        .into_iter()
        .filter(|s| s.name == "GET")
        .collect();
    spans.sort_by_key(|s| s.start_time);
    assert_eq!(spans.len(), 3, "one span per hop — the chain is flat");

    assert_eq!(attr(&spans[0], "http.request.resend_count"), None);
    assert_eq!(
        attr(&spans[1], "http.request.resend_count"),
        Some(&Value::I64(1))
    );
    assert_eq!(
        attr(&spans[2], "http.request.resend_count"),
        Some(&Value::I64(2))
    );

    // The split the sum destroys, kept beside it under names that are
    // visibly not OTel's: *third send, first hop* and *first send, third
    // hop* are different failures.
    assert_eq!(attr(&spans[2], "hclient.hop"), Some(&Value::I64(2)));
    assert_eq!(attr(&spans[2], "hclient.resend"), Some(&Value::I64(0)));

    // **Flat, not nested**, and this is the assertion that says so: all
    // three are children of the caller's own span and none of the other
    // two. A chain where hop 2 parented hop 3 would satisfy "one trace"
    // just as well, which is why the parent is asserted rather than the
    // trace id alone.
    assert!(
        spans
            .iter()
            .all(|s| s.span_context.trace_id() == operation_trace)
    );
    assert!(spans.iter().all(|s| s.parent_span_id == operation_span));
}

#[test]
fn every_hop_of_the_chain_names_its_own_url() {
    let exporter = recording();
    let mock = MockTransport::new();
    mock.push_response(
        http::Response::builder()
            .status(302)
            .header(http::header::LOCATION, "https://api.test/two")
            .body("")
            .unwrap(),
    );
    mock.push_response(http::Response::builder().status(200).body("done").unwrap());

    let client = hclient::Client::builder(Instrumented::otel(mock))
        .build()
        .expect("built");
    block_on(async {
        client
            .get("https://api.test/one")
            .send()
            .await
            .expect("chain")
            .collect()
            .await
            .expect("body");
    });

    let mut spans = finished(&exporter);
    spans.sort_by_key(|s| s.start_time);
    assert_eq!(
        string_attr(&spans[0], "url.full").as_deref(),
        Some("https://api.test/one")
    );
    assert_eq!(
        string_attr(&spans[1], "url.full").as_deref(),
        Some("https://api.test/two")
    );
    // The first hop is a 302, which is not an error: reaching a redirect
    // means the policy is about to decide, and a span calling it a
    // failure would overrule that.
    assert_eq!(spans[0].status, Status::Unset);
}

#[test]
fn the_decorator_reports_the_transport_s_own_capabilities() {
    // Verbatim, and it has to be: `Client::build` refuses a configuration
    // the transport cannot honour, so a decorator inventing a
    // `Capabilities` would refuse settings the stack supports — the
    // defect `hclient-tower`'s `ServiceTransport` documents from the
    // other side.
    let mut caps = hclient_core::Capabilities::default();
    caps.full_duplex = true;
    caps.version_reported = true;
    let t = Instrumented::otel(MockTransport::new().with_capabilities(caps));
    assert!(t.capabilities().full_duplex);
    assert!(t.capabilities().version_reported);
    assert!(!t.capabilities().streaming_request_body);
}
