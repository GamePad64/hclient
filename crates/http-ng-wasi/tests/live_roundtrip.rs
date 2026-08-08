//! An integration run of `WasiHttp::execute` against a real `wasi:http`
//! host (Task 16).
//!
//! Closes the specific gap described in the doc comment on
//! `Body::is_end_stream` (`crates/http-ng-wasi/src/body.rs`): the
//! `Inner::Incoming(i) => i.is_end_stream()` branch has no
//! `IncomingResponseBody` constructor without a live host, so it can't
//! be checked with a unit test — the review's mutation run confirmed
//! that replacing this branch with a hard `false` (the very `act`-side
//! host bug this whole task exists for) doesn't fail a single test in
//! `#[cfg(test)]`.
//!
//! The mock server below deliberately responds with
//! `Transfer-Encoding: chunked` plus a trailer, rather than plain
//! `Content-Length`: without trailers the mutation has no externally
//! observable difference from the honest implementation (both branches
//! become `Inner::Done`/`true` in exactly the same `poll_frame` call, see
//! the module doc comment on `examples/live_roundtrip_guest.rs` — which
//! also cites the `wasip3::http_compat` source this is established
//! from). With a trailer, `is_end_stream()` on the real host becomes
//! `true` one frame before our own `Body` transitions to `Inner::Done` —
//! and that window is exactly what the guest catches.
//!
//! # Why this file is native, not `#[cfg(target_os = "wasi")]`
//!
//! The first version put the mock server (a raw `TcpListener`) and the
//! client-side `WasiHttp::execute` call into the same guest task under
//! `wasm32-wasip2`, glued together with `futures::join!`. This trapped
//! wasmtime: `cannot block a synchronous task before returning`, as soon
//! as `client::send` reached a point where it genuinely had nothing left
//! to poll non-blockingly. The root cause: an ordinary `fn main()`
//! compiles to a SYNCHRONOUS `wasi:cli/run@0.2.0` export, and a
//! synchronous root task in the Component Model can't genuinely wait
//! (`task.wait`) on its subtasks asynchronously — no matter what else it's
//! doing alongside. The export that can wait is the asynchronous
//! `wasi:cli/run@0.3.0` (`wasip3::cli::command::export!`), which is
//! incompatible with an ordinary `fn main()`/`#[test]` target (see the
//! doc comment on `wasip3::cli::command::export!`) — only with `cdylib`.
//!
//! So the division of labor here is:
//! - The mock server — here, native, plain `std::net` on its own OS
//!   thread; no WASI, no sync/async collision.
//! - The client call — `examples/live_roundtrip_guest.rs`, a separate
//!   `cdylib` component with an async `run()`, launched under `wasmtime`
//!   as a subprocess. This component's only job is to wait on
//!   `WasiHttp::execute`; it does nothing synchronous at all.
//!
//! `#![cfg(not(target_arch = "wasm32"))]`: this file itself never
//! compiles under `wasm32-wasip2` — it uses `std::process::Command` to
//! launch wasmtime, which wouldn't make sense (and probably wouldn't
//! work) from inside a guest. `cargo test -p http-ng-wasi --target
//! wasm32-wasip2` still runs 21 clean unit tests from `src/`; this file
//! just doesn't take part in that run.
#![cfg(not(target_arch = "wasm32"))]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Must match `EXPECTED_BODY` in `live_roundtrip_guest.rs`.
const RESPONSE_BODY: &[u8] = b"hello from a real wasi:http host";

#[test]
fn wasi_transport_round_trips_a_real_response_through_wasmtime() {
    let Some(wasmtime) =
        require_wasmtime("wasi_transport_round_trips_a_real_response_through_wasmtime")
    else {
        return;
    };

    let (stdout, stderr, status) = run_guest_against_mock_server(&wasmtime, None, drain_headers);
    if !status.success() || !stdout.contains("ROUNDTRIP_OK") {
        panic!(
            "live wasi:http round-trip failed (exit {:?})\n--- guest stdout ---\n{stdout}\n--- guest stderr ---\n{stderr}",
            status.code(),
        );
    }
}

