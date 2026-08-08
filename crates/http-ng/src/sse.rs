use crate::client::Client;
use crate::response::Response;
use bytes::Bytes;
use http_body::Body as HttpBody;
use http_ng_core::unversioned::{Timer, Transport};
use http_ng_core::{Error, ErrorKind, RequestBody};
use http_ng_proto::backoff::Backoff;
use http_ng_proto::sse::{SseDecoder, SseEvent};
use std::time::Duration;

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
/// Reconnection is **not** implemented here — this type has no `Client`, no
/// URL, and no way to resend a request, only a `Response` it was handed
/// once. [`Client::sse`] is the reconnecting entry point, built on top of
/// this type rather than replacing it: it opens a fresh `SseStream` on every
/// (re)connect and forwards to it, so the WHATWG terminal rules and the
/// fatal-error-then-forever-`None` ordering below are exercised identically
/// whether or not reconnect is involved, instead of being re-derived a
/// second time. `last_event_id()` was already available before reconnect
/// existed, so adding it didn't change this type's public API.
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

#[derive(Debug, thiserror::Error)]
#[error("not an SSE stream: {0}")]
struct SseRejected(&'static str);

/// WHATWG's terminal rules for a (re)connection attempt, factored out of
/// `SseStream::new` so [`Client::sse`]'s reconnect path shares exactly this
/// check on every reopen rather than re-deriving it: status ≠ 200 is an
/// error (204 in particular means "stop forever", not "empty stream" —
/// WHATWG doesn't special-case 204 beyond the general "status ≠ 200 fails
/// the connection permanently" rule, so no separate branch for it exists
/// here or anywhere reconnect consumes this); `Content-Type` ≠
/// `text/event-stream` is also an error, not a silent coercion.
fn validate_sse_response<B>(resp: &Response<B>) -> Result<(), Error> {
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
    Ok(())
}

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
        Self::new_with_decoder(resp, SseDecoder::new(max_event_size))
    }

    /// Shared by `new` and [`crate::sse::open`] (the reconnect path): the
    /// only difference between opening a stream fresh and reopening one on
    /// reconnect is which `SseDecoder` it starts from — a plain one, or one
    /// seeded with the previously known last event ID
    /// (`SseDecoder::new_with_last_event_id`) so a message dispatched
    /// before the new connection sends its own `id:` line still reports
    /// the right id. The WHATWG terminal-rule validation is identical
    /// either way.
    fn new_with_decoder(resp: Response<B>, decoder: SseDecoder) -> Result<Self, Error> {
        validate_sse_response(&resp)?;
        Ok(Self {
            resp,
            decoder,
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

// ─────────────────────────────── Reconnect ────────────────────────────────
//
// Everything below builds ON TOP of `SseStream` above, which stays
// completely unmodified in its own logic (only `new`'s validation moved
// into the free function `validate_sse_response`, called with identical
// arguments and identical error values). `SseStream::new`/`next` and their
// existing tests keep working whether or not any of this exists.

/// Configuration for [`Client::sse`]'s reconnecting stream.
///
/// Plain public fields, no `#[non_exhaustive]` (mirrors `Backoff` in
/// `http-ng-proto`, and `HeConfig` before it): a small value struct meant to
/// be built with a literal or `..Default::default()`, not something a
/// caller constructs through a builder of its own.
///
/// No `reconnect: bool` field — an earlier version of this had one, and
/// removing it was deliberate: whether reconnect is enabled at all is
/// decided at the TYPE level now, by whether [`SseBuilder::with_timer`] was
/// called (see its doc comment), not by a runtime flag that could disagree
/// with the type. Keeping both would have meant two things claiming to
/// control the same behavior with only one of them exercised by most
/// tests — the same shape of redundancy this crate's own mutation testing
/// on `ReconnectingSseStream` already caught once, elsewhere, this task.
#[derive(Debug, Clone, Copy)]
pub struct SseOptions {
    pub max_event_size: usize,
    /// The base retry policy, in the absence of a server-sent `retry:`
    /// field. See [`ReconnectingSseStream`]'s doc comment on `next` for how
    /// the two interact once the server does send one.
    ///
    /// **Inert without [`SseBuilder::with_timer`].** `SseOptions` is shared
    /// by both branches (`SseBuilder::connect` and
    /// `ReconnectingSseBuilder::connect`), and `max_event_size` applies to
    /// both — but a plain, non-reconnecting `SseStream` never reconnects at
    /// all, so it never reads `backoff`. Setting it and calling the plain
    /// `.connect()` isn't rejected (there's nothing wrong with the value
    /// itself, just no reconnect loop to apply it to), but it's silently
    /// unused — the one piece of "one struct feeding two type branches"
    /// residue the `reconnect: bool` removal didn't fully close.
    pub backoff: Backoff,
}

impl Default for SseOptions {
    fn default() -> Self {
        Self {
            max_event_size: http_ng_proto::sse::DEFAULT_MAX_EVENT_SIZE,
            backoff: Backoff::default(),
        }
    }
}

/// Builds either a plain [`SseStream`] (`connect`) or a reconnecting one
/// (`with_timer(..).connect()`). Returned by [`Client::sse`].
#[derive(Debug)]
pub struct SseBuilder<'a, T> {
    client: &'a Client<T>,
    url: String,
    headers: http::HeaderMap,
    options: SseOptions,
    /// The first build error, same pattern and same reason as
    /// `RequestBuilder::error` (`request.rs`): a silently dropped invalid
    /// header would be exactly the kind of silent no-op this project
    /// refuses to ship, and `RequestBuilder::header`'s own history (Task
    /// 13) is the direct precedent — its brief's reference code dropped an
    /// invalid pair with no `else` branch at all.
    error: Option<Error>,
}

impl<'a, T: Transport> SseBuilder<'a, T> {
    pub(crate) fn new(client: &'a Client<T>, url: &str) -> Self {
        Self {
            client,
            url: url.to_owned(),
            headers: http::HeaderMap::new(),
            options: SseOptions::default(),
            error: None,
        }
    }

    /// A header sent with the initial connection AND — if this builder
    /// goes on to [`with_timer`](Self::with_timer) — with every reconnect.
    /// The first invalid `(name, value)` pair wins and survives further
    /// calls — see `RequestBuilder::header`'s doc comment for the identical
    /// contract and the reasoning behind it.
    pub fn header(mut self, name: &str, value: &str) -> Self {
        if self.error.is_some() {
            return self;
        }
        match (
            name.parse::<http::HeaderName>(),
            value.parse::<http::HeaderValue>(),
        ) {
            (Ok(n), Ok(v)) => {
                self.headers.insert(n, v);
            }
            (Err(e), _) => self.error = Some(Error::new(ErrorKind::Other, e)),
            (_, Err(e)) => self.error = Some(Error::new(ErrorKind::Other, e)),
        }
        self
    }

    pub fn options(mut self, o: SseOptions) -> Self {
        self.options = o;
        self
    }

    /// Supplies the clock reconnect needs to wait out a backoff delay
    /// between attempts, and switches this builder from `SseStream`
    /// (single attempt) to [`ReconnectingSseStream`] (reconnects
    /// automatically). Returns [`ReconnectingSseBuilder`], which otherwise
    /// offers the same `header`/`options` calls before its own `connect`.
    ///
    /// **Why an explicit input, not something `http-ng` supplies on its
    /// own** — this crate tried the alternative first (an ambient,
    /// per-target clock built into `http-ng` itself, so `connect()` alone
    /// would reconnect with no timer argument anywhere) and it was
    /// rejected on review, for two reasons that both hold independently of
    /// each other:
    ///
    /// 1. **It puts per-target runtime code in the facade crate.** `http-ng`
    ///    is supposed to be the SAME code on every target, with the
    ///    *transport* swapped for the platform — that's the whole point of
    ///    `crates/http-ng-rt-pair-check`, which exists to fail the day a
    ///    `#[cfg]` shows up that only one target satisfies. A clock inside
    ///    `http-ng` needs a native branch and a browser branch, which is
    ///    exactly that `#[cfg]`. Checked directly: `http-ng-rt-pair-check`
    ///    depends on the runtime crates, not on `http-ng`, so it would not
    ///    even have caught this if it had landed.
    /// 2. **A `std::thread`-backed sleep compiles on `wasm32-wasip2` while
    ///    having no OS thread to actually hand out under stock `wasmtime`
    ///    at runtime** — a capability that LOOKS supported and silently
    ///    isn't, which is precisely the class of defect this project's
    ///    reviews exist to catch (it produced vertical 1's headline finding
    ///    and vertical 2's one blocking finding). An ambient clock in
    ///    `http-ng` would carry that lie into every crate that depends on
    ///    it, with no way for a caller to see it from the type signature.
    ///
    /// Requiring `Timer` here instead is not a quarantine violation: a
    /// caller asking for reconnect WITH exponential backoff is asking for
    /// timed behavior by definition, so the capability stating its own
    /// dependency is the honest shape — `Native<R: TcpConnect + Timer, ..>`
    /// already requires exactly this from `R`, and nobody has called that a
    /// leak of the backend contract. In practice a caller on `http-ng-rt-
    /// tokio` or `http-ng-rt-smol` already has a `Timer` in scope (`Tokio`/
    /// `Smol` both implement it) for the SAME reason their transport needed
    /// one — nothing new to plumb through. `http-ng`'s own `test-util`
    /// feature carries [`crate::mock::TestTimer`] for exactly this call, so
    /// reconnect stays testable on the bare `futures_executor` this crate's
    /// test suite uses everywhere, without a real runtime and without
    /// sleeping for real — `TestTimer::sleep` records the requested
    /// `Duration` and resolves immediately, so a test can assert the
    /// backoff actually computed the interval it should have, which a
    /// thread-backed clock could only do by really waiting.
    pub fn with_timer<Tm: Timer>(self, timer: Tm) -> ReconnectingSseBuilder<'a, T, Tm> {
        ReconnectingSseBuilder {
            builder: self,
            timer,
        }
    }

    /// A single connection attempt, exactly [`SseStream::new`]'s contract —
    /// no reconnect, because there is no timer to wait out a backoff delay
    /// with. For reconnect, add [`with_timer`](Self::with_timer) first.
    pub async fn connect(self) -> Result<SseStream<T::Body>, Error>
    where
        T::Body: HttpBody<Data = Bytes> + Unpin,
        <T::Body as HttpBody>::Error: std::error::Error + Send + Sync + 'static, // send-bound-exception: amendment-C1
        T::Error: Send + Sync + 'static, // send-bound-exception: amendment-C1
    {
        if let Some(e) = self.error {
            return Err(e);
        }
        open(
            self.client,
            &self.headers,
            &self.url,
            None,
            self.options.max_event_size,
        )
        .await
    }
}

/// Builds a [`ReconnectingSseStream`]. Returned by [`SseBuilder::with_timer`].
#[derive(Debug)]
pub struct ReconnectingSseBuilder<'a, T, Tm> {
    builder: SseBuilder<'a, T>,
    timer: Tm,
}

