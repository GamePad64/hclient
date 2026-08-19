//! `act`'s `http-client` component, ported to `http-ng` — the acceptance
//! for the `Transport` shape.
//!
//! The original lives at `act/components/http-client/src/lib.rs` (156
//! lines) and was written against `wasi-fetch` **before** this library
//! existed. Everything below that is not `act_sdk` glue is a line-for-line
//! port of it: the same per-request redirect limit computed from
//! `follow_redirects`, the same per-request timeout, the same
//! `content-type` defaulting for a JSON body, and the same streaming loop
//! that attaches status and headers to the first chunk only, with the same
//! fallback for a body that turns out to be empty.
//!
//! What matters is what is **not** here: not a single `#[cfg]`. One source
//! file, three targets:
//!
//! ```text
//! cargo build -p http-ng --example portable
//! cargo build -p http-ng --example portable --target wasm32-wasip2
//! cargo build -p http-ng --example portable --target wasm32-unknown-unknown
//! ```
//!
//! The consumer's own generic parameter is the transport, and nothing
//! else: `fetch` takes `&Client<T>` and never names a backend, a runtime,
//! a TLS stack, or a target. Picking `T` is the caller's job, and it is the
//! only line of a real component that would differ per target.
//!
//! Three things are deliberately **not** ported, because none of them is
//! about the transport seam:
//!
//! * `act_sdk`'s `#[act_component]`/`#[act_tool]` macros and its
//!   `ActContext`. `ActContext::send_content` is modelled by the
//!   [`ContentSink`] trait below — same three arguments, same call order.
//! * `serde`/`schemars` derives on the argument struct. The component gets
//!   `FetchArgs` deserialised from CBOR; how it arrives has nothing to do
//!   with `http-ng`, so [`FetchArgs`] here is a plain struct.
//! * `act_sdk::cbor::to_cbor` for the metadata values. The keys, the pair
//!   count, and the "first chunk only" placement are preserved exactly; the
//!   *encoding* is a plain byte rendering, so this example pulls in no
//!   dependency `http-ng` does not already have.
//!
//! Two places where the port is **not** behaviour-for-behaviour, both
//! reported rather than smoothed over — see `docs/porting-wasi-fetch.md`
//! for the full list:
//!
//! 1. `follow_redirects: false` becomes `RedirectPolicy::Limited(0)`,
//!    which makes a 3xx an `ErrorKind::Redirect` here, where `wasi-fetch`
//!    handed the 3xx response back to the caller.
//! 2. Against a backend that follows redirects internally (the browser
//!    `fetch` transport, `RedirectSupport::Internal`) **both** values of
//!    `follow_redirects` are rejected with `ErrorKind::Unsupported`,
//!    because this component states a redirect intent on every call and
//!    that backend can honour neither. `wasi-fetch` targets wasip3 only
//!    and never met that backend.
//!
//! Both are pinned by `tests/portable_example.rs`, which runs this file's
//! `fetch` against `MockTransport`.

use bytes::Bytes;
use http_ng::{Client, RedirectPolicy, RequestBody, Timeouts};
use http_ng_core::unversioned::Transport;
use std::collections::HashMap;
use std::error::Error as StdError;
use std::time::Duration;

/// Component-specific metadata keys — the originals, unchanged.
const META_HTTP_STATUS: &str = "http-client:status";
const META_HTTP_HEADERS: &str = "http-client:headers";

/// The request body, in the three shapes the component accepts.
///
/// The original is an untagged `serde` enum over `body_raw` / `body_json` /
/// `body`. `Json` holds already-serialised JSON text here rather than a
/// `serde_json::Value`: the only thing the request path does with the
/// distinction is [`Body::is_json`], and keeping `serde_json` out of an
/// example whose subject is the transport seam costs nothing.
#[derive(Debug, Clone)]
pub enum Body {
    /// Raw binary request body.
    Raw(Vec<u8>),
    /// JSON request body. Auto-sets `content-type`.
    Json(String),
    /// Text request body (UTF-8).
    Text(String),
}

impl Body {
    fn into_bytes(self) -> Vec<u8> {
        match self {
            Body::Raw(body_raw) => body_raw,
            Body::Json(body_json) => body_json.into_bytes(),
            Body::Text(body) => body.into_bytes(),
        }
    }