/// Review resolution, finding B-1, live run: a `Streaming` request body
/// really does emit trailers with no `Trailer:` in the headers —
/// `WasiHttp::execute` must return an error, not silently lose them
/// (measured: `wasi:http`'s HTTP/1.1 encoder drops undeclared trailers on
/// the wire). The mock server doesn't check the actual trailer bytes on
/// the wire — that was already measured while preparing the fix round;
/// this test checks that our guard (`convert::TrailerWatch` and
/// `convert::undeclared_trailers`) actually reaches the caller as a typed
/// error.
#[test]
fn wasi_transport_rejects_streaming_request_trailers_without_a_trailer_header() {
    let Some(wasmtime) = require_wasmtime(
        "wasi_transport_rejects_streaming_request_trailers_without_a_trailer_header",
    ) else {
        return;
    };

    let (stdout, stderr, status) = run_guest_against_mock_server(
        &wasmtime,
        Some("request-trailers-undeclared"),
        drain_request_fully,
    );
    if !status.success() || !stdout.contains("TRAILERS_REJECTED_OK") {
        panic!(
            "expected WasiHttp::execute to reject undeclared streaming request trailers \
             (exit {:?})\n--- guest stdout ---\n{stdout}\n--- guest stderr ---\n{stderr}",
            status.code(),
        );
    }
}

/// Symmetry to the test above: the same `Streaming` stream with
/// trailers, but the `Trailer:` header is declared correctly — the guard
/// must not false-positive on legitimate trailer use.
#[test]
fn wasi_transport_accepts_streaming_request_trailers_when_declared() {
    let Some(wasmtime) =
        require_wasmtime("wasi_transport_accepts_streaming_request_trailers_when_declared")
    else {
        return;
    };

    let (stdout, stderr, status) = run_guest_against_mock_server(
        &wasmtime,
        Some("request-trailers-declared"),
        drain_request_fully,
    );
    if !status.success() || !stdout.contains("TRAILERS_ACCEPTED_OK") {
        panic!(
            "expected WasiHttp::execute to accept declared streaming request trailers \
             (exit {:?})\n--- guest stdout ---\n{stdout}\n--- guest stderr ---\n{stderr}",
            status.code(),
        );
    }
}

/// Review resolution, fix round 2 finding 2, live run: `Trailer:` is
/// present but names a DIFFERENT field (`X-Other`) than what the body
/// actually emits (`x-checksum`) — measured that the wire loses
/// `x-checksum` exactly as it would if the header were absent entirely.
/// The guard must compare NAMES, not just whether the header is present.
#[test]
fn wasi_transport_rejects_streaming_request_trailers_with_the_wrong_declared_name() {
    let Some(wasmtime) = require_wasmtime(
        "wasi_transport_rejects_streaming_request_trailers_with_the_wrong_declared_name",
    ) else {
        return;
    };

    let (stdout, stderr, status) = run_guest_against_mock_server(
        &wasmtime,
        Some("request-trailers-wrong-name"),
        drain_request_fully,
    );
    if !status.success() || !stdout.contains("TRAILERS_REJECTED_OK") {
        panic!(
            "expected WasiHttp::execute to reject a Trailer: header naming the wrong field \
             (exit {:?})\n--- guest stdout ---\n{stdout}\n--- guest stderr ---\n{stderr}",
            status.code(),
        );
    }
}

