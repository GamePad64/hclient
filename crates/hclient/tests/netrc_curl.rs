//! The drift guard: for every fixture in `tests/fixtures/netrc/`, real
//! curl sends what it sends against a local listener, and
//! [`Netrc::lookup`](hclient::auth::netrc::Netrc::lookup) must answer
//! the same credentials. The parser's expected values in
//! `auth::netrc::tests` were captured from curl 8.20 once; this test
//! re-captures live, so a curl that changes behaviour fails here
//! instead of rotting the parser silently — and a parser that drifts
//! from curl fails here too.
//!
//! Skips when no `curl` binary is on PATH (printed, not silent): the
//! three CI platforms ship one.

use hclient::auth::netrc::Netrc;
use std::io::{Read, Write};
use std::path::Path;

fn have_curl() -> bool {
    std::process::Command::new("curl")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// The `Authorization` header value curl sends for one fixture, or
/// `None` when it authenticates nothing. One listener, one request.
fn curl_sends(fixture: &Path) -> Option<String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = sock.read(&mut chunk).unwrap();
            if n > 0 {
                buf.extend_from_slice(&chunk[..n]);
            }
            if n == 0 || buf.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        let text = String::from_utf8_lossy(&buf);
        let mut auth = None;
        for line in text.lines() {
            let lower = line.to_ascii_lowercase();
            if let Some(v) = lower.strip_prefix("authorization: ") {
                auth = Some(line[line.len() - v.len()..].to_owned());
            }
        }
        let _ = sock.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n");
        auth
    });
    // No `-o /dev/null`: the path does not exist on Windows (curl exits
    // 23, a write error), and the body going to the captured stdout
    // costs nothing.
    let out = std::process::Command::new("curl")
        .args(["-s", "--max-time", "5", "--netrc-file"])
        .arg(fixture)
        .arg(format!("http://127.0.0.1:{port}/p"))
        .output()
        .expect("curl spawn");
    assert!(out.status.success(), "curl failed: {out:?}");
    handle.join().unwrap()
}

/// The `Authorization` value this crate's lookup produces, encoded the
/// way the client encodes it. The test keeps its own base64 so it does
/// not depend on a private path of the library.
fn ours(netrc: &Netrc, host: &str) -> String {
    match netrc.lookup(host) {
        Some(e) => format!(
            "Basic {}",
            base64_of(format!("{}:{}", e.login(), e.password()).as_bytes())
        ),
        None => "(none)".to_owned(),
    }
}

fn base64_of(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[test]
fn lookup_matches_what_curl_sends_on_the_wire() {
    // 17 fixtures, not 18: `hash_after_token` lives in
    // `netrc-divergent/` because the oracle disagrees with itself on it
    // — curl 8.20 sends the `#` as the literal login, the CI curls read
    // it as a comment to end of line. A comparison that cannot hold on
    // both is excluded from both, and the chosen behaviour is pinned by
    // `a_mid_line_hash_is_a_literal_on_the_pinned_curl` in `netrc.rs`.
    if !have_curl() {
        eprintln!("skipping: no curl binary on PATH");
        return;
    }
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/netrc");
    let mut checked = 0;
    for fixture in std::fs::read_dir(&dir).unwrap() {
        let fixture = fixture.unwrap().path();
        if fixture.extension().and_then(|e| e.to_str()) != Some("netrc") {
            continue;
        }
        let name = fixture.file_stem().unwrap().to_str().unwrap().to_owned();

        let wire = curl_sends(&fixture).unwrap_or_else(|| "(none)".to_owned());
        let text = std::fs::read_to_string(&fixture).unwrap();
        let ours = ours(&Netrc::parse(&text), "127.0.0.1");
        assert_eq!(
            wire, ours,
            "fixture {name}: curl and Netrc::lookup disagree"
        );
        checked += 1;
    }
    assert!(
        checked >= 17,
        "only {checked} fixtures ran — the corpus shrank"
    );
}
