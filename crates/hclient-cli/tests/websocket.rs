//! `--ws`, run as a binary against a hand-written WebSocket server.
//!
//! # The server speaks RFC 6455 by hand, on purpose
//!
//! `hclient-tungstenite/tests/websocket.rs` opens with the argument and
//! this file follows it: the client under test is framed by `tungstenite`,
//! and a fixture that shared its frame codec could not tell a correct
//! frame from a consistently wrong one. So [`Wire`] parses and builds
//! frames itself, and what is asserted is opcodes, the mask bit and
//! payload bytes — the layer RFC 6455 is written in.
//!
//! The one thing borrowed is `tungstenite::handshake::derive_accept_key`,
//! and it is defused the same way: [`the_accept_key_derivation_matches_rfc_6455`]
//! pins it against RFC 6455 §1.3's own worked example, so a client and a
//! server agreeing on a *wrong* key is a failure this file notices. It is
//! free — `tungstenite` is already in this crate's graph through
//! `hclient-tungstenite`, at the same feature set.
//!
//! # Nothing here is asserted by a clock
//!
//! Every test ends because the peer closed, because stdin reached EOF, or
//! because a refusal happened before anything connected. `--ws` has no
//! timeout of its own — that is refused by name — so a test that hung
//! would hang rather than pass, which is the direction this workspace
//! prefers.
#![cfg(feature = "websocket")]

use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use tungstenite::handshake::derive_accept_key;

/// One frame as the fixture saw it: `(opcode, was_masked, payload)`.
///
/// Named because RFC 6455 §5.1's masking rule is the middle field, and a
/// bare tuple in a `Vec` in an `Arc<Mutex<..>>` reads as noise where the
/// whole point is that the mask bit is being watched.
type Seen = (u8, bool, Vec<u8>);

/// A socket read and written as frames, by hand.
struct Wire {
    sock: TcpStream,
    buf: Vec<u8>,
}

impl Wire {
    fn new(sock: TcpStream) -> Self {
        Self {
            sock,
            buf: Vec::new(),
        }
    }

    fn fill(&mut self) -> bool {
        let mut chunk = [0u8; 16 * 1024];
        match self.sock.read(&mut chunk) {
            Ok(0) | Err(_) => false,
            Ok(n) => {
                self.buf.extend_from_slice(&chunk[..n]);
                true
            }
        }
    }

    /// The request head, up to and including the blank line.
    fn head(&mut self) -> Option<String> {
        loop {
            if let Some(i) = self.buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&self.buf[..i + 4]).into_owned();
                self.buf.drain(..i + 4);
                return Some(head);
            }
            if !self.fill() {
                return None;
            }
        }
    }

    /// One frame: `(opcode, was_masked, payload)`, unmasked.
    fn frame(&mut self) -> Option<Seen> {
        loop {
            if let Some(f) = self.parse_frame() {
                return Some(f);
            }
            if !self.fill() {
                return None;
            }
        }
    }

    fn parse_frame(&mut self) -> Option<Seen> {
        if self.buf.len() < 2 {
            return None;
        }
        let opcode = self.buf[0] & 0x0f;
        let masked = self.buf[1] & 0x80 != 0;
        let (len, mut at) = match usize::from(self.buf[1] & 0x7f) {
            126 => {
                if self.buf.len() < 4 {
                    return None;
                }
                (
                    usize::from(u16::from_be_bytes([self.buf[2], self.buf[3]])),
                    4,
                )
            }
            127 => {
                if self.buf.len() < 10 {
                    return None;
                }
                let mut n = [0u8; 8];
                n.copy_from_slice(&self.buf[2..10]);
                (
                    usize::try_from(u64::from_be_bytes(n)).expect("no test sends 2^64 bytes"),
                    10,
                )
            }
            n => (n, 2),
        };
        let mut mask = [0u8; 4];
        if masked {
            if self.buf.len() < at + 4 {
                return None;
            }
            mask.copy_from_slice(&self.buf[at..at + 4]);
            at += 4;
        }
        if self.buf.len() < at + len {
            return None;
        }
        let mut payload = self.buf[at..at + len].to_vec();
        if masked {
            for (i, b) in payload.iter_mut().enumerate() {
                *b ^= mask[i % 4];
            }
        }
        self.buf.drain(..at + len);
        Some((opcode, masked, payload))
    }

    /// A server-to-client frame, which RFC 6455 §5.1 forbids to be masked.
    fn send(&mut self, opcode: u8, payload: &[u8]) -> bool {
        let mut out = vec![0x80 | opcode];
        match payload.len() {
            n if n < 126 => out.push(u8::try_from(n).expect("under 126")),
            n if n <= usize::from(u16::MAX) => {
                out.push(126);
                out.extend_from_slice(&u16::try_from(n).expect("under 65536").to_be_bytes());
            }
            n => {
                out.push(127);
                out.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        out.extend_from_slice(payload);
        self.sock.write_all(&out).is_ok()
    }

    fn send_raw(&mut self, bytes: &[u8]) -> bool {
        self.sock.write_all(bytes).is_ok()
    }
}

fn accept_101(key: &str) -> String {
    format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Accept: {}\r\n\r\n",
        derive_accept_key(key.as_bytes())
    )
}