/// Review resolution, fix round 2 finding 3, live run: the body emits an
/// empty trailers frame (`Frame::trailers(HeaderMap::new())`) with no
/// `Trailer:` — nothing to lose on the wire, the guard must not reject
/// it.
#[test]
fn wasi_transport_accepts_an_empty_trailers_frame_without_a_trailer_header() {
    let Some(wasmtime) =
        require_wasmtime("wasi_transport_accepts_an_empty_trailers_frame_without_a_trailer_header")
    else {
        return;
    };

    let (stdout, stderr, status) = run_guest_against_mock_server(
        &wasmtime,
        Some("request-trailers-empty-frame"),
        drain_request_fully,
    );
    if !status.success() || !stdout.contains("TRAILERS_ACCEPTED_OK") {
        panic!(
            "expected WasiHttp::execute to accept an empty trailers frame \
             (exit {:?})\n--- guest stdout ---\n{stdout}\n--- guest stderr ---\n{stderr}",
            status.code(),
        );
    }
}

/// How long the mock server watches the connection before concluding the
/// guest is still there — see [`observe_client_end`].
///
/// Between the guest's own two numbers (`IN_FLIGHT_NS` = 200ms, when it
/// acts; `STAY_ALIVE_NS` = 1.5s, how long it then lives) with a wide
/// margin on both sides, so one constant serves both directions: the drop
/// happens well inside this window, and the hold outlasts it.
const CANCEL_OBSERVATION_WINDOW: std::time::Duration = std::time::Duration::from_millis(700);

/// What the mock server saw at its end of the connection.
#[derive(Debug, PartialEq, Eq)]
enum ClientEnd {
    /// The peer closed: `read` returned `Ok(0)`, or the connection was
    /// reset.
    WentAway,
    /// [`CANCEL_OBSERVATION_WINDOW`] passed with the connection intact.
    StillThere,
}

/// Reads the request head, then watches the connection for
/// [`CANCEL_OBSERVATION_WINDOW`] and reports what became of the client end.
///
/// This is the whole of the v0.2 W1 measurement for this backend, and the
/// reason it lives on the native side of the harness: it is an observer
/// **outside** the guest. A guest that dropped a future and then reported
/// "I dropped a future" would prove only that it stopped polling — the
/// exact vacuous shape this project keeps catching. `Ok(0)` here is the
/// kernel reporting a closed peer.
///
/// Reading the head first is not preamble: it establishes that there *was*
/// an exchange in flight to cancel. If the guest's timing were ever wrong
/// and it acted before the request went out, the run fails here rather
/// than passing with nothing having happened.
///
/// After reporting, the connection is deliberately kept open until the
/// peer goes away. Closing it at the end of the window would end the
/// exchange from this side, and the `cancel-hold` guest — still polling
/// its future — would get a transport error instead of the pending
/// exchange the control needs.
fn observe_client_end(
    stream: &mut std::net::TcpStream,
    verdict: &std::sync::mpsc::Sender<ClientEnd>,
) {
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(30)))
        .expect("set_read_timeout");
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    loop {
        let n = stream.read(&mut buf).expect("reading the request head");
        assert_ne!(
            n, 0,
            "the guest closed the connection before sending a request at all — there was \
             never an in-flight exchange for this test to cancel"
        );
        head.extend_from_slice(&buf[..n]);
        if head.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }

    stream
        .set_read_timeout(Some(CANCEL_OBSERVATION_WINDOW))
        .expect("set_read_timeout");
    let seen = match stream.read(&mut buf) {
        Ok(0) => ClientEnd::WentAway,
        Ok(_) => ClientEnd::StillThere,
        Err(e)
            if e.kind() == std::io::ErrorKind::WouldBlock
                || e.kind() == std::io::ErrorKind::TimedOut =>
        {
            ClientEnd::StillThere
        }
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => ClientEnd::WentAway,
        Err(e) => panic!("unexpected error observing the guest's end: {e}"),
    };
    verdict
        .send(seen)
        .expect("the test must still be listening");

    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(30)))
        .expect("set_read_timeout");
    while matches!(stream.read(&mut buf), Ok(n) if n > 0) {}
}

