//! Assertions about the public API's shape, kept outside `src`.
//!
//! The `no-declared-send` CI check scans only `crates/*/src`, so the
//! ordinary generic form here doesn't conflict with it, and the exception
//! list keeps its meaning of "a justified exception in production code."

use bytes::Bytes;
use http_ng_core::unversioned::{Timer, Transport};
use http_ng_core::{Capabilities, Error, ErrorKind, RequestBody, Timeouts, UnsupportedCapability};

fn assert_send_sync<T: Send + Sync>() {}
fn assert_send<T: Send>() {}

#[test]
fn capability_types_are_send_and_sync() {
    assert_send_sync::<Capabilities>();
    assert_send_sync::<Timeouts>();
    assert_send_sync::<UnsupportedCapability>();
}

/// `Error: Send + Sync` — spec amendment-C1, the single documented exception
/// from "the core declares no Send/Sync": `Error::source` must be
/// `Send + Sync`, or the future `Client::execute` returns could never make
/// it into `tokio::spawn` for any backend. Was a compile-time-only
/// assertion inside `error.rs`'s own `#[cfg(test)] mod tests` until Task 12's
/// fix round 1 moved it here (amendment-C3: such assertions belong in
/// `tests/`, not `src`) — the runtime construction below keeps it from being
/// a vacuous no-op, same as the original.
#[test]
fn error_is_send_sync_and_constructs_a_real_error_not_just_compiles() {
    assert_send_sync::<Error>();
    let e = Error::new(ErrorKind::Other, Never);
    assert_eq!(e.kind(), &ErrorKind::Other);
}

/// `RequestBody: Send` and `http::Request<RequestBody>: Send` — spec
/// amendment-C2: without `+ Send` on both of `RequestBody`'s trait objects,
/// `RequestBody` and therefore `http::Request<RequestBody>` would be
/// `!Send`, and `Transport::execute`'s future with it. Relocated from
/// `body.rs`'s own test module for the same C3 reason as the `Error` test
/// above.
#[test]
fn request_body_and_its_request_are_send() {
    assert_send::<RequestBody>();
    assert_send::<http::Request<RequestBody>>();
}

struct Echo {
    caps: Capabilities,
}

#[derive(Debug)]
struct Never;
impl std::fmt::Display for Never {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "never")
    }
}
impl std::error::Error for Never {}

impl Transport for Echo {
    type Body = http_body_util::Full<Bytes>;
    type Error = Error;
    async fn execute(
        &self,
        _req: http::Request<RequestBody>,
    ) -> Result<http::Response<Self::Body>, Self::Error> {
        Ok(http::Response::new(http_body_util::Full::new(
            Bytes::from_static(b"ok"),
        )))
    }
    fn to_error(&self, e: Self::Error) -> Error {
        e
    }
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
}

/// A backend with its own error type that doesn't override `to_error` —
/// the whole reason the hook has a default (branch final review B2).
struct Bare {
    caps: Capabilities,
}

#[derive(Debug, PartialEq)]
struct Custom;
impl std::fmt::Display for Custom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "backend said no")
    }
}
impl std::error::Error for Custom {}

impl Transport for Bare {
    type Body = http_body_util::Full<Bytes>;
    type Error = Custom;
    async fn execute(
        &self,
        _req: http::Request<RequestBody>,
    ) -> Result<http::Response<Self::Body>, Self::Error> {
        Err(Custom)
    }
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
}

/// A backend whose error is already our `Error`, and which does NOT
/// override `to_error`. Exists only for the test below: both real backends
/// override the hook explicitly, so without this stand-in there'd be
/// nothing to observe the default's guarantee with.
struct Forgetful {
    caps: Capabilities,
}

impl Transport for Forgetful {
    type Body = http_body_util::Full<Bytes>;
    type Error = Error;
    async fn execute(
        &self,
        _req: http::Request<RequestBody>,
    ) -> Result<http::Response<Self::Body>, Self::Error> {
        Err(Error::new(ErrorKind::Resolve, Never))
    }
    // `to_error` is deliberately NOT overridden.
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
}