impl<'a, T: Transport, Tm: Timer> ReconnectingSseBuilder<'a, T, Tm> {
    /// Forwards to [`SseBuilder::header`] — see its doc comment.
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.builder = self.builder.header(name, value);
        self
    }

    /// Forwards to [`SseBuilder::options`] — see its doc comment.
    pub fn options(mut self, o: SseOptions) -> Self {
        self.builder = self.builder.options(o);
        self
    }

    /// Makes exactly one connection attempt — no retry, regardless of
    /// whether the failure would later be classified as retryable by the
    /// reconnect loop. Mirrors `SseStream::new`'s own contract (a failed
    /// construction is an `Err`, not a stream that starts and immediately
    /// ends): the very first connection is a decision the caller is
    /// actively waiting on, not a background retry loop that hasn't
    /// started yet. Reconnect (with backoff) only applies to a stream that
    /// was successfully opened at least once and later dropped — see
    /// `ReconnectingSseStream::next`.
    pub async fn connect(self) -> Result<ReconnectingSseStream<'a, T, Tm>, Error>
    where
        T::Body: HttpBody<Data = Bytes> + Unpin,
        <T::Body as HttpBody>::Error: std::error::Error + Send + Sync + 'static, // send-bound-exception: amendment-C1
        T::Error: Send + Sync + 'static, // send-bound-exception: amendment-C1
    {
        if let Some(e) = self.builder.error {
            return Err(e);
        }
        let inner = open(
            self.builder.client,
            &self.builder.headers,
            &self.builder.url,
            None,
            self.builder.options.max_event_size,
        )
        .await?;
        let cached_last_event_id = inner.last_event_id().map(str::to_owned);
        Ok(ReconnectingSseStream {
            client: self.builder.client,
            url: self.builder.url,
            headers: self.builder.headers,
            options: self.builder.options,
            timer: self.timer,
            cached_last_event_id,
            attempt: 0,
            server_retry: None,
            state: ReconnectState::Live(Box::new(inner)),
        })
    }
}