/// v0.2 W1, the claim: dropping the future `WasiHttp::execute` returns
/// stops the exchange, even though the guest owns no socket and can only
/// ask the host to stop.
///
/// The design document guessed the other way ("a WASI host may not expose
/// it"), which is why this is measured rather than declared. The mechanism
/// is the Component Model's `subtask.cancel`, reached through
/// `wit-bindgen`'s `WaitableOperation::drop`; what is asserted here is not
/// that mechanism but its effect on a real socket, under a real wasmtime.
///
/// Mutation that turns this red: replace `drop(exec)` with
/// `std::mem::forget(exec)` in the guest. That is exactly the difference
/// between "the future stopped being polled" and "the exchange stopped" —
/// `forget` stops the polling and releases nothing — and the server then
/// sits out the whole window with the connection open.
#[test]
fn dropping_the_execute_future_closes_the_connection_the_server_sees() {
    let Some(wasmtime) =
        require_wasmtime("dropping_the_execute_future_closes_the_connection_the_server_sees")
    else {
        return;
    };
    let (stdout, stderr, status, seen) = run_guest_against_silent_server(&wasmtime, "cancel-drop");
    assert!(
        status.success() && stdout.contains("CANCEL_DROPPED_OK"),
        "the guest run itself failed (exit {:?})\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}",
        status.code(),
    );
    assert_eq!(
        seen,
        ClientEnd::WentAway,
        "dropping the execute future must end the exchange on the wire: the server still had \
         the connection {CANCEL_OBSERVATION_WINDOW:?} after the guest dropped it, while the \
         guest itself was still running"
    );
}

/// The control for the test above, and the half that gives it meaning: the
/// same guest, the same server, the same window — the future kept alive
/// and polled instead of dropped.
///
/// Without this, "the connection closed" could be the host's own idle
/// policy, the guest exiting, or anything else that happens to coincide
/// with the window.
#[test]
fn holding_the_execute_future_leaves_the_connection_open() {
    let Some(wasmtime) = require_wasmtime("holding_the_execute_future_leaves_the_connection_open")
    else {
        return;
    };
    let (stdout, stderr, status, seen) = run_guest_against_silent_server(&wasmtime, "cancel-hold");
    assert!(
        status.success() && stdout.contains("CANCEL_HELD_OK"),
        "the guest run itself failed (exit {:?})\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}",
        status.code(),
    );
    assert_eq!(
        seen,
        ClientEnd::StillThere,
        "a live, polled execute future must keep its exchange: if this connection closes on \
         its own, the drop test above is measuring the passage of time and not the drop"
    );
}

/// The declaration and the measurement are kept in one file on purpose.
///
/// `CancelSupport::None` is a legitimate answer for a backend that cannot
/// cancel — and it is therefore also the quiet way to make the two tests
/// above stop applying here. Asserting the declared value closes that
/// exit: `WasiHttp` giving up cancellation would take an explicit edit to
/// this test rather than a capability flip that leaves a green suite.
///
/// Not behind `require_wasmtime`: no host is involved in reading a
/// capability, so this one runs everywhere the rest of the file compiles.
#[test]
fn wasi_declares_the_cancellation_it_performs() {
    use http_ng_core::unversioned::Transport;
    assert_eq!(
        http_ng_wasi::WasiHttp::new().capabilities().cancel_on_drop,
        http_ng_core::CancelSupport::Supported,
        "the live runs in this file measure a cancellation that `WasiHttp` must also \
         declare — a backend is free to declare `None`, but not to declare `None` while \
         behaving otherwise, nor to quietly stop being covered by the measurement"
    );
}