/// The default's main guarantee: the category can't be lost by forgetting
/// to override the hook.
///
/// Before this round, a forgetful backend got `ErrorKind::Other` and a
/// `Display` reading `Other: Resolve: …`, the compiler was happy, its own
/// classification tests were green — only the consumer got it wrong. Prose
/// in three places was the only guard; now structure holds it, and this
/// test is what keeps the structure from silently evaporating.
#[test]
fn the_default_passes_our_own_error_through_even_when_a_backend_forgets_to_override() {
    let t = Forgetful {
        caps: Capabilities::none(),
    };
    let e = t.to_error(Error::new(ErrorKind::Tls, Never));
    assert_eq!(
        e.kind(),
        &ErrorKind::Tls,
        "the default must recognize its own `Error` and pass it through unchanged"
    );
    assert_eq!(
        e.to_string(),
        "Tls: never",
        "and not nest a second category in front of the real one"
    );
}

/// The default `Transport::to_error` wraps with `ErrorKind::Other`,
/// **keeping the source whole**: a backend that has nothing to say about
/// the category doesn't need to write anything, and the caller still gets
/// a typed source, not a string.
#[test]
fn to_error_defaults_to_other_and_keeps_the_source_intact() {
    let t = Bare {
        caps: Capabilities::none(),
    };
    let e = t.to_error(Custom);
    assert_eq!(e.kind(), &ErrorKind::Other);
    let src = std::error::Error::source(&e).expect("Error::new always sets a source");
    assert_eq!(
        src.downcast_ref::<Custom>(),
        Some(&Custom),
        "the source must remain itself, not become a string"
    );
}

/// And a backend whose error is already `Error` overrides the hook with an
/// identity — otherwise its category is lost and `Display` prints the
/// source twice. `Echo` here stands in for `http-ng-wasi`, whose
/// `type Error = http_ng_core::Error` and which does exactly this.
#[test]
fn a_backend_whose_error_is_already_ours_can_pass_it_through_unchanged() {
    let t = Echo {
        caps: Capabilities::none(),
    };
    let e = t.to_error(Error::new(ErrorKind::Tls, Never));
    assert_eq!(
        e.kind(),
        &ErrorKind::Tls,
        "the identity must preserve the category, not rebuild the error"
    );
    assert_eq!(
        e.to_string(),
        "Tls: never",
        "and not nest a second category in front of the real one"
    );
}

/// Asserts the core's main architectural property: `Send` is declared
/// nowhere, yet is inferred by auto-traits when the transport actually is
/// Send.
#[test]
fn send_propagates_without_being_declared() {
    fn assert_send<T: Send>(_: T) {}
    let t = Echo {
        caps: Capabilities::none(),
    };
    let fut = t.execute(http::Request::new(RequestBody::Empty));
    assert_send(fut);
}

#[test]
fn non_send_transport_still_satisfies_the_trait() {
    struct Local {
        caps: Capabilities,
        _rc: std::rc::Rc<()>,
    }
    impl Transport for Local {
        type Body = http_body_util::Full<Bytes>;
        type Error = Error;
        async fn execute(
            &self,
            _req: http::Request<RequestBody>,
        ) -> Result<http::Response<Self::Body>, Self::Error> {
            Err(Error::new(ErrorKind::Other, Never))
        }
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
    }
    let _ = Local {
        caps: Capabilities::none(),
        _rc: std::rc::Rc::new(()),
    };
}

/// The same invariant, but along the axis `to_error` could have broken
/// (branch final review B2): a transport whose ERROR is genuinely `!Send`.
///
/// This is the one reason `to_error` is a defaulted method with a
/// where-clause, rather than `Transport::Error: Into<Error>` on the trait
/// or `Error` as the seam's error type: either of those two forms would
/// require `Send + Sync` from every backend's error and would throw this
/// type out of `Transport` entirely. Amendment C1 keeps it representable —
/// it can't use `Client` (and can't call `to_error`), but it does implement
/// `Transport`. The test doesn't "check" anything at runtime; it fails to
/// compile if the invariant is violated, and the `Rc` inside the error
/// isn't decoration, it's what makes it genuinely `!Send`.
#[test]
fn a_transport_whose_error_is_not_send_still_implements_the_trait() {
    // `PhantomData<Rc<()>>`, not `Rc<()>` as a field: the same thing for
    // auto-traits (the type is genuinely `!Send`), but without an unread
    // field — and so without `#[allow(dead_code)]`, which this branch is
    // better off having nowhere.
    #[derive(Debug)]
    struct NotSend(std::marker::PhantomData<std::rc::Rc<()>>);
    impl std::fmt::Display for NotSend {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "not send")
        }
    }
    impl std::error::Error for NotSend {}

    struct LocalErr {
        caps: Capabilities,
    }
    impl Transport for LocalErr {
        type Body = http_body_util::Full<Bytes>;
        type Error = NotSend;
        async fn execute(
            &self,
            _req: http::Request<RequestBody>,
        ) -> Result<http::Response<Self::Body>, Self::Error> {
            Err(NotSend(std::marker::PhantomData))
        }
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
    }
    let t = LocalErr {
        caps: Capabilities::none(),
    };
    assert!(!t.capabilities().streaming_request_body);
}

