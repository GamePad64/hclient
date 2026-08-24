use bytes::Bytes;
use std::fmt::Debug;
use std::sync::Arc;

/// Whether this body can be replayed — known **before** sending.
///
/// `reqwest::Request::try_clone() -> Option<Request>` answers the same
/// question after the retry layer has already decided to retry, and so
/// silently disables retries on streaming bodies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryKind {
    /// Replays for free.
    Free,
    /// Replays by calling the factory.
    ViaFactory,
    /// Cannot be replayed.
    Impossible,
}

/// `Send + Sync` bounds — a documented exception to the crate invariant
/// "declare `Send`/`Sync` nowhere" (spec amendment-C2, sibling of C1 on
/// [`crate::Error`]). Without them `RequestBody` would be `!Send`, so
/// `http::Request<RequestBody>` would be `!Send`, so the future
/// `Transport::execute` returns would be `!Send` for every backend —
/// `tokio::spawn(client.get(u).send())` would never build. `Sync` is only
/// needed here, for `Arc`: `Arc<T>: Send` requires `T: Send + Sync`, whereas
/// `Box<T>: Send` (see [`RequestBody::Streaming`]) requires only `T: Send`.
pub type RewindFactory = Arc<dyn Fn() -> RequestBody + Send + Sync>; // send-bound-exception: amendment-C2

/// A request body with an explicit replay contract.
#[derive(Default)]
pub enum RequestBody {
    #[default]
    Empty,
    Full(Bytes),
    /// Replays by calling the factory.
    ///
    /// **Factory contract.** It must be pure: every call must produce a body
    /// equivalent to the previous one (same content, same size). A factory
    /// with hidden state that hands back a different body on each call is a
    /// bug source that's obvious in hindsight but undocumented otherwise,
    /// which is why `size_hint()` deliberately returns `None` for this
    /// variant: guessing from the first call is dangerous if the contract
    /// is violated.
    ///
    /// The factory is legally allowed to return `RequestBody::Streaming` —
    /// that isn't a live lie, because `retry_kind()` and `rewind()` are
    /// always recomputed from whatever object currently sits inside
    /// `RequestBody`, not cached at the moment `Rewindable` was created.
    /// **Invariant that matters for the retry layer: always ask
    /// `retry_kind()` of the body you're currently holding, and never cache
    /// it across a `rewind()`.**
    Rewindable(RewindFactory),
    /// A single-pass body. The concrete stream is set by the transport; in
    /// v0.1 the core only needs to know it can't be replayed.
    ///
    /// `+ Send` — the same C2 exception as [`RewindFactory`]: `Box<T>: Send`
    /// requires only `T: Send`, `Sync` isn't needed here.
    Streaming(Box<dyn http_body::Body<Data = Bytes, Error = crate::Error> + Unpin + Send>), // send-bound-exception: amendment-C2
}

impl Debug for RequestBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RequestBody::Empty => f.write_str("Empty"),
            RequestBody::Full(b) => write!(f, "Full({} bytes)", b.len()),
            RequestBody::Rewindable(_) => f.write_str("Rewindable(..)"),
            RequestBody::Streaming(_) => f.write_str("Streaming(..)"),
        }
    }
}

impl RequestBody {
    pub fn rewindable<F>(f: F) -> Self
    where
        F: Fn() -> RequestBody + Send + Sync + 'static, // send-bound-exception: amendment-C2
    {
        RequestBody::Rewindable(Arc::new(f))
    }

    pub fn retry_kind(&self) -> RetryKind {
        match self {
            RequestBody::Empty | RequestBody::Full(_) => RetryKind::Free,
            RequestBody::Rewindable(_) => RetryKind::ViaFactory,
            RequestBody::Streaming(_) => RetryKind::Impossible,
        }
    }

    pub fn rewind(&self) -> Option<RequestBody> {
        match self {
            RequestBody::Empty => Some(RequestBody::Empty),
            RequestBody::Full(b) => Some(RequestBody::Full(b.clone())),
            RequestBody::Rewindable(f) => Some(f()),
            RequestBody::Streaming(_) => None,
        }
    }

    pub fn size_hint(&self) -> Option<u64> {
        match self {
            RequestBody::Empty => Some(0),
            RequestBody::Full(b) => Some(b.len() as u64),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use std::pin::Pin;
    use std::task::Context;
    use std::task::Poll;

    #[test]
    fn replayability_is_knowable_before_sending() {
        assert_eq!(RequestBody::Empty.retry_kind(), RetryKind::Free);
        assert_eq!(
            RequestBody::Full(Bytes::from_static(b"x")).retry_kind(),
            RetryKind::Free
        );
    }

    #[test]
    fn rewindable_replays_through_factory() {
        let b = RequestBody::rewindable(|| RequestBody::Full(Bytes::from_static(b"same")));
        assert_eq!(b.retry_kind(), RetryKind::ViaFactory);
        let again = b.rewind().expect("rewindable must rewind");
        assert!(matches!(again, RequestBody::Full(ref x) if &x[..] == b"same"));
    }

    #[test]
    fn full_rewind_preserves_the_payload() {
        let b = RequestBody::Full(Bytes::from_static(b"abc"));
        match b.rewind().expect("Full replays") {
            RequestBody::Full(x) => assert_eq!(&x[..], b"abc"),
            other => panic!("expected Full, got {other:?}"),
        }
    }

    #[test]
    fn a_factory_survives_repeated_replays() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let b = RequestBody::rewindable(move || {
            c.fetch_add(1, Ordering::SeqCst);
            RequestBody::Full(Bytes::from_static(b"same"))
        });
        for _ in 0..3 {
            let again = b.rewind().expect("rewindable replays");
            assert!(matches!(again, RequestBody::Full(ref x) if &x[..] == b"same"));
            assert_eq!(
                b.retry_kind(),
                RetryKind::ViaFactory,
                "kind doesn't change across replays"
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    /// The `Empty`/`Full` pair are the only variants whose size is known
    /// ahead of time. `Rewindable` and `Streaming` are covered separately
    /// (`rewindable_replays_through_factory`,
    /// `streaming_is_honest_about_being_unreplayable`) and aren't included
    /// here — the test's name shouldn't promise coverage it doesn't have.
    #[test]
    fn size_hint_is_known_for_empty_and_full_bodies() {
        assert_eq!(RequestBody::Empty.size_hint(), Some(0));
        assert_eq!(
            RequestBody::Full(Bytes::from_static(b"abcd")).size_hint(),
            Some(4)
        );
    }

    /// A body with not a single byte in its buffer: `poll_frame` returns
    /// `Ready(None)` immediately. Needed only to construct
    /// `RequestBody::Streaming` in tests — the concrete transport supplies
    /// its own implementation.
    struct EmptyStream;
    impl http_body::Body for EmptyStream {
        type Data = Bytes;
        type Error = crate::Error;
        fn poll_frame(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
            Poll::Ready(None)
        }
    }

    #[test]
    fn streaming_is_honest_about_being_unreplayable() {
        let b = RequestBody::Streaming(Box::new(EmptyStream));
        assert_eq!(b.retry_kind(), RetryKind::Impossible);
        assert!(b.rewind().is_none(), "must return None, not panic");
        assert_eq!(b.size_hint(), None);
    }

    // `RequestBody: Send` and `http::Request<RequestBody>: Send`
    // (amendment-C2) are asserted in `crates/hclient-core/tests/shape.rs`,
    // for `error.rs`'s reason: a bare `fn assert_send<T: Send>() {}` inside
    // `src` is what the `no-declared-send` guard's regex matches, and an
    // assertion outside `src` needs no exception marker at all.
}