/// Brings up a server that reads the request and then says nothing at all,
/// runs the guest against it in `mode`, and returns the guest's output
/// alongside the server's [`ClientEnd`] verdict.
///
/// Separate from [`run_guest_against_mock_server`] because a response is
/// precisely what must not happen here: an answered request leaves nothing
/// in flight, and a cancellation test with nothing to cancel passes for
/// free.
fn run_guest_against_silent_server(
    wasmtime: &Path,
    mode: &str,
) -> (String, String, std::process::ExitStatus, ClientEnd) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("local_addr").port();
    let (verdict_tx, verdict_rx) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        observe_client_end(&mut stream, &verdict_tx);
    });

    let artifact = build_guest();
    let output = Command::new(wasmtime)
        .args([
            "run",
            "-S",
            "http",
            "--",
            artifact.to_str().expect("utf8 path"),
            &port.to_string(),
            mode,
        ])
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn wasmtime");

    server.join().expect("mock server thread panicked");
    let seen = verdict_rx
        .recv()
        .expect("the server thread must report a verdict");

    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status,
        seen,
    )
}

/// Reads only up through the end of the request headers — used by the
/// `response-roundtrip` scenario, where the request has no body at all
/// (`RequestBody::Empty`), so nothing more would arrive anyway.
fn drain_headers(stream: &mut std::net::TcpStream) {
    let mut buf = [0u8; 1024];
    let mut seen = Vec::new();
    loop {
        let n = stream.read(&mut buf).expect("read");
        if n == 0 {
            break;
        }
        seen.extend_from_slice(&buf[..n]);
        if seen.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
}

/// Reads until there's a pause with no new bytes — used by scenarios
/// with a request body (`request-trailers-*`), where chunked data frames
/// (and maybe trailers) still arrive after the headers. Doesn't read "to
/// EOF": `wasi:http` isn't required to close the TCP connection after
/// the request body. Bodies in these tests are a handful of bytes, so
/// even a tightly cut silence window comfortably covers the time the
/// guest needs to write.
fn drain_request_fully(stream: &mut std::net::TcpStream) {
    stream
        .set_read_timeout(Some(std::time::Duration::from_millis(1500)))
        .expect("set_read_timeout");
    let mut buf = [0u8; 4096];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(_) => continue,
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                break;
            }
            Err(e) => panic!("unexpected read error while draining request: {e}"),
        }
    }
}

/// Shared scaffolding: bring up the mock server (accept one connection,
/// drain the request via `drain`, respond with a known `chunked`+
/// `Trailer:` response), build the guest, and run it under `wasmtime` in
/// the given mode, pointing it at the mock server via argv. Returns
/// `(stdout, stderr, ExitStatus)` for the guest — the calling test
/// decides for itself what counts as success for its mode.
fn run_guest_against_mock_server(
    wasmtime: &Path,
    mode: Option<&str>,
    drain: fn(&mut std::net::TcpStream),
) -> (String, String, std::process::ExitStatus) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("local_addr").port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        drain(&mut stream);
        // `chunked` + `Trailer:`, not `Content-Length` — see the module
        // doc comment for why only a trailer gives this test any chance
        // of catching a hardcoded-`false` mutation of `is_end_stream()`.
        // A shared response for all modes — the `request-trailers-*`
        // modes check behavior on the REQUEST side and don't read this
        // response meaningfully.
        let mut out =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nTrailer: X-Checksum\r\n\r\n"
                .to_vec();
        out.extend_from_slice(format!("{:x}\r\n", RESPONSE_BODY.len()).as_bytes());
        out.extend_from_slice(RESPONSE_BODY);
        out.extend_from_slice(b"\r\n0\r\nX-Checksum: deadbeef\r\n\r\n");
        stream.write_all(&out).expect("write");
        let _ = stream.flush();
    });

    let artifact = build_guest();

    let mut args = vec![
        "run".to_string(),
        "-S".to_string(),
        "http".to_string(),
        "--".to_string(),
        artifact.to_str().expect("utf8 path").to_string(),
        port.to_string(),
    ];
    if let Some(mode) = mode {
        args.push(mode.to_string());
    }
    let output = Command::new(wasmtime)
        .args(&args)
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn wasmtime");

    server.join().expect("mock server thread panicked");

    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status,
    )
}

/// The variable a job that promises to install `wasmtime` sets to
/// announce that promise. See `require_wasmtime`.
const REQUIRE_MARKER: &str = "HTTP_NG_REQUIRE_WASMTIME";

