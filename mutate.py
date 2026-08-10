#!/usr/bin/env python3
"""Throwaway mutation harness for the W4 review. Not part of the tree."""
import pathlib
import subprocess
import sys

WS = pathlib.Path("crates/http-ng-native/src/websocket.rs")


def apply(path, edits):
    s = path.read_text()
    for old, new, count in edits:
        got = s.count(old)
        if got != count:
            print(f"ANCHOR MISMATCH: expected {count}, found {got} for:\n{old[:200]}")
            sys.exit(2)
        s = s.replace(old, new, count)
    path.write_text(s)


def run():
    r = subprocess.run(
        [
            "cargo", "nextest", "run", "-p", "http-ng-native",
            "--features", "websocket", "--test", "websocket",
            "--no-fail-fast", "--color", "never",
        ],
        capture_output=True,
        text=True,
    )
    return r.returncode, r.stdout + r.stderr


MUTATIONS = {}

# M1 — Parts::read_buf discarded.
MUTATIONS["m1-read-buf-discarded"] = [(
    """            ctx: WebSocketContext::from_partially_read(
                read_buf.to_vec(),
                Role::Client,
                Some(WebSocketConfig::default()),
            ),""",
    """            ctx: {
                let _ = read_buf;
                WebSocketContext::new(Role::Client, Some(WebSocketConfig::default()))
            },""",
    1,
)]

# M2 — the 101 detected AFTER the connection is polled out, not before.
MUTATIONS["m2-status-checked-after-into-parts"] = [
    (
        """    // Rule 1: by status, and before the connection is polled out. A `200`
    // with a body leaves `poll_without_shutdown` `Pending` for ever, so
    // this check is what stands between a wrong answer and a hang.
    let (head, body) = resp.into_parts();
    if head.status != http::StatusCode::SWITCHING_PROTOCOLS {
        return Err(Error::new(
            ErrorKind::Status,
            NotSwitchingProtocols(head.status),
        ));
    }""",
        """    let (head, body) = resp.into_parts();""",
        1,
    ),
    (
        """    let http1::Parts { io, read_buf, .. } = conn.into_parts();
    Ok((io, read_buf, http::Response::from_parts(head, ())))""",
        """    let http1::Parts { io, read_buf, .. } = conn.into_parts();
    if head.status != http::StatusCode::SWITCHING_PROTOCOLS {
        return Err(Error::new(
            ErrorKind::Status,
            NotSwitchingProtocols(head.status),
        ));
    }
    Ok((io, read_buf, http::Response::from_parts(head, ())))""",
        1,
    ),
]

# M2c — the WHOLE validation block moved after into_parts, so the upgrade
#       is taken apart before anybody has looked at what came back.
MUTATIONS["m2c-all-validation-after-into-parts"] = [
    (
        """    let (head, body) = resp.into_parts();
    if head.status != http::StatusCode::SWITCHING_PROTOCOLS {
        return Err(Error::new(
            ErrorKind::Status,
            NotSwitchingProtocols(head.status),
        ));
    }""",
        """    let (head, body) = resp.into_parts();""",
        1,
    ),
    (
        """    if !head
        .headers
        .get(http::header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
    {
        return Err(Error::new(ErrorKind::Status, BadUpgradeHeader("Upgrade")));
    }
    if !has_upgrade_token(head.headers.get(http::header::CONNECTION)) {
        return Err(Error::new(
            ErrorKind::Status,
            BadUpgradeHeader("Connection"),
        ));
    }""",
        """""",
        1,
    ),
    (
        """    let expected = tungstenite::handshake::derive_accept_key(key.as_bytes());
    if head
        .headers
        .get(http::header::SEC_WEBSOCKET_ACCEPT)
        .map(HeaderValue::as_bytes)
        != Some(expected.as_bytes())
    {
        return Err(Error::new(ErrorKind::Status, AcceptKeyMismatch));
    }""",
        """""",
        1,
    ),
    (
        """    let http1::Parts { io, read_buf, .. } = conn.into_parts();
    Ok((io, read_buf, http::Response::from_parts(head, ())))""",
        """    let http1::Parts { io, read_buf, .. } = conn.into_parts();
    if head.status != http::StatusCode::SWITCHING_PROTOCOLS {
        return Err(Error::new(
            ErrorKind::Status,
            NotSwitchingProtocols(head.status),
        ));
    }
    if !head
        .headers
        .get(http::header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
    {
        return Err(Error::new(ErrorKind::Status, BadUpgradeHeader("Upgrade")));
    }
    if !has_upgrade_token(head.headers.get(http::header::CONNECTION)) {
        return Err(Error::new(
            ErrorKind::Status,
            BadUpgradeHeader("Connection"),
        ));
    }
    let expected = tungstenite::handshake::derive_accept_key(key.as_bytes());
    if head
        .headers
        .get(http::header::SEC_WEBSOCKET_ACCEPT)
        .map(HeaderValue::as_bytes)
        != Some(expected.as_bytes())
    {
        return Err(Error::new(ErrorKind::Status, AcceptKeyMismatch));
    }
    Ok((io, read_buf, http::Response::from_parts(head, ())))""",
        1,
    ),
]

