//! The `tracing` front, against a `Layer` that records into a `Vec`.
//!
//! `docs/otel-design.md` §10's other instrument, and it needs no pipeline
//! either: thirty lines of `tracing_subscriber::Layer` give every
//! assertion §5a asks for, plus the one thing a duration test needs —
//! *when did this span close*, read as a fact rather than as a clock.
#![cfg(feature = "tracing")]

use hclient_core::unversioned::Transport;
use hclient_mock::MockTransport;
use hclient_otel::Instrumented;
use http_body_util::BodyExt;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;

// ── the instrument ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
struct Recorded {
    spans: Arc<Mutex<Vec<SpanRecord>>>,
}

#[derive(Debug, Clone)]
struct SpanRecord {
    name: &'static str,
    fields: BTreeMap<String, String>,
    /// Which `Visit` method each field arrived through.
    ///
    /// A second map rather than a tagged value, so that every other
    /// assertion in this file stays a plain string comparison and one
    /// test carries the whole of the typing question.
    kinds: BTreeMap<String, &'static str>,
    closed: bool,
}

impl Recorded {
    fn spans(&self) -> Vec<SpanRecord> {
        self.spans.lock().expect("not poisoned").clone()
    }

    /// The one span this crate opened, and an assertion that there is
    /// exactly one — a decorator emitting two per request is the failure
    /// the constructor-picks-the-front rule exists to prevent.
    fn only(&self) -> SpanRecord {
        let spans = self.spans();
        assert_eq!(spans.len(), 1, "exactly one span per request");
        spans.into_iter().next().expect("checked")
    }
}

impl SpanRecord {
    fn field(&self, key: &str) -> Option<&str> {
        self.fields.get(key).map(String::as_str)
    }

    fn kind_of(&self, key: &str) -> Option<&'static str> {
        self.kinds.get(key).copied()
    }
}

/// Where a span's index lives while it is open.
#[derive(Debug, Clone, Copy)]
struct Index(usize);

/// Every field as the string a subscriber would print.
///
/// `record_str` as well as `record_debug`, because `tracing` hands a
/// `&str` field through the first and a `u16` through the second, and a
/// `Debug` rendering of a string would carry its quotes into every
/// assertion below.
struct Fields<'a> {
    values: &'a mut BTreeMap<String, String>,
    kinds: &'a mut BTreeMap<String, &'static str>,
}

impl Fields<'_> {
    fn put(&mut self, field: &tracing::field::Field, kind: &'static str, value: String) {
        self.values.insert(field.name().to_owned(), value);
        self.kinds.insert(field.name().to_owned(), kind);
    }
}

impl tracing::field::Visit for Fields<'_> {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.put(field, "str", value.to_owned());
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.put(field, "i64", value.to_string());
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.put(field, "u64", value.to_string());
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.put(field, "debug", format!("{value:?}"));
    }
}

impl<S> tracing_subscriber::Layer<S> for Recorded
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::Id,
        ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut fields = BTreeMap::new();
        let mut kinds = BTreeMap::new();
        attrs.record(&mut Fields {
            values: &mut fields,
            kinds: &mut kinds,
        });
        let mut spans = self.spans.lock().expect("not poisoned");
        spans.push(SpanRecord {
            name: attrs.metadata().name(),
            fields,
            kinds,
            closed: false,
        });
        let index = Index(spans.len() - 1);
        drop(spans);
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(index);
        }
    }

    fn on_record(
        &self,
        id: &tracing::Id,
        values: &tracing::span::Record<'_>,
        ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let Some(span) = ctx.span(id) else { return };
        let Some(&Index(i)) = span.extensions().get::<Index>() else {
            return;
        };
        let mut spans = self.spans.lock().expect("not poisoned");
        let record = &mut spans[i];
        values.record(&mut Fields {
            values: &mut record.fields,
            kinds: &mut record.kinds,
        });
    }

    fn on_close(&self, id: tracing::Id, ctx: tracing_subscriber::layer::Context<'_, S>) {
        let Some(span) = ctx.span(&id) else { return };
        let Some(&Index(i)) = span.extensions().get::<Index>() else {
            return;
        };
        self.spans.lock().expect("not poisoned")[i].closed = true;
    }
}

/// Install the layer for this process.
///
/// Global rather than scoped, and that is nextest's doing rather than a
/// shortcut: each test runs in its own process, so a global default here
/// is a fact about one test.
fn recording() -> Recorded {
    let recorded = Recorded::default();
    let subscriber = tracing_subscriber::registry().with(recorded.clone());
    tracing::subscriber::set_global_default(subscriber).expect("one per process");
    recorded
}

/// See `tests/otel_front.rs` for why this is tokio and not
/// `futures_executor`. Nothing here opens a socket.
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