/// One (re)connection attempt: builds the request (base `Accept`, the
/// caller's own headers, and `Last-Event-ID` when there is a non-empty one
/// to send — see [`ReconnectingSseStream`]'s doc comment on `next` for why
/// "non-empty" matters), sends it through `client.execute` (so redirects,
/// timeouts, and the rest of `Client`'s stages apply exactly as they do to
/// an ordinary request), and validates the response with the SAME
/// `validate_sse_response` the plain `SseStream::new` uses.
async fn open<T>(
    client: &Client<T>,
    caller_headers: &http::HeaderMap,
    url: &str,
    last_event_id: Option<&str>,
    max_event_size: usize,
) -> Result<SseStream<T::Body>, Error>
where
    T: Transport,
    T::Body: HttpBody<Data = Bytes> + Unpin,
    <T::Body as HttpBody>::Error: std::error::Error + Send + Sync + 'static, // send-bound-exception: amendment-C1
    T::Error: Send + Sync + 'static, // send-bound-exception: amendment-C1
{
    let uri = crate::config::effective_uri(client.config().base_url.as_ref(), url)?;

    let mut headers = caller_headers.clone();
    headers.insert(http::header::ACCEPT, http::HeaderValue::from_static(MIME));
    // WHATWG: `Last-Event-ID` is sent only if it's non-empty.
    // `reqwest-eventsource` sends an empty header on the first reconnect,
    // which the spec forbids — see the design doc's §4.10. `None` (never
    // saw an `id:` field at all) and `Some("")` (saw one, and its value
    // was empty — WHATWG's rule for CLEARING the last event ID, not the
    // same as never having set it) both take this branch and both send
    // nothing; they differ only in what `ReconnectingSseStream::
    // last_event_id()` reports, not in request behavior.
    if let Some(id) = last_event_id
        && !id.is_empty()
    {
        let v = http::HeaderValue::from_str(id).map_err(|e| Error::new(ErrorKind::Other, e))?;
        headers.insert(http::HeaderName::from_static("last-event-id"), v);
    }

    let mut req = http::Request::new(RequestBody::Empty);
    *req.method_mut() = http::Method::GET;
    *req.uri_mut() = uri.clone();
    *req.headers_mut() = headers;

    let resp = client.execute(req).await?;
    let decoder =
        SseDecoder::new_with_last_event_id(max_event_size, last_event_id.map(str::to_owned));
    SseStream::new_with_decoder(Response::new(resp, uri), decoder)
}