/// A trivial `Timer` whose `Instant` is just a counter. Enough to check a
/// property of the trait, not the behavior of a real timer.
struct Fake(std::cell::Cell<u64>);

impl Timer for Fake {
    type Instant = u64;
    /// The whole point of `type Sleep`: a name. `Ready<()>` is the
    /// cheapest one that satisfies the trait.
    type Sleep = std::future::Ready<()>;

    fn sleep(&self, _d: core::time::Duration) -> Self::Sleep {
        std::future::ready(())
    }

    fn now(&self) -> Self::Instant {
        let v = self.0.get();
        self.0.set(v + 1);
        v
    }

    fn elapsed_since(&self, earlier: Self::Instant) -> core::time::Duration {
        core::time::Duration::from_secs(self.now().saturating_sub(earlier))
    }
}

/// Compares two already-captured `Instant`s, knowing about them only what
/// the `Timer` trait itself gives — that is, **generically** over
/// `T: Timer`, without monomorphizing down to the concrete
/// `Fake::Instant = u64`. This matters: if the test compared `a < b` on a
/// concrete `u64`, the compiler would find `Ord`/`PartialOrd` on `u64`
/// directly and let the missing bound on the trait itself slip by
/// unnoticed. Here, `a` and `b` have the abstract type `T::Instant`, and
/// without `PartialOrd` in `Timer::Instant`'s declaration, the line
/// `a < b` doesn't compile — `E0369: binary operation '<' cannot be
/// applied to type '<T as Timer>::Instant'`.
fn are_ordered<T: Timer>(a: T::Instant, b: T::Instant) -> bool {
    a < b
}

/// A consumer holding two already-captured `Instant`s must be able to
/// compare them directly — without a third call to `now()` (a third call
/// isn't equivalent: it measures the moment of the comparison, not the
/// moment of the second `now()`).
#[test]
fn captured_instants_are_orderable_without_a_third_now_call() {
    let t = Fake(std::cell::Cell::new(0));
    let a = t.now();
    let b = t.now();
    assert!(
        are_ordered::<Fake>(a, b),
        "second capture must order after the first"
    );
}

/// The WebSocket seam (v0.3 W4) declares no `Send` either, and a backend
/// whose types are genuinely `!Send` implements it.
///
/// The same claim `non_send_transport_still_satisfies_the_trait` makes for
/// `Transport`, made for the second seam before there is a second backend
/// to discover it with. It is not idle: `http-ng-native` reaches this
/// trait through `hyper::client::conn::http1::Parts`, and the obvious way
/// to get there — `hyper::upgrade::Upgraded` — is
/// `Rewind<Box<dyn Io + Send>>`. A seam that had inherited that bound
/// would have shut out every single-threaded runtime, and nothing short of
/// a test like this would have said so before a backend tried.
#[test]
fn a_non_send_backend_still_satisfies_the_websocket_seam() {
    use futures_core::Stream;
    use futures_sink::Sink;
    use http_ng_core::unversioned::{Message, WebSocket, WebSocketConnect};
    use std::pin::Pin;
    use std::task::{Context, Poll};

    struct LocalSocket {
        _rc: std::rc::Rc<()>,
    }

    impl Stream for LocalSocket {
        type Item = Result<Message, Error>;
        fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            Poll::Ready(None)
        }
    }
    impl Sink<Message> for LocalSocket {
        type Error = Error;
        fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
            Poll::Ready(Ok(()))
        }
        fn start_send(self: Pin<&mut Self>, _item: Message) -> Result<(), Error> {
            Ok(())
        }
        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
            Poll::Ready(Ok(()))
        }
        fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
            Poll::Ready(Ok(()))
        }
    }
    impl WebSocket for LocalSocket {}

    struct LocalBackend(std::rc::Rc<()>);
    impl WebSocketConnect for LocalBackend {
        type WebSocket = LocalSocket;
        async fn websocket(&self, _req: http::Request<()>) -> Result<LocalSocket, Error> {
            Ok(LocalSocket {
                _rc: self.0.clone(),
            })
        }
    }

    let _ = LocalBackend(std::rc::Rc::new(()));
}

