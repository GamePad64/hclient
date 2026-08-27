//! The browser's WebSocket, behind `hclient_core::unversioned::
//! WebSocketConnect`.
//!
//! # Why almost every test here runs against a stand-in constructor
//!
//! There is no WebSocket server in this test environment. `wasm-pack test
//! --headless` serves the harness page over plain HTTP and nothing in this
//! repository speaks RFC 6455 to a browser; a public echo server would put
//! the whole suite on the network, which nothing else here is.
//!
//! So the same trick `caps.rs`'s
//! `the_probe_follows_the_browsers_behaviour_in_both_directions` already
//! uses: `web_sys::WebSocket::new(..)` compiles to `new WebSocket(..)` in
//! wasm-bindgen's glue, where `WebSocket` is a free variable resolved
//! through the scope chain to `globalThis` at call time, so replacing
//! `globalThis.WebSocket` changes what the crate actually constructs. The
//! stand-in is not a mock of this crate — it is a stand-in for the
//! *browser*, and everything under test (the header refusal, the queue,
//! the close-code mapping, the send path) runs unmodified against it.
//!
//! What that buys is the two things a real server could not give on
//! demand: exact close codes with `wasClean` either way, and a message
//! delivered at a chosen moment relative to the caller's first poll.
//!
//! What it cannot give is proof that the real `WebSocket` global answers
//! to the same names. The last test in this file constructs a real one,
//! against the harness's own origin — a server that answers the upgrade
//! with ordinary HTTP — and asserts the honest outcome both engines
//! produce. Chrome and Firefox reach it by different paths and do not
//! agree on the close code, which is why that test asserts the
//! `ErrorKind` and not a number: asserting one engine's answer is the
//! shape that has bitten this crate before.
#![cfg(target_arch = "wasm32")]

use futures_util::{SinkExt, StreamExt};
use hclient_core::ErrorKind;
use hclient_core::unversioned::{CloseFrame, Message, WebSocketConnect};
use hclient_fetch::Fetch;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

// ---------------------------------------------------------------------
// The stand-in for the browser's own `WebSocket`.
// ---------------------------------------------------------------------