# M2d — the status is not looked at at all: whatever came back is framed
#       as WebSocket if its headers happen to look right.
MUTATIONS["m2d-status-not-checked"] = [(
    """    if head.status != http::StatusCode::SWITCHING_PROTOCOLS {
        return Err(Error::new(
            ErrorKind::Status,
            NotSwitchingProtocols(head.status),
        ));
    }""",
    """    let _ = NotSwitchingProtocols(head.status);""",
    1,
)]

# M2b — poll_without_shutdown swapped for Connection's own Future impl.
MUTATIONS["m2b-future-poll-instead"] = [
    (
        """                match conn.poll_without_shutdown(cx) {""",
        """                match std::future::Future::poll(Pin::new(&mut conn), cx) {""",
        1,
    ),
    (
        """        std::future::poll_fn(|cx| conn.poll_without_shutdown(cx))""",
        """        std::future::poll_fn(|cx| std::future::Future::poll(Pin::new(&mut conn), cx))""",
        1,
    ),
]

# M3 — a wrong Sec-WebSocket-Accept accepted.
MUTATIONS["m3-accept-key-unchecked"] = [(
    """    let expected = tungstenite::handshake::derive_accept_key(key.as_bytes());
    if head
        .headers
        .get(http::header::SEC_WEBSOCKET_ACCEPT)
        .map(HeaderValue::as_bytes)
        != Some(expected.as_bytes())
    {
        return Err(Error::new(ErrorKind::Status, AcceptKeyMismatch));
    }""",
    """    let _ = (key, AcceptKeyMismatch);""",
    1,
)]

# M4 — a partial write reported as a whole one.
MUTATIONS["m4-partial-write-dropped"] = [(
    """        match Pin::new(&mut *self.io).poll_write(self.cx, buf) {
            Poll::Ready(r) => r,
            Poll::Pending => Err(std::io::ErrorKind::WouldBlock.into()),
        }""",
    """        match Pin::new(&mut *self.io).poll_write(self.cx, buf) {
            Poll::Ready(Ok(_)) => Ok(buf.len()),
            Poll::Ready(Err(e)) => Err(e),
            Poll::Pending => Err(std::io::ErrorKind::WouldBlock.into()),
        }""",
    1,
)]

# M5 — poll_next reads through a shim that cannot write, so the pong
# tungstenite queued in answer to a ping never leaves.
MUTATIONS["m5-no-pong"] = [(
    """            match ctx.read(&mut Shim { io, cx }) {""",
    """            struct ReadOnly<S>(S);
            impl<S: std::io::Read> std::io::Read for ReadOnly<S> {
                fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
                    self.0.read(b)
                }
            }
            impl<S> std::io::Write for ReadOnly<S> {
                fn write(&mut self, _b: &[u8]) -> std::io::Result<usize> {
                    Err(std::io::ErrorKind::WouldBlock.into())
                }
                fn flush(&mut self) -> std::io::Result<()> {
                    Err(std::io::ErrorKind::WouldBlock.into())
                }
            }
            match ctx.read(&mut ReadOnly(Shim { io, cx })) {""",
    1,
)]

