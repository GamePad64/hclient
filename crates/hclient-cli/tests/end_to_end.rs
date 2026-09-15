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
                line.trim_end().clone_into(&mut seen.line);
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

/// **A file item alone is a body**, so the method becomes POST with no
/// data item anywhere. `has_body` ors four conditions and `files` is the
/// only one no test reached: with `|| !files.is_empty()` weakened, an
/// upload goes out as a **GET carrying a multipart body**, which is a
/// request most servers answer with 400 and no client should build.
///
/// Measured before this test existed: that mutation left all 117 tests
/// green, because every existing body test also has a `name=value`.
#[test]
fn a_file_item_on_its_own_makes_the_request_a_post() {
    let (addr, log) = serve(200, "text/plain", "ok");
    let dir = std::env::temp_dir().join(format!("hc-upload-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temp dir");
    let path = dir.join("note.txt");
    std::fs::write(&path, b"file contents").expect("write");

    let r = hc(&[
        &url(addr, "/u"),
        &format!("doc@{}", path.to_str().expect("utf-8 path")),
    ]);
    assert_eq!(r.code, 0, "{}", r.stderr);

    let seen = log.lock().unwrap();
    assert_eq!(
        seen[0].line, "POST /u HTTP/1.1",
        "a file is a body, and a body makes it a POST"
    );
    assert!(
        seen[0]
            .header("content-type")
            .is_some_and(|ct| ct.starts_with("multipart/form-data")),
        "{:?}",
        seen[0].header("content-type")
    );
    // The file's bytes and its name both reached the part, which is what
    // says the upload was assembled rather than merely announced.
    assert!(seen[0].body.contains("file contents"), "{}", seen[0].body);
    assert!(seen[0].body.contains("note.txt"), "{}", seen[0].body);
    std::fs::remove_dir_all(&dir).ok();
}

/// **`--raw-body` is not JSON**, and the `--print H` head must not claim
/// it is. The content type this tool *implies* is guarded by
/// `request_body_preview.is_some() && cli.raw_body.is_none()`; relaxing
/// the `&&` to `||` labels a raw body `application/json` — a header the
/// caller never wrote, describing bytes it may not describe.
///
/// Measured: that mutation left all 117 tests green, because no test
/// printed the head of a `--raw-body` request.
#[test]
fn a_raw_body_is_not_labelled_json_in_the_printed_head() {
    let (addr, _log) = serve(200, "text/plain", "ok");
    let dir = std::env::temp_dir().join(format!("hc-rawbody-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temp dir");
    let path = dir.join("raw.txt");
    std::fs::write(&path, b"not json at all").expect("write");

    let r = hc(&[
        "--print",
        "H",
        &url(addr, "/r"),
        "--raw-body",
        path.to_str().expect("utf-8 path"),
    ]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(
        !r.stdout.contains("content-type: application/json"),
        "a raw body was labelled JSON: {}",
        r.stdout
    );
    // The control: the same flag with data items *does* imply JSON, so
    // the assertion above is about `--raw-body` rather than about this
    // tool never implying a content type at all.
    let (addr2, _log2) = serve(200, "text/plain", "ok");
    let j = hc(&["--print", "H", &url(addr2, "/j"), "a=1"]);
    assert!(
        j.stdout.contains("content-type: application/json"),
        "{}",
        j.stdout
    );
    std::fs::remove_dir_all(&dir).ok();
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

/// **`time_starttransfer` is the *first* head's**, and on a followed
/// redirect that is the only thing separating it from the last hop's.
///
/// `Recorder::on` keeps `ttfb` behind `if t.heads == 0`, so the `heads`
/// counter is what makes it stick. With the increment mutated to `*= 1`
/// the counter never leaves zero and every head overwrites `ttfb` — so
/// the milestone reported is about the hop the caller never asked for.
/// Measured before this test existed: that mutation left all 121 tests
/// green, because nothing rendered `heads` and no test drove two heads
/// through the recorder.
///
/// `Head` is `#[non_exhaustive]` with no public constructor, so this
/// cannot be a unit test: two real heads need a real redirect.
///
/// Asserted as an **ordering**, never as a threshold — the second hop is
/// made slow by the server holding it, so `starttransfer < total` is
/// causal rather than a race. Three timing assertions in this workspace
/// have turned out to be flakes, and this one is written to not be a
/// fourth: it compares two numbers from the same run.
#[test]
fn the_transfer_milestone_belongs_to_the_first_hop_of_a_redirect_chain() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    // Which connection this is, counted the way `serve_raw` above counts
    // its own: the two hops need different answers and the accept loop
    // hands each to its own thread.
    let nth = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let n = nth.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            std::thread::spawn(move || {
                let mut stream = stream;
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_err() || line.is_empty() {
                        return;
                    }
                    if line.trim_end().is_empty() {
                        break;
                    }
                }
                if n == 0 {
                    // The first hop answers at once, which is what makes
                    // its head the early one.
                    let _ = stream.write_all(
                        b"HTTP/1.1 302 Found\r\nlocation: /second\r\ncontent-length: 0\r\n\
                          connection: close\r\n\r\n",
                    );
                } else {
                    // The second is held, so the two heads are far apart
                    // on the timeline and a `ttfb` taken from the wrong
                    // one is unmistakable rather than a near-tie.
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    let _ = stream.write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 2\r\n\
                          connection: close\r\n\r\nhi",
                    );
                }
                let _ = stream.flush();
            });
        }
    });

    let ran = hc(&[
        "-L",
        &url(addr, "/first"),
        "-w",
        r"%{time_starttransfer}|%{time_total}|%{num_redirects}",
    ]);
    assert_eq!(ran.code, 0, "stderr: {}", ran.stderr);

    // The body is printed first and `-w` appended after it, curl's
    // placement — and `output::body` adds the newline, because the body
    // does not end in one.
    let report = ran
        .stdout
        .strip_prefix("hi\n")
        .unwrap_or_else(|| panic!("the body came first: {:?}", ran.stdout));
    let parts: Vec<f64> = report
        .split('|')
        .take(2)
        .map(|s| s.parse().expect("six decimal places"))
        .collect();
    let (starttransfer, total) = (parts[0], parts[1]);
    assert!(
        report.ends_with("|1"),
        "one redirect was followed: {report:?}"
    );
    // The first head arrived before the second hop's 300 ms wait, and the
    // total contains that wait — so the milestone is the first hop's.
    // With the counter stuck at zero the last head wins and the two
    // numbers converge.
    assert!(
        starttransfer < total / 2.0,
        "`time_starttransfer` looks like the last hop's rather than the first's: \
         starttransfer={starttransfer} total={total}"
    );
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

