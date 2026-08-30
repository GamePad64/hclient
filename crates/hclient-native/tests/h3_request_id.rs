//! The identity on the events, over HTTP/3.
//!
//! The QUIC arm is a second emitter rather than a second entry point into
//! the first — its own `Connected`, `Reused`, `Head` and `Progress`, in
//! its own module — so the claim `tests/request_id.rs` pins for the TCP
//! stack has to be pinned again here. What differs is what makes the test
//! sharper: a QUIC connection is **shared**, so the second request joins
//! the first's connection while it is still alive, and one connection id
//! genuinely covers two requests. The request id is then the only thing
//! separating them.
#![cfg(all(feature = "http3", not(target_family = "wasm")))]

#[path = "h3_server.rs"]
mod server;

use hclient_core::RequestBody;
use hclient_core::unversioned::{Attempt, Event, Hooks, RequestId, Transport};
use hclient_dns::IpLiteralOnly;
use hclient_native::H3;
use hclient_rt_tokio::TokioHandle;
use http_body_util::BodyExt;
use server::Behaviour;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Line {
    kind: Kind,
    request: RequestId,
    connection: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Connected,
    Reused,
    Head,
    Progress,
}

#[derive(Clone, Default)]
struct Recorder {
    seen: Arc<Mutex<Vec<Line>>>,
}

impl Recorder {
    fn lines(&self) -> Vec<Line> {
        self.seen.lock().expect("recorder").clone()
    }

    fn kinds(&self) -> Vec<Kind> {
        self.lines().into_iter().map(|l| l.kind).collect()
    }

    fn requests(&self) -> Vec<RequestId> {
        let mut out: Vec<RequestId> = Vec::new();
        for l in self.lines() {
            if !out.contains(&l.request) {
                out.push(l.request);
            }
        }
        out
    }

    fn requests_for(&self, kind: Kind) -> Vec<RequestId> {
        self.lines()
            .into_iter()
            .filter(|l| l.kind == kind)
            .map(|l| l.request)
            .collect()
    }
}

impl Hooks for Recorder {
    fn on(&self, event: &Event<'_>) {
        let line = match event {
            Event::Connected(e) => Line {
                kind: Kind::Connected,
                request: e.request,
                connection: e.id.get(),
            },
            Event::Reused(e) => Line {
                kind: Kind::Reused,
                request: e.request,
                connection: e.id.get(),
            },
            Event::Head(e) => Line {
                kind: Kind::Head,
                request: e.request,
                connection: e.id.get(),
            },
            Event::Progress(e) => Line {
                kind: Kind::Progress,
                request: e.request,
                connection: e.id.get(),
            },
            // `Closed` carries none, deliberately — and on a *shared* QUIC
            // connection that is the sharpest case for it: the two
            // requests below are equally the subject of its end.
            _ => return,
        };
        self.seen.lock().expect("recorder").push(line);
    }
}

fn watched(
    cert: &rustls::pki_types::CertificateDer<'static>,
    rec: &Recorder,
) -> H3<TokioHandle, hclient_tls_rustls::Rustls, IpLiteralOnly, Recorder> {
    H3::new(
        TokioHandle::current().expect("inside #[tokio::test]"),
        server::client_tls(cert),
        IpLiteralOnly,
    )
    .expect("H3::new does no I/O")
    .hooks(rec.clone())
}

fn get(addr: SocketAddr, attempt: Option<Attempt>) -> http::Request<RequestBody> {
    let mut req = http::Request::builder()
        .uri(format!("https://{addr}/hello"))
        .body(RequestBody::Empty)
        .expect("request");
    if let Some(a) = attempt {
        req.extensions_mut().insert(a);
    }
    req
}

fn post(addr: SocketAddr, attempt: Attempt) -> http::Request<RequestBody> {
    let mut req = http::Request::builder()
        .method(http::Method::POST)
        .uri(format!("https://{addr}/hello"))
        .body(RequestBody::Full(bytes::Bytes::from_static(
            b"a payload worth counting",
        )))
        .expect("request");
    req.extensions_mut().insert(attempt);
    req
}

async fn drive<T: Transport>(t: &T, req: http::Request<RequestBody>)
where
    T::Body: http_body::Body<Data = bytes::Bytes>,
    <T::Body as http_body::Body>::Error: std::fmt::Debug,
{
    let r = t.execute(req).await.map_err(|_| ()).expect("h3 request");
    assert_eq!(r.status(), 200);
    // Drained, because `Progress` is reported from the body.
    let _ = r.into_body().collect().await.expect("body");
}

/// **Every event of one exchange names the request that was sent**, and
/// the second exchange's name the second request — over one shared QUIC
/// connection, so the connection id cannot be what tells them apart.
#[tokio::test(flavor = "multi_thread")]
async fn every_event_over_quic_names_the_request_that_was_sent() {
    let s = server::start(Behaviour::Echo);
    let rec = Recorder::default();
    let t = watched(&s.cert_der, &rec);

    let first = Attempt::new(RequestId::next());
    drive(&t, get(s.addr, Some(first))).await;
    // A body on the second, so the **outbound** direction has octets to
    // report: it goes through `Reporting`, a different wrapper from the
    // response body's `Counting`, and a mutation in it survives a request
    // that sends nothing.
    let second = Attempt::new(RequestId::next());
    drive(&t, post(s.addr, second)).await;

    assert_eq!(
        s.accepted(),
        1,
        "premise: one QUIC connection served both requests, so there is a \
         `Reused` to assert on"
    );
    let kinds = rec.kinds();
    for expected in [Kind::Connected, Kind::Reused, Kind::Head, Kind::Progress] {
        assert!(
            kinds.contains(&expected),
            "premise: {expected:?} was reported at all — saw {kinds:?}",
        );
    }

    assert_eq!(
        rec.requests(),
        vec![first.id, second.id],
        "every event names the request it belongs to, and only those two",
    );
    assert_eq!(rec.requests_for(Kind::Connected), vec![first.id]);
    assert_eq!(
        rec.requests_for(Kind::Reused),
        vec![second.id],
        "the reuse belongs to the request taking the connection, not to \
         the one that opened it",
    );
    assert_eq!(rec.requests_for(Kind::Head), vec![first.id, second.id]);

    let connections: Vec<u64> = rec.lines().into_iter().map(|l| l.connection).collect();
    assert!(
        connections.windows(2).all(|w| w[0] == w[1]),
        "premise: one connection id throughout: {connections:?}",
    );
}

/// The control: no `Attempt`, and the QUIC arm under-reports rather than
/// inventing an identity.
#[tokio::test(flavor = "multi_thread")]
async fn a_quic_transport_with_no_attempt_reports_unidentified() {
    let s = server::start(Behaviour::Echo);
    let rec = Recorder::default();
    let t = watched(&s.cert_der, &rec);

    drive(&t, get(s.addr, None)).await;

    let kinds = rec.kinds();
    assert!(
        kinds.contains(&Kind::Connected) && kinds.contains(&Kind::Head),
        "premise: events were reported at all — saw {kinds:?}",
    );
    assert_eq!(rec.requests(), vec![RequestId::UNIDENTIFIED]);
}
