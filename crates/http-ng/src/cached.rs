//! The response cache wired into `Client`: the body that can come from the
//! store instead of the wire, and the plan that decides which.
//!
//! # Why there is a wrapper at all, and why it is always there
//!
//! A cache hit has no transport body — there was no exchange — so
//! `Client::run` cannot go on returning `http::Response<T::Body>`.
//! [`Cached`] is the type that can be either, and it is the third of the
//! three wrappers in [`crate::ClientBody`] for the same reason the other
//! two are always present: **a type cannot appear and disappear with a
//! runtime value.** Without the `cache` feature its two extra fields are
//! `#[cfg]`-ed away entirely, exactly as `decompress::Decoder`'s variants
//! are, so a build that wants no cache carries a newtype and one `Option`
//! test per frame rather than a stub that exists to be absent.
//!
//! # Recording, rather than buffering before the head
//!
//! A response that may be stored is **teed**: its frames go to the caller
//! and into a buffer at the same time, and the buffer is committed to the
//! store when the body ends cleanly. The alternative — collect the body
//! inside `Client::execute` and hand back the bytes — would delay the
//! response head by however long the body takes, which for a cacheable
//! multi-megabyte download is exactly wrong, and would make a streaming
//! consumer of a cacheable response impossible.
//!
//! Three ways a recording is abandoned, all of them silent by design and
//! each with a reason:
//!
//! - **the caller drops the body**, so nothing is stored — a partial entry
//!   served later is indistinguishable from a complete one;
//! - **the body errors**, likewise;
//! - **a trailer frame arrives.** RFC 9111 has nothing to say about
//!   storing trailers and this cache stores a status, headers and bytes;
//!   keeping the entry and dropping the trailers would serve a message
//!   that is not the one that arrived.
//!
//! The fourth is not silent to the store: a body over
//! [`Limits::max_body_bytes`](http_ng_cache::Limits) stops being recorded
//! the moment it passes the bound, and streams on untouched.
//!
//! # What the cache is *below*
//!
//! `ClientBody` is `Decompressed<Deadline<Cached<B>, Tm>>`, so the
//! decompressor is **outside** the cache: what is recorded is what the
//! transport handed over, still carrying its `Content-Encoding`, and a
//! stored response is decoded on the way out by the same
//! `decompress::decoder_for` call that decodes a fresh one. Recording the
//! decoded bytes instead would have meant either relabelling the response
//! — a claim about a transformation the cache did not make — or decoding
//! it twice; and it would have broken `Vary: Accept-Encoding`, whose
//! stored variant is keyed on the coding actually asked for.
//!
//! The deadline is outside it too, which costs a cache hit nothing: a hit
//! yields its one frame on the first poll, and a bound it cannot exceed is
//! not a bound that had to be excluded.

use bytes::Bytes;
use std::pin::Pin;
use std::task::{Context, Poll};

/// A response body that came off the wire, or out of the store.
///
/// The third wrapper of [`crate::ClientBody`] — see this module's doc
/// comment for why it is always present and what it costs when nothing is
/// cached.
#[derive(Debug)]
pub struct Cached<B> {
    /// The transport's body. `None` for a cache hit, which had no
    /// exchange to have one.
    inner: Option<B>,
    /// The stored bytes, handed over as a single frame before `inner` is
    /// polled at all. Only ever `Some` for a hit, where `inner` is `None`.
    #[cfg(feature = "cache")]
    stored: Option<Bytes>,
    /// **Boxed, and the argument is `decompress::Decoder`'s verbatim** —
    /// which is worth knowing because it was written down there, ignored
    /// here, and then reproduced as a defect.
    ///
    /// `Cached` wraps EVERY response body this client hands back, recorded
    /// or not, and is moved by value with it; a `Recorder` unboxed is
    /// ~230 bytes of `Storing` — a `HeaderMap`, a `Key`, a `Selector` —
    /// carried by every plain uncompressed response that will never be
    /// stored. Measured through `Client::execute`'s future, which holds
    /// several of these at once: **4,040 bytes before this feature, 4,984
    /// unboxed, 4,152 boxed.** The unboxed version did not merely cost
    /// memory: `http-ng-native`'s `checkout_walks_past_a_dead_connection_
    /// to_a_live_one`, a pool test in another crate that happens to drive
    /// an `http_ng::Client`, **overflowed its stack** and aborted.
    #[cfg(feature = "cache")]
    recorder: Option<Box<Recorder>>,
}