/// Whether a failure is worth reconnecting over, or ends the stream for
/// good — decided once, here, and consulted from every place `next()` can
/// hit a failure (a live connection breaking, or a reconnect attempt's own
/// `open` failing), so the two paths can't silently drift apart.
///
/// TERMINAL (this function returns `false`) — non-transient by
/// construction, retrying changes nothing:
/// - `Decode`: this crate's only source of a `Decode` `ErrorKind` on this
///   path is `SseDecoder`'s own event-size limit, and that is documented
///   as fatal and not retried (`http-ng-proto/src/sse/decode.rs`,
///   `DEFAULT_MAX_EVENT_SIZE`'s doc comment) — resurrecting it via
///   reconnect would undo exactly what vertical 1 hardened. A content-type
///   rejection from `validate_sse_response` is ALSO `Decode`, for the same
///   reason: the server is sending something that isn't SSE, and a delay
///   won't change its `Content-Type` header.
/// - `Status`: `validate_sse_response`'s non-200 rejection. WHATWG treats
///   any non-200 (204 very much included) as a permanent connection
///   failure, not a transient one.
/// - `Unsupported`: a capability the transport fundamentally doesn't have
///   (e.g. a `Timeouts` setting `check_timeouts_supported` rejects) —
///   waiting and asking again doesn't grow the capability.
/// - `Cancelled`: the runtime itself is shutting down (see this kind's own
///   doc comment in `http-ng-core`) — reconnecting into a shutting-down
///   process is pointless at best.
/// - `Redirect`: too many hops or an unparsable `Location` from
///   `Client::execute`'s own redirect stage — a policy/config problem, not
///   a network blip.
///
/// Everything else — today `Resolve`, `Connect`, `Tls`, `Timeout(_)`,
/// `Body`, `Other`, and, since `ErrorKind` is `#[non_exhaustive]` (amendment
/// C6: only the defining crate may exhaustively match it), any kind added
/// later — is treated as RETRYABLE. This is a deliberate default-open
/// choice, not an oversight: it matches the browser's own `EventSource`,
/// which keeps retrying forever on any transport-level hiccup unless
/// explicitly told to stop (a terminal status/content-type, or the
/// application closing it) — and it's not equivalent to "retries forever
/// no matter what": `Backoff::max_attempts` (still `None`/unlimited only by
/// the caller's own explicit choice, per Task 6) bounds it, and a caller
/// that wants a NEW `ErrorKind` treated as terminal can already do so today
/// by using the plain `SseStream` (`SseBuilder::connect` with no
/// `with_timer`) and handling every `Err` itself instead of reconnecting.
fn is_retryable(kind: &ErrorKind) -> bool {
    !matches!(
        kind,
        ErrorKind::Decode
            | ErrorKind::Status
            | ErrorKind::Unsupported
            | ErrorKind::Cancelled
            | ErrorKind::Redirect
    )
}

/// A fresh, uniform jitter draw for `Backoff::delay`, over the CLOSED
/// interval `[0.0, 1.0]` — not the half-open `[0.0, 1.0)` an earlier
/// version of this comment claimed. Verified empirically, not assumed: an
/// `f64`'s 53-bit mantissa can't represent `u64::MAX` (`2^64 - 1`)
/// exactly, so `u64::MAX as f64` rounds UP to `2^64` — and so does any u64
/// draw within about 2^12 of `u64::MAX` (the local rounding granularity
/// near that magnitude), so numerator and denominator can come out equal
/// and the ratio exactly `1.0`, at roughly 1-in-10^16 odds per draw. This
/// is within `Backoff::delay`'s own documented domain (it treats
/// `jitter >= 1.0`, not just `> 1.0`, as "clamp to full reduction" — see
/// `backoff.rs`'s `jitter_at_or_above_one_clamps_to_full_reduction`), so
/// it was never a correctness bug — but a caller reading this specific
/// doc comment deserves the true range, not the tidier-looking wrong one.
///
/// Randomness isn't a seam anyone has defined in this project (unlike
/// `Timer`, which reconnect takes as an explicit input — see
/// `SseBuilder::with_timer`'s doc comment for why the two are treated
/// differently), so `http-ng` sources this itself rather than asking the
/// reconnect caller for an RNG, the same way `http-ng-proto`'s
/// `Backoff::delay` already expects SOME caller to supply `jitter`.
///
/// `getrandom` failing is a documented but exceedingly rare condition (the
/// OS's entropy source is unavailable). Silently discarding the error
/// (`.unwrap_or(())`) would leave the buffer at all-zeros with no record
/// that anything went wrong — this project's "no silent no-ops" rule, so
/// the fallback is written out instead: an all-zeros draw maps to
/// `jitter = 0.0`, `Backoff`'s OWN documented resolution for an
/// out-of-domain jitter (see `backoff.rs`'s
/// `nan_jitter_does_not_panic_and_is_treated_as_no_reduction`) — no
/// reduction, the conservative (slower, not faster) direction, so a
/// starved entropy source degrades reconnect into un-jittered exponential
/// backoff rather than into hammering the server.
fn jitter() -> f64 {
    let mut buf = [0u8; 8];
    if getrandom::fill(&mut buf).is_err() {
        return 0.0;
    }
    (u64::from_le_bytes(buf) as f64) / (u64::MAX as f64)
}