/// Installs a stand-in `globalThis.WebSocket`; [`restore`] puts the real
/// one back.
///
/// The real constructor is stashed once, in `globalThis.__ws_real`, rather
/// than handed back to each caller — so a test that fails *before* its own
/// `restore` cannot leave the stand-in installed for every test after it.
/// That is not hypothetical tidiness: it is what the first mutation run
/// against this file produced — three failures where the mutation explains
/// two — and a mutation report is worth nothing if a killed test can take
/// an unrelated one down with it. A `Drop` guard would be the usual answer
/// and is not available here: `wasm32-unknown-unknown` does not unwind, so
/// a panicking `#[wasm_bindgen_test]` runs no destructor.
///
/// `plan` is a JS array literal of steps the stand-in plays back one
/// microtask after construction — which is when a real socket's `open`
/// arrives too, and is what lets `websocket().await` resolve at all.
/// Steps:
///
/// - `['open']` — `readyState = OPEN`, then `onopen`.
/// - `['text', 'hi']` / `['binary', [1,2,3]]` / `['raw', 7]` — an
///   `onmessage` whose `data` is a string, an `ArrayBuffer`, or whatever
///   was given.
/// - `['close', code, reason, wasClean]` — `readyState = CLOSED`, then
///   `onclose`.
///
/// [`fire`] plays one more step against the socket last constructed, which
/// is how a test drives a socket it is already holding.
///
/// The stand-in deliberately follows the *standard* on the two points the
/// tests turn on: `send()` throws only while `CONNECTING` and silently
/// discards once `CLOSING`/`CLOSED` (which is what makes
/// `sending_after_the_peer_closed_is_an_error_rather_than_a_silent_drop`
/// a real check rather than one the fake grants), and `close()` on an
/// already-closed socket does nothing.
///
/// Two details that are not decoration. It does **not** borrow the real
/// `WebSocket.prototype`: that prototype defines
/// `readyState`/`binaryType`/`url` as accessors, and an object inheriting
/// them cannot carry the plain data properties this stand-in assigns. And
/// it copies a binary `send()` argument (`new Uint8Array(d)`) before
/// storing it: wasm-bindgen passes a `&[u8]` as a *view into wasm memory*,
/// which a real browser copies synchronously inside `send()` and a log
/// that kept the view would be reading whatever wasm put there next.
fn install(plan: &str) {
    let body = format!(
        r#"
        if (globalThis.__ws_real === undefined) {{
            globalThis.__ws_real = globalThis.WebSocket;
        }}
        const log = {{
            constructed: 0,
            urls: [],
            protocols: [],
            sent: [],
            closes: [],
            last: null,
        }};
        function fire(sock, step) {{
            const kind = step[0];
            if (kind === 'open') {{
                sock.readyState = 1;
                if (sock.onopen) sock.onopen({{ type: 'open' }});
            }} else if (kind === 'text') {{
                if (sock.onmessage) sock.onmessage({{ data: step[1] }});
            }} else if (kind === 'binary') {{
                const a = new Uint8Array(step[1]);
                if (sock.onmessage) sock.onmessage({{ data: a.buffer }});
            }} else if (kind === 'raw') {{
                if (sock.onmessage) sock.onmessage({{ data: step[1] }});
            }} else if (kind === 'close') {{
                sock.readyState = 3;
                if (sock.onclose) sock.onclose({{
                    code: step[1], reason: step[2], wasClean: step[3],
                }});
            }}
        }}
        const fake = function (url, protocols) {{
            const self = this;
            log.constructed += 1;
            log.urls.push(url);
            log.protocols.push(protocols === undefined ? null : Array.from(protocols));
            self.url = url;
            self.readyState = 0;
            self.binaryType = 'blob';
            self.bufferedAmount = 0;
            self.onopen = null;
            self.onmessage = null;
            self.onclose = null;
            self.onerror = null;
            self.send = function (d) {{
                if (self.readyState === 0) {{
                    throw new DOMException('still connecting', 'InvalidStateError');
                }}
                if (self.readyState !== 1) return;
                log.sent.push(typeof d === 'string' ? d : new Uint8Array(d));
            }};
            self.close = function (code, reason) {{
                if (self.readyState === 3) return;
                log.closes.push([
                    code === undefined ? null : code,
                    reason === undefined ? null : reason,
                ]);
                self.readyState = 3;
            }};
            log.last = self;
            const plan = {plan};
            queueMicrotask(function () {{
                for (const step of plan) fire(self, step);
            }});
        }};
        globalThis.WebSocket = fake;
        globalThis.__ws_log = log;
        globalThis.__ws_fire = function (step) {{ fire(log.last, step); }};
        "#
    );
    js_sys::Function::new_no_args(&body)
        .call0(&JsValue::NULL)
        .expect("installing the stand-in constructor must not throw");
}

/// Puts the real constructor back, whoever installed the stand-in. Called
/// at the *start* of the one test that needs the real global as well as at
/// the end of every test that replaced it, so a leak from a failing test
/// cannot reach it.
fn restore() {
    js_sys::Function::new_no_args(
        "if (globalThis.__ws_real !== undefined) { globalThis.WebSocket = globalThis.__ws_real; }",
    )
    .call0(&JsValue::NULL)
    .expect("restoring the real WebSocket must not throw");
    for name in ["__ws_log", "__ws_fire"] {
        let _ = js_sys::Reflect::delete_property(&js_sys::global(), &JsValue::from_str(name));
    }
}

fn log_field(name: &str) -> JsValue {
    let log = js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("__ws_log"))
        .expect("the stand-in installs __ws_log");
    js_sys::Reflect::get(&log, &JsValue::from_str(name)).expect("the field exists")
}