impl<B> Cached<B> {
    /// The ordinary case: whatever the transport sent, untouched.
    pub(crate) fn live(inner: B) -> Self {
        Self {
            inner: Some(inner),
            #[cfg(feature = "cache")]
            stored: None,
            #[cfg(feature = "cache")]
            recorder: None,
        }
    }
}

#[cfg(feature = "cache")]
impl<B> Cached<B> {
    /// A body that is entirely the store's.
    pub(crate) fn from_store(bytes: Bytes) -> Self {
        Self {
            inner: None,
            stored: Some(bytes),
            recorder: None,
        }
    }

    /// The transport's body, teed into `recorder` as it is read.
    pub(crate) fn recording(inner: B, recorder: Recorder) -> Self {
        Self {
            inner: Some(inner),
            stored: None,
            recorder: Some(Box::new(recorder)),
        }
    }
}

impl<B> http_body::Body for Cached<B>
where
    B: http_body::Body<Data = Bytes> + Unpin,
{
    type Data = Bytes;
    /// The transport's own error, unchanged.
    ///
    /// Not a wider type: nothing this wrapper does can fail. A body that
    /// exceeds the store's bound stops being recorded and goes on being
    /// delivered, and a store that refuses an entry has refused a
    /// *storage* decision, which is not the caller's exchange going wrong.
    type Error = B::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, B::Error>>> {
        // `B: Unpin`, so no projection and no `unsafe` — the same line
        // `Deadline` stands on one wrapper out.
        let this = self.get_mut();

        #[cfg(feature = "cache")]
        if let Some(bytes) = this.stored.take() {
            // An empty stored body falls through rather than yielding an
            // empty frame: `Response::collect` would cope either way, but
            // a caller reading frames would see one that carries nothing,
            // which no transport here produces.
            if !bytes.is_empty() {
                return Poll::Ready(Some(Ok(http_body::Frame::data(bytes))));
            }
        }

        let Some(inner) = this.inner.as_mut() else {
            return Poll::Ready(None);
        };
        let polled = Pin::new(inner).poll_frame(cx);

        #[cfg(feature = "cache")]
        match &polled {
            Poll::Ready(Some(Ok(frame))) => match frame.data_ref() {
                Some(data) => {
                    if let Some(r) = this.recorder.as_mut()
                        && !r.push(data)
                    {
                        this.recorder = None;
                    }
                }
                // A trailer frame: see the module doc comment.
                None => this.recorder = None,
            },
            Poll::Ready(Some(Err(_))) => this.recorder = None,
            Poll::Ready(None) => {
                if let Some(r) = this.recorder.take() {
                    r.commit();
                }
            }
            Poll::Pending => {}
        }

        polled
    }

    fn is_end_stream(&self) -> bool {
        #[cfg(feature = "cache")]
        if self.stored.is_some() {
            return false;
        }
        self.inner
            .as_ref()
            .is_none_or(http_body::Body::is_end_stream)
    }

    fn size_hint(&self) -> http_body::SizeHint {
        #[cfg(feature = "cache")]
        if let Some(bytes) = self.stored.as_ref() {
            return http_body::SizeHint::with_exact(bytes.len() as u64);
        }
        match self.inner.as_ref() {
            Some(b) => b.size_hint(),
            None => http_body::SizeHint::with_exact(0),
        }
    }
}

/// A response being copied into the store as the caller reads it.
#[cfg(feature = "cache")]
#[derive(Debug)]
pub(crate) struct Recorder {
    cache: Cache,
    storing: http_ng_cache::Storing,
    buffered: Vec<u8>,
}

#[cfg(feature = "cache")]
impl Recorder {
    pub(crate) fn new(cache: Cache, storing: http_ng_cache::Storing) -> Self {
        Self {
            cache,
            storing,
            buffered: Vec::new(),
        }
    }

    /// Takes a copy of one data frame. `false` means the bound is passed
    /// and this recorder is finished with — the caller drops it and the
    /// body streams on.
    ///
    /// The check is `>` on the running total rather than a reservation
    /// against `Content-Length`, because the ordinary case over HTTP/2 and
    /// chunked HTTP/1.1 declares no length at all. `storing` refuses a
    /// declared length over the bound before a byte reaches here; this is
    /// the other half of the same limit and neither implies the other.
    fn push(&mut self, data: &Bytes) -> bool {
        if self.buffered.len() as u64 + data.len() as u64 > self.storing.max_body_bytes() {
            return false;
        }
        self.buffered.extend_from_slice(data);
        true
    }

