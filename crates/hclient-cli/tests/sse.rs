//! `--sse`, run as a binary against a real server on loopback.
//!
//! The server is written by hand with `std::net`, for the reason
//! `end_to_end.rs` gives about its own: a fixture built out of the library
//! under test can agree with it about a mistake. What is asserted here is
//! the **bytes the server received** and the **bytes `hc` wrote**, which
//! are the two ends the unit tests in `src/sse.rs` and `src/mode.rs`
//! cannot reach — those prove that an event formats correctly and that a
//! flag is refused, and say nothing about whether either reached the wire.
//!
//! Nothing here is asserted by a clock. The reconnect test is released by
//! the server's own `retry:` field rather than by a sleep, and every
//! "this ended" is the connection closing.

use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{SocketAddr, TcpListener};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// A server that answers the *n*th connection with whatever `answer`
/// returns for it, and records the request head it was asked with.
///
/// It closes the socket after answering, which is how an SSE stream ends
/// cleanly — there is no other in-band terminator, and that is exactly
/// what makes "does `--sse` stop when the stream stops" a thing worth
/// testing.
fn serve<F>(answer: F) -> (SocketAddr, Arc<Mutex<Vec<String>>>)
where
    F: Fn(usize) -> Option<Vec<u8>> + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let heads: Arc<Mutex<Vec<String>>> = Arc::default();
    let sink = Arc::clone(&heads);
    let nth = Arc::new(AtomicUsize::new(0));
    let answer = Arc::new(answer);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let sink = Arc::clone(&sink);
            let nth = Arc::clone(&nth);
            let answer = Arc::clone(&answer);
            std::thread::spawn(move || {
                let mut stream = stream;
                let mut reader = BufReader::new(stream.try_clone().expect("dup"));
                let mut head = String::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_err() || line.is_empty() {
                        return;
                    }
                    let done = line.trim_end().is_empty();
                    head.push_str(&line);
                    if done {
                        break;
                    }
                }
                let i = nth.fetch_add(1, Ordering::SeqCst);
                sink.lock().expect("not poisoned").push(head);
                if let Some(bytes) = answer(i) {
                    let _ = stream.write_all(&bytes);
                    let _ = stream.flush();
                }
            });
        }
    });
    (addr, heads)
}

/// A `200` whose body is `body`, announced as an event stream and
/// terminated by the close rather than by a length.
fn stream(body: &str) -> Vec<u8> {
    format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{body}")
        .into_bytes()
}

struct Ran {
    code: i32,
    stdout: String,
    stderr: String,
}

/// A watchdog, never a threshold: every failure it can produce is "this
/// did not end", and no test passes because something happened quickly
/// enough.
///
/// It is here because the sharpest mutation this file has to kill —
/// making `--sse` reconnect unconditionally — does not produce a wrong
/// answer, it produces **no answer at all**: the stream reopens the
/// fixture for ever. Without a bound that mutation hangs the suite
/// instead of failing it, which is the one outcome worse than a silent
/// pass.
const BOUND: std::time::Duration = std::time::Duration::from_secs(20);

