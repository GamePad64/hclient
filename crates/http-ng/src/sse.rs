use crate::response::Response;
use bytes::Bytes;
use http_body::Body as HttpBody;
use http_ng_core::{Error, ErrorKind};
use http_ng_proto::sse::{SseDecoder, SseEvent};

const MIME: &str = "text/event-stream";

/// `Content-Type` matches the SSE MIME type exactly as a token, not as a
/// prefix: `text/event-stream` case-insensitively (HTTP media types are
/// case-insensitive — RFC 9110 §5.5), and the next byte is either end of
/// string, `;` (a parameter boundary, e.g. `; charset=utf-8`), or
/// whitespace. A bare `starts_with` (review round 1, Finding 2) accepted
/// `"text/event-streamfoo"` as a valid type and rejected
/// `"Text/Event-Stream"` over case — both defects are closed here by one
/// token-boundary check.
fn is_event_stream_content_type(v: &str) -> bool {
    let v = v.trim_start();
    let Some(head) = v.get(..MIME.len()) else {
        return false;
    };
    if !head.eq_ignore_ascii_case(MIME) {
        return false;
    }
    match v.as_bytes().get(MIME.len()) {
        None => true,
        Some(b';') => true,
        Some(b) => b.is_ascii_whitespace(),
    }
}

/// A stream of SSE events over any response body.
///
/// Reconnection is **not** implemented here: it requires resending the
/// request and will arrive with the retry stage in v0.2. `last_event_id()`
/// is already available, so adding reconnection won't change the public
/// API.
#[derive(Debug)]
pub struct SseStream<B> {
    resp: Response<B>,
    decoder: SseDecoder,
    /// A fatal error, held back until the decoder's queue is drained:
    /// events already parsed were received whole and correctly, and
    /// losing them for the sake of an earlier error message would mean
    /// losing correct data (review round 1, Finding 1). Stored here both
    /// by a transport body error (`Response::chunk` returned
    /// `Some(Err(_))`) and by the decoder's limit being exceeded — both
    /// paths must let already-ready events out first. `next()` hands it
    /// back exactly once (`Option::take`), after which `done` guarantees
    /// an infinite `None` — not a resurrection or a repeat of the error.
    fatal: Option<Error>,
    /// Stop reading new chunks from the body. Not the same as
    /// `fatal.is_some()`: a clean EOF (`chunk()` returned `None`, no
    /// error) also sets `done`, but `fatal` stays `None` in that case.
    done: bool,
}

#[derive(Debug)]
struct SseRejected(&'static str);
impl std::fmt::Display for SseRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "not an SSE stream: {}", self.0)
    }
}
impl std::error::Error for SseRejected {}

impl<B> SseStream<B>
where
    B: HttpBody<Data = Bytes> + Unpin,
    // `next()` calls `self.resp.chunk()`, which is only defined
    // (`response.rs`) under `B::Error: Send + Sync + 'static` (spec
    // amendment-C1). Rust doesn't propagate bounds through a call: a
    // generic function must repeat its callee's bound in its own
    // where-clause — the same trick `RequestBuilder::send` uses relative
    // to `Client::execute` (`request.rs`). The fourth independent chain of
    // this kind in the crate, after `Client::execute` (client.rs),
    // `Response::chunk`+`collect` sharing one bound (response.rs), and
    // `RequestBuilder::send` (request.rs) — see the counter and the rule
    // in `.github/workflows/ci.yml`.
    B::Error: std::error::Error + Send + Sync + 'static, // send-bound-exception: amendment-C1
{
    /// Builds a stream from a response. WHATWG's terminal rules are
    /// checked here, not deferred to the first `next()`: status ≠ 200 is
    /// an error (204 in particular means "stop forever", not "empty
    /// stream"); `Content-Type` ≠ `text/event-stream` is also an error,
    /// not a silent coercion of the content type.
    pub fn new(resp: Response<B>, max_event_size: usize) -> Result<Self, Error> {
        if resp.status() != http::StatusCode::OK {
            return Err(Error::new(
                ErrorKind::Status,
                SseRejected("status is not 200"),
            ));
        }
        let ok_ct = resp
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(is_event_stream_content_type);
        if !ok_ct {
            return Err(Error::new(
                ErrorKind::Decode,
                SseRejected("content-type is not text/event-stream"),
            ));
        }
        Ok(Self {
            resp,
            decoder: SseDecoder::new(max_event_size),
            fatal: None,
            done: false,
        })
    }

    pub fn last_event_id(&self) -> Option<&str> {
        self.decoder.last_event_id()
    }

    /// The next decoded event. Reads exactly as many chunks from the body
    /// as the decoder needs to assemble at least one ready event —
    /// transport chunk boundaries aren't required to line up with SSE
    /// event boundaries.
    ///
    /// The order on a fatal error (limit exceeded or a body error)
    /// matters: events already fully and correctly parsed BEFORE the error
    /// happened are handed back first — the decoder's queue drains before
    /// `next()` returns `Err`. After that the stream is over for good:
    /// `Err` is handed back exactly once, every call after that is
    /// `None`.
    pub async fn next(&mut self) -> Option<Result<SseEvent, Error>> {
        loop {
            if let Some(e) = self.decoder.next() {
                return Some(Ok(e));
            }
            if let Some(e) = self.fatal.take() {
                return Some(Err(e));
            }
            if self.done {
                return None;
            }
            match self.resp.chunk().await {
                Some(Ok(chunk)) => {
                    if let Err(e) = self.decoder.push(&chunk) {
                        // Exceeding the limit is fatal and isn't retried —
                        // but not before events that already settled into
                        // the decoder's queue from this same `push` reach
                        // the caller: the loop above drains them first via
                        // `self.decoder.next()`.
                        self.done = true;
                        self.fatal = Some(Error::new(ErrorKind::Decode, e));
                    }
                }
                Some(Err(e)) => {
                    // The same ordering as the limit-exceeded case:
                    // already-ready events aren't sacrificed for an
                    // earlier body-error message. In practice this
                    // `chunk()` is only called once `self.decoder.next()`
                    // at the top of the loop is already empty (otherwise
                    // the loop would have returned `Ok` earlier and never
                    // reached here), and `chunk()` itself never touches
                    // the decoder's queue — so the queue is always empty
                    // here, and an immediate `return` would be
                    // indistinguishable from the current fall-through by
                    // any test (review round 2, Finding 2: verified by
                    // mutation). Left symmetric with the limit-exceeded
                    // path for consistency, and in case this condition
                    // stops holding under a future refactor of `next()`.
                    self.done = true;
                    self.fatal = Some(e);
                }
                None => {
                    // End of body without a final empty line: an
                    // undispatched "tail" left in the decoder's data
                    // buffer is dropped silently — this is WHATWG's
                    // deliberate behavior (an event is only dispatched on
                    // an empty line; the browser's EventSource does the
                    // same thing when the connection closes without a
                    // final delimiter), not a defect in `SseStream`
                    // (review round 1, Finding 5).
                    self.done = true;
                }
            }
        }
    }
}