fn log_array(name: &str) -> js_sys::Array {
    js_sys::Array::from(&log_field(name))
}

/// Plays one more step against the socket the stand-in last built.
fn fire(step: &str) {
    let f = js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("__ws_fire"))
        .expect("the stand-in installs __ws_fire")
        .unchecked_into::<js_sys::Function>();
    let step = js_sys::JSON::parse(step).expect("the step literal is JSON");
    f.call1(&JsValue::NULL, &step)
        .expect("firing a step must not throw");
}

fn strings(v: &JsValue) -> Vec<String> {
    js_sys::Array::from(v)
        .iter()
        .map(|e| e.as_string().unwrap_or_else(|| format!("{e:?}")))
        .collect()
}

fn req(uri: &str) -> http::Request<()> {
    http::Request::builder().uri(uri).body(()).unwrap()
}

async fn open(uri: &str) -> hclient_fetch::FetchWebSocket {
    Fetch::new()
        .websocket(req(uri))
        .await
        .expect("the stand-in opens")
}

const ANY: &str = "wss://example.invalid/socket";

// ---------------------------------------------------------------------
// The duty: a header the browser cannot send is refused, not dropped.
// ---------------------------------------------------------------------

/// The deliverable of this backend, and the reason the seam takes an
/// `http::Request<()>` at all.
///
/// Two assertions, and the second is what makes this a test of *refusal*
/// rather than of an error message: the stand-in counts its constructions,
/// and a backend that dropped the header instead would have opened a
/// socket. `constructed == 0` is what a silently-dropped `Authorization`
/// cannot produce.
#[wasm_bindgen_test]
async fn a_header_the_browser_cannot_send_is_refused_and_nothing_is_opened() {
    install("[['open']]");
    let err = Fetch::new()
        .websocket(
            http::Request::builder()
                .uri(ANY)
                .header("authorization", "Bearer sesame")
                .body(())
                .unwrap(),
        )
        .await
        .expect_err("a header the browser cannot send must fail the call");
    let constructed = log_field("constructed").as_f64().unwrap_or(-1.0);
    restore();

    assert_eq!(*err.kind(), ErrorKind::Unsupported, "{err}");
    assert!(
        err.to_string().contains("authorization"),
        "the error must name the header it could not send: {err}"
    );
    assert_eq!(
        constructed, 0.0,
        "the refusal must happen before a socket exists — a backend that dropped the header \
         would have opened one"
    );
}

/// The same rule for headers a caller might think harmless, and for the
/// ones RFC 6455's own handshake owns. `hclient-native` refuses the second
/// group under a name of its own (`ReservedHeader`); here that distinction
/// has no subject, because the browser can send none of them, and this
/// test says so rather than letting the two files look like they disagree.
#[wasm_bindgen_test]
async fn every_other_header_is_refused_too_including_the_handshakes_own() {
    install("[['open']]");
    let mut seen = Vec::new();
    for name in [
        "host",
        "origin",
        "cookie",
        "sec-websocket-key",
        "sec-websocket-version",
        "sec-websocket-extensions",
        "x-anything",
    ] {
        let err = Fetch::new()
            .websocket(
                http::Request::builder()
                    .uri(ANY)
                    .header(name, "v")
                    .body(())
                    .unwrap(),
            )
            .await
            .expect_err("every header but Sec-WebSocket-Protocol must fail the call");
        seen.push((name, err.kind().clone(), err.to_string()));
    }
    let constructed = log_field("constructed").as_f64().unwrap_or(-1.0);
    restore();

    for (name, kind, text) in &seen {
        assert_eq!(*kind, ErrorKind::Unsupported, "{name}: {text}");
        assert!(text.contains(name), "{name} must be named: {text}");
    }
    assert_eq!(
        constructed, 0.0,
        "none of the seven may reach the constructor"
    );
}

