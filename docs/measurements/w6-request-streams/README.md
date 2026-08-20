# Streaming request bodies in a browser: what actually goes on the wire

A one-off, manual harness. It is **not** run by CI and is not a Rust test —
it exists because the question v0.2 W6 had to answer could not be answered
from inside the browser at all, and could not be answered by reading
documentation either.

The question: if `hclient-fetch` hands a `ReadableStream` to `fetch()` as a
request body, **what bytes does the server receive?** Not "did the promise
reject" — bytes.

## Why the observer has to be a server

The failure mode this harness was built to look for is not a rejection. It
is a browser accepting the request, reporting `200 OK`, and sending
different bytes than the ones it was given. No observer inside the page can
see that: `fetch` resolved, the response is a real response, and the caller
is happy. Only something on the other end of the socket can compare what
was sent with what was meant.

So the harness is three servers that record every `data` event with a
timestamp and echo back what they got:

- `servers.js` — HTTP/1.1 in the clear on `:8801`, and HTTP/2 over TLS on
  `:8802` (`allowHTTP1: true`, so ALPN decides).
- `https1.js` — HTTPS on `:8803` whose ALPN offers `http/1.1` **only**.
  This one exists to separate "streaming needs HTTP/2" from "streaming
  needs a secure context"; without it, `:8801` vs `:8802` differs in two
  variables at once.

`page.html` is served by each of them and runs the same scenarios
same-origin against its own server (no CORS anywhere, no preflight to
confuse the result), then POSTs its findings to `/report`. `drive.js` is a
~90-line WebDriver client that starts `chromedriver`/`geckodriver` (the
copies `wasm-pack` already caches), opens a session with
`acceptInsecureCerts: true` for the self-signed certificate, navigates, and
waits for the report file to appear.

## Running it

```sh
openssl req -x509 -newkey rsa:2048 -nodes -keyout key.pem -out cert.pem \
  -days 2 -subj "/CN=localhost" \
  -addext "subjectAltName=DNS:localhost,IP:127.0.0.1"
node servers.js &
node https1.js &
node drive.js chrome  "http://127.0.0.1:8801/page.html?label=chrome-h1"        chrome-h1
node drive.js chrome  "https://127.0.0.1:8802/page.html?label=chrome-h2"       chrome-h2
node drive.js chrome  "https://127.0.0.1:8803/page.html?label=chrome-https-h1" chrome-https-h1
node drive.js firefox "http://127.0.0.1:8801/page.html?label=firefox-h1"       firefox-h1
node drive.js firefox "https://127.0.0.1:8802/page.html?label=firefox-h2"      firefox-h2
```

The body under test is always the same: three 4-byte chunks — `AAAA`,
`BBBB`, `CCCC` — enqueued 300 ms apart, so a server that receives them as
one 12-byte lump at t≈0 was handed a buffer, and a server that receives
three 4-byte pieces spread across ~900 ms was handed a stream.

`results/` holds the raw reports as they came back.

## What came back

Chrome 151.0.7922.71, Firefox 153.0.1, Linux, both headless, 2026-08-09.

| browser | origin protocol | `fetch` outcome | bytes the server received |
|---|---|---|---|
| Chrome | h2 (TLS) | **200**, 1206 ms | `AAAABBBBCCCC` — 12 bytes, in **three** DATA frames at t = 294 / 594 / 894 ms |
| Chrome | http/1.1 (cleartext) | `TypeError: Failed to fetch` after 3 ms | *nothing — the request never reached the server* |
| Chrome | http/1.1 (TLS, ALPN `http/1.1` only) | `TypeError: Failed to fetch` after 3 ms | *nothing* |
| Firefox | h2 (TLS) | **200**, 5 ms | `[object ReadableStream]` — **23 bytes**, one lump, `Content-Type: text/plain;charset=UTF-8`, `Content-Length: 23` |
| Firefox | http/1.1 (cleartext) | **200**, 51 ms | `[object ReadableStream]` — **23 bytes**, identical |

Three separate conclusions, and they point in different directions.

**1. Firefox does not refuse a stream body — it replaces it.** There is no
error anywhere: the `Request` constructor succeeds, `fetch` resolves, the
server answers `200`. What arrives is the 23-byte ASCII string
`[object ReadableStream]` (`5b6f626a656374205265616461626c6553747265616d5d`),
which is what `USVString` conversion does to a `ReadableStream` when the
implementation does not recognise it as a body type. The stream itself is
never read: its `pull` never runs, so the caller's data is not consumed,
merely discarded. This is identical on HTTP/1.1 and HTTP/2 — it happens
during `Request` construction, before a protocol is chosen, so the protocol
cannot matter.

This is the reason `hclient-fetch` must decide **before** it hands anything
to `fetch`, and may not adopt a "try it and map the error" strategy: there
is no error to map.

**2. The corruption is detectable from inside, cheaply and synchronously.**
The detection is whatwg/fetch#1470's: construct one throwaway `Request`
with a `ReadableStream` body and a `duplex` **getter**, then ask two
questions — was the getter read, and did the browser invent a
`Content-Type`? A browser that streams reads `duplex` and sets no
`Content-Type`; a browser that stringifies never looks at `duplex` and sets
`text/plain;charset=UTF-8`, because that is the content type of the string
it just made up. Measured: Chrome `duplexAccessed: true`,
`hasContentType: false`; Firefox `duplexAccessed: false`,
`hasContentType: true`. No network, no request sent, no page-observable
effect.

This is strictly stronger than checking `'duplex' in Request.prototype`,
which is the exact gap #1470 was filed about — a browser could expose the
getter and still stringify. Today both agree (Chrome yes/yes, Firefox
no/no), which is itself worth pinning: the day they disagree is the day the
presence check starts lying.

**3. Chrome's support is real but conditional on HTTP/2, and that condition
is a fact about the origin, not about the browser.** Over h2 the body
genuinely streams — three frames, 300 ms apart, and the 1206 ms round trip
is the client's own production schedule, not latency. Over HTTP/1.1 it
fails in 3 ms with a bare `TypeError: Failed to fetch`, before the stream is
pulled and before anything reaches the server. The third row proves this is
not about TLS: the same TLS connection with ALPN restricted to `http/1.1`
fails exactly as the cleartext one does.

That failure is loud and early, and it is also **indistinguishable from a
connection failure** — `TypeError: Failed to fetch` is the same value the
Fetch Standard produces for a refused connection, with no `name`, `message`
or property separating them. A browser can know the answer before trying
(Chrome clearly does, hence 3 ms), but it exposes it nowhere a caller can
read ahead of time. `PerformanceResourceTiming.nextHopProtocol` reports it
**after** a first response to that origin, and only when the origin sends
`Timing-Allow-Origin` cross-origin — which is neither ahead of time nor
universally available.

So there is no per-request prediction available, and `Capabilities` has no
per-origin dimension to put it in. See `crates/hclient-fetch/src/caps.rs`
for what the crate declares as a result and why.
