//! The binary, run as a binary, against a real server on loopback.
//!
//! These are the tests the unit ones cannot be: the grammar tests in
//! `args.rs` prove a string classifies correctly and say nothing about
//! whether the classification reaches the wire. Everything here reads what
//! the **server** received, which is the same standard the rest of this
//! workspace holds its transports to.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{SocketAddr, TcpListener};
use std::process::Command;
use std::sync::{Arc, Mutex};

/// What one request looked like from the far side of the socket.
#[derive(Debug, Clone, Default)]
struct Seen {
    line: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// A server that answers one canned response and records what it was
/// asked, on a real socket, speaking HTTP/1.1 by hand.
///
/// By hand rather than through this workspace's own transport on purpose:
/// a fixture built from the library under test can agree with it about a
/// mistake. The bytes are what the CLI must get right.
fn serve(status: u16, content_type: &str, body: &str) -> (SocketAddr, Arc<Mutex<Vec<Seen>>>) {
    let content_type = content_type.to_owned();
    let body = body.to_owned();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let log: Arc<Mutex<Vec<Seen>>> = Arc::default();
    let sink = Arc::clone(&log);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let sink = Arc::clone(&sink);
            let content_type = content_type.clone();
            let body = body.clone();
            std::thread::spawn(move || {
                let mut stream = stream;
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut seen = Seen::default();
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() || line.is_empty() {
                    return;
                }
                seen.line = line.trim_end().to_owned();
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).is_err() {
                        return;
                    }
                    let h = h.trim_end();
                    if h.is_empty() {
                        break;
                    }
                    if let Some((n, v)) = h.split_once(':') {
                        seen.headers
                            .push((n.trim().to_owned(), v.trim().to_owned()));
                    }
                }
                let len: usize = seen
                    .header("content-length")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                if len > 0 {
                    let mut buf = vec![0u8; len];
                    if reader.read_exact(&mut buf).is_ok() {
                        seen.body = String::from_utf8_lossy(&buf).into_owned();
                    }
                }
                sink.lock().unwrap().push(seen);
                let head = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(body.as_bytes());
                let _ = stream.flush();
            });
        }
    });
    (addr, log)
}

struct Ran {
    code: i32,
    stdout: String,
    stderr: String,
}