/// The one header that *is* reachable, and it does not go out as a header:
/// `new WebSocket(url, protocols)`'s second argument is what becomes
/// `Sec-WebSocket-Protocol` on the wire. Read back out of the
/// constructor's own arguments, with both spellings RFC 6455 §4.1 allows —
/// a comma-separated list and a repeated header — so a backend that
/// accepted the header and then dropped it fails here.
#[wasm_bindgen_test]
async fn a_subprotocol_reaches_the_constructor() {
    install("[['open']]");
    let ws = Fetch::new()
        .websocket(
            http::Request::builder()
                .uri(ANY)
                .header("sec-websocket-protocol", "chat, superchat")
                .header("sec-websocket-protocol", "v2.example")
                .body(())
                .unwrap(),
        )
        .await
        .expect("a subprotocol is the one thing the browser can carry");
    let protocols = log_array("protocols").get(0);
    drop(ws);
    restore();

    assert_eq!(
        strings(&protocols),
        ["chat", "superchat", "v2.example"],
        "every token of every Sec-WebSocket-Protocol header must reach the constructor"
    );
}

/// No headers at all is the ordinary case, and it must not pass an empty
/// list: `new WebSocket(url, [])` is not the same call as
/// `new WebSocket(url)`, and the second is what a request with no
/// subprotocol asks for.
#[wasm_bindgen_test]
async fn no_subprotocol_means_the_one_argument_constructor() {
    install("[['open']]");
    let ws = open(ANY).await;
    let protocols = log_array("protocols").get(0);
    drop(ws);
    restore();

    assert!(
        protocols.is_null(),
        "with no Sec-WebSocket-Protocol the constructor must be called with one argument, \
         got {protocols:?}"
    );
}

// ---------------------------------------------------------------------
// The URL.
// ---------------------------------------------------------------------

/// `http`/`https` are accepted as `ws`/`wss` — the seam's own rule, so a
/// caller holding an origin does not have to rewrite its scheme — and the
/// URL that reaches the constructor is the rewritten one.
#[wasm_bindgen_test]
async fn http_and_https_are_opened_as_ws_and_wss() {
    install("[['open']]");
    for uri in [
        "http://example.invalid/one",
        "https://example.invalid/two?x=1",
        "ws://example.invalid:8080/three",
        "wss://example.invalid/four",
    ] {
        drop(open(uri).await);
    }
    let urls = strings(&log_field("urls"));
    restore();

    assert_eq!(
        urls,
        [
            "ws://example.invalid/one",
            "wss://example.invalid/two?x=1",
            "ws://example.invalid:8080/three",
            "wss://example.invalid/four",
        ]
    );
}

#[wasm_bindgen_test]
async fn a_scheme_that_is_not_a_websocket_scheme_is_refused() {
    install("[['open']]");
    let err = Fetch::new()
        .websocket(req("ftp://example.invalid/socket"))
        .await
        .expect_err("ftp is not a WebSocket scheme");
    let constructed = log_field("constructed").as_f64().unwrap_or(-1.0);
    restore();

    assert_eq!(*err.kind(), ErrorKind::Unsupported, "{err}");
    assert_eq!(constructed, 0.0, "nothing may be constructed for it");
}

// ---------------------------------------------------------------------
// Receiving.
// ---------------------------------------------------------------------

/// The event bridge's whole reason for holding a queue rather than a slot.
///
/// The stand-in plays `open`, `text`, `text` inside **one** microtask, so
/// both messages are delivered before the connect future's waker has run
/// and long before the caller polls the `Stream`. A single-slot state
/// keeps the second and loses the first, which is the mutation this test
/// exists to kill.
#[wasm_bindgen_test]
async fn two_messages_that_arrive_before_the_first_poll_are_both_kept() {
    install("[['open'],['text','first'],['text','second']]");
    let mut ws = open(ANY).await;
    let a = ws.next().await.expect("a first").expect("not an error");
    // Asserted here rather than after `restore()`, and the placement is
    // half the test: a single-slot state hands back `second` on the first
    // poll, and this line names that. Awaiting the second message first
    // would instead hang on one that no longer exists — a kill either way,
    // but "this test failed and here is what it saw" beats a suite the
    // runner had to time out.
    assert_eq!(a, Message::Text("first".into()));
    let b = ws.next().await.expect("a second").expect("not an error");
    drop(ws);
    restore();

    assert_eq!(b, Message::Text("second".into()));
}