fn header<'h>(head: &'h str, name: &str) -> Option<&'h str> {
    head.lines()
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case(name))
        .map(|(_, v)| v.trim())
}

/// Binds a loopback port and runs `f` on every connection, on a thread of
/// its own; the heads it saw are recorded for the caller.
fn serve<F>(f: F) -> (SocketAddr, Arc<Mutex<Vec<String>>>)
where
    F: Fn(&mut Wire, &str) + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let heads: Arc<Mutex<Vec<String>>> = Arc::default();
    let sink = Arc::clone(&heads);
    let f = Arc::new(f);
    std::thread::spawn(move || {
        for sock in listener.incoming().flatten() {
            let f = Arc::clone(&f);
            let sink = Arc::clone(&sink);
            std::thread::spawn(move || {
                let mut w = Wire::new(sock);
                let Some(head) = w.head() else { return };
                sink.lock().expect("not poisoned").push(head.clone());
                f(&mut w, &head);
            });
        }
    });
    (addr, heads)
}

/// A server that completes the handshake and then runs `f`.
fn serve_ws<F>(f: F) -> (SocketAddr, Arc<Mutex<Vec<String>>>)
where
    F: Fn(&mut Wire) + Send + Sync + 'static,
{
    serve(move |w, head| {
        let Some(key) = header(head, "sec-websocket-key").map(str::to_owned) else {
            return;
        };
        if !w.send_raw(accept_101(&key).as_bytes()) {
            return;
        }
        f(w);
    })
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

/// The binary, with `stdin` fed from `input` and closed after it — which
/// is what makes a session end: `--ws` sends a `Close` at EOF and keeps
/// reading until the peer's answer arrives.
fn hc_with_stdin(args: &[&str], input: &str) -> Ran {
    run_hc(args, input)
}

fn hc(args: &[&str]) -> Ran {
    run_hc(args, "")
}

fn url(addr: SocketAddr, path: &str) -> String {
    format!("ws://{addr}{path}")
}

/// The oracle the fixture's `101` rests on, pinned against RFC 6455
/// §1.3's own worked example rather than against the client that consumes
/// it — without this, a green handshake below would prove only that the
/// two sides agree, which is what a shared mistake looks like.
#[test]
fn the_accept_key_derivation_matches_rfc_6455() {
    assert_eq!(
        derive_accept_key(b"dGhlIHNhbXBsZSBub25jZQ=="),
        "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
    );
}

/// The happy path, and the control for everything else: a real handshake,
/// a line of stdin out as a masked Text frame, and the answer back on
/// stdout. Piped, so the payload form applies — one message per line, and
/// `hc --ws … | grep` needs no flag.
#[test]
fn a_line_of_stdin_becomes_a_text_frame_and_the_answer_reaches_stdout() {
    let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
    let sink = Arc::clone(&seen);
    let (addr, heads) = serve_ws(move |w| {
        while let Some(frame) = w.frame() {
            let opcode = frame.0;
            sink.lock().expect("not poisoned").push(frame);
            match opcode {
                // Text: echo it back, uppercased, so the printed line
                // cannot have come from the input by any other route.
                1 => {
                    let last = sink.lock().expect("not poisoned").last().cloned();
                    let payload = last.map(|(_, _, p)| p).unwrap_or_default();
                    let echo = String::from_utf8_lossy(&payload).to_uppercase();
                    if !w.send(1, echo.as_bytes()) {
                        return;
                    }
                }
                // Close: answer it, which is what lets the client's
                // stream end rather than hang.
                8 => {
                    let _ = w.send(8, &1000u16.to_be_bytes());
                    return;
                }
                _ => {}
            }
        }
    });

    let ran = hc_with_stdin(&["--ws", &url(addr, "/chat")], "hello\nworld\n");
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert_eq!(ran.stdout, "HELLO\nWORLD\n");

    // What the server actually received, at the layer RFC 6455 is written
    // in: two Text frames and a Close, all three **masked**, which §5.1
    // requires of a client and which nothing above this line would notice.
    let seen = seen.lock().expect("not poisoned");
    let kinds: Vec<u8> = seen.iter().map(|(op, ..)| *op).collect();
    assert_eq!(kinds, vec![1, 1, 8], "two texts and a close: {kinds:?}");
    assert!(
        seen.iter().all(|(_, masked, _)| *masked),
        "a client MUST mask every frame it sends"
    );
    assert_eq!(seen[0].2, b"hello");
    assert_eq!(seen[1].2, b"world");

    // And the handshake carried the headers RFC 6455 §4.1 requires plus
    // the one this tool adds, which is the same standard `--print H`
    // holds a request to.
    let heads = heads.lock().expect("not poisoned");
    assert!(heads[0].starts_with("GET /chat HTTP/1.1"), "{}", heads[0]);
    assert_eq!(header(&heads[0], "upgrade"), Some("websocket"));
    assert!(
        header(&heads[0], "sec-websocket-key").is_some(),
        "{}",
        heads[0]
    );
    assert!(
        header(&heads[0], "user-agent").is_some_and(|v| v.starts_with("hc/")),
        "{}",
        heads[0]
    );
}

/// **stdin reaching EOF sends a `Close` rather than dropping the socket**,
/// and the run keeps reading until the peer answers.
///
/// Written as a causal test rather than a timed one: the server holds its
/// reply back until it has *seen* the close, so a client that tore the
/// connection down at EOF could not print `late` at any speed.
#[test]
fn eof_on_stdin_closes_politely_and_the_answer_still_arrives() {
    let (addr, _) = serve_ws(|w| {
        while let Some((opcode, _, _)) = w.frame() {
            if opcode == 8 {
                // A message *after* the client's close, then the close
                // answer. RFC 6455 lets the closing peer keep reading.
                let _ = w.send(1, b"late");
                let _ = w.send(8, &1000u16.to_be_bytes());
                return;
            }
        }
    });
    let ran = hc_with_stdin(&["--ws", &url(addr, "/c")], "");
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert_eq!(ran.stdout, "late\n");
}

/// A peer that closes first ends the run with **0**: a goodbye is an
/// answer, not a failure, which is the same reading `error_for_status`
/// gives a `3xx`.
#[test]
fn a_peer_that_closes_first_ends_the_run_successfully() {
    let (addr, _) = serve_ws(|w| {
        let _ = w.send(8, &1000u16.to_be_bytes());
        // Keep the thread alive long enough for the close to be read.
        let _ = w.frame();
    });
    let ran = hc(&["--ws", &url(addr, "/c")]);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
}

/// The annotated form is a transcript: every line says which direction it
/// came from, and `-v` adds what this tool sent. Piped, so the default
/// would have been the payload form — which is the control that says `-v`
/// is doing the work rather than the terminal.
#[test]
fn verbose_prints_both_directions_and_the_handshake_it_asked_for() {
    let (addr, _) = serve_ws(|w| {
        while let Some((opcode, _, payload)) = w.frame() {
            match opcode {
                1 => {
                    if !w.send(1, &payload) {
                        return;
                    }
                }
                8 => {
                    let _ = w.send(8, &1000u16.to_be_bytes());
                    return;
                }
                _ => {}
            }
        }
    });
    let ran = hc_with_stdin(&["--ws", "-v", &url(addr, "/c")], "ping\n");
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert!(ran.stdout.contains("GET ws://"), "{}", ran.stdout);
    assert!(ran.stdout.contains("> ping"), "{}", ran.stdout);
    assert!(ran.stdout.contains("< ping"), "{}", ran.stdout);
    assert!(ran.stdout.contains("close 1000"), "{}", ran.stdout);
}

/// A binary message reaches a pipe **byte for byte**, which is the same
/// claim `end_to_end.rs` makes about a response body and for the same
/// reason: `anstream`'s strip filter is an ANSI parser, and it deletes
/// what it cannot read as text.
#[test]
fn a_binary_message_reaches_a_pipe_byte_for_byte() {
    let (addr, _) = serve_ws(|w| {
        let _ = w.send(2, &[0x89, b'P', b'N', b'G', 0x1a]);
        let _ = w.send(8, &1000u16.to_be_bytes());
        let _ = w.frame();
    });
    let out = {
        let mut child = Command::new(env!("CARGO_BIN_EXE_hc"))
            .args(["--ws", &url(addr, "/b"), "--no-color"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn");
        drop(child.stdin.take());
        child.wait_with_output().expect("wait")
    };
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(out.stdout, [0x89, b'P', b'N', b'G', 0x1a, b'\n']);
}

/// A `101` whose accept key is wrong is refused — the check that makes
/// the handshake a handshake rather than a status comparison. It runs on
/// the response head *before* the connection is taken apart, which is
/// structural in `hclient-tungstenite`; what this asserts is that `hc`
/// surfaces the refusal instead of proceeding.
#[test]
fn a_101_with_the_wrong_accept_key_is_refused() {
    let (addr, _) = serve(|w, _| {
        let _ = w.send_raw(
            b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
              Sec-WebSocket-Accept: AAAAAAAAAAAAAAAAAAAAAAAAAAA=\r\n\r\n",
        );
        let _ = w.frame();
    });
    let ran = hc(&["--ws", &url(addr, "/c")]);
    assert_eq!(ran.code, 4, "stdout {:?} stderr {}", ran.stdout, ran.stderr);
}

/// A server that answers the upgrade with an ordinary response fails the
/// run, and does not leave `hc` reading frames off an HTTP body.
#[test]
fn a_handshake_answered_with_a_200_is_a_failure() {
    let (addr, _) = serve(|w, _| {
        let _ = w.send_raw(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
    });
    let ran = hc(&["--ws", &url(addr, "/c")]);
    assert_eq!(ran.code, 4, "stdout {:?} stderr {}", ran.stdout, ran.stderr);
}

/// The refusals, through the real binary. `127.0.0.1:1` answers nothing,
/// so a run that reached the network would exit 4 — which is what makes
/// "before anything connects" an assertion rather than a hope.
#[test]
fn a_flag_this_mode_cannot_honour_is_refused_by_name_before_anything_connects() {
    for (flag, extra) in [
        ("--timeout", vec!["--timeout", "1"]),
        ("--follow", vec!["-L"]),
        ("--check-status", vec!["--check-status"]),
        ("-w", vec!["-w", "%{http_code}"]),
        ("--headers", vec!["--headers"]),
    ] {
        let mut args = vec!["--ws", "ws://127.0.0.1:1/c"];
        args.extend_from_slice(&extra);
        let ran = hc(&args);
        assert_eq!(ran.code, 2, "{flag}: {} {}", ran.stdout, ran.stderr);
        assert!(ran.stderr.contains("--ws"), "{flag}: {}", ran.stderr);
    }
    // And the control: the same URL with none of them reaches the network
    // and fails there.
    let ran = hc(&["--ws", "ws://127.0.0.1:1/c"]);
    assert_eq!(ran.code, 4, "{} {}", ran.stdout, ran.stderr);
}

/// `--backend`'s promise survived splitting the transport construction in
/// two: the WebSocket path goes through the same `backend::choose`, so a
/// name this build has not got is refused with the same exit code as it
/// is for a request.
#[test]
fn the_backend_refusal_is_the_same_one_on_this_path() {
    let ran = hc(&["--ws", "--backend", "native-tls", "ws://127.0.0.1:1/c"]);
    if cfg!(feature = "native-tls") {
        // The control: this build has it, so the failure is the
        // connection rather than the backend.
        assert_eq!(ran.code, 4, "{}", ran.stderr);
    } else {
        assert_eq!(ran.code, 3, "{}", ran.stderr);
        assert!(
            ran.stderr.contains("has no `native-tls` backend"),
            "{}",
            ran.stderr
        );
    }
}

/// `hc --version` says what the binary carries, and `ws` is in that list
/// for the same reason the backends are: `--ws` is a runtime flag, and a
/// caller must be able to find out from the binary rather than from a
/// request that does nothing.
#[test]
fn version_lists_ws_when_the_framing_is_compiled_in() {
    let ran = hc(&["--version"]);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert!(ran.stdout.contains("protocols:"), "{}", ran.stdout);
    assert!(
        ran.stdout
            .lines()
            .any(|l| l.starts_with("protocols:") && l.contains("ws")),
        "{}",
        ran.stdout
    );
}