fn run_hc(args: &[&str], input: &str) -> Ran {
    let mut child = Command::new(env!("CARGO_BIN_EXE_hc"))
        .args(args)
        .arg("--no-color")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the binary is built by cargo before this test runs");
    {
        let mut stdin = child.stdin.take().expect("piped");
        // Ignored: a run that refuses its command line has already exited
        // by now, and the broken pipe that produces is not the subject of
        // any test here.
        let _ = stdin.write_all(input.as_bytes());
        // Dropped here, which is the EOF `--ws` ends on.
    }
    let started = std::time::Instant::now();
    let mut timed_out = false;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() > BOUND => {
                timed_out = true;
                let _ = child.kill();
                break;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
            Err(e) => panic!("waiting on hc: {e}"),
        }
    }
    let out = child.wait_with_output().expect("wait");
    assert!(
        !timed_out,
        "hc {args:?} did not end within {BOUND:?} — the run was killed, so nothing below \
         this line is an assertion about its output"
    );
    Ran {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn hc(args: &[&str]) -> Ran {
    run_hc(args, "")
}

fn url(addr: SocketAddr, path: &str) -> String {
    format!("http://{addr}{path}")
}

fn head_has(head: &str, name: &str, value: &str) -> bool {
    head.lines().any(|l| {
        l.split_once(':').is_some_and(|(n, v)| {
            n.trim().eq_ignore_ascii_case(name) && v.trim().eq_ignore_ascii_case(value)
        })
    })
}

/// The default, piped: `data` and nothing else, one line per message —
/// which is what makes `hc --sse … | jq` work with no flag. And the run
/// **ends** when the stream does, which is the whole of what "`--sse` does
/// not reconnect" means from outside.
#[test]
fn a_piped_run_prints_data_alone_and_ends_when_the_stream_does() {
    let (addr, heads) = serve(|_| {
        Some(stream(
            ": warming up\ndata: one\n\nevent: tick\nid: 7\ndata: two\n\nretry: 5000\n\n",
        ))
    });
    let ran = hc(&["--sse", &url(addr, "/events")]);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    // Exactly the data, and nothing else: the comment and the `retry:`
    // are not messages, and a pipeline reading one line per message must
    // not be handed a line that is not one.
    assert_eq!(ran.stdout, "one\ntwo\n");
    // The `Accept` the SSE builder adds — which the caller never typed —
    // actually reached the wire.
    let heads = heads.lock().expect("not poisoned");
    assert_eq!(
        heads.len(),
        1,
        "one connection, because it does not reconnect"
    );
    assert!(
        head_has(&heads[0], "accept", "text/event-stream"),
        "{}",
        heads[0]
    );
    assert!(heads[0].starts_with("GET /events HTTP/1.1"), "{}", heads[0]);
}

/// `-v` prints the event back in SSE's own syntax, which is the property
/// that makes the annotated form diffable against the bytes that arrived
/// — and it prints the request this tool built, `Accept` included, for
/// the same reason `--print H` does for a request.
#[test]
fn verbose_re_serialises_each_event_in_the_wire_format() {
    let (addr, _) = serve(|_| {
        Some(stream(
            ": keep-alive\nevent: tick\nid: 7\ndata: hello\n\nretry: 5000\n\n",
        ))
    });
    let ran = hc(&["--sse", "-v", &url(addr, "/e")]);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert!(
        ran.stdout.contains("accept: text/event-stream"),
        "{}",
        ran.stdout
    );
    assert!(
        ran.stdout.contains("event: tick\nid: 7\ndata: hello\n\n"),
        "{}",
        ran.stdout
    );
    // The two that the payload form drops are present here, as what they
    // are: a comment is not data and `retry:` is an instruction.
    assert!(ran.stdout.contains(": keep-alive"), "{}", ran.stdout);
    assert!(ran.stdout.contains("retry: 5000"), "{}", ran.stdout);
}

/// WHATWG makes any status but 200 a **permanent** failure of an SSE
/// connection, so the stream never opens and `hc` exits 4 — the request
/// failed. This is also the control that says refusing `--check-status`
/// costs a caller nothing: the check it would have performed has already
/// happened, one layer down and unconditionally.
#[test]
fn a_status_that_is_not_200_fails_the_stream_without_check_status() {
    let (addr, _) =
        serve(|_| Some(b"HTTP/1.1 204 No Content\r\nconnection: close\r\n\r\n".to_vec()));
    let ran = hc(&["--sse", &url(addr, "/e")]);
    assert_eq!(ran.code, 4, "stdout {:?} stderr {}", ran.stdout, ran.stderr);
    assert!(ran.stderr.contains("not an SSE stream"), "{}", ran.stderr);
}

/// A `200` that is not an event stream is refused rather than printed —
/// the same rule, one header over, and the reason `--sse` cannot simply
/// be `hc` with a different printer.
#[test]
fn a_200_that_is_not_an_event_stream_is_refused() {
    let (addr, _) = serve(|_| {
        Some(
            b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\nconnection: close\r\n\r\ndata: x\n\n"
                .to_vec(),
        )
    });
    let ran = hc(&["--sse", &url(addr, "/e")]);
    assert_eq!(ran.code, 4, "{}", ran.stderr);
    assert!(ran.stderr.contains("content-type"), "{}", ran.stderr);
}

/// `--sse-reconnect` reopens, carries `Last-Event-ID`, and honours the
/// server's own `retry:` — which is what releases this test, so nothing
/// here is waiting on a clock of the test's choosing.
///
/// The third connection answers `204`, which WHATWG makes terminal, so
/// the run ends by a decision rather than by being killed.
#[test]
fn reconnect_reopens_carrying_the_last_event_id_and_stops_on_a_terminal_status() {
    let (addr, heads) = serve(|i| match i {
        // `retry: 10` first: the reconnect delay is the server's from
        // then on, so the two reopens below cost ~10ms rather than the
        // policy's default second.
        0 => Some(stream("retry: 10\n\nid: 7\ndata: one\n\n")),
        1 => Some(stream("data: two\n\n")),
        _ => Some(b"HTTP/1.1 204 No Content\r\nconnection: close\r\n\r\n".to_vec()),
    });
    let ran = hc(&["--sse", "--sse-reconnect", &url(addr, "/e")]);
    assert_eq!(ran.code, 4, "stdout {:?} stderr {}", ran.stdout, ran.stderr);
    assert_eq!(ran.stdout, "one\ntwo\n");

    let heads = heads.lock().expect("not poisoned");
    assert_eq!(heads.len(), 3, "two reopens after the first connection");
    // The id the first connection established travels on the second,
    // which is the whole point of reconnecting rather than restarting.
    assert!(head_has(&heads[1], "last-event-id", "7"), "{}", heads[1]);
    // And it is still 7 on the third, because the second connection sent
    // no `id:` of its own — WHATWG's last event ID belongs to the stream
    // rather than to a connection.
    assert!(head_has(&heads[2], "last-event-id", "7"), "{}", heads[2]);
}

/// The control for the test above, and for `mode::select`'s default: the
/// same server, without the flag, is asked exactly once.
#[test]
fn without_the_flag_the_same_server_is_asked_once() {
    let (addr, heads) = serve(|i| match i {
        0 => Some(stream("id: 7\ndata: one\n\n")),
        _ => Some(stream("data: two\n\n")),
    });
    let ran = hc(&["--sse", &url(addr, "/e")]);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert_eq!(ran.stdout, "one\n");
    assert_eq!(heads.lock().expect("not poisoned").len(), 1);
}

/// `--follow` is the one flag in this mode whose effect had somewhere
/// else to travel: `SseBuilder` has no redirect setter, so the policy
/// goes on the `Client`. Without the flag the same hop is the answer and
/// the stream refuses it, which is the control that says the policy is
/// doing the work.
#[test]
fn follow_is_honoured_for_a_stream_by_putting_the_policy_on_the_client() {
    let (addr, _) = serve(|i| {
        Some(if i == 0 {
            b"HTTP/1.1 302 Found\r\nlocation: /moved\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_vec()
        } else {
            stream("data: arrived\n\n")
        })
    });
    let followed = hc(&["--sse", "-L", &url(addr, "/e")]);
    assert_eq!(followed.code, 0, "{}", followed.stderr);
    assert_eq!(followed.stdout, "arrived\n");

    let (addr, _) = serve(|i| {
        Some(if i == 0 {
            b"HTTP/1.1 302 Found\r\nlocation: /moved\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_vec()
        } else {
            stream("data: arrived\n\n")
        })
    });
    let not_followed = hc(&["--sse", &url(addr, "/e")]);
    assert_eq!(not_followed.code, 4, "{}", not_followed.stderr);
    assert_eq!(not_followed.stdout, "");
}

/// The refusals, through the real binary: exit 2, the flag named, and the
/// mode named. `src/mode.rs` proves the table is complete; this proves the
/// table is *reached* — a refusal computed and never consulted would pass
/// every unit test in that file.
#[test]
fn a_flag_this_mode_cannot_honour_is_refused_by_name_before_anything_connects() {
    // `127.0.0.1:1` answers nothing, so a run that got as far as
    // connecting would fail with 4 rather than 2 — which is what makes
    // "before anything connects" an assertion rather than a hope.
    for (flag, extra) in [
        ("--check-status", vec!["--check-status"]),
        ("-w", vec!["-w", "%{http_code}"]),
        ("--print", vec!["--print", "hb"]),
        ("--auth", vec!["-a", "u:p"]),
    ] {
        let mut args = vec!["--sse", "http://127.0.0.1:1/e"];
        args.extend_from_slice(&extra);
        let ran = hc(&args);
        assert_eq!(ran.code, 2, "{flag}: {} {}", ran.stdout, ran.stderr);
        assert!(ran.stderr.contains("--sse"), "{flag}: {}", ran.stderr);
    }
    // A body item is the same refusal reached through the grammar rather
    // than through a flag.
    let ran = hc(&["--sse", "http://127.0.0.1:1/e", "a=1"]);
    assert_eq!(ran.code, 2, "{} {}", ran.stdout, ran.stderr);
    assert!(ran.stderr.contains("name=value"), "{}", ran.stderr);
}

/// The control for the test above: the same URL with none of those flags
/// gets as far as the connection and fails there, so exit 2 above really
/// was the refusal and not something every `--sse` run does.
#[test]
fn without_a_refused_flag_the_same_command_line_reaches_the_network() {
    let ran = hc(&["--sse", "http://127.0.0.1:1/e"]);
    assert_eq!(ran.code, 4, "{} {}", ran.stdout, ran.stderr);
}

/// A `ws://` URL outside `--ws` is refused rather than turned into a
/// request to a host named `wss`.
#[test]
fn a_websocket_url_without_ws_is_refused_by_name() {
    let ran = hc(&["wss://example.invalid/s"]);
    assert_eq!(ran.code, 2, "{} {}", ran.stdout, ran.stderr);
    assert!(ran.stderr.contains("--ws"), "{}", ran.stderr);
}