fn hc(args: &[&str]) -> Ran {
    let out = Command::new(env!("CARGO_BIN_EXE_hc"))
        .args(args)
        // Deterministic output regardless of where the suite runs: the
        // colour decision is otherwise a property of the terminal, and a
        // test asserting on escape sequences would pass or fail by
        // environment.
        .arg("--no-color")
        .output()
        .expect("the binary is built by cargo before this test runs");
    Ran {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn url(addr: SocketAddr, path: &str) -> String {
    format!("http://{addr}{path}")
}

#[test]
fn a_get_prints_the_body_and_nothing_else_when_stdout_is_not_a_terminal() {
    let (addr, log) = serve(200, "text/plain", "hello");
    let r = hc(&[&url(addr, "/x")]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    // A pipe gets the body alone — this is what makes `hc … | jq` work
    // without a flag, and the assertion is that the head is absent rather
    // than merely that the body is present.
    assert_eq!(r.stdout, "hello\n");
    let seen = log.lock().unwrap();
    assert_eq!(seen[0].line, "GET /x HTTP/1.1");
}

#[test]
fn data_items_become_a_json_body_and_the_method_becomes_post() {
    let (addr, log) = serve(200, "text/plain", "ok");
    let r = hc(&[&url(addr, "/p"), "name=alice", "n:=42", "tags:=[1,2]"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let seen = log.lock().unwrap();
    let req = &seen[0];
    // The method is inferred from there being a body — httpie's rule, and
    // the reason `hc example.com name=alice` reads as it does.
    assert_eq!(req.line, "POST /p HTTP/1.1");
    assert_eq!(req.header("content-type"), Some("application/json"));
    // `=` is a string and `:=` is not, which is the whole distinction
    // between the two separators.
    let v: serde_json::Value = serde_json::from_str(&req.body).unwrap();
    assert_eq!(v["name"], serde_json::json!("alice"));
    assert_eq!(v["n"], serde_json::json!(42));
    assert_eq!(v["tags"], serde_json::json!([1, 2]));
}

#[test]
fn query_items_reach_the_request_target_and_are_form_encoded() {
    let (addr, log) = serve(200, "text/plain", "ok");
    let r = hc(&[&url(addr, "/s"), "a==1", "b==x y"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    // A space is `+` in the WHATWG serialiser, which is what a form parser
    // on the other end expects — not `%20`.
    assert_eq!(log.lock().unwrap()[0].line, "GET /s?a=1&b=x+y HTTP/1.1");
}

#[test]
fn a_form_body_is_urlencoded_and_says_so() {
    let (addr, log) = serve(200, "text/plain", "ok");
    let r = hc(&["-f", &url(addr, "/f"), "a=1", "b=hello world"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let seen = log.lock().unwrap();
    assert_eq!(
        seen[0].header("content-type"),
        Some("application/x-www-form-urlencoded")
    );
    assert_eq!(seen[0].body, "a=1&b=hello+world");
}

#[test]
fn the_default_user_agent_is_sent_and_an_empty_item_removes_it() {
    let (addr, log) = serve(200, "text/plain", "ok");
    assert_eq!(hc(&[&url(addr, "/1")]).code, 0);
    assert_eq!(hc(&[&url(addr, "/2"), "User-Agent:mine/1"]).code, 0);
    assert_eq!(hc(&[&url(addr, "/3"), "User-Agent:"]).code, 0);

    let seen = log.lock().unwrap();
    let by_path = |p: &str| seen.iter().find(|s| s.line.contains(p)).unwrap();
    assert!(
        by_path("/1")
            .header("user-agent")
            .unwrap()
            .starts_with("hc/")
    );
    assert_eq!(by_path("/2").header("user-agent"), Some("mine/1"));
    // The one case that has to be checked at the wire rather than in a
    // unit test: not sent at all, as against sent empty.
    assert_eq!(by_path("/3").header("user-agent"), None);
}

#[test]
fn a_named_backend_this_build_does_not_have_is_refused_by_name_with_its_own_exit_code() {
    // The tool's one promise over curl, and the only test that can state
    // it: `CURL_SSL_BACKEND` in a non-MultiSSL build is accepted and
    // ignored. The exit code is separate from a network failure's so a
    // script can tell them apart.
    let r = hc(&["--backend", "native-tls", "http://127.0.0.1:1/"]);
    if cfg!(feature = "native-tls") {
        // This build has it, so the failure must be the connection rather
        // than the backend — which is the control that says the assertion
        // below is about the refusal and not about the name being unknown.
        assert_eq!(r.code, 4, "{}", r.stderr);
    } else {
        assert_eq!(r.code, 3, "{}", r.stderr);
        assert!(
            r.stderr.contains("has no `native-tls` backend"),
            "{}",
            r.stderr
        );
        assert!(r.stderr.contains("It carries:"), "{}", r.stderr);
    }
}

#[test]
fn version_lists_the_backends_this_build_carries() {
    let r = hc(&["--version"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(r.stdout.starts_with("hc "), "{}", r.stdout);
    assert!(r.stdout.contains("backends:"), "{}", r.stdout);
    #[cfg(feature = "rustls")]
    assert!(r.stdout.contains("rustls"), "{}", r.stdout);
}

#[test]
fn check_status_turns_a_4xx_into_a_nonzero_exit_and_still_prints_the_body() {
    let (addr, _) = serve(404, "text/plain", "gone");
    let plain = hc(&[&url(addr, "/missing")]);
    // Without the flag a 404 is an ordinary answer, which is the same
    // decision `error_for_status` is built on: about half the requests
    // ever made have a status the caller wants to read rather than raise.
    assert_eq!(plain.code, 0, "{}", plain.stderr);

    let (addr, _) = serve(404, "text/plain", "gone");
    let checked = hc(&["--check-status", &url(addr, "/missing")]);
    assert_eq!(checked.code, 5, "{}", checked.stderr);
    // The body still comes out: a script that exits on a 4xx usually needs
    // the server's explanation of it.
    assert_eq!(checked.stdout, "gone\n");
}

#[test]
fn json_output_is_reindented_only_when_it_parses() {
    let (addr, _) = serve(200, "application/json", "{\"b\":1,\"a\":[2]}");
    let good = hc(&[&url(addr, "/j")]);
    assert!(good.stdout.contains("\n    \"b\": 1"), "{}", good.stdout);

    // A body that claims JSON and is not passes through untouched, because
    // a tool that swallowed a malformed payload would be hiding the one
    // thing its caller needs to see.
    let (addr, _) = serve(200, "application/json", "{not json");
    let bad = hc(&[&url(addr, "/j")]);
    assert_eq!(bad.stdout, "{not json\n");
}

#[test]
fn print_shows_what_was_actually_sent_including_the_headers_this_tool_adds() {
    let (addr, _) = serve(200, "text/plain", "ok");
    let r = hc(&["--print", "H", &url(addr, "/p"), "a=1"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    // A printed head that omitted the `User-Agent` and `Content-Type` this
    // program causes would be a diagnostic that lies.
    assert!(r.stdout.contains("POST /p"), "{}", r.stdout);
    assert!(
        r.stdout.contains("content-type: application/json"),
        "{}",
        r.stdout
    );
    assert!(r.stdout.contains("user-agent: hc/"), "{}", r.stdout);
}

#[test]
fn a_usage_mistake_exits_two_and_names_the_argument() {
    let r = hc(&["http://127.0.0.1:1/", "nonsense"]);
    assert_eq!(r.code, 2, "{}", r.stderr);
    assert!(r.stderr.contains("nonsense"), "{}", r.stderr);

    // And the trap the grammar sets for its own users.
    let r = hc(&["http://127.0.0.1:1/", "https://second.example"]);
    assert_eq!(r.code, 2, "{}", r.stderr);
    assert!(r.stderr.contains("looks like a URL"), "{}", r.stderr);
}

#[test]
fn resolve_sends_a_name_to_an_address_of_the_callers_choosing() {
    let (addr, log) = serve(200, "text/plain", "ok");
    // The name is one no resolver can answer, so a green run is the
    // override working rather than DNS happening to agree.
    let r = hc(&[
        "--resolve",
        &format!("nowhere.invalid:{}", addr.ip()),
        &format!("http://nowhere.invalid:{}/r", addr.port()),
    ]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let seen = log.lock().unwrap();
    // And the request still carries the name, which is the point: the
    // certificate and the `Host` are the name's, only the address moved.
    assert_eq!(
        seen[0].header("host"),
        Some(format!("nowhere.invalid:{}", addr.port()).as_str())
    );
}

#[test]
fn without_resolve_that_same_name_fails() {
    // The control for the test above. Without it, a green run there would
    // also be green on a machine whose resolver answers `.invalid`.
    let r = hc(&["http://nowhere.invalid:1/r"]);
    assert_ne!(r.code, 0);
    assert!(
        r.stderr.contains("Resolve") || r.stderr.contains("resolve"),
        "{}",
        r.stderr
    );
}

/// `--write-out` against a real server, through the real binary.
///
/// The unit tests in `timings.rs` cover the vocabulary as a pure
/// function; what only an end-to-end run can say is that the hooks were
/// **installed** — a recorder that is built and never handed to the
/// transport renders a report of zeros and passes every unit test.
#[test]
fn write_out_reports_a_real_exchange_and_the_hooks_were_installed() {
    let (addr, _log) = serve(200, "text/plain", "hello");
    let ran = hc(&[
        &url(addr, "/x"),
        "-w",
        r"|%{http_code}|%{num_connects}|%{size_download}|%{url_effective}|%{remote_port}|",
    ]);
    assert_eq!(ran.code, 0, "stderr: {}", ran.stderr);

    let report = ran.stdout.rsplit('|').nth(5).map(str::to_owned);
    assert!(
        ran.stdout
            .ends_with(&format!("|200|1|5|{}|{}|", url(addr, "/x"), addr.port())),
        "the report is appended after the body: {:?} (field: {report:?})",
        ran.stdout
    );
    assert!(
        ran.stdout.starts_with("hello"),
        "and the body still came first: {:?}",
        ran.stdout
    );
}

/// The timings are real numbers off the wire rather than zeros, which is
/// what says the recorder reached the transport. Asserted as *some time
/// passed and it is ordered*, never as a threshold — three timing-based
/// assertions in this workspace have turned out to be flakes.
#[test]
fn the_time_milestones_are_ordered_and_not_all_zero() {
    let (addr, _log) = serve(200, "text/plain", "hello");
    let ran = hc(&[
        &url(addr, "/x"),
        "-w",
        r"%{time_connect} %{time_starttransfer} %{time_total}",
    ]);
    assert_eq!(ran.code, 0, "stderr: {}", ran.stderr);

    let nums: Vec<f64> = ran
        .stdout
        .trim_start_matches("hello")
        .split_whitespace()
        .map(|s| s.parse().expect("six decimal places"))
        .collect();
    assert_eq!(nums.len(), 3, "{:?}", ran.stdout);
    assert!(nums[2] > 0.0, "total is real: {nums:?}");
    assert!(
        nums[0] <= nums[1] && nums[1] <= nums[2],
        "milestones are on one timeline: {nums:?}"
    );
}

/// Plain `http://` has no handshake, so `time_appconnect` is zero — and
/// `num_connects` is `1`, which is the pair that lets a reader tell that
/// from a pooled request.
#[test]
fn without_tls_appconnect_is_zero_while_a_connection_was_still_made() {
    let (addr, _log) = serve(200, "text/plain", "");
    let ran = hc(&[
        &url(addr, "/x"),
        "-w",
        r"%{time_appconnect}/%{num_connects}",
    ]);
    assert_eq!(ran.code, 0, "stderr: {}", ran.stderr);
    assert_eq!(ran.stdout, "0.000000/1");
}

/// An unknown variable is refused by name, with exit 2 — a usage mistake,
/// which is what 2 already means in this tool.
#[test]
fn an_unknown_write_out_variable_is_refused_by_name() {
    let (addr, _log) = serve(200, "text/plain", "hello");
    let ran = hc(&[&url(addr, "/x"), "-w", "%{time_pretransfer}"]);
    assert_eq!(ran.code, 2, "stdout: {} stderr: {}", ran.stdout, ran.stderr);
    assert!(
        ran.stderr.contains("time_pretransfer"),
        "it names what it could not do: {}",
        ran.stderr
    );
    assert!(
        ran.stderr.contains("time_total"),
        "and lists what it can: {}",
        ran.stderr
    );
}