/// Review resolution (Task 16, finding B-7): missing `wasmtime` used to
/// lead to the same thing everywhere — a `NOTICE` on stderr and `return`
/// from a test that then looks like `ok` itself. On a laptop without
/// `wasmtime` that's a reasonable compromise, but where `wasmtime` is
/// promised, it's exactly the class of defect fixed across every other
/// job in this vertical: a green `cargo test` stops meaning anything was
/// actually checked.
///
/// **The key is `HTTP_NG_REQUIRE_WASMTIME`, not `CI` (B3 of the branch's
/// final review).** The guard was right in intent and wrong in signal:
/// `CI` is set by GitHub Actions for EVERY job, while exactly one job
/// installs `wasmtime` — `wasip2`. The matrix `test` job runs `cargo
/// test --workspace --all-features`, picks up this file (it's native,
/// `#![cfg(not(target_arch = "wasm32"))]`), has no `wasmtime`, and used
/// to fail on all three runners — reproduced by simulating the runner:
/// `0 passed; 5 failed`. Meaning this branch's CI, going by the tree,
/// was never actually green: the file was invalid YAML from `68d91f3` to
/// `123b88c`, and the very first push after the fix would have run
/// straight into this.
///
/// `CI` means "some job somewhere"; what's needed is "the job that
/// promised to install wasmtime". The strictness stays exactly where the
/// promise was made; `test`, `msrv`, any third-party CI, and a laptop
/// all equally skip the run with a `NOTICE`. The name symmetry between
/// the guard and the workflow is held by the
/// `the_job_that_installs_wasmtime_exports_the_marker_this_guard_keys_on`
/// test.
fn require_wasmtime(test_name: &str) -> Option<PathBuf> {
    if let Some(p) = find_wasmtime() {
        return Some(p);
    }
    if std::env::var_os(REQUIRE_MARKER).is_some() {
        panic!(
            "`wasmtime` not found even though `{REQUIRE_MARKER}` is set (`{test_name}`) — the \
             `wasip2` job was supposed to install it before this test; the environment is \
             broken, not deliberately limited the way a laptop without wasmtime is."
        );
    }
    eprintln!(
        "NOTICE: `wasmtime` not found — skipping the live run `{test_name}`. This environment \
         can't confirm it against a real host."
    );
    None
}