    fn is_json(&self) -> bool {
        matches!(self, Body::Json(_))
    }
}

/// The tool's arguments — `FetchArgs` in the original, minus the derives.
#[derive(Debug, Clone)]
pub struct FetchArgs {
    /// URL to fetch.
    pub url: String,
    /// HTTP method (the original defaults this to GET).
    pub method: http::Method,
    /// Request headers as key-value pairs.
    pub headers: HashMap<String, String>,
    /// Request body.
    pub body: Option<Body>,
    /// Request timeout in milliseconds.
    pub timeout_ms: Option<u64>,
    /// Whether to follow redirects (the original defaults this to `true`).
    pub follow_redirects: bool,
}

/// `ActContext::send_content`, and nothing else from `ActContext`.
///
/// The component streams: every chunk that comes off the wire is forwarded
/// as it arrives instead of being accumulated, which is the whole reason a
/// download of any size works there at all. That is the property this trait
/// exists to keep visible in the port — a `collect()`-shaped example would
/// compile just as well for all three targets and would prove strictly
/// less.
pub trait ContentSink {
    fn send_content(
        &mut self,
        data: Vec<u8>,
        content_type: Option<String>,
        metadata: Vec<(String, Vec<u8>)>,
    );
}

/// `ActError`, narrowed to the two constructors this component uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComponentError {
    /// `ActError::invalid_args` — the caller's fault.
    InvalidArgs(String),
    /// `ActError::internal` — everything else.
    Internal(String),
}

impl std::fmt::Display for ComponentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ComponentError::InvalidArgs(m) => write!(f, "invalid arguments: {m}"),
            ComponentError::Internal(m) => write!(f, "{m}"),
        }
    }
}

impl StdError for ComponentError {}

/// The original's `map_err` on `send()`, ported.
///
/// `wasi-fetch` had a dedicated `Error::Url(String)` variant, and the
/// component split on it: a URL the caller mistyped is `invalid_args`,
/// anything else is `internal`. `http-ng` has no `ErrorKind` for that —
/// a URL that does not parse comes back as `ErrorKind::Other` carrying
/// `http_ng::UriError` as its source (`config::effective_uri`) — so the
/// same split is available, but through `source().is::<..>()` rather than
/// through `kind()`. That is a downgrade in ergonomics, not in
/// expressiveness, and it is recorded as such in the porting guide.
///
/// `UriError` rather than `http::uri::InvalidUri`, which this line used to
/// name: since URL parsing moved behind `http_ng_proto::uri`, one type
/// covers every way a caller's URL can be unusable — unparseable, an
/// unusable base, or a non-ASCII host in a build without the `idn`
/// feature. All three are the caller's argument being wrong, which is
/// exactly the split this function exists to make; matching the inner
/// `InvalidUri` alone would have classified the other two as `internal`.
fn classify(e: http_ng::Error) -> ComponentError {
    if e.source().is_some_and(|s| s.is::<http_ng::UriError>()) {
        ComponentError::InvalidArgs(e.to_string())
    } else {
        ComponentError::Internal(format!("HTTP error: {e}"))
    }
}

/// Render a `HeaderMap` for the metadata value.
///
/// The original calls `act_sdk::cbor::to_cbor` through an `http_serde`
/// wrapper. The pair count and the keys are what the port has to preserve;
/// the encoding is not part of the transport seam, so this is plain
/// `name: value` lines.
fn encode_headers(map: &http::HeaderMap) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, value) in map {
        out.extend_from_slice(name.as_str().as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(value.as_bytes());
        out.push(b'\n');
    }
    out
}

fn status_headers_metadata(status: u16, headers: &http::HeaderMap) -> Vec<(String, Vec<u8>)> {
    vec![
        (
            META_HTTP_STATUS.to_string(),
            status.to_string().into_bytes(),
        ),
        (META_HTTP_HEADERS.to_string(), encode_headers(headers)),
    ]
}