/// Binary arrives as an `ArrayBuffer` and comes out as bytes — which is
/// only true because `websocket()` sets `binaryType = "arraybuffer"`; see
/// the next test for why that is asserted separately.
#[wasm_bindgen_test]
async fn a_binary_message_arrives_as_bytes() {
    install("[['open'],['binary',[1,2,255]]]");
    let mut ws = open(ANY).await;
    let got = ws.next().await;
    drop(ws);
    restore();

    assert_eq!(
        got.expect("a message").expect("not an error"),
        Message::Binary(bytes::Bytes::from_static(&[1, 2, 255]))
    );
}

/// `binaryType` is asserted directly rather than inferred from the test
/// above, because the stand-in hands back an `ArrayBuffer` whatever the
/// setting says. A real browser left at the default `"blob"` hands back a
/// `Blob`, whose bytes do not exist until an asynchronous read has
/// finished — so every binary message would come out of `decode` as a
/// `Decode` error, and no test that only sends text would notice.
#[wasm_bindgen_test]
async fn binary_type_is_set_to_arraybuffer_before_anything_can_arrive() {
    install("[['open']]");
    let ws = open(ANY).await;
    let last = log_field("last");
    let binary_type = js_sys::Reflect::get(&last, &JsValue::from_str("binaryType"))
        .expect("the stand-in has the property");
    drop(ws);
    restore();

    assert_eq!(binary_type.as_string().as_deref(), Some("arraybuffer"));
}

/// The read succeeded and the shape is wrong — `ErrorKind::Decode`, the
/// same category `body.rs`'s `NotAByteChunk` uses for exactly this
/// distinction. Defensive against a browser that ignored `binaryType`,
/// and reachable only from a stand-in.
#[wasm_bindgen_test]
async fn a_message_that_is_neither_text_nor_bytes_is_a_decode_error() {
    install("[['open'],['raw',7]]");
    let mut ws = open(ANY).await;
    let got = ws.next().await;
    drop(ws);
    restore();

    let err = got
        .expect("an item")
        .expect_err("a number is not a WebSocket message");
    assert_eq!(*err.kind(), ErrorKind::Decode, "{err}");
}

// ---------------------------------------------------------------------
// Closing.
// ---------------------------------------------------------------------

/// The close code and reason are the peer's answer, and they must survive
/// the trip. A backend that reported `Close(None)` for every clean close
/// would pass every other test in this file.
#[wasm_bindgen_test]
async fn the_close_code_and_reason_are_reported() {
    install("[['open'],['close',4001,'so long',true]]");
    let mut ws = open(ANY).await;
    let got = ws.next().await;
    let after = ws.next().await;
    drop(ws);
    restore();

    assert_eq!(
        got.expect("a close message")
            .expect("a clean close is not an error"),
        Message::Close(Some(CloseFrame {
            code: 4001,
            reason: "so long".into(),
        }))
    );
    assert!(after.is_none(), "the stream ends after the peer's close");
}

/// **A stream that has ended stays ended**, and a browser can still fire
/// `onmessage` after `onclose` — an event queued before the close is
/// dispatched afterwards, and nothing in the DOM forbids it.
///
/// Without `on_message`'s `ended` guard the late text is pushed onto the
/// queue behind the close, so `next()` yields `Close` and then yields a
/// `Text` *after* it. That is not a lost message, which would be
/// forgivable; it is a stream that ends and then speaks, which no caller
/// can be asked to handle.
///
/// This test exists because the guard survived a mutation run: every
/// other plan in this file delivers its messages before the close, so
/// deleting the guard changed nothing anywhere.
#[wasm_bindgen_test]
async fn a_message_that_arrives_after_the_close_is_not_delivered() {
    install("[['open'],['close',1000,'bye',true],['text','too late']]");
    let mut ws = open(ANY).await;
    let first = ws.next().await;
    let second = ws.next().await;
    drop(ws);
    restore();

    assert_eq!(
        first
            .expect("the close arrives")
            .expect("a clean close is not an error"),
        Message::Close(Some(CloseFrame {
            code: 1000,
            reason: "bye".into(),
        }))
    );
    assert!(
        second.is_none(),
        "the stream ended at the close; it must not speak again: {second:?}"
    );
}