/// The delay before the next (re)connect attempt — pure, and taking
/// `jitter` as an explicit parameter rather than reading `jitter()` (above)
/// itself, for exactly the reason `Backoff::delay` itself takes `jitter` as
/// a parameter (`http-ng-proto/src/backoff.rs`'s own doc comment): a
/// function that reads live entropy internally can only be tested
/// probabilistically. Factored out of `next()`'s `Disconnected`
/// arm specifically so the "server's `retry:` REPLACES `options.backoff.
/// base`" decision (see `ReconnectingSseStream::next`'s doc comment) can be
/// pinned with `jitter = 0.0` in a deterministic unit test below, instead
/// of only being checkable end-to-end through real, non-deterministic
/// sleeps (`tests/sse_reconnect.rs`'s `honours_server_sent_retry_over_the_
/// policy` does that too, but a real jitter draw can — rarely, not
/// impossibly — land close enough to `1.0` to make even a wrong `base`
/// finish quickly by luck; only this unit test is actually guaranteed to
/// catch a regression here).
fn effective_delay(
    backoff: &Backoff,
    server_retry: Option<Duration>,
    attempt: u32,
    jitter: f64,
) -> Option<Duration> {
    let base = server_retry.unwrap_or(backoff.base);
    Backoff { base, ..*backoff }.delay(attempt, jitter)
}

/// Surfaced exactly once, via `next()`, when `Backoff::delay` returns
/// `None` (the configured `max_attempts` is exhausted): "the stream ended"
/// (a clean, un-reconnected EOF, or a terminal error) and "we gave up
/// retrying" must be distinguishable, not two different shapes of silence.
/// `downcast_ref::<ReconnectExhausted>()` on `Error::source()` is how a
/// caller tells them apart — the same idiom as `mock::QueueEmpty` and
/// `client::TooMany`.
#[derive(Debug, thiserror::Error)]
#[error("gave up reconnecting the SSE stream after {attempts} attempt(s)")]
struct ReconnectExhausted {
    /// How many reconnect attempts were actually made before giving up —
    /// NOT `max_attempts` restated: `Backoff::max_attempts` is the ceiling,
    /// this is what was observed, useful for a log line without re-reading
    /// the configured policy.
    attempts: u32,
}
/// Where a [`ReconnectingSseStream`] currently stands. `B` is `T::Body` —
/// the SAME concrete body type every (re)connection produces, since every
/// (re)connection goes through the same `Client<T>`.
#[derive(Debug)]
enum ReconnectState<B> {
    /// A live, currently-open `SseStream`, forwarded to almost unchanged.
    /// Boxed: `Disconnected`/`Terminated` carry no data at all, and without
    /// indirection the whole enum would be padded out to `SseStream`'s
    /// size for every `ReconnectingSseStream`, live connection or not
    /// (`clippy::large_enum_variant`).
    Live(Box<SseStream<B>>),
    /// The previous connection ended (cleanly or on a retryable error): the
    /// next `next()` call will wait out a backoff delay, then attempt to
    /// reopen. Reconnect is always enabled for this type — a
    /// `ReconnectingSseStream` only exists because [`SseBuilder::with_timer`]
    /// was called, which is the only place that decision is made now (no
    /// runtime flag to disagree with it — see `SseOptions`'s doc comment).
    Disconnected,
    /// Forever. Set on a terminal error, or on `Backoff` exhaustion.
    Terminated,
}