/// `-v` over plaintext prints no handshake line, because there was no
/// handshake. The positive half needs a TLS server and is pinned one
/// crate down, in `hclient-native/tests/tls_facts.rs`, against a real
/// rustls peer.
#[test]
fn verbose_over_plaintext_prints_no_ssl_line() {
    let (addr, _log) = serve(200, "text/plain", "hello");
    let ran = hc(&[&url(addr, "/x"), "-v"]);
    assert_eq!(ran.code, 0, "stderr: {}", ran.stderr);
    assert!(
        !ran.stdout.contains("SSL connection"),
        "nothing was negotiated: {:?}",
        ran.stdout
    );
    assert!(
        ran.stdout.contains("200"),
        "and the head was still printed: {:?}",
        ran.stdout
    );
}

/// A server that answers one canned **byte** response, for the two tests
/// whose subject is bytes rather than text.
fn serve_raw(response: Vec<u8>) -> (SocketAddr, Arc<std::sync::atomic::AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = Arc::clone(&asked);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let response = response.clone();
            let counter = Arc::clone(&counter);
            std::thread::spawn(move || {
                let mut stream = stream;
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_err() || line.is_empty() {
                        return;
                    }
                    if line.trim_end().is_empty() {
                        break;
                    }
                }
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let _ = stream.write_all(&response);
                let _ = stream.flush();
            });
        }
    });
    (addr, asked)
}

/// **A binary body survives a pipe byte for byte**, which this module's
/// neighbour `output.rs` has claimed in its opening paragraph since it
/// was written and was not delivering.
///
/// Everything went through one `anstream::AutoStream`, and with colour
/// off that is an ANSI parser: it deletes bytes it cannot read as text.
/// Measured through this binary before the fix — a PNG's magic
/// `89 50 4e 47 0d 0a 1a 0a` came out as `50 4e 47 0d 0a 0a`, so
/// `hc … > out.png` wrote a file no decoder will open. Colour off is
/// every pipe, every `--no-color` and every `NO_COLOR`, so this test runs
/// in the configuration the defect lived in.
///
/// The magic bytes are the fixture on purpose: `0x89` is not valid UTF-8
/// on its own and `0x1a` is a control character, which are the two shapes
/// the filter removed.
#[test]
fn a_binary_body_reaches_a_pipe_byte_for_byte() {
    let payload: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, b'x'];
    let mut response =
        format!("HTTP/1.1 200 X\r\ncontent-type: image/png\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", payload.len())
            .into_bytes();
    response.extend_from_slice(payload);
    let (addr, _) = serve_raw(response);

    let out = Command::new(env!("CARGO_BIN_EXE_hc"))
        .args([&url(addr, "/i.png"), "--no-color"])
        .output()
        .expect("the binary is built by cargo before this test runs");
    assert_eq!(out.status.code(), Some(0));
    // The trailing newline is this tool's, and only because the body does
    // not end in one — `output::body`'s own rule, unchanged.
    assert_eq!(out.stdout, [payload, b"\n"].concat());
}

/// **`--follow` decides, and without it the `3xx` is the answer.**
///
/// It did neither: `Client` falls back to `Limit::default()` — ten hops —
/// when nobody sets a policy, so `hc` followed redirects whether or not
/// `-L` was given, and the flag did nothing at all. Measured against the
/// built binary before it was believed, with and without the flag: the
/// same second URL's body, the same exit code.
///
/// The negative half is `Forbid` rather than `Limit::new(0)`, which is
/// the distinction `RedirectVerdict` exists to keep: the first hands the
/// `3xx` back as an answer, the second is an error, and a caller who did
/// not ask to follow has not asked to fail.
#[test]
fn a_redirect_is_followed_only_when_it_was_asked_for() {
    let hop =
        b"HTTP/1.1 302 Found\r\nlocation: /moved\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
            .to_vec();

    let (addr, asked) = serve_raw(hop.clone());
    let plain = hc(&[&url(addr, "/e")]);
    assert_eq!(plain.code, 0, "a 3xx is an answer, not a failure");
    assert_eq!(
        asked.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "one request, because nothing was followed"
    );

    let (addr, asked) = serve_raw(hop);
    let followed = hc(&["-L", &url(addr, "/e")]);
    // The server answers every request with the same hop, so following
    // runs into `--max-redirects` — which is the point: the flag changed
    // what happened, and it is the *count* that says so rather than a
    // body that could have come from either arm.
    assert_ne!(followed.code, 0, "{}", followed.stderr);
    assert!(
        asked.load(std::sync::atomic::Ordering::SeqCst) > 1,
        "`-L` did not follow anything"
    );
}
