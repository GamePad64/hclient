//! The loop closed: a tower stack sitting UNDER `Client`, not instead of it.
//!
//! `Native -> TransportService -> [layers] -> ServiceTransport -> Client`.
//! What these tests are really checking is that the facade's own work —
//! redirect following, the capability check — still happens with a tower
//! stack in the middle, because losing it was the entire reason the reverse
//! adapter had to exist.

use http_ng::Client;
use http_ng_core::{Capabilities, RedirectSupport, RequestBody, unversioned::Transport};
use http_ng_mock::MockTransport;
use http_ng_tower::{ServiceTransport, TransportService};
use std::sync::{Arc, Mutex};
use tower::ServiceBuilder;

/// Records every request that passes through the middle of the stack, so a
/// green test cannot be explained by the layer having been skipped.
type Seen = Arc<Mutex<Vec<http::Uri>>>;

fn stack(m: MockTransport, seen: Seen) -> impl Transport<Error = http_ng_core::Error> + Clone {
    let caps = m.capabilities().clone();
    let svc = TransportService::new(m);
    let layered = ServiceBuilder::new()
        .map_request(move |req: http::Request<RequestBody>| {
            seen.lock()
                .expect("seen lock poisoned")
                .push(req.uri().clone());
            req
        })
        .service(svc);
    ServiceTransport::new(layered, caps)
}

#[test]
fn a_request_travels_client_through_the_layer_and_back() {
    let m = MockTransport::new();
    m.push_response(http::Response::builder().status(200).body("ok").unwrap());
    let seen: Seen = Default::default();

    let c = Client::builder(stack(m, Arc::clone(&seen)))
        .build()
        .expect("the mock's capabilities allow the default config");
    let resp = futures_executor::block_on(c.get("https://example.com/thing").send())
        .expect("the stack answers");

    assert_eq!(resp.status(), 200);
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "the layer must actually be traversed — a green test with zero sightings would mean the stack was bypassed"
    );
}

/// The reason the reverse adapter exists. Applying a layer on top of the
/// transport would have meant giving up `Client`'s redirect stage; here the
/// stage still runs, and the layer sees BOTH hops.
#[test]
fn the_clients_redirect_stage_still_runs_with_a_layer_underneath() {
    let m = MockTransport::new();
    m.push_response(
        http::Response::builder()
            .status(302)
            .header("location", "https://example.com/second")
            .body("")
            .unwrap(),
    );
    m.push_response(
        http::Response::builder()
            .status(200)
            .body("arrived")
            .unwrap(),
    );
    let seen: Seen = Default::default();

    let c = Client::builder(stack(m, Arc::clone(&seen)))
        .build()
        .unwrap();
    let resp = futures_executor::block_on(c.get("https://example.com/first").send()).unwrap();

    assert_eq!(resp.status(), 200, "the redirect must have been followed");
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2, "both hops must pass through the layer");
    assert_eq!(seen[0].path(), "/first");
    assert_eq!(seen[1].path(), "/second");
}

/// The capabilities argument is load-bearing, not decoration. A stack that
/// declares a browser-shaped backend must still be refused a redirect
/// policy at `build()` — the adapter passes the claim through rather than
/// swallowing it.
#[test]
fn capabilities_passed_through_the_adapter_still_gate_the_builder() {
    let m = MockTransport::new();
    let mut caps = m.capabilities().clone();
    caps.redirects = RedirectSupport::Internal;

    let svc = TransportService::new(m);
    let t = ServiceTransport::new(svc, caps);

    let err = Client::builder(t)
        .redirect(http_ng::RedirectPolicy::Limited(3))
        .build()
        .expect_err("a policy against an Internal backend must be refused");
    assert!(
        err.to_string().contains("redirect_policy"),
        "the refusal must name what could not be honoured: {err}"
    );
}

/// `Capabilities::none()` is the tempting argument to pass and the wrong
/// one — it makes the stack claim it can do nothing, and `Client::build`
/// then refuses configurations the stack supports perfectly well. Pinned
/// here so the doc comment's warning is not the only thing holding it.
#[test]
fn passing_none_capabilities_loses_what_the_stack_can_actually_do() {
    let mut real = Capabilities::none();
    real.timeouts.connect = true;
    let m = MockTransport::new().with_capabilities(real.clone());

    let honest = ServiceTransport::new(TransportService::new(m), real);
    assert!(
        honest.capabilities().timeouts.connect,
        "capabilities taken from the transport must survive the adapter"
    );

    let m2 = MockTransport::new().with_capabilities({
        let mut c = Capabilities::none();
        c.timeouts.connect = true;
        c
    });
    let careless = ServiceTransport::new(TransportService::new(m2), Capabilities::none());
    assert!(
        !careless.capabilities().timeouts.connect,
        "and none() genuinely discards them — this is not a harmless placeholder"
    );
}

/// tower's contract: `poll_ready` must reach `Ready` before `call`, and a
/// service is entitled to panic otherwise. Nothing else in this suite pins
/// it — every other test service is trivially ready, so deleting the
/// readiness drive from the adapter left all of them green. The layers
/// where it actually matters (concurrency and rate limits reserve their
/// permit in `poll_ready`) would otherwise hand out work they have no
/// budget for.
#[derive(Clone, Default)]
struct DemandsReadiness {
    /// Per-instance, and `Clone` starts a fresh one at `false` — which is
    /// the point: the adapter clones and must drive readiness on the CLONE,
    /// not rely on the original having been polled.
    ready: bool,
}

impl tower_service::Service<http::Request<RequestBody>> for DemandsReadiness {
    type Response = http::Response<http_body_util::Full<bytes::Bytes>>;
    type Error = http_ng_core::Error;
    type Future =
        std::pin::Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>>>>;

    fn poll_ready(
        &mut self,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.ready = true;
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, _: http::Request<RequestBody>) -> Self::Future {
        assert!(
            self.ready,
            "called before poll_ready reached Ready — tower's contract, and the point of this test"
        );
        Box::pin(async {
            Ok(http::Response::builder()
                .status(204)
                .body(http_body_util::Full::new(bytes::Bytes::new()))
                .unwrap())
        })
    }
}

#[test]
fn readiness_is_driven_on_the_clone_before_the_call() {
    let t = ServiceTransport::new(DemandsReadiness::default(), Capabilities::none());
    let resp = futures_executor::block_on(
        t.execute(
            http::Request::builder()
                .uri("https://example.com/")
                .body(RequestBody::Empty)
                .unwrap(),
        ),
    )
    .expect("the service answers once readiness was driven");
    assert_eq!(resp.status(), 204);
}