/// A reconnecting SSE stream: on a clean end of stream or a retryable
/// failure, resends the request (filling in `Last-Event-ID` when there's a
/// non-empty one to send) after a jittered backoff delay, waited out on
/// `Tm`. Built by [`ReconnectingSseBuilder::connect`], reachable from
/// [`Client::sse`]`.with_timer(..)`.
#[derive(Debug)]
pub struct ReconnectingSseStream<'a, T: Transport, Tm> {
    client: &'a Client<T>,
    url: String,
    headers: http::HeaderMap,
    options: SseOptions,
    timer: Tm,
    /// The last event ID observed so far, kept OUTSIDE the live
    /// `SseStream`'s own decoder so it survives the decoder being replaced
    /// on every reconnect. Snapshotted from the live stream's
    /// `last_event_id()` every time the live connection is about to be
    /// torn down (on a retryable error, a clean EOF, or a terminal error —
    /// the snapshot is harmless even when about to terminate, and cheap).
    /// A fresh reconnect DOES seed this into the new `SseDecoder` — `open`
    /// passes it to `SseDecoder::new_with_last_event_id`, so the decoder
    /// never starts id-less on a reconnect the way a first connect does
    /// (see `open`'s own doc comment, and `last_event_id()` below, which
    /// depends on this seeding for its own correctness). This field's job
    /// is twofold: it's the value `open` actually seeds the decoder AND
    /// sends as `Last-Event-ID` with, and it's what answers
    /// `last_event_id()` while disconnected/terminated, when there is no
    /// live decoder to ask at all.
    cached_last_event_id: Option<String>,
    /// Zero-based, matches `Backoff::delay`'s own convention. Reset to `0`
    /// on every SUCCESSFUL (re)open — growth is a property of consecutive
    /// failures, not of the stream's lifetime as a whole.
    attempt: u32,
    /// The most recently seen server `retry:` value, if any — see `next`'s
    /// doc comment for how it interacts with `options.backoff`.
    server_retry: Option<Duration>,
    state: ReconnectState<T::Body>,
}

