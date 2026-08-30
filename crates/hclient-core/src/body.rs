use crate::error::RewindTooDeep;
use bytes::Bytes;
use std::fmt::Debug;
use std::sync::Arc;

/// Whether this body can be replayed — known **before** sending.
///
/// `reqwest::Request::try_clone() -> Option<Request>` answers the same
/// question after the retry layer has already decided to retry, and so
/// silently disables retries on streaming bodies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
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
#[non_exhaustive]
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

    /// What is visible without consuming this body — see [`BodyView`].
    ///
    /// The match is here rather than in a consumer because `RequestBody`
    /// is `#[non_exhaustive]`, so an exhaustive one is legal only inside
    /// this crate (amendment C6). A variant added later is a compile
    /// error on this line, which is the point.
    ///
    /// **That it cannot run a factory is enforced by the signature rather
    /// than by care.** Borrowing `self` means the answer borrows from
    /// `self`, so the tempting `Rewindable(f) => f().view()` is `E0515`,
    /// *cannot return value referencing temporary value* — checked by
    /// writing it. A looker therefore cannot be the reason a
    /// `Rewindable`'s factory is called, whoever writes the consumer.
    #[must_use]
    pub fn view(&self) -> BodyView<'_> {
        match self {
            Self::Empty => BodyView::Empty,
            // An empty `Full` stays `Bytes`, unlike in `reduce`: there the
            // collapse spares every backend a framing decision, and here
            // the two are the same answer to anyone looking.
            Self::Full(b) => BodyView::Bytes(b),
            Self::Rewindable(_) | Self::Streaming(_) => BodyView::Opaque,
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

/// Upper bound on the nesting of [`RequestBody::Rewindable`], whose
/// factory may legally return another `Rewindable`.
///
/// **This number was already picked twice, in two crates, and disagreed
/// with twice more.** `hclient-wasi` and `hclient-winhttp` each bounded
/// the chain at 16 and refused past it; `hclient-native`'s body and its
/// HTTP/3 pump each recursed without a bound, the second with a written
/// argument that *"defending against it here would mean picking a depth
/// limit nobody can justify"*. Four backends, one question, three
/// answers — and the two outcomes are not equivalent: unbounded recursion
/// is a stack overflow, which nothing can catch, where a bound is a
/// refusal a caller can read.
///
/// There is no legitimate scenario for the nesting either: a factory
/// calling a factory referring to a third buys the caller nothing.
pub const MAX_REWIND_DEPTH: u8 = 16;

/// What a transport actually has to send: bytes, or a stream.
///
/// [`RequestBody`] has four variants and a transport has two cases, and
/// every backend in this workspace wrote the same reduction between them —
/// four copies of one `match`, which had already diverged on the question
/// [`MAX_REWIND_DEPTH`] answers. [`RequestBody::reduce`] is that match,
/// written once.
///
/// **This enum is exhaustive on purpose, where `RequestBody` is not.** A
/// transport must handle every case here, and there is no third: a body
/// added to `RequestBody` later — one backed by a file, say — reduces to a
/// stream like any other. That is what makes marking `RequestBody`
/// additive rather than a `_` arm nobody can write correctly.
pub enum Reduced {
    /// No body at all. Distinct from `Bytes` of length zero, which a
    /// transport may still have to frame — every backend here treats an
    /// empty `Full` as this, which is why the reduction does it for them.
    Empty,
    /// A body already in memory.
    Bytes(Bytes),
    /// A body that has to be pumped, and can be sent once.
    Streaming(Box<dyn http_body::Body<Data = Bytes, Error = crate::Error> + Unpin + Send>), // send-bound-exception: amendment-C2
}

/// What can be seen of a [`RequestBody`] **without consuming it and
/// without calling a factory**.
///
/// [`Reduced`] is the other half of the same idea and they are not
/// interchangeable: `reduce` takes the body by value and runs a
/// `Rewindable`'s factory to get an answer, where this borrows and never
/// does. A caller that only wants to *look* — to sign the payload, to log
/// its size — must not be the reason a factory is called a second time,
/// because "one snapshot per hop" is a rule this workspace's own tests
/// pin by counting those calls.
///
/// **Exhaustive on purpose, where `RequestBody` is not**, and for
/// `Reduced`'s reason word for word: a variant added later is either
/// visible bytes or it is not, and the crate that adds it owns this match.
/// A `_` arm in a consumer is where a new body shape would go to be
/// silently mis-signed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyView<'a> {
    /// There is no body. **Not the same as zero bytes to a scheme that
    /// hashes payloads** — AWS SigV4 hashes the empty string here, which
    /// is a real value rather than an absence.
    Empty,
    /// The bytes that will be sent.
    Bytes(&'a Bytes),
    /// There is a body and it cannot be shown without work the looker did
    /// not ask for.
    ///
    /// Two ways to arrive, and a looker cannot tell them apart because
    /// nothing it could do would differ: a `Streaming` body has no bytes
    /// until it is pumped, and a `Rewindable` one has them only behind a
    /// call to its factory.
    Opaque,
}

/// Hand-written for the same reason [`RequestBody`]'s is: a boxed
/// `http_body::Body` has no `Debug`, and what a reader wants here is the
/// shape rather than the bytes.
impl std::fmt::Debug for Reduced {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str("Empty"),
            Self::Bytes(b) => write!(f, "Bytes({} bytes)", b.len()),
            Self::Streaming(_) => f.write_str("Streaming(..)"),
        }
    }
}

