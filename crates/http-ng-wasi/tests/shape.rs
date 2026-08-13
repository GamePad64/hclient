//! Assertions about `http-ng-wasi`'s public API shape, kept outside `src`
//! — the same trick as `http-ng-core/tests/shape.rs` (see its doc
//! comment): CI's `no-declared-send` only scans `crates/*/src`, so an
//! ordinary `T: Send` here doesn't get confused with the "core doesn't
//! declare Send/Sync" invariant in production code.
//!
//! Compiles and runs on any target (not gated under `wasm32-wasip2`): the
//! test below never polls the future it builds — it only checks its
//! TYPE. Neither `WasiHttp::new()`, nor building an `http::Request`, nor
//! calling the `async fn execute` itself touches a single `wasi:http`
//! call: `execute` is an `async fn`, its body doesn't run until the
//! future is polled, and the `WasiHttp`/`http::Request` constructors make
//! no host calls at all.

fn assert_send<T: Send>(_: T) {}

/// Review resolution (Task 16, finding B-13): `convert::Payload::Streaming`
/// carries `+ Send` (`send-bound-exception: amendment-C2`) precisely so
/// that `WasiHttp::execute`'s future stays `Send` for streaming request
/// bodies. The review found the marker was justified — but if the bound
/// and the marker were removed TOGETHER, `no-declared-send` would stay
/// green (the marker would just disappear along with what it permitted),
/// and this property would break silently. This test catches exactly
/// that regression from the outside: `pub(crate) enum Payload` isn't
/// visible from here (`tests/` only sees the crate's public API), so the
/// only way to check its Send-ness is to observe the shape of the future
/// `execute` produces on a real streaming body.
#[test]
fn execute_future_is_send_even_for_a_streaming_request_body() {
    use http_ng_core::RequestBody;
    use http_ng_core::unversioned::Transport;

    struct OneShot(Option<bytes::Bytes>);
    impl http_body::Body for OneShot {
        type Data = bytes::Bytes;
        type Error = http_ng_core::Error;
        fn poll_frame(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Result<http_body::Frame<bytes::Bytes>, http_ng_core::Error>>>
        {
            std::task::Poll::Ready(self.0.take().map(|b| Ok(http_body::Frame::data(b))))
        }
    }

    let transport = http_ng_wasi::WasiHttp::new();
    let req = http::Request::builder()
        .uri("http://example.invalid/")
        .body(RequestBody::Streaming(Box::new(OneShot(Some(
            bytes::Bytes::from_static(b"x"),
        )))))
        .unwrap();
    let fut = transport.execute(req);
    assert_send(fut);
}

/// Symmetry: an `Empty` body shouldn't accidentally stop being `Send`
/// either — this branch never goes through `Payload::Streaming` at all,
/// so it has its own path to a `Send` future.
#[test]
fn execute_future_is_send_for_an_empty_request_body() {
    use http_ng_core::RequestBody;
    use http_ng_core::unversioned::Transport;

    let transport = http_ng_wasi::WasiHttp::new();
    let req = http::Request::builder()
        .uri("http://example.invalid/")
        .body(RequestBody::Empty)
        .unwrap();
    let fut = transport.execute(req);
    assert_send(fut);
}

/// P13 for this backend, the half that costs something if it breaks: a
/// hook that **is** `Send` must leave `execute`'s future `Send`.
///
/// The seam declares no `Send` at all, which is the answer P13 wanted —
/// but an unconditionally `!Send`-poisoning seam would satisfy that too,
/// and would silently undo what the `send-bound-exception: amendment-C2`
/// marker on `convert::Payload::Streaming` was spent on. Auto traits pass
/// through in both directions or they are not auto traits.
#[test]
fn a_send_hook_leaves_the_execute_future_send() {
    use http_ng_core::RequestBody;
    use http_ng_core::unversioned::{Event, Hooks, Transport};

    struct Atomic(std::sync::atomic::AtomicUsize);
    impl Hooks for Atomic {
        fn on(&self, _event: Event<'_>) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    let transport =
        http_ng_wasi::WasiHttp::new().hooks(Atomic(std::sync::atomic::AtomicUsize::new(0)));
    let req = http::Request::builder()
        .uri("http://example.invalid/")
        .body(RequestBody::Empty)
        .unwrap();
    assert_send(transport.execute(req));
}

/// The other half, and the one the guest exercises for real: a hook whose
/// state is behind an `Rc` is genuinely `!Send`, and the transport holding
/// it still implements `Transport`.
///
/// Nothing here asserts `!Send` — that is not expressible — so what this
/// test is worth is that it **compiles and runs**: a `Send` bound anywhere
/// on the hook's path would make it an `E0277` rather than a failure.
/// `examples/live_roundtrip_guest.rs`'s `Recorder` is the same shape,
/// under a real host.
#[test]
fn a_non_send_hook_still_gives_a_working_transport() {
    use http_ng_core::RequestBody;
    use http_ng_core::unversioned::{Event, Hooks, Transport};

    #[derive(Clone, Default)]
    struct Local(std::rc::Rc<std::cell::Cell<usize>>);
    impl Hooks for Local {
        fn on(&self, _event: Event<'_>) {
            self.0.set(self.0.get() + 1);
        }
    }

    let seen = Local::default();
    let transport = http_ng_wasi::WasiHttp::new().hooks(seen.clone());
    let req = http::Request::builder()
        .uri("http://example.invalid/")
        .body(RequestBody::Empty)
        .unwrap();
    // Built, not polled — the same rule the rest of this file follows, and
    // for the same reason: polling would need a `wasi:http` host.
    let fut = transport.execute(req);
    drop(fut);
    assert_eq!(seen.0.get(), 0, "nothing was polled, so nothing was told");
}

/// The hook costs nothing to carry: `NoHooks` is zero-sized, so
/// `WasiHttp` is the same size it was before the type parameter existed,
/// and the default parameter names the same type rather than a second one.
#[test]
fn the_no_op_hook_takes_up_no_room_in_the_transport() {
    use http_ng_core::unversioned::NoHooks;
    assert_eq!(std::mem::size_of::<NoHooks>(), 0);
    assert_eq!(
        std::mem::size_of::<http_ng_wasi::WasiHttp<NoHooks>>(),
        std::mem::size_of::<http_ng_wasi::WasiHttp>(),
    );
}
