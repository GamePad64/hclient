# Migrating `wasi-fetch` → `http-ng`

`wasi-fetch` 0.2.0 is 548 lines of Rust across four files — `request.rs`
278, `body.rs` 148, `lib.rs` 77, `error.rs` 45 — of which the redirect loop
inside `RequestBuilder::send` is 66 (`request.rs:93-158`). Almost all of it
has a counterpart here, and the parts that do not are the parts that were
`wasip3` plumbing.

Everything below was checked against the two sources, not inferred from
memory: `wasi-fetch` 0.2.0 and this repository at the commit that added
this file. A worked migration of a real consumer —
`act/components/http-client`, which is what `wasi-fetch` was written for —
is in [`crates/http-ng/examples/portable.rs`](../crates/http-ng/examples/portable.rs).
That example and this table say the same thing about timeouts, redirects
and the body; if they ever stop agreeing, the example is the one that is
compiled and tested.

## The mapping

| `wasi-fetch` 0.2.0 | `http-ng` |
|---|---|
| `Client` (a unit struct) + `get/post/put/delete/patch/head/request` | `http_ng::Client<T>` + the same methods |
| `Client::query` (the `QUERY` verb) | `client.request(http::Method::from_bytes(b"QUERY")?, url)` |
| `wasi_fetch::send(req)`, the free function | `Client::execute` |
| `RequestBuilder::{header, headers, body}` | `http_ng::RequestBuilder::{header, headers, body}` |
| `RequestBuilder::json(&T)` | **no equivalent** — serialize yourself, set `content-type` yourself |
| `timeout(d)` — sets the wasip3 `connect` **and** `first_byte` options from one `Duration` | `Timeouts { connect: Some(d), first_byte: Some(d), .. }` — **both** fields, or the connect timeout is silently lost |
| `between_bytes_timeout(d)` | `Timeouts { between_bytes: Some(d), .. }`, the third field of the same struct |
| `redirect_limit(n)` + the 66-line loop in `send` | `RedirectPolicy { limit: n }`, on `ClientBuilder::redirect` or per request on `RequestBuilder::redirect`, carried out by the `Redirect` stage in `Client::execute` |
| `send_raw`, `BodyWriter`, `join!`, `to_wasi_method` | `http-ng-wasi` — you no longer write any of it |
| `Body::chunk` | `Response::chunk` (and it now reports errors, see below) |
| `Body::{bytes, text, json}` | `Response::collect` → `Collected::{bytes, text, json}` (`json` behind the `json` feature) |
| `Error::{Url, Transport, Utf8, Json}` | `http_ng::Error` with `ErrorKind::{Connect, Tls, Timeout, Redirect, Body, Decode, …}` |
| the seven `let _ =` on wasip3 setters (`request.rs:209,210,213,234,235,237,239`) | `Capabilities` + `UnsupportedCapability` — an unsupported setting is an error, not a discarded return value |

## What the migration fixes

1. **300, 304, 305 and 306 are no longer followed.** The old loop gated on
   `status.is_redirection()`, which is every 3xx. `redirect::decide`
   matches `301 | 302 | 303 | 307 | 308` and nothing else — 300 needs a
   user choice, 304 is the answer to a conditional request, 305 has not
   been honoured by browsers since 2014, 306 is reserved.
2. **`Authorization`, `Cookie` and `Proxy-Authorization` are stripped on a
   cross-origin hop.** The old loop cloned the caller's `HeaderMap` onto
   every hop unchanged, so a redirect to another host carried the bearer
   token with it. `strip_sensitive` fires on a change of host, scheme
   **or** port, with the scheme's default port substituted in first so
   `https://a:443/` → `https://a/` does not read as a change.
3. **301 and 302 with POST are downgraded to GET, and the body is
   dropped**, the same as 303 — matching browsers and reqwest. The old
   loop downgraded only 303, and re-sent the POST body to the redirect
   target.
4. **A rejected option stops being silent.** Seven `let _ =` discarded the
   result of a wasip3 setter, including all three timeouts; a host that
   answered `request-options-error::not-supported` produced a request with
   no timeout and no diagnostic. Here the transport declares what it can do
   in `Capabilities`, and a setting it cannot honour is
   `UnsupportedCapability` — at `build()` for a client-level setting, out
   of `send()` for a per-request one.
5. **An invalid header is no longer dropped on the floor.**
   `wasi-fetch::RequestBuilder::header` was `if let (Ok(name), Ok(value)) =
   … { insert }` with no `else`. `http_ng::RequestBuilder::header` records
   the first failure and `send()` returns it.
