use bytes::{Bytes, BytesMut};
use http_body::Body as HttpBody;
use http_ng_core::{Error, ErrorKind};
use std::pin::Pin;

/// A response with its URL preserved. `into_parts` gives full fidelity;
/// `chunk`/`collect` are convenience on top of it.
#[derive(Debug)]
pub struct Response<B> {
    parts: http::response::Parts,
    body: B,
    url: http::Uri,
    /// Set once `chunk()` has returned `Some(Err(_))`, after which
    /// `chunk()` returns `None`, never touching `body` again.
    ///
    /// m6 of the branch's final review: without it, `chunk()` after an
    /// error polled the underlying body again, and a caller working with
    /// `Response::chunk` directly could spin in a loop over a body that
    /// returns an error on every poll. `SseStream` compensated for this
    /// with its own `done` flag and tested it thoroughly — but only for
    /// itself. Terminality here is exactly the same: the error is handed
    /// back once, then it's the end of the stream.
    sealed: bool,
}

impl<B> Response<B> {
    pub(crate) fn new(resp: http::Response<B>, url: http::Uri) -> Self {
        let (parts, body) = resp.into_parts();
        Self {
            parts,
            body,
            url,
            sealed: false,
        }
    }
    pub fn status(&self) -> http::StatusCode {
        self.parts.status
    }
    pub fn headers(&self) -> &http::HeaderMap {
        &self.parts.headers
    }
    pub fn version(&self) -> http::Version {
        self.parts.version
    }
    pub fn url(&self) -> &http::Uri {
        &self.url
    }
    pub fn into_parts(self) -> (http::response::Parts, B) {
        (self.parts, self.body)
    }
}

impl<B> Response<B>
where
    B: HttpBody<Data = Bytes> + Unpin,
    // `B::Error: Send + Sync` — the second exception point in the "core
    // declares no Send/Sync" invariant, sibling of the bound on
    // `Client::execute` in `client.rs` (spec amendment-C1). Without it,
    // `Error::new(ErrorKind::Body, e)` below wouldn't compile: `Error`
    // stores its source as `Arc<dyn Error + Send + Sync>`, and type erasure
    // doesn't let an unbounded trait object's auto-traits through. The same
    // bound as `Client::execute`, only this time required on
    // `T::Body::Error` rather than `T::Error` — the body is read after the
    // transport has already returned it.
    B::Error: std::error::Error + Send + Sync + 'static, // send-bound-exception: amendment-C1
{
    /// The next data chunk. Trailer frames are skipped — for those, go
    /// through `into_parts` and poll the body directly.
    ///
    /// The error is terminal: after `Some(Err(_))` the body is sealed and
    /// all subsequent calls return `None` without polling it again (m6 of
    /// the branch's final review — see the `sealed` field).
    pub async fn chunk(&mut self) -> Option<Result<Bytes, Error>> {
        if self.sealed {
            return None;
        }
        loop {
            let frame = std::future::poll_fn(|cx| Pin::new(&mut self.body).poll_frame(cx)).await;
            match frame {
                Some(Ok(f)) => match f.into_data() {
                    Ok(d) => return Some(Ok(d)),
                    Err(_) => continue, // trailers
                },
                Some(Err(e)) => {
                    self.sealed = true;
                    return Some(Err(classify_body_error(e)));
                }
                None => {
                    self.sealed = true;
                    return None;
                }
            }
        }
    }

    pub async fn collect(mut self) -> Result<Collected, Error> {
        let mut acc = BytesMut::new();
        while let Some(c) = self.chunk().await {
            acc.extend_from_slice(&c?);
        }
        Ok(Collected {
            parts: self.parts,
            url: self.url,
            body: acc.freeze(),
        })
    }
}

/// Classifies a response body's read error — the response-half twin of
/// `Transport::to_error`'s default (`http-ng-core/src/unversioned/
/// transport.rs`), and for the same reason: if `e` is already our own
/// `Error`, its `kind()` was set at the point the backend actually
/// classified the failure (`ErrorKind::Cancelled` from a shutting-down
/// runtime, `ErrorKind::Tls` from a mid-stream handshake failure — whatever
/// it genuinely was), and re-wrapping it here would be exactly finding B2 of
/// vertical 1's final review, reproduced one seam later: `kind()` becomes
/// `Body` for everything, every `is_*` predicate lies, and `Display` prints
/// the category twice. Only a body whose error type carries no category of
/// its own — the common case for backends whose bodies are plain
/// `std::io::Error` or similar — falls back to `ErrorKind::Body`, which
/// remains the right default for a genuinely opaque body failure.
///
/// Found by vertical 2's final review (finding F2): `chunk()` used to wrap
/// unconditionally, and nothing in the test suite noticed, because
/// `NativeBody::poll_frame`'s own fallback already defaults to
/// `ErrorKind::Body` — the double-wrap was invisible by coincidence, not
/// because it was harmless. `Body`'s own `chunk_is_terminal_after_an_error_
/// and_does_not_poll_the_body_again` (in `tests/response.rs`) now pins the
/// non-coincidental case directly.
fn classify_body_error<E>(e: E) -> Error
where
    E: std::error::Error + Send + Sync + 'static, // send-bound-exception: amendment-C1
{
    let boxed: Box<dyn std::any::Any> = Box::new(e);
    match boxed.downcast::<Error>() {
        Ok(already_ours) => *already_ours,
        Err(foreign) => Error::new(
            ErrorKind::Body,
            *foreign.downcast::<E>().unwrap_or_else(|_| {
                // Unreachable, and not an invariant spanning two distant
                // places: established three lines above in the same
                // expression — boxed exactly `E`, the first downcast
                // missed, so the second is guaranteed to hit.
                unreachable!("boxed exactly E three lines above")
            }),
        ),
    }
}

/// The body read **together with** the status, headers, and URL.
///
/// reqwest's `Response::{text,json,bytes}` take `self` by value, which
/// leaves the status unreachable once the body's been read (issue #1542).
#[derive(Debug, Clone)]
pub struct Collected {
    parts: http::response::Parts,
    url: http::Uri,
    body: Bytes,
}

impl Collected {
    pub fn status(&self) -> http::StatusCode {
        self.parts.status
    }
    pub fn headers(&self) -> &http::HeaderMap {
        &self.parts.headers
    }
    pub fn url(&self) -> &http::Uri {
        &self.url
    }
    pub fn bytes(&self) -> &Bytes {
        &self.body
    }
    pub fn text(&self) -> Result<String, Error> {
        String::from_utf8(self.body.to_vec()).map_err(|e| Error::new(ErrorKind::Decode, e))
    }
    /// Deserializes the body as JSON. Part of the interface declared for
    /// this task (`Collected::json<T>()`), but missing from step 3 of the
    /// brief — see the task report.
    ///
    /// Behind the `json` feature, off by default: `serde`/`serde_json`
    /// aren't needed by a consumer who only streams the body or reads it
    /// as bytes — see the comment on the feature in Cargo.toml about the
    /// cost on wasm.
    #[cfg(feature = "json")]
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T, Error> {
        serde_json::from_slice(&self.body).map_err(|e| Error::new(ErrorKind::Decode, e))
    }
}
