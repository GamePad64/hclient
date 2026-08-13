#!/usr/bin/env python3
"""Mutation run for `http-ng-webtransport`.

Restore is `git checkout` **plus an explicit `os.utime`**: a copy that
preserves mtime leaves cargo believing the mutated artifact is current, and
every run after the first would then score against a stale binary. Six runs
elsewhere in this session were mis-scored exactly that way.

The anchor is verified before the first mutation and after the last, and one
mutation in the table is a **control** that nothing can observe. A harness
that reports "killed" unconditionally fails on the control.
"""

import os
import re
import subprocess
import sys
import time

ANSI = re.compile(r"\x1b\[[0-9;]*m")

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
LIB = "crates/http-ng-webtransport/src/lib.rs"
ANCHOR = 33

# (id, file, old, new, note)
MUTATIONS = [
    (
        "M1",
        LIB,
        ".extension(h3::ext::Protocol::WEB_TRANSPORT)",
        ".extension(h3::ext::Protocol::CONNECT_UDP)",
        "the :protocol value is connect-udp, not webtransport",
    ),
    (
        "M2",
        LIB,
        ".method(http::Method::CONNECT)",
        ".method(http::Method::GET)",
        "not a CONNECT, so h3 drops :protocol entirely",
    ),
    (
        "M3",
        LIB,
        "const WEBTRANSPORT_STREAM: u64 = 0x41;",
        "const WEBTRANSPORT_STREAM: u64 = 0x42;",
        "the stream signal value is one off",
    ),
    (
        "M4",
        LIB,
        "let id = SessionId(stream.id().into_inner());",
        "let id = SessionId(stream.id().index());",
        "the session id is h3's `index()` — the ID without its two type bits",
    ),
    (
        "M5",
        LIB,
        "    if v < (1 << 6) {\n        buf.push(v as u8);",
        "    if v < (1 << 7) {\n        buf.push(v as u8);",
        "the varint short branch takes one value too many, so 0x41 is one byte",
    ),
    (
        "M6",
        LIB,
        "    if announced.webtransport && announced.extended_connect {",
        "    if true {",
        "the settings gate is gone: the CONNECT goes to anyone",
    ),
    (
        "M7",
        LIB,
        "    if announced.webtransport && announced.extended_connect {",
        "    if announced.webtransport || announced.extended_connect {",
        "either setting is enough, rather than both",
    ),
    (
        "M8",
        LIB,
        """    match poll_fn(|cx| inner.poll_control(cx)).await {
        Ok(Frame::Settings(_)) => {}
        // Unreachable rather than impossible, and typed rather than
        // `unwrap`ed: `h3` turns any other first frame into
        // `H3_MISSING_SETTINGS` on the line above, so this arm exists for
        // the version of `h3` that stops doing so.
        Ok(other) => {
            return Err(Error::new(
                ErrorKind::Connect,
                std::io::Error::other(format!("first control frame was {other:?}, not SETTINGS")),
            ));
        }
        Err(e) => return Err(connect_error(e)),
    }
""",
        "    let _ = inner;\n",
        "the peer's SETTINGS are never awaited, so the defaults are read as the answer",
    ),
    (
        "M9",
        LIB,
        "        if !resp.status().is_success() {",
        "        if false {",
        "any status establishes the session",
    ),
    (
        "M10",
        LIB,
        "            .enable_extended_connect(true)",
        "            .enable_extended_connect(false)",
        "our own SETTINGS no longer announce extended CONNECT",
    ),
    (
        "M11",
        LIB,
        "        let mut header = Vec::with_capacity(16);",
        "        let mut header = Vec::new();",
        "CONTROL — an allocation hint, observable by nothing",
    ),
    # --- v0.4, datagrams -------------------------------------------------
    (
        "D1",
        LIB,
        "        self.id.0 >> 2\n",
        "        self.id.0\n",
        "the Quarter Stream ID is the stream ID itself, unshifted",
    ),
    (
        "D2",
        LIB,
        "        self.id.0 >> 2\n",
        "        0\n",
        "the Quarter Stream ID is hard-coded zero",
    ),
    (
        "D3",
        LIB,
        "        Ok(settings.enable_datagram())",
        "        Ok(true)",
        "the peer is taken to support HTTP Datagrams whatever it announced",
    ),
    (
        "D4",
        LIB,
        "        Ok(settings.enable_datagram())",
        "        Ok(false)",
        "the peer is taken to support none, whatever it announced",
    ),
    (
        "D5",
        LIB,
        "            && payload.len() > budget",
        "            && false",
        "the budget check never fires; an oversized payload goes to quinn",
    ),
    (
        "D6",
        LIB,
        "            .max_datagram_size()?\n            .checked_sub(varint_len(self.quarter_stream_id()))",
        "            .max_datagram_size()\n            .filter(|_| varint_len(self.quarter_stream_id()) > 0)",
        "the header is not subtracted, so the budget is a byte too generous",
    ),
    (
        "D7",
        LIB,
        "            if id != quarter {\n                continue;\n            }",
        "            if false {\n                continue;\n            }",
        "a datagram addressed to another session is delivered as this one's",
    ),
    (
        "D8",
        LIB,
        "            let Some((id, header)) = get_varint(&frame) else {\n                continue;\n            };",
        "            let (id, header) = get_varint(&frame).unwrap_or((quarter, 0));",
        "a frame too short to carry a Quarter Stream ID is delivered as payload",
    ),
    (
        "D9",
        LIB,
        "    Some((v, len))\n}",
        "    Some((v, 1))\n}",
        "the decoder reports a one-byte header whatever it read",
    ),
    (
        "D10",
        LIB,
        "    if v < (1 << 6) {\n        1\n",
        "    if v < (1 << 6) {\n        2\n",
        "varint_len disagrees with put_varint about a one-byte value",
    ),
    (
        "D11",
        LIB,
        "            .enable_datagram(true)",
        "            .enable_datagram(false)",
        "our own SETTINGS no longer announce SETTINGS_H3_DATAGRAM",
    ),
    (
        "D12",
        LIB,
        '    #[error("the QUIC connection carries no datagrams")]',
        '    #[error("nope")]',
        "CONTROL — an error's Display, which nothing in the suite reads",
    ),
    # --- v0.4, the capsule protocol and the end of a session -------------
    (
        "C1",
        LIB,
        "const CLOSE_WEBTRANSPORT_SESSION: u64 = 0x2843;",
        "const CLOSE_WEBTRANSPORT_SESSION: u64 = 0x2844;",
        "the capsule type is one off, so nothing on either side is a close",
    ),
    (
        "C2",
        LIB,
        "    put_varint(&mut capsule, payload as u64);\n",
        "",
        "RFC 9297 §3's Capsule Length is not written",
    ),
    (
        "C3",
        LIB,
        "capsule.extend_from_slice(&error_code.to_be_bytes());",
        "capsule.extend_from_slice(&error_code.to_le_bytes());",
        "the application error code goes out little-endian",
    ),
    (
        "C4",
        LIB,
        "    capsule.extend_from_slice(reason.as_bytes());\n",
        "",
        "the reason is not written, only its length claimed",
    ),
    (
        "C5",
        LIB,
        "        drop(writer);",
        "        std::mem::forget(writer);",
        "the capsule goes out and the CONNECT stream is never finished",
    ),
    (
        "C6",
        LIB,
        "        if reason.len() > BadCloseCapsule::MAX_REASON {",
        "        if false {",
        "an over-long reason is sent rather than refused",
    ),
    (
        "C7",
        LIB,
        "        if reason.len() > BadCloseCapsule::MAX_REASON {",
        "        if reason.len() >= BadCloseCapsule::MAX_REASON {",
        "the limit is off by one, so a reason of exactly 1024 is refused",
    ),
    (
        "C8",
        LIB,
        "            return Err(Error::new(ErrorKind::Unsupported, AlreadyClosed));",
        "            return Ok(());",
        "a second close answers Ok for a capsule that never left",
    ),
    (
        "C9",
        LIB,
        """                Poll::Ready(Err(e)) => {
                    return Poll::Ready(Err(Error::new(
                        ErrorKind::Body,
                        std::io::Error::other(e.to_string()),
                    )));
                }""",
        """                Poll::Ready(Err(_e)) => {
                    return Poll::Ready(Ok(SessionClose::ENDED_WITHOUT_A_CAPSULE));
                }""",
        "a reset stream and a lost connection are reported as a clean close",
    ),
    (
        "C10",
        LIB,
        "                        Ok(SessionClose::ENDED_WITHOUT_A_CAPSULE)",
        "                        Err(Error::new(ErrorKind::Body, BadCloseCapsule::Truncated { have: 0 }))",
        "a bare FIN is reported as unclean, against draft §5",
    ),
    (
        "C11",
        LIB,
        "    let taken = if kind == CLOSE_WEBTRANSPORT_SESSION {",
        "    let taken = if true {",
        "every capsule type is read as a close",
    ),
    (
        "C12",
        LIB,
        "        code: u32::from_be_bytes(code.try_into().expect(",
        "        code: u32::from_le_bytes(code.try_into().expect(",
        "the peer's error code is read little-endian",
    ),
    (
        "C13",
        LIB,
        "        read_close(&buf[start..end])",
        "        read_close(&buf[start..])",
        "the Capsule Length is ignored on receipt, so the reason runs on",
    ),
    (
        "C14",
        LIB,
        """        if let Some(ended) = &self.ended {
            return Poll::Ready(ended.clone());
        }
""",
        "",
        "the end is read again rather than remembered",
    ),
    (
        "C15",
        LIB,
        "                        self.buf.extend_from_slice(chunk);",
        "                        self.buf = chunk.to_vec();",
        "each DATA frame is taken for a whole capsule",
    ),
    (
        "C16",
        LIB,
        "    let payload = 4 + reason.len();",
        "    let payload = reason.len();",
        "the Capsule Length leaves out the four bytes of the error code",
    ),
    (
        "C17",
        LIB,
        """    let mut capsule = Vec::with_capacity(
        varint_len(CLOSE_WEBTRANSPORT_SESSION) + varint_len(payload as u64) + payload,
    );""",
        "    let mut capsule = Vec::new();",
        "CONTROL — an allocation hint, observable by nothing",
    ),
]