impl RetryKind {
    /// May a request carrying this body be sent again?
    ///
    /// **Ask this rather than matching**, and then a variant added later
    /// answers for itself. `RetryKind` is `#[non_exhaustive]`, so a
    /// `match` outside this crate needs a `_` arm — and the only safe
    /// content for one is *do not replay*, which is what this returns for
    /// anything it does not know. That is the understating direction this
    /// workspace applies to every capability constant: a retry withheld
    /// costs a request, a retry taken wrongly sends a body twice.
    ///
    /// The match below has no `_` arm and needs none: `#[non_exhaustive]`
    /// is inert inside the defining crate, so a variant added here is a
    /// compile error **here** — which is the same shape `Event` and
    /// `Capabilities` use, one exhaustive match in the crate that owns the
    /// type and no break for anybody outside.
    ///
    /// **It does not answer "may an attacker send this again"**, which is
    /// method safety and a notion this codebase deliberately does not
    /// have. `POST /transfer` with a buffered body is [`Self::Free`] and
    /// is precisely what must never enter early data — see
    /// `AllowEarlyData`.
    #[must_use]
    pub fn may_replay(self) -> bool {
        match self {
            Self::Free | Self::ViaFactory => true,
            Self::Impossible => false,
        }
    }
}

impl RequestBody {
    /// Reduce to what a transport can act on.
    ///
    /// Follows a [`RequestBody::Rewindable`] chain to its terminal body,
    /// up to [`MAX_REWIND_DEPTH`], and refuses past it rather than
    /// recursing for ever. See [`Reduced`] for why this lives here instead
    /// of once per backend.
    pub fn reduce(self) -> Result<Reduced, RewindTooDeep> {
        let mut body = self;
        for _ in 0..MAX_REWIND_DEPTH {
            match body {
                Self::Empty => return Ok(Reduced::Empty),
                // An empty `Full` is `Empty`: every backend collapsed the
                // two, and one that did not would frame a zero-length body
                // where its neighbours frame none.
                Self::Full(b) if b.is_empty() => return Ok(Reduced::Empty),
                Self::Full(b) => return Ok(Reduced::Bytes(b)),
                Self::Streaming(s) => return Ok(Reduced::Streaming(s)),
                // Legal, and documented on the variant: the factory may
                // return anything, including another `Rewindable`.
                Self::Rewindable(f) => body = f(),
            }
        }
        Err(RewindTooDeep)
    }
}

#[cfg(test)]
mod tests {

    /// The four variants collapse to the three facts a transport has, and
    /// the collapse of an empty `Full` is part of it: every backend in
    /// this workspace did that, and one that did not would frame a
    /// zero-length body where its neighbours frame none.
    #[test]
    fn every_variant_reduces_to_what_a_transport_can_act_on() {
        assert!(matches!(RequestBody::Empty.reduce(), Ok(Reduced::Empty)));
        assert!(matches!(
            RequestBody::Full(Bytes::new()).reduce(),
            Ok(Reduced::Empty)
        ));
        assert!(matches!(
            RequestBody::Full(Bytes::from_static(b"x")).reduce(),
            Ok(Reduced::Bytes(b)) if b == "x"
        ));
        use http_body_util::BodyExt as _;
        let streaming = RequestBody::Streaming(Box::new(
            http_body_util::Full::new(Bytes::from_static(b"s"))
                .map_err(|e: std::convert::Infallible| match e {}),
        ));
        assert!(matches!(streaming.reduce(), Ok(Reduced::Streaming(_))));
    }

    /// A factory is allowed to return another `Rewindable`, and the chain
    /// is followed to its terminal body.
    #[test]
    fn a_nested_rewindable_is_followed_to_the_body_at_the_end() {
        let inner = || RequestBody::Full(Bytes::from_static(b"deep"));
        let outer = RequestBody::rewindable(move || RequestBody::rewindable(inner));
        assert!(matches!(outer.reduce(), Ok(Reduced::Bytes(b)) if b == "deep"));
    }

    /// **The bound, and it is the whole reason this lives in the core.**
    ///
    /// Two backends picked 16 and refused; two recursed without a bound,
    /// one of them arguing that no limit could be justified. The outcomes
    /// are not equivalent — unbounded recursion is a stack overflow, which
    /// nothing can catch, and this is an error a caller can read.
    #[test]
    fn a_factory_that_never_terminates_is_refused_rather_than_followed() {
        fn endless() -> RequestBody {
            RequestBody::rewindable(endless)
        }
        assert!(endless().reduce().is_err());
    }

    /// Exactly at the bound, not one short of it: a chain of
    /// `MAX_REWIND_DEPTH` factories ending in a body is accepted, and one
    /// more is not. An off-by-one here is invisible without the pair.
    #[test]
    fn the_bound_is_where_it_says_it_is() {
        fn chain(depth: u8) -> RequestBody {
            if depth == 0 {
                RequestBody::Full(Bytes::from_static(b"end"))
            } else {
                RequestBody::rewindable(move || chain(depth - 1))
            }
        }
        // `MAX_REWIND_DEPTH - 1` factories, then the body: the loop's last
        // turn reads the terminal body.
        assert!(chain(MAX_REWIND_DEPTH - 1).reduce().is_ok());
        assert!(chain(MAX_REWIND_DEPTH).reduce().is_err());
    }
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