# M5b — the control-frame arm returns Pending instead of reading again.
MUTATIONS["m5b-control-frame-returns-pending"] = [(
    """                Ok(Frame::Ping(_) | Frame::Pong(_) | Frame::Frame(_)) => continue,""",
    """                Ok(Frame::Ping(_) | Frame::Pong(_) | Frame::Frame(_)) => {
                    return Poll::Pending;
                }""",
    1,
)]

# M5c — instrumented M5b: count poll_next entries and control frames.
MUTATIONS["m5c-instrumented-pending"] = [(
    """                Ok(Frame::Ping(_) | Frame::Pong(_) | Frame::Frame(_)) => continue,""",
    """                Ok(Frame::Ping(_) | Frame::Pong(_) | Frame::Frame(_)) => {
                    eprintln!("MUT: control frame seen, returning Pending");
                    return Poll::Pending;
                }""",
    1,
), (
    """        let Self { io, ctx, ended } = self.get_mut();
        if *ended {""",
    """        let Self { io, ctx, ended } = self.get_mut();
        eprintln!("MUT: poll_next entered");
        if *ended {""",
    1,
)]

# M6 — Connection: Upgrade unchecked.
MUTATIONS["m6-connection-token-unchecked"] = [(
    """    if !has_upgrade_token(head.headers.get(http::header::CONNECTION)) {
        return Err(Error::new(
            ErrorKind::Status,
            BadUpgradeHeader("Connection"),
        ));
    }""",
    """    let _ = has_upgrade_token;""",
    1,
)]

# M6b — Connection read by equality rather than as a token list, which is
#        what tungstenite's own verify_response does.
MUTATIONS["m6b-connection-by-equality"] = [(
    """    v.and_then(|v| v.to_str().ok()).is_some_and(|v| {
        v.split(',')
            .any(|t| t.trim().eq_ignore_ascii_case("upgrade"))
    })""",
    """    v.and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("upgrade"))""",
    1,
)]

# M9 — the framing role is Server, so client frames go out unmasked.
MUTATIONS["m9-server-role"] = [(
    """                read_buf.to_vec(),
                Role::Client,""",
    """                read_buf.to_vec(),
                Role::Server,""",
    1,
)]

# M7 — the Upgrade header unchecked (only the status is read).
MUTATIONS["m7-upgrade-header-unchecked"] = [(
    """    if !head
        .headers
        .get(http::header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
    {
        return Err(Error::new(ErrorKind::Status, BadUpgradeHeader("Upgrade")));
    }""",
    """""",
    1,
)]

# M8 — the four handshake headers overwritten rather than refused.
MUTATIONS["m8-reserved-headers-overwritten"] = [(
    """    for name in OURS {
        if req.headers().contains_key(&name) {
            return Err(Error::new(ErrorKind::Unsupported, ReservedHeader(name)));
        }
    }""",
    """    let _ = (OURS, ReservedHeader(http::header::UPGRADE));""",
    1,
)]

# M9 — the WebSocket is opened through the pool's HTTP/1.1 candidates.
#      (approximated: the request goes out on a pooled connection.)

if __name__ == "__main__":
    name = sys.argv[1]
    edits = MUTATIONS[name]
    backup = WS.read_text()
    try:
        apply(WS, edits)
        code, out = run()
        print(f"=== {name}: exit {code} ===")
        for line in out.splitlines():
            if any(k in line for k in ("PASS", "FAIL", "TIMEOUT", "Summary", "error[", "error:", "SLOW")):
                print(line)
    finally:
        WS.write_text(backup)