def run_tests():
    r = subprocess.run(
        ["cargo", "nextest", "run", "-p", "http-ng-webtransport", "--no-fail-fast"],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    return r.returncode, r.stdout + r.stderr


def touch(path):
    now = time.time()
    os.utime(os.path.join(ROOT, path), (now, now))


def restore(path):
    subprocess.run(["git", "checkout", "--", path], cwd=ROOT, check=True)
    touch(path)


def apply(path, old, new):
    full = os.path.join(ROOT, path)
    s = open(full).read()
    if s.count(old) != 1:
        raise SystemExit(f"pattern occurs {s.count(old)} times, not once:\n{old}")
    open(full, "w").write(s.replace(old, new))
    touch(path)


def failed_tests(out):
    names = []
    for line in out.splitlines():
        clean = ANSI.sub("", line).strip()
        if clean.startswith("FAIL ") and "]" in clean:
            names.append(clean.rsplit(" ", 1)[-1])
    return sorted(set(names))


def summary_line(out):
    for line in out.splitlines():
        if "tests run:" in line:
            return ANSI.sub("", line).strip()
    return "(no summary — build failure?)"


def main():
    code, out = run_tests()
    line = summary_line(out)
    if code != 0 or f"{ANCHOR} tests run" not in line:
        raise SystemExit(f"anchor is not {ANCHOR} green tests: {line}")
    print(f"anchor OK: {line}\n")

    results = []
    for mid, path, old, new, note in MUTATIONS:
        try:
            apply(path, old, new)
            code, out = run_tests()
        finally:
            restore(path)
        verdict = "KILLED" if code != 0 else "SURVIVED"
        results.append((mid, verdict, note, summary_line(out)))
        print(f"{mid}: {verdict:9} {note}")
        print(f"     {summary_line(out)}")
        print(f"     killed by: {', '.join(failed_tests(out)) or '(nothing)'}")

    code, out = run_tests()
    line = summary_line(out)
    if code != 0 or f"{ANCHOR} tests run" not in line:
        raise SystemExit(f"anchor did not come back: {line}")
    print(f"\nanchor restored: {line}")
    killed = sum(1 for _, v, _, _ in results if v == "KILLED")
    print(f"{killed} killed, {len(results) - killed} survived, of {len(results)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
