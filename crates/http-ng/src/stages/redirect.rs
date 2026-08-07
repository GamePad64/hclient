//! Applying the decision made in `http-ng-proto`. Only data shuffling
//! lives here: all the logic is in the pure function
//! `proto::redirect::decide`.

use http_ng_core::RequestBody;
use http_ng_proto::redirect::{Follow, SENSITIVE_HEADERS};

/// Everything that carries over between hops, except the body.
///
/// A separate type because `http::request::Parts` **doesn't implement
/// `Clone`**, and between hops the method, URI, and headers are needed
/// both before and after sending. `HeaderMap`, `Uri`, `Method`, and
/// `Extensions` are all cloneable — verified.
#[derive(Debug, Clone)]
pub(crate) struct HopParts {
    pub(crate) method: http::Method,
    pub(crate) uri: http::Uri,
    pub(crate) headers: http::HeaderMap,
    pub(crate) version: http::Version,
    pub(crate) extensions: http::Extensions,
}

impl HopParts {
    pub(crate) fn to_request(&self, body: RequestBody) -> http::Request<RequestBody> {
        let mut req = http::Request::new(body);
        *req.method_mut() = self.method.clone();
        *req.uri_mut() = self.uri.clone();
        *req.headers_mut() = self.headers.clone();
        *req.version_mut() = self.version;
        *req.extensions_mut() = self.extensions.clone();
        req
    }
}

/// Build the next hop. `replay` is a snapshot of the body taken **before**
/// the previous attempt was sent; `None` means the body can't be replayed.
///
/// Returns `None` when the body can't be replayed and the method wasn't
/// downgraded: in that case it's more honest to return the 3xx as-is than
/// to send an empty body where one is expected.
///
/// **`extensions` carry over to the next hop unconditionally, including
/// across origins** — unlike `headers`, where `strip_sensitive` scrubs
/// credentials. The asymmetry has no consequences today: the only type in
/// `extensions` is `Timeouts`, which is safe, and necessary, to carry
/// across an origin boundary (otherwise timeouts would vanish after a
/// redirect, and with B1 that's exactly where they get put). Recorded as a
/// known debt, not an overlooked one: design §4.9 puts authorization and
/// policy into the per-request config, and at that point `extensions` will
/// need the same sensitivity filter headers already have (m7 of the
/// branch's final review).
pub(crate) fn next_hop(
    prev: &HopParts,
    replay: Option<RequestBody>,
    follow: &Follow,
) -> Option<(HopParts, RequestBody)> {
    let mut headers = prev.headers.clone();
    if follow.strip_sensitive {
        for h in SENSITIVE_HEADERS {
            headers.remove(&h);
        }
    }
    let body = if follow.drop_body {
        headers.remove(http::header::CONTENT_LENGTH);
        headers.remove(http::header::CONTENT_TYPE);
        RequestBody::Empty
    } else {
        replay?
    };
    Some((
        HopParts {
            method: follow.method.clone(),
            uri: follow.uri.clone(),
            headers,
            version: prev.version,
            extensions: prev.extensions.clone(),
        },
        body,
    ))
}