// ── what it records ─────────────────────────────────────────────────────

#[test]
fn the_span_is_named_for_the_method_and_carries_the_attribute_set() {
    let recorded = recording();
    let mock = MockTransport::new();
    mock.push_response(http::Response::builder().status(200).body("ok").unwrap());
    let t = Instrumented::tracing(mock);

    block_on(async {
        let res = t
            .execute(
                http::Request::builder()
                    .method(http::Method::DELETE)
                    .uri("https://api.test:8443/v1/thing")
                    .header(http::header::USER_AGENT, "hclient/0.1")
                    .body(hclient_core::RequestBody::Empty)
                    .unwrap(),
            )
            .await
            .expect("answers");
        res.into_body().collect().await.expect("body");
    });

    let s = recorded.only();
    // The name is the method, which costs ten macro expansions because a
    // `tracing` span's name lives in a `static Metadata` and must be a
    // constant expression. The alternative — one fixed name plus
    // `otel.name` — leaves a plain `tracing` user reading a span called
    // something other than the method.
    assert_eq!(s.name, "DELETE");
    // `tracing-opentelemetry`'s own vocabulary, and the only way a
    // `tracing` span can say `CLIENT` at all.
    assert_eq!(s.field("otel.kind"), Some("client"));
    assert_eq!(s.field("http.request.method"), Some("DELETE"));
    assert_eq!(s.field("url.full"), Some("https://api.test:8443/v1/thing"));
    assert_eq!(s.field("server.address"), Some("api.test"));
    assert_eq!(s.field("server.port"), Some("8443"));
    assert_eq!(s.field("user_agent.original"), Some("hclient/0.1"));
    assert_eq!(s.field("http.response.status_code"), Some("200"));
    // An unset field is not recorded at all rather than recorded as
    // `Empty`: a reader must be able to tell *we did not observe this*
    // from a value.
    assert_eq!(s.field("otel.status_code"), None);
    assert_eq!(s.field("error.type"), None);
    assert_eq!(s.field("http.request.method_original"), None);
    assert_eq!(s.field("network.protocol.version"), None);
}

#[test]
fn every_numeric_attribute_is_recorded_as_an_i64() {
    // **Not a style choice — measured against the bridge this front
    // exists for.** `tracing-opentelemetry` 0.33 maps a `u16` or a `u64`
    // field to a *string* and only an `i64` to an integer; probed
    // directly on four fields, at creation and after it, in a scratch
    // consumer outside this workspace. `server.port`,
    // `http.response.status_code` and `http.request.resend_count` are
    // `int` in the OTel registry, so recording them in their natural
    // width puts quoted numbers in front of every collector that groups
    // on them.
    //
    // A plain `tracing` subscriber cannot tell the two apart, which is
    // why this test reads the `Visit` method each field arrived through
    // rather than its rendering.
    let recorded = recording();
    let mock = MockTransport::new();
    mock.push_response(
        http::Response::builder()
            .status(302)
            .header(http::header::LOCATION, "https://api.test/two")
            .body("")
            .unwrap(),
    );
    mock.push_response(http::Response::builder().status(200).body("ok").unwrap());
    let client = hclient::Client::builder(Instrumented::tracing(mock))
        .build()
        .expect("built");
    block_on(async {
        client
            .get("https://api.test:8443/one")
            .send()
            .await
            .expect("chain")
            .collect()
            .await
            .expect("body");
    });

    // The second hop, because it is the only one carrying all five.
    let s = &recorded.spans()[1];
    for key in [
        "server.port",
        "http.response.status_code",
        "http.request.resend_count",
        "hclient.hop",
        "hclient.resend",
    ] {
        assert_eq!(s.kind_of(key), Some("i64"), "{key}");
    }
    // And the strings stay strings, so the widening did not reach them.
    assert_eq!(s.kind_of("url.full"), Some("str"));
    assert_eq!(s.kind_of("http.request.method"), Some("str"));
}

#[test]
fn an_unknown_method_becomes_other_and_the_original_is_kept() {
    let recorded = recording();
    let mock = MockTransport::new();
    mock.push_response(http::Response::builder().status(200).body("ok").unwrap());
    let t = Instrumented::tracing(mock);

    block_on(async {
        let res = t
            .execute(
                http::Request::builder()
                    .method(http::Method::from_bytes(b"PROPFIND").unwrap())
                    .uri("https://api.test/x")
                    .body(hclient_core::RequestBody::Empty)
                    .unwrap(),
            )
            .await
            .expect("answers");
        res.into_body().collect().await.expect("body");
    });

    let s = recorded.only();
    // The span name is bounded by construction: a caller who invents a
    // method per request cannot put one in it.
    assert_eq!(s.name, "_OTHER");
    assert_eq!(s.field("http.request.method"), Some("_OTHER"));
    assert_eq!(s.field("http.request.method_original"), Some("PROPFIND"));
}