/// RFC 6455 §7.4.1's 1005 is "no status code was actually present" and is
/// never on the wire: it is how a browser reports the empty close payload
/// that `tungstenite` reports to `hclient-native` as `None`. Reporting
/// `Close(Some(1005))` would invent a code the peer did not send.
#[wasm_bindgen_test]
async fn a_close_with_no_status_is_reported_as_no_frame_at_all() {
    install("[['open'],['close',1005,'',true]]");
    let mut ws = open(ANY).await;
    let got = ws.next().await;
    drop(ws);
    restore();

    assert_eq!(
        got.expect("a close message").expect("not an error"),
        Message::Close(None)
    );
}

/// `wasClean == false` means no close handshake happened — code 1006,
/// which RFC 6455 forbids on the wire. Delivering it as a `Message::Close`
/// would tell a caller that only inspects close messages that its peer
/// said goodbye; it is an error on the `Stream` instead, and the stream
/// still ends.
#[wasm_bindgen_test]
async fn a_connection_that_broke_is_an_error_and_not_a_close_message() {
    install("[['open'],['close',1006,'',false]]");
    let mut ws = open(ANY).await;
    let got = ws.next().await;
    let after = ws.next().await;
    drop(ws);
    restore();

    let err = got
        .expect("an item")
        .expect_err("an unclean close is not a close message");
    assert_eq!(*err.kind(), ErrorKind::Body, "{err}");
    assert!(err.to_string().contains("1006"), "{err}");
    assert!(after.is_none(), "the stream ends after the failure too");
}

/// The seam's own contract: "a `Stream` that has ended stays ended."
#[wasm_bindgen_test]
async fn the_stream_stays_ended() {
    install("[['open'],['close',1000,'bye',true]]");
    let mut ws = open(ANY).await;
    let _close = ws.next().await;
    let ends = [ws.next().await, ws.next().await, ws.next().await];
    drop(ws);
    restore();

    assert!(ends.iter().all(Option::is_none), "{ends:?}");
}

// ---------------------------------------------------------------------
// Sending.
// ---------------------------------------------------------------------

/// Text goes out as a JS string and binary as bytes — two different
/// `send()` overloads, and a backend that stringified the bytes would put
/// `[object Uint8Array]` on the wire. That is not a hypothetical shape
/// here: Firefox once stringified a `ReadableStream` request body into
/// `[object ReadableStream]` while answering 200, which is the whole
/// reason `caps::supports_streaming_request_body` exists.
#[wasm_bindgen_test]
async fn text_and_binary_go_out_as_the_two_things_they_are() {
    install("[['open']]");
    let mut ws = open(ANY).await;
    ws.send(Message::Text("hello".into())).await.expect("text");
    ws.send(Message::Binary(bytes::Bytes::from_static(&[7, 8])))
        .await
        .expect("binary");
    let sent = log_array("sent");
    let first = sent.get(0);
    let second = sent.get(1);
    drop(ws);
    restore();

    assert_eq!(first.as_string().as_deref(), Some("hello"));
    assert!(
        second.is_instance_of::<js_sys::Uint8Array>(),
        "binary must reach send() as bytes, not as a string: {second:?}"
    );
    assert_eq!(
        second.unchecked_ref::<js_sys::Uint8Array>().to_vec(),
        [7, 8]
    );
}