/// The `require_wasmtime` guard and `ci.yml` hold the same contract from
/// two sides, and the variable name falling out of sync would make the
/// `wasip2` job completely silent: every live test in this file would
/// print `NOTICE` and report `ok`, having checked nothing. The same class of defect
/// that `sse-complexity-guard` guards against by counting "exactly one
/// test ran", and the same technique — verify the symmetry, don't just
/// trust it.
///
/// Both strings are searched for INSIDE the `wasip2` job's block, not
/// anywhere in the file: a marker that drifted into a different job (or
/// was left in the file after the wasmtime install was removed from it)
/// is exactly the breakage this test is supposed to catch.
///
/// The marker is looked for as a YAML ASSIGNMENT, in a block with
/// comment lines stripped out, not as a substring. The first version of
/// this test survived both mutations it was written to catch (removing
/// `env:` and moving the marker into a different job): the variable name
/// is also named in `ci.yml` in a nearby comment, and in the text of the
/// `echo "::error::…"` that complains about it. The test was reading
/// prose and mistaking it for the implementation — exactly the class of
/// vacuous check this branch was cleaning up everywhere. `ci.yml`'s
/// diagnostics are full of the names of things done nearby; searching
/// them proves nothing at all.
#[test]
fn the_job_that_installs_wasmtime_exports_the_marker_this_guard_keys_on() {
    let ci = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github/workflows/ci.yml");
    let raw = std::fs::read_to_string(&ci)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", ci.display()));
    // Comments are stripped, but LINES are kept (as empty): the job
    // block boundaries below are found by indentation, and a shifted
    // line count would make them wrong.
    let text: String = raw
        .lines()
        .map(|l| {
            if l.trim_start().starts_with('#') {
                ""
            } else {
                l
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    // The job block runs from the `  wasip2:` line to the next key at
    // the same nesting level (two spaces). Crude, but good enough — the
    // alternative is dragging a YAML parser into dev-dependencies for
    // one check.
    let start = text.find("\n  wasip2:\n").expect(
        "ci.yml has no `wasip2` job — the `require_wasmtime` guard is left with no installer",
    );
    let rest = &text[start + 1..];
    let end = rest
        .lines()
        .scan(0usize, |off, line| {
            let here = *off;
            *off += line.len() + 1;
            Some((here, line))
        })
        .skip(1)
        .find(|(_, line)| {
            line.starts_with("  ") && !line.starts_with("   ") && line.trim_end().ends_with(':')
        })
        .map_or(rest.len(), |(off, _)| off);
    let job = &rest[..end];

    assert!(
        job.contains("cargo install wasmtime-cli"),
        "job `wasip2` no longer installs wasmtime — the `require_wasmtime` guard is strict \
         where nothing promises it anymore"
    );
    // An assignment, not a mention: `KEY: value` on its own line (the
    // usual form) or inside an inline `env: { KEY: value }` map. The
    // diagnostic `echo` in the same job names the same variable, and a
    // bare `contains` is enough for the test to pass with `env:` removed
    // entirely — checked by mutation.
    let assigned = job.lines().any(|l| {
        let t = l.trim_start();
        let assignment = format!("{REQUIRE_MARKER}:");
        t.starts_with(&assignment) || (t.starts_with("env:") && t.contains(&assignment))
    });
    assert!(
        assigned,
        "job `wasip2` installs wasmtime but doesn't set `{REQUIRE_MARKER}` — every live test \
         in this file will silently skip, and the job will come back green having checked \
         nothing"
    );
}

fn find_wasmtime() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("WASMTIME") {
        let p = PathBuf::from(path);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let candidate = Path::new(&home).join(".cargo/bin/wasmtime");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    for dir in std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
    {
        let candidate = dir.join("wasmtime");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Builds `examples/live_roundtrip_guest.rs` under `wasm32-wasip2` and
/// returns the path to the resulting `.wasm`, read out of
/// `--message-format=json` — not assembled by hand from a relative path
/// (which breaks under a non-standard `CARGO_TARGET_DIR`).
fn build_guest() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let output = Command::new(env!("CARGO"))
        .args([
            "build",
            "--manifest-path",
            &format!("{manifest_dir}/Cargo.toml"),
            "--target",
            "wasm32-wasip2",
            "--example",
            "live_roundtrip_guest",
            "--message-format=json",
        ])
        .output()
        .expect("failed to spawn cargo build for the guest");

    if !output.status.success() {
        panic!(
            "failed to build live_roundtrip_guest for wasm32-wasip2 \
             (is the `wasm32-wasip2` rustup target installed?)\n--- stderr ---\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if msg.get("reason").and_then(|r| r.as_str()) != Some("compiler-artifact") {
            continue;
        }
        let is_our_target = msg
            .get("target")
            .and_then(|t| t.get("name"))
            .and_then(|n| n.as_str())
            == Some("live_roundtrip_guest");
        if !is_our_target {
            continue;
        }
        // A `cdylib` with no `fn main()` doesn't count as "executable" to
        // cargo (this field stays `null`) — the path to the `.wasm`
        // comes from `filenames`.
        let wasm = msg
            .get("filenames")
            .and_then(|f| f.as_array())
            .into_iter()
            .flatten()
            .filter_map(|f| f.as_str())
            .find(|f| f.ends_with(".wasm"));
        if let Some(path) = wasm {
            return PathBuf::from(path);
        }
    }
    panic!(
        "cargo build did not report a .wasm artifact for live_roundtrip_guest; raw output:\n{stdout}"
    );
}
