//! A direct guard on the panic-versus-cancellation split
//! (`Blocking::run -> Result<T, Cancelled>`). That split's whole
//! justification is that the two failure modes are different in kind: a
//! panicking closure is a bug and must keep propagating with its original
//! payload intact via `resume_unwind`, while pool-shutdown-before-running is
//! an environmental condition and must become a typed `Cancelled`, never a
//! panic. Nothing else in the tree pins that distinction directly - the
//! implementer's own `blocking_propagates_the_original_panic_payload` test
//! uses a plain `panic!("boom")` (a `&str` payload) and does not probe
//! concurrency at all.
//!
//! What each test here proves, and does not prove:
//!
//! - `panic_payload_survives_intact_with_original_type_and_value` is the
//!   primary evidence: a non-string, custom-typed payload (not just a
//!   string message) survives `Tokio::run` with its exact type and value,
//!   proving `resume_unwind` really carries the original payload through
//!   and not a replacement constructed along the way.
//! - `panic_is_never_observed_as_cancelled_under_concurrent_load` is
//!   insurance, not the primary argument. Reading tokio 1.53.1's source
//!   (`runtime/task/harness.rs`) establishes that a genuine in-progress
//!   panic and a
//!   pool-shutdown-before-running cancellation go through two entirely
//!   disjoint code paths: `poll_future`'s own `catch_unwind` (feeding
//!   `panic_to_error`) for a task that actually runs, versus `cancel_task`
//!   (feeding `JoinError::cancelled`) for a task shut down without ever
//!   being polled. A task is either polled or shut-down-unpolled, never
//!   both, so the misclassification this test watches for is structurally
//!   ruled out at the source level, not merely unobserved in N runs. The
//!   64-way concurrent run below did not find a counterexample, which is
//!   the outcome that source reading predicts - treat it as confirmation of
//!   that structural argument, not as a probabilistic argument on its own
//!   that would need more iterations to be trusted further.
//!
//! Run with `cargo test -p hclient-rt-tokio --test panic_fidelity_probes
//! --all-features`.
use hclient_rt::{Blocking, Cancelled};
use hclient_rt_tokio::Tokio;

#[derive(Debug, PartialEq, Eq)]
struct DistinctivePayload {
    marker: u64,
    label: &'static str,
}

#[test]
fn panic_payload_survives_intact_with_original_type_and_value() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rt.block_on(Tokio.run(|| {
            std::panic::panic_any(DistinctivePayload {
                marker: 0xDEAD_BEEF_CAFE_F00D,
                label: "reviewer-canary",
            });
            #[allow(unreachable_code)]
            42i32
        }))
    }));

    let err = caught.expect_err("closure panicked; run() must propagate it as a real panic");
    let payload = err.downcast::<DistinctivePayload>().expect(
        "payload must survive with its ORIGINAL type, not a String or Box<dyn Any> replacement",
    );
    assert_eq!(
        *payload,
        DistinctivePayload {
            marker: 0xDEAD_BEEF_CAFE_F00D,
            label: "reviewer-canary",
        },
        "payload must survive with its ORIGINAL value, not be replaced by run()'s own message"
    );
}

/// Same probe, but under load: many concurrent panicking closures at once,
/// racing against tokio's blocking-pool scheduling. If any timing window
/// let a genuine panic slip through classify() as Cancelled instead of
/// resume_unwind, this would surface it as a "did not panic" join failure
/// instead of a downcast success. See the module doc above for why this is
/// insurance on top of a structural (source-level) argument, not the
/// primary proof.
#[test]
fn panic_is_never_observed_as_cancelled_under_concurrent_load() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .max_blocking_threads(4)
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let mut handles = Vec::new();
        for i in 0..64u64 {
            handles.push(tokio::spawn(async move {
                let caught = std::panic::AssertUnwindSafe(Tokio.run(move || {
                    std::panic::panic_any(DistinctivePayload {
                        marker: i,
                        label: "load-canary",
                    });
                    #[allow(unreachable_code)]
                    0i32
                }));
                let result: Result<Result<i32, Cancelled>, _> =
                    futures_lite_block_on_catch(caught).await;
                result
            }));
        }
        for (i, h) in handles.into_iter().enumerate() {
            let outcome = h
                .await
                .expect("observer task itself must not panic/be cancelled");
            match outcome {
                Err(payload) => {
                    let p = payload
                        .downcast::<DistinctivePayload>()
                        .expect("payload type must survive under concurrent load too");
                    assert_eq!(p.marker, i as u64);
                }
                Ok(Ok(_)) => panic!("closure was supposed to panic, not return a value"),
                Ok(Err(Cancelled)) => {
                    panic!("iteration {i}: a genuine panic was misclassified as Cancelled")
                }
            }
        }
    });
}

// `catch_unwind` cannot directly wrap a `.await` in an async fn (the
// compiler rejects it: futures aren't UnwindSafe across suspension points
// in general, and there's no synchronous boundary to catch across). Route
// through `FutureExt::catch_unwind`-equivalent by hand: poll to completion
// inside a `catch_unwind`-protected `block_on`-free driver using
// `futures_lite`'s available primitive would pull in a new dependency, so
// instead we drive it with tokio's own `spawn` + `JoinHandle`, which
// already turns a task panic into `Err(JoinError)` we can inspect directly
// without needing `catch_unwind` at all.
async fn futures_lite_block_on_catch<
    F: std::future::Future<Output = Result<i32, Cancelled>> + Send + 'static,
>(
    fut: F,
) -> Result<Result<i32, Cancelled>, Box<dyn std::any::Any + Send>> {
    // Spawn so a panic inside `fut` is caught by tokio's own task
    // machinery and reported as a JoinError, exactly like Blocking::run
    // itself relies on for the underlying spawn_blocking call - this lets
    // us inspect the panic payload without fighting UnwindSafe bounds.
    match tokio::spawn(fut).await {
        Ok(v) => Ok(v),
        Err(e) if e.is_panic() => Err(e.into_panic()),
        Err(e) => panic!("unexpected non-panic JoinError from the observer task: {e}"),
    }
}