/// The close handshake is `close(code, reason)`, and the caller's code and
/// reason are what go into it. A backend that called the argument-less
/// `close()` would send "no status" for every close a caller asked to
/// carry a code.
#[wasm_bindgen_test]
async fn closing_the_sink_closes_the_socket_with_the_callers_code() {
    install("[['open']]");
    let mut ws = open(ANY).await;
    ws.send(Message::Close(Some(CloseFrame {
        code: 4002,
        reason: "done".into(),
    })))
    .await
    .expect("a close is a message like any other");
    let closes = log_array("closes");
    let count = closes.length();
    let first = js_sys::Array::from(&closes.get(0));
    drop(ws);
    restore();

    assert_eq!(count, 1, "exactly one close(..) call");
    assert_eq!(first.get(0).as_f64(), Some(4002.0));
    assert_eq!(first.get(1).as_string().as_deref(), Some("done"));
}

/// `Message::Close(None)` is the argument-less `close()`, which is what
/// makes the browser send an empty close payload — the wire form 1005
/// stands for, and the one the seam's `None` means.
#[wasm_bindgen_test]
async fn closing_with_no_frame_calls_close_with_no_arguments() {
    install("[['open']]");
    let mut ws = open(ANY).await;
    ws.send(Message::Close(None)).await.expect("close");
    let first = js_sys::Array::from(&log_array("closes").get(0));
    let (code, reason) = (first.get(0), first.get(1));
    drop(ws);
    restore();

    assert!(code.is_null(), "no code: {code:?}");
    assert!(reason.is_null(), "no reason: {reason:?}");
}