/// **P13, settled by construction: an observability hook can avoid a
/// `Send` bound.**
///
/// `docs/v04-design.md` records P13 as unverified — *"every other seam
/// here manages it, but a hook stored in a transport and called from a
/// body is a different shape"* — and the difference is real: the two
/// probes above put an `Rc` in a transport and in a `WebSocketConnect`,
/// both of which are only ever *borrowed* by the request path. A response
/// body outlives `Transport::execute`, so it cannot borrow the transport;
/// it has to **hold** the hook. If anything on that path declared `Send`,
/// a single-threaded runtime could observe nothing.
///
/// So this is that shape, minimally: a `Transport` holding a hook whose
/// counter is an `Rc<Cell<_>>` — genuinely `!Send`, not a `PhantomData`
/// gesture — a body that carries a clone of it past the end of `execute`,
/// and a call to `Hooks::on` from inside `poll_frame`. It compiles, and
/// the assertion below is that the call actually happened rather than
/// that the file type-checks: a probe whose body is never polled would
/// pass while proving nothing about the site the question is about.
#[test]
fn a_non_send_hook_reaches_a_bodys_poll_frame_and_the_transport_still_implements_transport() {
    use http_body::{Body as HttpBody, Frame};
    use http_ng_core::unversioned::{CloseReason, Closed, ConnectionId, Event, Hooks};
    use std::pin::Pin;
    use std::task::{Context, Poll, Waker};

    /// Genuinely `!Send`: the count lives behind an `Rc`, and the test
    /// reads it afterwards through a second handle to the same cell.
    #[derive(Clone)]
    struct LocalHook(std::rc::Rc<std::cell::Cell<usize>>);

    impl Hooks for LocalHook {
        fn on(&self, _event: Event<'_>) {
            self.0.set(self.0.get() + 1);
        }
    }

    /// The site P13 is about: a body that calls the hook while streaming.
    struct HookedBody<H> {
        hooks: H,
        told: bool,
    }

    /// `H: Unpin` here rather than a pin projection, exactly as
    /// `h1::H1Body` does it: this workspace forbids `unsafe`, so the safe
    /// projection is the only one available, and every hook worth writing
    /// is `Unpin` already.
    impl<H: Hooks + Unpin> HttpBody for HookedBody<H> {
        type Data = Bytes;
        type Error = Error;
        fn poll_frame(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Error>>> {
            let this = self.get_mut();
            if !this.told {
                this.told = true;
                this.hooks.on(Event::Closed(Closed {
                    id: ConnectionId::UNWATCHED,
                    reason: CloseReason::Ended,
                }));
            }
            Poll::Ready(None)
        }
    }

    struct Watched<H> {
        caps: Capabilities,
        hooks: H,
    }

    impl<H: Hooks + Clone + Unpin> Transport for Watched<H> {
        type Body = HookedBody<H>;
        type Error = Error;
        async fn execute(
            &self,
            _req: http::Request<RequestBody>,
        ) -> Result<http::Response<Self::Body>, Self::Error> {
            Ok(http::Response::new(HookedBody {
                hooks: self.hooks.clone(),
                told: false,
            }))
        }
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
    }

    let seen = std::rc::Rc::new(std::cell::Cell::new(0));
    let t = Watched {
        caps: Capabilities::none(),
        hooks: LocalHook(std::rc::Rc::clone(&seen)),
    };

    let mut cx = Context::from_waker(Waker::noop());
    let mut fut = std::pin::pin!(t.execute(http::Request::new(RequestBody::Empty)));
    let Poll::Ready(Ok(resp)) = fut.as_mut().poll(&mut cx) else {
        panic!("this transport answers on the first poll");
    };
    let mut body = std::pin::pin!(resp.into_body());
    assert!(
        matches!(body.as_mut().poll_frame(&mut cx), Poll::Ready(None)),
        "the body ends on its first poll"
    );
    assert_eq!(
        seen.get(),
        1,
        "the hook must have been called from poll_frame — the whole of P13"
    );
}

/// The other half of P13, and the half a `!Send` probe cannot see: a hook
/// that *is* `Send` must leave the transport and its body `Send`.
///
/// Without this, `Hooks` could satisfy the test above by being
/// unconditionally `!Send`-poisoning — a hook holding a `PhantomData<Rc>`
/// in the seam itself, say — and every backend would lose `tokio::spawn`
/// on its responses in exchange for the single-threaded case. Auto traits
/// are supposed to pass through in both directions, and this is the
/// direction `http-ng-native/tests/shape.rs` then re-checks on the real
/// transport.
#[test]
fn a_send_hook_leaves_the_transport_and_its_body_send() {
    use http_ng_core::unversioned::{Event, Hooks, NoHooks};

    struct Counting(std::sync::atomic::AtomicUsize);
    impl Hooks for Counting {
        fn on(&self, _event: Event<'_>) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    assert_send::<NoHooks>();
    assert_send_sync::<NoHooks>();
    assert_send::<Counting>();
    assert_send::<std::sync::Arc<Counting>>();
    assert_eq!(
        std::mem::size_of::<NoHooks>(),
        0,
        "the no-op hook must be zero-sized, or every transport that stores \
         one pays for a caller who asked for nothing"
    );
    // A `const` block, because clippy is right that this is decided at
    // compile time — which is the point of the claim, not a weakness of
    // the check: `NoHooks::WATCHING` is what a monomorphised build
    // deletes the clock reads on, and a `true` here would leave every
    // no-hook build reading them for nobody.
    const { assert!(!NoHooks::WATCHING) };
}

/// **`ConnectionId::UNWATCHED` is a value no connection can ever have**,
/// which is the whole of what makes it mean *this event names no
/// connection* rather than *this event names connection zero*.
///
/// Two of the four backends carry it on every `Head` they emit —
/// `http-ng-fetch` and `http-ng-wasi` own no connection to take an id from
/// — and a hook that reads it looks it up in whatever table it keeps of
/// the connections it was told about. That lookup has to miss, always, on
/// every id the counter has handed out or ever will. So the counter
/// starting at `1` is not cosmetic, and this is the line that says so:
/// `AtomicU64::new(0)` there compiles, passes every hook test in the four
/// backends, and quietly makes the first connection of the process
/// indistinguishable from no connection at all.
///
/// The loop is short on purpose. `next()` is a process-wide counter and
/// nothing here owns it, so what is asserted is the property — *never
/// `UNWATCHED`, and never twice* — rather than any particular number.
///
/// `docs/v04-w2-hooks-ambient.md` §9 is the argument this pins down: it is
/// why a second value meaning "there is no connection" is not owed by this
/// seam.
#[test]
fn the_id_that_names_no_connection_is_one_the_counter_never_hands_out() {
    use http_ng_core::unversioned::ConnectionId;

    let mut seen = std::collections::HashSet::new();
    for _ in 0..64 {
        let id = ConnectionId::next();
        assert_ne!(
            id,
            ConnectionId::UNWATCHED,
            "the counter handed out the one id that is supposed to mean \
             *no connection* — every hook matching a `Head` against the \
             connections it was told about would now find one"
        );
        assert_ne!(
            id.get(),
            ConnectionId::UNWATCHED.get(),
            "and the same through `get()`, which is what a log line prints \
             and what `http-ng-wasi`'s guest transcript compares"
        );
        assert!(
            seen.insert(id),
            "ids must be distinct, or a `Closed` cannot be matched to the \
             `Connected` that opened the same socket — which is the only \
             reason this type exists"
        );
    }
}