6. **`headers()` adds instead of replacing.** `wasi-fetch`'s assigned
   (`self.headers = headers`), discarding everything `header()` had set,
   with no diagnostic.
7. **A truncated body is no longer indistinguishable from a complete
   one.** `wasi-fetch::Body::chunk` returned `Option<Bytes>` and mapped
   `Some(Err(_))` onto `None` (`body.rs`), so a connection that broke
   mid-download ended the stream and returned success.
   `Response::chunk` returns `Option<Result<Bytes, Error>>`, and the error
   is terminal rather than repeating.
8. **Reading the body no longer consumes the status.** `Body::{bytes, text,
   json}` took `self` by value out of a response that had already been
   destructured. `Collected` keeps status, headers and the final URL
   alongside the bytes.
9. **One codebase covers three targets.** `wasi-fetch` is `wasip3` only.
   The same consumer code on `http_ng::Client<T>` builds for native, for
   `wasm32-wasip2` and for the browser with no `#[cfg]` — that is what
   `examples/portable.rs` and the `portable-example-three-targets` CI job
   check.

## What the migration changes or does not carry over

Four items, none of them papered over.

1. **`redirect_limit(0)` maps to `RedirectPolicy::None`, never to
   `Limited(0)`.** This is the one substitution a mechanical migration gets
   wrong, and it is silent when it does.

   In `wasi-fetch` the redirect loop was gated on
   `if redirect_limit > 0 && status.is_redirection()` (`request.rs:135`), so
   a limit of `0` skipped the branch entirely and returned the 3xx **to the
   caller as an ordinary response**. `http-ng` spells that
   `RedirectPolicy::None`: `decide` answers `Stop` before any hop counting,
   and the response arrives with its `Location` intact.

   `RedirectPolicy::Limited(0)` is a different instruction — follow up to
   zero hops, so the first 301/302/303/307/308 carrying a `Location` becomes
   `Err(ErrorKind::Redirect)`. Translating `0` to `0` therefore turns an
   answer into an error, and does it only on the redirect path, where a test
   suite that never redirects will not notice.

   It is the distinction `reqwest` draws between `Policy::none()` and
   `Policy::limited(0)`. `RedirectPolicy` was a `struct { limit: u8 }` and
   could express only the second; porting `act`'s `http-client` component,
   whose `follow_redirects: false` forwards the 302 upward, is what surfaced
   that, and the type became an enum before v0.1 shipped.
2. **There is still no total-deadline timeout, in either library** —
   `Timeouts` is `connect`/`first_byte`/`between_bytes`, `wasi-fetch` had
   the same three and no whole-request deadline, so a response that starts
   promptly and then dribbles just under the `between_bytes` threshold runs
   unbounded on both; the migration is faithful here and a total deadline
   is a documented non-goal rather than something lost on the way.
3. **`RequestBuilder::json(&T)` has no counterpart.** Serialize the value
   and set `content-type: application/json` yourself — which is what a
   consumer that wants control over the failure already does, since
   `wasi-fetch::json` swallowed a serialization error (`if let Ok(bytes)`)
   and sent the request with no body at all.
4. **A backend that follows redirects internally refuses any redirect
   policy.** The browser `fetch` transport reports
   `RedirectSupport::Internal`: it follows redirects inside the browser and
   nothing above it can see or override that. Stating *any*
   `RedirectPolicy` against it — including `limit: 10`, which is also the
   default — is `ErrorKind::Unsupported` rather than a setting that quietly
   does nothing. This cannot happen on wasip3, and it is not a regression;
   it is new ground that opens up the moment the same code is also built
   for the browser. Code that has no opinion about redirects should set no
   policy at all: `Config::redirect` is an `Option` precisely so that "I
   never mentioned redirects" stays distinguishable from "follow up to ten,
   and I mean it".

## `wasi-fetch` 0.3

The plan is for `wasi-fetch` to stay findable: a thin facade over
`http_ng::Client<http_ng_wasi::WasiHttp>` keeping the old names, so users
migrate by changing a dependency rather than their code.

It will not be a pure renaming, and the two reasons are items 1 and 3
above. `redirect_limit(0)` has to keep meaning "return the 3xx", which the
facade cannot express through `RedirectPolicy` today, and `json()` has to
keep existing. Everything else on the list — `header`, `headers`, `body`,
`timeout`, `between_bytes_timeout`, `chunk`, `bytes`, `text`, `json` on the
response, and the whole of `send_raw` — maps straight through.

Note also that `wasi_fetch::Client` was a unit struct constructed per call
(`Client::new().request(..)`), while `http_ng::Client<T>` owns a transport.
Build it once and share it; that is the point of holding one.