/// The component's one tool, `fetch`.
///
/// Generic over the transport and over nothing else. The bounds are the
/// ones `http-ng`'s own signatures require, spelled out because a generic
/// function has to repeat its callee's where-clause:
///
/// * `T::Error: Send + Sync + 'static` — `RequestBuilder::send` needs it
///   (spec amendment-C1).
/// * `T::Body: Unpin` and `<T::Body as http_body::Body>::Error:
///   std::error::Error + Send + Sync + 'static` — `Response::chunk` needs
///   them.
///
/// Not one of those bounds mentions a target, a runtime, or `Send` on the
/// returned future.
pub async fn fetch<T, S>(
    client: &Client<T>,
    args: FetchArgs,
    ctx: &mut S,
) -> Result<(), ComponentError>
where
    T: Transport,
    T::Error: Send + Sync + 'static,
    T::Body: http_body::Body<Data = Bytes> + Unpin,
    <T::Body as http_body::Body>::Error: StdError + Send + Sync + 'static,
    S: ContentSink,
{
    // The original, verbatim: `let redirect_limit = if args.follow_redirects
    // { 10 } else { 0 };`, then `.redirect_limit(redirect_limit)` on the
    // per-request builder. Before `RequestBuilder::redirect` existed, the
    // only way to say this through `http-ng` was one `Client` per request.
    //
    // `false` maps to `RedirectPolicy::None`, NOT to `Limited(0)`. The
    // consumer forwards the 3xx upward — status and `Location` become its
    // output — and `wasi-fetch`, which it migrates from, did the same:
    // `redirect_limit(0)` there skipped the redirect branch entirely rather
    // than failing. `Limited(0)` would turn that answer into an error, which
    // is the mistake a mechanical migration makes. This branch was
    // inexpressible until the Task 10 acceptance found it and `RedirectPolicy`
    // became an enum.
    let redirect = if args.follow_redirects {
        RedirectPolicy::Limited(10)
    } else {
        RedirectPolicy::None
    };

    let mut builder = client
        .request(args.method.clone(), &args.url)
        .redirect(redirect);

    // Set headers
    for (k, v) in &args.headers {
        builder = builder.header(k.as_str(), v.as_str());
    }

    // Set body
    if let Some(body) = args.body {
        // Auto-set Content-Type for JSON if not already set
        if body.is_json()
            && !args
                .headers
                .keys()
                .any(|k| k.eq_ignore_ascii_case("content-type"))
        {
            builder = builder.header("content-type", "application/json");
        }
        builder = builder.body(RequestBody::Full(Bytes::from(body.into_bytes())));
    }

    // Set timeout. `wasi-fetch::RequestBuilder::timeout` set the wasip3
    // `connect` **and** `first_byte` options from its single `Duration`
    // (verified in `wasi-fetch/src/request.rs`: two `set_*_timeout` calls
    // with the same `ns`), so both fields are set here. Mapping to
    // `first_byte` alone would silently drop the connect timeout the
    // component has today — a request that hangs while the connection is
    // being established would stop timing out at all.
    if let Some(ms) = args.timeout_ms {
        let d = Duration::from_millis(ms);
        builder = builder.timeouts(Timeouts {
            resolve: None,
            connect: Some(d),
            first_byte: Some(d),
            ..Default::default()
        });
    }

    let mut response = builder.send().await.map_err(classify)?;

    // Non-destructive: unlike `wasi-fetch`, reading the body does not
    // consume the status and headers. They are still cloned up front,
    // because the loop below holds `&mut response` and the original clones
    // them too.
    let status = response.status().as_u16();
    let resp_headers = response.headers().clone();
    let content_type = resp_headers
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    // Stream response body chunks
    let mut first_chunk = true;

    while let Some(chunk) = response.chunk().await {
        // `wasi-fetch::Body::chunk` returned `Option<Bytes>` and turned a
        // mid-body failure into `None`, so a truncated download reached the
        // caller as a complete one. `Response::chunk` returns
        // `Option<Result<Bytes, Error>>`; the `?` here is the whole
        // difference.
        let chunk = chunk.map_err(classify)?;
        let metadata = if first_chunk {
            first_chunk = false;
            status_headers_metadata(status, &resp_headers)
        } else {
            vec![]
        };
        ctx.send_content(chunk.to_vec(), content_type.clone(), metadata);
    }

    // If no body was received, still send status/headers
    if first_chunk {
        ctx.send_content(
            vec![],
            content_type.clone(),
            status_headers_metadata(status, &resp_headers),
        );
    }

    Ok(())
}

fn main() {
    println!(
        "act's http-client component on http-ng: one source, no #[cfg], \
         built for native, wasm32-wasip2 and wasm32-unknown-unknown"
    );
}