    /// The body ended cleanly: hand it to the store.
    ///
    /// The store's refusal is dropped, and deliberately — one uncacheable
    /// response must not fail an exchange that has already succeeded, the
    /// same call `CookieJar::store_response`'s contract makes for a
    /// malformed `Set-Cookie`. The reasons are typed
    /// (`http_ng_cache::NotStored`) for whoever drives the cache directly.
    fn commit(self) {
        let _ = lock(&self.cache).store(self.storing, Bytes::from(self.buffered));
    }
}

/// The cache a `Client` holds, shared with every clone of it and with
/// every recording body it has handed out.
///
/// `Mutex` and not `RefCell`, for the reason the cookie jar gives one
/// field up: a `Client` is meant to cross a `tokio::spawn`, and `!Sync`
/// here would take that away from every client whether or not it caches.
/// The lock is held across no `.await` at all — every method on
/// `HttpCache` is a pure function of the store, the message and a `now`,
/// which is what the sans-io shape of `http-ng-cache` buys here.
///
/// **The store is a caller's choice and is not a type parameter**, which
/// is [`crate::AnyStore`]'s whole subject: `HttpCache<S>` here would put
/// `S` on this type, on `Client`, and — because a recording body holds one
/// — on the public `ClientBody` alias, whose arity is not something a
/// feature nobody in a graph asked for should change. Erasing at the store
/// leaves every arity fixed and every method of `HttpCache` reachable. The
/// cookie jar's list is the same shape one field up, for the same reason.
#[cfg(feature = "cache")]
pub(crate) type Cache = std::sync::Arc<std::sync::Mutex<http_ng_cache::HttpCache<crate::AnyStore>>>;

/// Locks the cache, recovering from poisoning rather than propagating it —
/// see [`crate::Client::cache`] for why a poisoned cache is still a usable
/// one.
#[cfg(feature = "cache")]
pub(crate) fn lock(
    c: &Cache,
) -> std::sync::MutexGuard<'_, http_ng_cache::HttpCache<crate::AnyStore>> {
    c.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// What the cache decided about a hop, carried from before the request
/// goes out to after the answer comes back.
///
/// `None` is an ordinary send. `Some` says this hop is a **revalidation**
/// of that entry, and is what makes a `304` mean *serve the stored body*
/// rather than *hand the caller a bodyless 304*.
#[cfg(feature = "cache")]
#[derive(Debug, Default)]
pub(crate) struct Plan(pub(crate) Option<Box<Revalidating>>);

#[cfg(feature = "cache")]
#[derive(Debug)]
pub(crate) struct Revalidating {
    pub(crate) key: http_ng_cache::Key,
    pub(crate) stale: http_ng_cache::StoredResponse,
}

/// The twin without the feature: there is exactly one plan, and it carries
/// nothing.
///
/// A separate declaration rather than a `#[cfg]` inside `Client::run`, and
/// the same choice `attach_cookies` makes: the call site says what happens
/// and when, and burying it in a conditional would put the per-hop
/// reasoning behind a feature flag too.
#[cfg(not(feature = "cache"))]
#[derive(Debug, Default)]
pub(crate) struct Plan;

/// A `504` this client made, for RFC 9111 §5.2.1.7's `only-if-cached` with
/// nothing to serve.
///
/// A response and not an `Error`, because that is what the RFC asks for
/// and because it is the honest shape: the caller said *do not go to the
/// network*, and this is the answer to the request they made, not a
/// failure of it. `Response::status()` reads `504` and the body is empty.
#[cfg(feature = "cache")]
pub(crate) fn only_if_cached_miss<B>() -> http::Response<Cached<B>> {
    let mut resp = http::Response::new(Cached::from_store(Bytes::new()));
    *resp.status_mut() = http::StatusCode::GATEWAY_TIMEOUT;
    resp
}

/// A stored response, as an `http::Response` a caller cannot tell from a
/// live one except by its `Age`.
///
/// The version is the stored one — see
/// [`StoredResponse::version`](http_ng_cache::StoredResponse::version) for
/// why a stale truth beats `http`'s builder default.
#[cfg(feature = "cache")]
pub(crate) fn serve<B>(stored: http_ng_cache::StoredResponse) -> http::Response<Cached<B>> {
    let mut resp = http::Response::new(Cached::from_store(stored.body().clone()));
    *resp.status_mut() = stored.status();
    *resp.version_mut() = stored.version();
    *resp.headers_mut() = stored.headers().clone();
    resp
}
