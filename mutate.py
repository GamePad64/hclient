#!/usr/bin/env python3
"""Scratch mutation harness for v0.3 W4 step 3. Not part of the tree."""
import subprocess
import sys

SRC = 'crates/http-ng-fetch/src/websocket.rs'

MUTATIONS = {
    # 1. a header the browser cannot send is DROPPED instead of refused
    'drop-unsendable-header': (SRC, """        if name != SENDABLE {
            return Err(Error::new(
                ErrorKind::Unsupported,
                HeaderNotSendable(name.clone()),
            ));
        }""", """        if name != SENDABLE {
            continue;
        }"""),

    # 2. the close code is not reported
    'close-code-not-reported': (SRC, """                        let frame = (e.code() != NO_STATUS_RECEIVED).then(|| CloseFrame {
                            code: e.code(),
                            reason: e.reason(),
                        });
                        s.queue.push_back(Ok(Message::Close(frame)));""",
                                """                        s.queue.push_back(Ok(Message::Close(None)));"""),

    # 3. a message arriving before the caller polls is lost (single slot)
    'queue-is-a-single-slot': (SRC, """                    s.queue.push_back(item);
                    s.wake_later()""",
                               """                    s.queue.clear();
                    s.queue.push_back(item);
                    s.wake_later()"""),

    # 4. binaryType left at the browser default
    'no-arraybuffer': (SRC, """        ws.set_binary_type(web_sys::BinaryType::Arraybuffer);""",
                       """        ws.set_binary_type(web_sys::BinaryType::Blob);"""),

    # 5. wasClean ignored: every close is a clean close
    'was-clean-ignored': (SRC, """                    } else if e.was_clean() {""",
                          """                    } else if true {"""),

    # 6. send after close is not refused
    'sendable-always-ok': (SRC, """        let state = self.socket.ws.ready_state();
        if state == web_sys::WebSocket::OPEN {
            Ok(())
        } else {
            Err(Error::new(ErrorKind::Body, NotOpen(state)))
        }""", """        let _ = self.socket.ws.ready_state();
        Ok(())"""),

    # 7. the caller's close code never reaches close()
    'close-without-the-code': (SRC, """            Message::Close(Some(f)) => ws.close_with_code_and_reason(f.code, &f.reason),""",
                               """            Message::Close(Some(_)) => ws.close(),"""),

    # 8. 1005 reported as a real code
    'no-status-is-a-code': (SRC, """                        let frame = (e.code() != NO_STATUS_RECEIVED).then(|| CloseFrame {""",
                            """                        let frame = (e.code() != 0).then(|| CloseFrame {"""),

    # 9. the subprotocol list is parsed and then not passed on
    'subprotocol-not-passed': (SRC, """    let made = if protocols.is_empty() {""",
                               """    let made = if true {"""),

    # 10. dropping the socket leaves it open
    'drop-does-not-close': (SRC, """        // `close()` on an already-closed socket does nothing, per the
        // standard; there is nothing to check first.
        let _ = self.ws.close();""", """        // mutation: no close on drop"""),

    # 11. poll_next reads `ended` before draining the queue
    'ended-before-the-queue': (SRC, """        if let Some(item) = s.queue.pop_front() {
            return Poll::Ready(Some(item));
        }
        if s.ended {
            return Poll::Ready(None);
        }""", """        if s.ended {
            return Poll::Ready(None);
        }
        if let Some(item) = s.queue.pop_front() {
            return Poll::Ready(Some(item));
        }"""),
}


def apply(name):
    path, old, new = MUTATIONS[name]
    s = open(path).read()
    assert s.count(old) == 1, f"{name}: {s.count(old)} matches"
    open(path, 'w').write(s.replace(old, new, 1))


def revert():
    subprocess.run(['git', 'checkout', '--', SRC], check=True)


if __name__ == '__main__':
    name = sys.argv[1]
    if name == 'revert':
        revert()
        sys.exit(0)
    apply(name)
    print(f'applied {name}')