impl<'a, T, Tm> ReconnectingSseStream<'a, T, Tm>
where
    T: Transport,
    T::Body: HttpBody<Data = Bytes> + Unpin,
    <T::Body as HttpBody>::Error: std::error::Error + Send + Sync + 'static, // send-bound-exception: amendment-C1
    T::Error: Send + Sync + 'static, // send-bound-exception: amendment-C1
    Tm: Timer,
{
    /// The last event ID seen, across any number of reconnects — the value
    /// that would go out as `Last-Event-ID` on the NEXT reconnect (subject
    /// to the same "only if non-empty" rule `open` applies).
    ///
    /// Reads through to the live connection's own decoder while connected,
    /// and falls back to the cached snapshot while disconnected or
    /// terminated (there is no live decoder to ask at all in that state).
    /// This is safe — not just "usually right" — because `open` ALWAYS
    /// seeds the new `SseDecoder` from `cached_last_event_id` via
    /// `SseDecoder::new_with_last_event_id` (see `open`'s own doc
    /// comment): a freshly (re)opened decoder starts AT LEAST as current as
    /// the cache, and only ever moves forward from there via its own new
    /// `id:` lines. Per WHATWG the last event ID buffer is a property of
    /// the EventSource as a whole, not of one connection — the seeding is
    /// what makes that true here, this accessor just reads whichever
    /// source currently has the freshest value on hand.
    pub fn last_event_id(&self) -> Option<&str> {
        match &self.state {
            ReconnectState::Live(s) => s.last_event_id(),
            ReconnectState::Disconnected | ReconnectState::Terminated => {
                self.cached_last_event_id.as_deref()
            }
        }
    }

    /// The next event. On a clean end of stream or a retryable failure
    /// (`is_retryable`), this reconnects internally — after a backoff delay
    /// waited out on `Tm` — rather than ending the stream or surfacing the
    /// failure: a caller iterating this for `SseEvent`s should not see a
    /// spurious `Err` for a hiccup that was automatically recovered from,
    /// matching real `EventSource`'s behavior of retrying silently in the
    /// background. A TERMINAL failure surfaces as `Err` exactly once, then
    /// the stream is over for good — the same fatal-then-forever-`None`
    /// contract `SseStream` itself already makes, extended across
    /// reconnects rather than broken by them.
    ///
    /// **The server's `retry:` field REPLACES `options.backoff.base` for
    /// every delay computed from the moment it's received onward** (until
    /// a newer `retry:` replaces it again, or the stream ends) — not a
    /// ceiling, not a floor, not ignored. `retry:` is WHATWG's name for
    /// "the reconnection time" — the server's own stated preference for how
    /// long to wait — and a policy default configured before any server was
    /// ever talked to has no special claim over it: an application that
    /// configures `backoff.base = 30s` and a server that answers with
    /// `retry: 100` are both "following a rule", and honoring the more
    /// specific, more recent, server-supplied one is what "replaces" means.
    /// Exponential growth (`2^attempt`), jitter, and `options.backoff.max`
    /// still all apply on top of the server's value — a server can shorten
    /// the base delay, it can't lift the ceiling this client itself
    /// configured, and repeated failures still back off rather than
    /// hammering at a server-requested fixed rate forever.
    pub async fn next(&mut self) -> Option<Result<SseEvent, Error>> {
        loop {
            match &mut self.state {
                ReconnectState::Terminated => return None,
                ReconnectState::Live(inner) => match inner.next().await {
                    Some(Ok(event)) => {
                        if let SseEvent::Retry(d) = event {
                            self.server_retry = Some(d);
                        }
                        return Some(Ok(event));
                    }
                    Some(Err(err)) => {
                        self.cached_last_event_id = inner.last_event_id().map(str::to_owned);
                        if !is_retryable(err.kind()) {
                            self.state = ReconnectState::Terminated;
                            return Some(Err(err));
                        }
                        self.state = ReconnectState::Disconnected;
                    }
                    None => {
                        self.cached_last_event_id = inner.last_event_id().map(str::to_owned);
                        self.state = ReconnectState::Disconnected;
                    }
                },
                ReconnectState::Disconnected => {
                    let jitter = jitter();
                    let Some(delay) = effective_delay(
                        &self.options.backoff,
                        self.server_retry,
                        self.attempt,
                        jitter,
                    ) else {
                        let attempts = self.attempt;
                        self.state = ReconnectState::Terminated;
                        return Some(Err(Error::new(
                            ErrorKind::Other,
                            ReconnectExhausted { attempts },
                        )));
                    };
                    self.attempt = self.attempt.saturating_add(1);
                    self.timer.sleep(delay).await;

                    match open(
                        self.client,
                        &self.headers,
                        &self.url,
                        self.cached_last_event_id.as_deref(),
                        self.options.max_event_size,
                    )
                    .await
                    {
                        Ok(inner) => {
                            self.attempt = 0;
                            // Not re-snapshotting `cached_last_event_id`
                            // from `inner` here is a deliberate simplicity
                            // choice, not a correctness requirement: `open`
                            // seeds every new decoder FROM
                            // `cached_last_event_id` (see `last_event_id`'s
                            // doc comment), so at this exact point the two
                            // already agree — resyncing would be a no-op.
                            // An earlier version of this code DID clobber
                            // it here, before the decoder was seedable;
                            // that bug is now structurally impossible to
                            // reintroduce by accident, since `open` is the
                            // only place `cached_last_event_id` feeds a new
                            // decoder, and it always feeds it forward, never
                            // back.
                            self.state = ReconnectState::Live(Box::new(inner));
                        }
                        Err(err) => {
                            if !is_retryable(err.kind()) {
                                self.state = ReconnectState::Terminated;
                                return Some(Err(err));
                            }
                            // Stay Disconnected: loop around, backoff grows
                            // from the already-incremented `attempt`.
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod reconnect_tests {
    use super::*;

    fn backoff() -> Backoff {
        Backoff {
            base: Duration::from_secs(1),
            max: Duration::from_secs(30),
            max_attempts: None,
        }
    }

    // Review round 1, Minor-3: `Decode`, `Status`, and `Unsupported` were
    // each covered by an integration test that observes `is_retryable`'s
    // effect end-to-end; `Cancelled` and `Redirect` were not — deleting
    // both from the `matches!` at once (mutation M4) left the whole suite
    // green. Direct unit tests on the function itself, mirroring how
    // `effective_delay` below is tested, rather than another end-to-end
    // detour through `MockTransport` for two one-line facts.
    #[test]
    fn is_retryable_treats_cancelled_as_terminal() {
        assert!(
            !is_retryable(&ErrorKind::Cancelled),
            "a shutting-down runtime is not a reason to keep retrying"
        );
    }

    #[test]
    fn is_retryable_treats_redirect_as_terminal() {
        assert!(
            !is_retryable(&ErrorKind::Redirect),
            "too many hops or a bad Location is a policy problem, not a network blip"
        );
    }

    #[test]
    fn no_server_retry_uses_the_configured_base() {
        assert_eq!(
            effective_delay(&backoff(), None, 0, 0.0),
            Some(Duration::from_secs(1))
        );
    }

    /// The decision this project's review culture asked to be justified,
    /// not just implemented: the server's `retry:` REPLACES the configured
    /// base outright — it isn't maxed against it (a large configured base
    /// would then defeat a server asking for a fast retry) and it isn't
    /// ignored. `jitter = 0.0` makes this exact, not approximate.
    #[test]
    fn server_retry_replaces_the_configured_base_not_maxed_against_it() {
        let slow_policy = Backoff {
            base: Duration::from_secs(30),
            max: Duration::from_secs(30),
            max_attempts: None,
        };
        assert_eq!(
            effective_delay(&slow_policy, Some(Duration::from_millis(100)), 0, 0.0),
            Some(Duration::from_millis(100)),
            "a server-requested 100ms must win over a configured 30s base — \
             `.max(..)` would have produced 30s here instead"
        );
    }

    /// The server's value is a new BASE, not a new fixed delay: exponential
    /// growth on repeated failures still applies on top of it.
    #[test]
    fn server_retry_still_grows_exponentially_on_repeated_attempts() {
        let server_retry = Some(Duration::from_millis(10));
        assert_eq!(
            effective_delay(&backoff(), server_retry, 0, 0.0),
            Some(Duration::from_millis(10))
        );
        assert_eq!(
            effective_delay(&backoff(), server_retry, 2, 0.0),
            Some(Duration::from_millis(40)),
            "2^2 = 4x the server-supplied base"
        );
    }

    /// The client's own `max` ceiling is NOT lifted by a server asking for
    /// a long delay: safety rails are the client's, not negotiable away by
    /// whatever the server sends.
    #[test]
    fn the_configured_max_still_caps_a_large_server_retry() {
        let capped = Backoff {
            base: Duration::from_secs(1),
            max: Duration::from_secs(5),
            max_attempts: None,
        };
        assert_eq!(
            effective_delay(&capped, Some(Duration::from_secs(999)), 0, 0.0),
            Some(Duration::from_secs(5)),
            "a server asking for a 999s wait is still capped at the client's own 5s max"
        );
    }

    #[test]
    fn jitter_still_scales_the_delay_down() {
        assert_eq!(
            effective_delay(&backoff(), None, 0, 0.5),
            Some(Duration::from_millis(500))
        );
    }

    #[test]
    fn max_attempts_exhaustion_still_propagates_as_none() {
        let limited = Backoff {
            max_attempts: Some(2),
            ..backoff()
        };
        assert!(effective_delay(&limited, None, 1, 0.0).is_some());
        assert!(effective_delay(&limited, None, 2, 0.0).is_none());
    }

    /// `ReconnectingSseStream::attempt` resets to `0` on every SUCCESSFUL
    /// (re)open, not just once — growth is a property of CONSECUTIVE
    /// failures, not of the stream's lifetime as a whole. Checked directly
    /// against the private field (this test lives in the same module as
    /// the struct) rather than inferred indirectly through timing: a
    /// wall-clock-based version of this test would be exactly the kind of
    /// probabilistic, jitter-dependent test `effective_delay`'s own tests
    /// above were extracted to avoid.
    #[test]
    #[cfg(feature = "test-util")]
    fn attempt_resets_to_zero_after_a_successful_reopen_not_just_once() {
        use crate::client::Client;
        use crate::mock::{MockTransport, TestTimer};

        let m = MockTransport::new();
        // Connection 1: one event, then a retryable body error.
        m.push_response_frames_then_error(
            http::Response::builder()
                .status(200)
                .header("content-type", "text/event-stream")
                .body(vec!["data: a\n\n"])
                .unwrap(),
            Error::new(ErrorKind::Body, std::io::Error::other("down once")),
        );
        // Connection 2 (the reconnect): opens fine, THEN also breaks —
        // this second failure is what `attempt` at the time of computing
        // ITS delay actually proves: reset to 0 (correct) or carried over
        // at 1+ (the mutation this test exists to catch).
        m.push_response_frames_then_error(
            http::Response::builder()
                .status(200)
                .header("content-type", "text/event-stream")
                .body(vec!["data: b\n\n"])
                .unwrap(),
            Error::new(ErrorKind::Body, std::io::Error::other("down twice")),
        );

        let c = Client::builder(m).build().unwrap();
        let opts = SseOptions {
            backoff: Backoff {
                base: Duration::from_millis(1),
                max: Duration::from_millis(5),
                max_attempts: None,
            },
            ..Default::default()
        };
        let mut s = futures_executor::block_on(
            c.sse("https://a/stream")
                .options(opts)
                .with_timer(TestTimer::new())
                .connect(),
        )
        .unwrap();

        // Call 1: connection 1's decoder already has "a" ready before its
        // error is even reached — this returns immediately, WITHOUT ever
        // touching `Disconnected`. `attempt` is untouched here (still its
        // initial 0), so checking it after only this call would be
        // vacuous — it would read 0 whether or not the reset logic exists
        // at all, since nothing has incremented it yet.
        let a = futures_executor::block_on(s.next()).unwrap().unwrap();
        assert_eq!(
            a,
            SseEvent::Message {
                event: None,
                data: "a".into(),
                id: None
            }
        );

        // Call 2 is the one that actually exercises the reset: internally,
        // ONE call to `next()` here hits connection 1's error (retryable,
        // not surfaced), computes a delay at `attempt == 0`, increments
        // `attempt` to `1`, sleeps, reopens connection 2 successfully
        // (which is where `attempt` resets back to `0`), and THEN returns
        // "b" from the new live connection — all before this `.await`
        // resolves.
        let b = futures_executor::block_on(s.next()).unwrap().unwrap();
        assert_eq!(
            b,
            SseEvent::Message {
                event: None,
                data: "b".into(),
                id: None
            }
        );
        assert_eq!(
            s.attempt, 0,
            "attempt must be back to 0 right after the successful reopen, \
             not left at 1 from the failure that preceded it"
        );
    }
}
