//! Applying the decision made in `hclient-proto`. Only data shuffling
//! lives here: all the logic is in the pure function
//! `proto::redirect::decide`.

use hclient_core::{AllowEarlyData, RequestBody};
use hclient_proto::redirect::{Follow, SENSITIVE_HEADERS};

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
/// **`extensions` cross an origin boundary with one exception.** Carrying
/// them over unconditionally has no consequences while
/// `Timeouts` is the only type in the bag — safe, and necessary, to carry
/// across an origin (otherwise timeouts would vanish after a redirect,
/// which is exactly where B1 puts them). It recorded a debt for the day
/// something else moves in, at which point `extensions` need the same
/// sensitivity filter headers already have. [`AllowEarlyData`] is that.
///
/// It is not a credential, so `SENSITIVE_HEADERS` reasoning does not reach
/// it directly, but it is the same *kind* of thing: a statement the caller
/// made about a request **to a particular server**. "Replaying this is
/// safe" is a claim about what the request does at origin `A`; carried to
/// `B`, it is a judgement nobody made — and acting on it means sending
/// early data, which an attacker can replay, to a server the caller never
/// vouched for. So it comes off exactly where the credentials do, on the
/// hop `decide` marked `strip_sensitive` (host or scheme changed), and the
/// invariant that leaves is one sentence: **the mark survives only while
/// the chain stays inside the origin the caller addressed.**
///
/// Deliberately NOT removed on a same-origin hop — that judgement still
/// applies there, and withdrawing it would be a silent downgrade of a
/// setting the caller wrote. Nor on a method downgrade: a `303` turning
/// `POST` into `GET` yields a request at most as consequential as the one
/// that was marked. Both directions are pinned next to each other in
/// `crates/hclient/tests/too_early.rs`, with the client's other use of
/// this mark (the `425` retry strips it from the retry alone).
///
/// `Timeouts` still carries over unconditionally, for the reason above.
pub(crate) fn next_hop(
    prev: &HopParts,
    replay: Option<RequestBody>,
    follow: &Follow,
) -> Option<(HopParts, RequestBody)> {
    let mut headers = prev.headers.clone();
    let mut extensions = prev.extensions.clone();
    if follow.strip_sensitive {
        for h in SENSITIVE_HEADERS {
            headers.remove(&h);
        }
        // The extension half of the same boundary — see this function's
        // doc comment for why a replay-safety judgement does not cross an
        // origin, and why nothing else in the bag moves with it.
        extensions.remove::<AllowEarlyData>();
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
            extensions,
        },
        body,
    ))
}