/// The standard makes `send()` on a `CLOSING`/`CLOSED` socket discard the
/// data **without throwing** — the one place the whole API drops something
/// silently. The backend has to check `readyState` itself, and this test
/// asserts both halves: the caller gets an error, and nothing reached
/// `send()`.
#[wasm_bindgen_test]
async fn sending_after_the_peer_closed_is_an_error_rather_than_a_silent_drop() {
    install("[['open']]");
    let mut ws = open(ANY).await;
    fire(r#"["close",1000,"bye",true]"#);
    let err = ws
        .send(Message::Text("too late".into()))
        .await
        .expect_err("a closed socket cannot carry a message");
    let sent = log_array("sent").length();
    drop(ws);
    restore();

    assert_eq!(*err.kind(), ErrorKind::Body, "{err}");
    assert_eq!(sent, 0, "nothing may reach the browser's send()");
}

// ---------------------------------------------------------------------
// Lifetime.
// ---------------------------------------------------------------------

/// Dropping the socket closes it. Without this a caller that walked away
/// would leave a WebSocket open in the tab until the page went away — the
/// same promise `Body`'s cancel-on-drop already makes for a response
/// stream.
#[wasm_bindgen_test]
async fn dropping_the_socket_closes_it() {
    install("[['open']]");
    let ws = open(ANY).await;
    let before = log_array("closes").length();
    drop(ws);
    let after = log_array("closes").length();
    restore();

    assert_eq!(before, 0, "nothing closed while the socket was held");
    assert_eq!(after, 1, "dropping the socket must close it");
}

/// `WebSocketConnect::websocket`'s cancellation contract: dropping the
/// future before it completes stops the attempt rather than leaving a
/// handshake running for nobody.
///
/// The stand-in's plan is empty, so the socket never opens and the future
/// is still pending when it is dropped; `close()` on it is the observable.
#[wasm_bindgen_test]
async fn dropping_the_connect_future_closes_the_socket_it_had_opened() {
    use std::future::Future;
    use std::task::{Context, Poll, Waker};

    install("[]");
    {
        let f = Fetch::new();
        let mut fut = Box::pin(f.websocket(req(ANY)));
        // One poll, so the future reaches its `await`; the constructor
        // runs synchronously before it, inside the same poll.
        let mut cx = Context::from_waker(Waker::noop());
        let polled = fut.as_mut().poll(&mut cx);
        assert!(
            matches!(polled, Poll::Pending),
            "the stand-in never opens this one"
        );
        drop(fut);
    }
    let closes = log_array("closes").length();
    let constructed = log_field("constructed").as_f64().unwrap_or(-1.0);
    restore();

    assert_eq!(constructed, 1.0, "the socket was opened");
    assert_eq!(closes, 1, "and dropping the future closed it");
}

// ---------------------------------------------------------------------
// The one test against the real global.
// ---------------------------------------------------------------------

/// Everything above replaces `globalThis.WebSocket`; this one does not.
///
/// The harness's own origin serves ordinary HTTP and answers no upgrade,
/// so a real `WebSocket` to it fails its handshake — the only live
/// WebSocket outcome reachable without putting this suite on the network.
/// What it proves is the part a stand-in cannot: that the real global
/// answers to the same property and method names this crate uses, that
/// `binaryType`/`onopen`/`onclose` are where `web-sys` says they are, and
/// that a failed handshake reaches the caller as an error rather than a
/// hang.
///
/// The assertion is the `ErrorKind`, not a close code. Chrome and Firefox
/// both fail here and neither is required to report the same number — the
/// standard hides the reason from the page deliberately — and asserting
/// one engine's answer is the shape that has bitten this crate before.
#[wasm_bindgen_test]
async fn a_handshake_against_a_server_that_is_not_a_websocket_server_fails() {
    // First, not last: this is the one test that must run against the real
    // constructor, and a test above it that failed before its own
    // `restore` would otherwise hand it a stand-in.
    restore();
    let href = web_sys::window()
        .expect("wasm_bindgen_test_configure!(run_in_browser) guarantees a window")
        .location()
        .href()
        .expect("the currently loaded page always has an href");
    let err = Fetch::new()
        .websocket(req(&href))
        .await
        .expect_err("the harness's own server speaks HTTP, not RFC 6455");
    assert_eq!(
        *err.kind(),
        ErrorKind::Connect,
        "a handshake that never completed is a connect failure: {err}"
    );
}

/// The socket handle crosses a thread.
///
/// # Why this is worth pinning when the seam does not ask for it
///
/// `WebSocketConnect`/`WebSocket` declare no `Send` anywhere — that is the
/// allowance this backend was the predicted subject of, and it has not
/// moved. What changed is that being `!Send` stopped being *free*: every
/// other type this crate hands back is `Send`, so a caller who holds a
/// socket beside a response body had one type deciding the auto traits of
/// the struct around it, for a reason that was an implementation detail
/// (`Rc<RefCell<..>>`) rather than anything about WebSockets.
///
/// It costs no `unsafe` of its own: the state cell is `Arc<Mutex<..>>`
/// like `promise::State` beside it, and the three `Closure`s ride
/// `promise::SingleThreaded`, which already carries this crate's one
/// `unsafe impl Send` (amendment C7). Giving the closures a `Send` inner
/// `dyn` instead does not work and is not a matter of taste:
/// `WasmClosure` is implemented for `dyn FnMut(..) -> R + 'a` and no other
/// shape, so `Closure<dyn FnMut() + Send>` is a type that exists and
/// cannot be constructed.
///
/// **So this is `Send` exactly as far as `JsValue` is**, and it disappears
/// under `-Ctarget-feature=+atomics` with the `cfg` that strips
/// wasm-bindgen's own impl — which `fetch-must-fail-under-atomics` still
/// requires. A JS `WebSocket` belongs to the realm that made it, so an
/// honest `Send` under wasm threads would have to be an actor holding the
/// socket on its own thread; that is deliberately not built, because it
/// would move `start_send`'s refusal and `poll_close` off the synchronous
/// path they are on today, and the seam asks for none of it.
#[wasm_bindgen_test]
fn the_socket_handle_crosses_a_thread() {
    fn is_send<T: Send>() {}
    is_send::<hclient_fetch::FetchWebSocket>();
}
