//! The headline, executed rather than claimed: a real `axum::Router`
//! answering a real `hclient::Client`, with no socket.
//!
//! `tests/app_transport.rs` covers the transport's own behaviour against a
//! `service_fn`. What only this file can say is that the shape actually
//! fits **axum**, whose request body is its own type — a claim about a
//! third party, and this workspace's rule is that such a claim is exactly
//! as perishable as the check behind it.

use axum::Router;
use axum::routing::{get, post};
use hclient_core::RequestBody;
use hclient_tower::app::{AppTransport, OutgoingBody};
use tower::ServiceExt as _;

/// The one line an axum caller writes.
///
/// `axum::Router` is a `Service<http::Request<axum::body::Body>>` and this
/// transport hands it a [`OutgoingBody`], so the bodies are mapped at the
/// call site with tower's own combinator. **Deliberately not a type
/// parameter on `AppTransport`**: a body conversion is what
/// `ServiceExt::map_request` is for, and adding a parameter plus a stored
/// closure to spare one line would be this crate reimplementing a
/// combinator the caller already has.
fn transport(
    app: Router,
) -> AppTransport<
    impl tower_service::Service<
        http::Request<OutgoingBody>,
        Response = http::Response<axum::body::Body>,
        Error = std::convert::Infallible,
        Future: Send,
    > + Clone
    + Send
    + Sync
    + 'static,
> {
    AppTransport::new(
        "testserver",
        app.map_request(|r: http::Request<OutgoingBody>| r.map(axum::body::Body::new)),
    )
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn a_real_axum_router_answers_a_real_client_with_no_socket() {
    let app = Router::new()
        .route(
            "/one",
            get(|| async { axum::response::Redirect::to("/two") }),
        )
        .route("/two", get(|| async { "arrived" }))
        .route("/echo", post(|body: String| async move { body }));

    let client = hclient::Client::builder(transport(app))
        .build()
        .expect("supported");

    let (followed, echoed) = rt()
        .block_on(async {
            let followed = client
                .get("http://testserver/one")
                .send()
                .await?
                .collect()
                .await?
                .text()?;
            let echoed = client
                .post("http://testserver/echo")
                .body(RequestBody::Full(bytes::Bytes::from_static(b"payload")))
                .send()
                .await?
                .collect()
                .await?
                .text()?;
            Ok::<_, hclient::Error>((followed, echoed))
        })
        .expect("both exchanges");

    // The redirect was followed by the **client**, through the router,
    // twice into the same in-process service.
    assert_eq!(followed, "arrived");
    // And a request body reached an axum extractor.
    assert_eq!(echoed, "payload");
}

/// A status axum produced reaches the client as a status rather than as a
/// failure — the ordinary case, asserted because a transport that turned
/// every non-200 into an error would pass the test above.
#[test]
fn an_axum_status_arrives_as_a_status() {
    let app = Router::new().route(
        "/gone",
        get(|| async { (http::StatusCode::GONE, "no longer here") }),
    );
    let client = hclient::Client::builder(transport(app))
        .build()
        .expect("supported");

    let resp = rt()
        .block_on(async { client.get("http://testserver/gone").send().await })
        .expect("a 410 is an answer");
    assert_eq!(resp.status(), 410);
}