#[test]
fn url_full_is_redacted() {
    let recorded = recording();
    let mock = MockTransport::new();
    mock.push_response(http::Response::builder().status(200).body("ok").unwrap());
    let t = Instrumented::tracing(mock);

    block_on(async {
        let res = t
            .execute(get("https://alice:hunter2@api.test/x"))
            .await
            .expect("answers");
        res.into_body().collect().await.expect("body");
    });

    let s = recorded.only();
    assert_eq!(
        s.field("url.full"),
        Some("https://REDACTED:REDACTED@api.test/x")
    );
}

#[test]
fn a_four_xx_is_an_error_on_a_client_span() {
    let recorded = recording();
    let mock = MockTransport::new();
    mock.push_response(http::Response::builder().status(503).body("no").unwrap());
    let t = Instrumented::tracing(mock);

    block_on(async {
        let res = t.execute(get("https://api.test/x")).await.expect("answers");
        res.into_body().collect().await.expect("body");
    });

    let s = recorded.only();
    assert_eq!(s.field("otel.status_code"), Some("ERROR"));
    assert_eq!(s.field("error.type"), Some("503"));
}

#[test]
fn it_writes_no_traceparent_and_that_is_the_honest_answer() {
    // Not an omission: a `tracing` span's identity is a
    // `tracing::span::Id` handed out by whatever subscriber is installed,
    // so there is no W3C trace-id for a propagator to write. The crate
    // says so at the constructor rather than shipping a header that is
    // silently absent — `src/context.rs`'s module doc has the whole of
    // it.
    let _recorded = recording();
    let mock = MockTransport::new();
    mock.push_response(http::Response::builder().status(200).body("ok").unwrap());
    let t = Instrumented::tracing(mock);

    block_on(async {
        let res = t.execute(get("https://api.test/x")).await.expect("answers");
        res.into_body().collect().await.expect("body");
    });

    let sent = t.get_ref().requests();
    assert!(!sent[0].headers.contains_key("traceparent"));
    assert!(!sent[0].headers.contains_key("baggage"));
}

// ── when the span closes: the same pair, in tracing's vocabulary ────────

#[test]
fn the_span_closes_at_the_end_of_the_body_and_not_at_the_head() {
    let recorded = recording();
    let mock = MockTransport::new();
    mock.push_response_frames(
        http::Response::builder()
            .status(200)
            .body(vec!["first", "second"])
            .unwrap(),
    );
    let t = Instrumented::tracing(mock);

    block_on(async {
        let res = t.execute(get("https://api.test/x")).await.expect("answers");
        assert!(!recorded.only().closed, "the head is not the end");

        let mut body = res.into_body();
        while let Some(frame) = body.frame().await {
            frame.expect("a frame");
        }
        // `body` is deliberately still alive: a `SpanBody` that closed
        // only on `Drop` fails here and passes its neighbour.
        assert!(recorded.only().closed, "the body ended, so the span did");
        drop(body);
    });
}

#[test]
fn a_body_dropped_before_it_ends_still_closes_the_span() {
    let recorded = recording();
    let mock = MockTransport::new();
    mock.push_response_frames(
        http::Response::builder()
            .status(200)
            .body(vec!["first", "second"])
            .unwrap(),
    );
    let t = Instrumented::tracing(mock);

    block_on(async {
        let res = t.execute(get("https://api.test/x")).await.expect("answers");
        let mut body = res.into_body();
        body.frame().await.expect("one frame").expect("ok");
        assert!(!recorded.only().closed, "one frame read of two");
        drop(body);
        assert!(recorded.only().closed, "the drop closed it");
    });
}

// ── through a real `Client` ─────────────────────────────────────────────

#[test]
fn resend_count_is_hop_plus_resend_over_a_real_redirect_chain() {
    let recorded = recording();
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

    let client = hclient::Client::builder(Instrumented::tracing(mock))
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

    let spans = recorded.spans();
    assert_eq!(spans.len(), 3, "one span per hop");
    // Reading `resend` alone — the mapping the field names invite —
    // reports nothing for any of these, because `resend` is 0 on every
    // hop of a redirect chain.
    assert_eq!(spans[0].field("http.request.resend_count"), None);
    assert_eq!(spans[1].field("http.request.resend_count"), Some("1"));
    assert_eq!(spans[2].field("http.request.resend_count"), Some("2"));
    assert_eq!(spans[2].field("hclient.hop"), Some("2"));
    assert_eq!(spans[2].field("hclient.resend"), Some("0"));
    assert!(spans.iter().all(|s| s.closed));
}
