//! http-ng transport over `wasi:http` 0.3 (the `wasip3` package).
//!
//! Builds under `wasm32-wasip2`. No `wasip3` type appears in this crate's
//! public API: `Body::Error` is `http_ng_core::Error`, which erases the
//! source into `Arc<dyn std::error::Error + Send + Sync>`.
#![forbid(unsafe_code)]

mod body;
mod convert;
mod hooks;

pub use body::Body;

use convert::{Payload, TrailerWatch};
use http_ng_core::unversioned::{ConnectionId, Event, Head, Hooks, NoHooks, Transport};
use http_ng_core::{
    CancelSupport, Capabilities, Error, RedirectSupport, RequestBody, ReuseSupport, TimeoutSupport,
    Timeouts, TlsSupport,
};
use wasip3::http::types::{ErrorCode, Fields, Request, RequestOptions};
use wasip3::http_compat::{BodyWriter, http_from_wasi_response};

/// Headers the `wasi:http` host refuses to accept from the guest (review
/// resolution, finding B-6): `connection`/`keep-alive` — connection
/// management here is entirely on the host side; `transfer-encoding` — the
/// host itself decides the transfer encoding for the actual body;
/// `upgrade` — no protocol upgrade support, and this crate implements
/// neither `wasi:http`'s `HTTP-upgrade-failed` nor
/// `http_ng_core::unversioned::WebSocketConnect`, which is how a backend
/// says it can now (there is no capability field to read: see that
/// trait's own module doc); `host` — the host computes it itself
/// from `authority`. Measured by trying to send each of them —
/// `wasi_request.set_method` goes through, but the host rejects the
/// request itself if it carries any of these headers in `Fields`. The list
/// used to be empty, so a caller filtering headers by exactly this field —
/// the entire point of its existence — would catch a runtime error instead
/// of simply never setting the forbidden header.
// `LazyLock`, not `static X: &[..] = &[..]`: `HeaderName` stores its name in
// `Bytes`, which has interior mutability (an atomic refcount) — the
// compiler refuses to "promote" a literal with such a value directly into
// a static slice (`E0492`). Initialized once on first access and lives
// until the process ends — a `'static` reference to it is sound.
static FORBIDDEN_REQUEST_HEADERS: std::sync::LazyLock<[http::HeaderName; 5]> =
    std::sync::LazyLock::new(|| {
        [
            http::header::CONNECTION,
            http::header::HeaderName::from_static("keep-alive"),
            http::header::TRANSFER_ENCODING,
            http::header::UPGRADE,
            http::header::HOST,
        ]
    });

/// Transport over the ambient `wasi:http/client.send` — the guest holds no
/// socket of its own, all network interaction is delegated to the host.
///
/// # `H`, the observability hook (v0.4 W2)
///
/// [`NoHooks`] by default — a zero-sized type whose `Hooks::WATCHING` is
/// `false` — so `WasiHttp` still names the transport it always named, and
/// a build that asks for nothing reads no clock. [`WasiHttp::hooks`] is
/// how a caller asks; what comes back is a *different type*, because the
/// hook is a type parameter rather than a `Box<dyn Hooks>`, which is the
/// whole of the zero-cost claim.
///
/// **This backend emits exactly one of the four events**, and which one is
/// the finding rather than an omission: `wasi:http@0.3.0` has no
/// connection resource anywhere in it, so `Connected`, `Reused` and
/// `Closed` have nothing to be about. See `crate::hooks`.
#[derive(Debug)]
pub struct WasiHttp<H = NoHooks> {
    caps: Capabilities,
    /// Where the events go. `NoHooks` is a ZST, so this field costs a
    /// build that wants nothing exactly nothing.
    hooks: H,
}

impl<H> WasiHttp<H> {
    /// Send this transport's events to `hooks` — see
    /// [`http_ng_core::unversioned::Hooks`] for what it hears and what it
    /// costs, and `crate::hooks` for the three quarters of the
    /// vocabulary `wasi:http` cannot speak.
    ///
    /// **It returns a different type**, and that is the zero-cost
    /// mechanism rather than an inconvenience: the hook is a type
    /// parameter, so the `NoHooks` build monomorphises to code with no
    /// clock reads in it at all, where a `Box<dyn Hooks>` field would
    /// leave every no-hook build carrying a null check on the request
    /// path.
    ///
    /// The hook may be `!Send`: nothing on this path declares it, so an
    /// `Rc` inside a hook makes this transport `!Send` and leaves it
    /// working (P13; `crates/http-ng-core/tests/shape.rs`). The cost is
    /// visible and is the caller's to weigh — `tests/shape.rs` here pins
    /// that a `Send` hook leaves `execute`'s future `Send`, which is what
    /// the streaming-body path in this crate spends a
    /// `send-bound-exception` marker on.
    pub fn hooks<H2>(self, hooks: H2) -> WasiHttp<H2> {
        WasiHttp {
            caps: self.caps,
            hooks,
        }
    }
}

impl WasiHttp {
    pub fn new() -> Self {
        let mut caps = Capabilities::none();
        // `streaming_request_body`: `RequestBody::Streaming` goes straight
        // into `BodyWriter::send_http_body` as-is, frame by frame, without
        // buffering in memory — honest streaming, see
        // `convert::resolve_payload`.
        caps.streaming_request_body = true;
        // `full_duplex = false` — review resolution, finding B-2;
        // attribution rewritten per M1 of the branch's final review. The
        // `wasi:http` 0.3 protocol itself does support body duplex (body
        // data can flow while the response hasn't arrived yet), and the
        // host genuinely provides it — the review measured this with
        // direct `wasip3` calls bypassing this crate: `send` resolved with
        // a full 200 after only 1.6% of a 16-megabyte body had gone into
        // the writer. What does NOT provide it is THIS implementation of
        // `execute`: `race_send_with_body` waits for both `send` and the
        // body write to finish (except for one case — an early rejection
        // from `send`, see B-5). Measured on a live host: the response
        // existed on the server side at t≈0.10s, but the caller only saw
        // it at t≈2.00s, once the body write finished; for a body with no
        // end, it would never have seen it at all.
        //
        // **The seam's shape has nothing to do with this** — an earlier
        // version of this comment claimed the opposite (that "the fix
        // changes the seam signature's shape"), and that was wrong.
        // `Transport::execute` returns `http::Response<Self::Body>`, and
        // `Self::Body` is `Body` from this same crate: the unfinished
        // write future can be carried into it and polled further from
        // `poll_frame`, handing the response to the caller immediately and
        // turning a transmission failure into a terminal body error. The
        // review implemented this as a proof-of-concept: about forty
        // lines, one new `Inner` variant, `Transport::execute`'s signature
        // untouched — and measured it on the same guest and server: the
        // branch as it stands hangs until killed at 25s, the variant with
        // the future in `Body` hands back the response head in 0.094s.
        // The trick isn't new: the doc comment on `convert::resolve_send`
        // proposes exactly this for a DIFFERENT discarded future
        // (`transmitted`) — the reasoning was already done and simply
        // never applied to `write_fut`.
        //
        // Deferred (vertical 2, entirely within this crate) for three real
        // costs, none of which is about the seam:
        //  1. The guard against undeclared trailers can't run before
        //     `execute` returns: trailer names are only known once the
        //     body has ended. It moves into `Body` and becomes a terminal
        //     body error — that's a genuine rework of the function, one
        //     the branch has already put a lot of work into (see
        //     `caps.request_trailers` below).
        //  2. `resolve_send`'s policy ("a response that arrives over a
        //     failed body write is not a success") turns from an
        //     `execute`-level error into a body-level error. Weaker —
        //     though arguably more correct: the caller already has the
        //     response in hand.
        //  3. A caller who never reads the response body would then also
        //     never finish writing the request body. Inherent to duplex
        //     without `spawn`, and needs documenting in the contract.
        caps.full_duplex = false;
        // `request_trailers`/`response_trailers`: trailers DO WORK on
        // `wasi:http`, but only for fields whose NAMES the request
        // declared in advance via the `Trailer:` header — measured
        // (review resolution, finding B-1, refined by fix round 2 finding
        // 2) that the host's HTTP/1.1 encoder silently drops on the wire
        // any trailer field whose name wasn't declared up front, even if
        // `Trailer:` is present but names a DIFFERENT field. Trailer
        // names are only known once the body has ended — they can't be
        // predicted before the headers, and there's no one here to inject
        // `Trailer:` on the caller's behalf. The caller is responsible for
        // setting `Trailer:` themselves with the names of ALL fields their
        // `RequestBody::Streaming` will emit as trailers;
        // `Transport::execute` checks the names that actually arrived
        // against the declared ones (see `convert::TrailerWatch`,
        // `convert::declared_trailer_names`, `convert::undeclared_trailers`)
        // and catches the violation as a typed error instead of silently
        // losing data. **This error arrives after the fact**: by the time
        // the caller sees it, the request has already reached the server
        // and got a response (the guard fires only after
        // `race_send_with_body` already succeeded) — not a reason to
        // blindly retry a non-idempotent request.
        caps.request_trailers = true;
        caps.response_trailers = true;
        caps.timeouts = TimeoutSupport {
            // **`false`, and honestly so.** `wasi:http` 0.3's
            // `request-options` has connect-timeout, first-byte-timeout
            // and between-bytes-timeout and nothing for resolution: the
            // host resolves, and there is no moment in this guest at which
            // a bound could be applied or a failure attributed. Declaring
            // it would be the shape `Capabilities` exists to refuse.
            resolve: false,
            connect: true,
            first_byte: true,
            between_bytes: true,
        };
        // And poorer everywhere else: the spec has no TLS, no proxy, no
        // version selection, no upgrade.
        //
        // Redirects are `Transparent`, not `None` (M2 of the branch's
        // final review). Measured on a live host (Task 16 review
        // resolution, finding B-9): a 3xx reaches the guest as an ordinary
        // response, and following the chain is entirely on the guest —
        // meaning the `Client`'s redirect stage works fully here. `None`
        // couldn't say that: `Capabilities::none()` returns that same
        // value, so a caller couldn't tell "the backend is transparent"
        // from "the field was never filled in" and, deciding from
        // `redirects == None` that redirects were impossible here, would
        // be wrong about the one backend that actually exists.
        caps.redirects = RedirectSupport::Transparent;
        // The guest owns no socket, so this is the host's to perform — and
        // the Component Model requires it to. `wasip3::http::client::send`
        // is an `[async-lower]` import, and the future wit-bindgen
        // generates for it is a `WaitableOperation` whose `Drop` calls
        // `[subtask-cancel]` synchronously
        // (`wit-bindgen-0.57.1/src/rt/async_support/{waitable.rs,
        // subtask.rs}`); the host must then either return the result or
        // report the subtask cancelled, it may not simply keep going.
        //
        // v0.2's design document guessed the other way — "a WASI host may
        // not expose it" — so this was measured, on a live wasmtime host,
        // rather than read off the bindings: `tests/live_roundtrip.rs`'s
        // `dropping_the_execute_future_closes_the_connection_the_server_sees`
        // watches the mock server's own socket, and sees it close 0.3s in,
        // at the drop, while the guest itself stays alive for another 1.5s.
        // The control run, holding the same future instead of dropping it,
        // leaves the connection open through the whole observation window.
        caps.cancel_on_drop = CancelSupport::Supported;
        // The guest has no socket of its own; the host makes the request
        // and the host decides whether to keep the connection. This field
        // read `Supported` until it was measured, on the reasoning that
        // "every `wasi:http` host worth using keeps HTTP/1.1 connections
        // alive between outbound requests" — and the measurement says the
        // host this project actually runs does not.
        //
        // `tests/live_roundtrip.rs`'s
        // `two_guest_requests_to_one_origin_open_two_connections` sends two
        // sequential requests from the guest to one origin and counts what
        // the server accepted: **two**. The observer is the same one
        // `http-ng-native`'s pool uses, and it works here for the same
        // reason cancellation was measurable — the server is a plain
        // `TcpListener` on a host thread, outside the sandbox, with the
        // guest as a wasmtime subprocess.
        //
        // The reason is structural rather than a setting.
        // `wasmtime_wasi_http::p3::default_send_request`
        // (`crates/wasi-http/src/p3/request.rs` at v47.0.3) does
        // `TcpStream::connect(&authority)` and then
        // `hyper::client::conn::http1::Builder::new().handshake(..)` inside
        // the per-request function: there is no pool for a second request
        // to find. Nor is the host announcing it — the request heads that
        // server receives carry no `Connection: close`.
        //
        // So `None`, which is the conservative base and here also the
        // literal truth on the measured host: every request opens a
        // connection and closes it when it is done. An embedder that
        // replaces the outbound hook may well pool, and against that one
        // this under-claims — the direction this project's capability model
        // requires, since a caller who reads `None` plans for a handshake
        // per request and is merely pessimistic rather than deadlocked or
        // surprised. Moving it back up needs a measurement, the way this
        // move down had one.
        caps.connection_reuse = ReuseSupport::None;
        caps.tls_config = TlsSupport::None;
        caps.forbidden_request_headers = FORBIDDEN_REQUEST_HEADERS.as_slice();
        Self {
            caps,
            hooks: NoHooks,
        }
    }
}

impl Default for WasiHttp {
    fn default() -> Self {
        Self::new()
    }
}

impl<H: Hooks> Transport for WasiHttp<H> {
    type Body = Body;
    type Error = Error;

    async fn execute(
        &self,
        req: http::Request<RequestBody>,
    ) -> Result<http::Response<Body>, Error> {
        // Gated on `H::WATCHING`, a `const` — see `crate::hooks`. Taken
        // before anything else so that `Head::elapsed` covers the whole
        // call, conversion included, exactly as `http-ng-native`'s does.
        // No `Uri` has to be cloned beside it, unlike `http-ng-fetch`:
        // `parts` outlives `send` here, so the request's own `Uri` is
        // still there to borrow when the response arrives.
        let began = hooks::mark::<H>();

        let (parts, body) = req.into_parts();
        let scheme = convert::scheme_of(&parts.uri)?;

        // Review resolution, finding B-1 (refined by fix round 2 finding
        // 2): captured BEFORE the headers go into `Fields` — needed
        // afterward too, to check against the field names
        // `RequestBody::Streaming` actually emits, not just against the
        // fact that the header is present.
        let declared_trailer_names = convert::declared_trailer_names(&parts.headers);

        let header_list: Vec<(String, Vec<u8>)> = parts
            .headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.as_bytes().to_vec()))
            .collect();
        let fields = Fields::from_list(&header_list).map_err(convert::fields_error)?;

        let timeouts = parts
            .extensions
            .get::<Timeouts>()
            .copied()
            .unwrap_or_default();
        let opts = RequestOptions::new();
        // Review resolution, finding B-10: `as u64` on the `u128` from
        // `Duration::as_nanos()` truncates silently for durations beyond
        // ~584 years. `u64::MAX` nanoseconds is already ~584 years, so the
        // truncation is physically unreachable for any sensible timeout
        // here, but `try_from` + `unwrap_or(u64::MAX)` names that
        // explicitly instead of silently wrapping via `as`.
        let nanos = |d: core::time::Duration| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);
        convert::apply_timeouts(
            &opts,
            timeouts.connect.map(nanos),
            timeouts.first_byte.map(nanos),
            timeouts.between_bytes.map(nanos),
        )?;

        // `writer` and `payload` are set up together, in a single
        // `Option`: keeping them as two independent `Option`s that
        // "should" agree by construction would be exactly the class of
        // invariant you later end up closing with an unreachable `match`
        // arm. Here this pair can't drift apart — there's nothing to
        // write exactly when there's no one to write it.
        let payload = convert::resolve_payload(body)?;
        let (writer_and_payload, contents, trailers) = match payload {
            None => {
                let (_, trailers) =
                    wasip3::wit_future::new::<Result<Option<Fields>, ErrorCode>>(|| Ok(None));
                (None, None, trailers)
            }
            Some(p) => {
                let (w, reader, trailers) = BodyWriter::new();
                (Some((w, p)), Some(reader), trailers)
            }
        };

        // Review resolution, finding B-3, revisited experimentally while
        // preparing fix round 1, wording refined in fix round 2 (finding
        // 5). `Request::new`'s second return value is a
        // `FutureReader<Result<(), ErrorCode>>`, documented upstream as
        // "resolves to result of transmission of this request". The
        // review's plan — fold it in as a third arm of
        // `race_send_with_body` — was implemented and ROLLED BACK:
        // measured on a live host that this future is NOT GUARANTEED to
        // resolve before the response body has been fully read (a small
        // `Content-Length` response resolves it without a single
        // `poll_frame` call — checked separately), but for `chunked`,
        // trailer-bearing responses, and any body of appreciable size it
        // measurably does not resolve until `Body` is drained by hand.
        // `execute()` hands `Body` to the caller BEFORE they decide
        // whether to read it — normally later, partially, or never.
        // Waiting on this future here unconditionally would mean either:
        // `execute()` doesn't return for a typical response with a body
        // until it has drained it entirely itself (destroying the
        // streaming `Body` exists for), or it hangs for the ordinary case
        // of "the body is read after getting the `Response`". Neither
        // option is compatible with THIS shape of the seam — but carrying
        // the future into `Body` and awaiting it at the end of the
        // stream, surfacing a transmission failure as a terminal body
        // error, is compatible and remains a candidate for v0.2 (details
        // and full argument in the `convert::resolve_send` doc comment).
        // Dropped here explicitly, with this comment, rather than via
        // `let (.., _) = ..`.
        let (wasi_request, transmitted) = Request::new(fields, contents, trailers, Some(opts));
        drop(transmitted);
        wasi_request
            .set_method(&convert::to_wasi_method(&parts.method))
            .map_err(|_| convert::rejected("method"))?;
        wasi_request
            .set_scheme(Some(&scheme))
            .map_err(|_| convert::rejected("scheme"))?;
        if let Some(a) = parts.uri.authority() {
            wasi_request
                .set_authority(Some(a.as_str()))
                .map_err(|_| convert::rejected("authority"))?;
        }
        wasi_request
            .set_path_with_query(parts.uri.path_and_query().map(|p| p.as_str()))
            .map_err(|_| convert::rejected("path_with_query"))?;

        // `send` and the body write are not dropped (see the doc comments
        // on `convert::resolve_send`/`race_send_with_body`):
        // short-circuiting on an early `send` rejection (finding B-5) is
        // the one exception to "wait for both", and it doesn't change what
        // the policy would ALREADY have discarded in that case.
        let wasi_response = match writer_and_payload {
            None => wasip3::http::client::send(wasi_request)
                .await
                .map_err(convert::wasi_err)?,
            Some((w, Payload::Bytes(bytes))) => {
                let mut b = Body::from_bytes(bytes);
                convert::race_send_with_body(
                    wasip3::http::client::send(wasi_request),
                    w.send_http_body(&mut b),
                )
                .await?
            }
            Some((w, Payload::Streaming(s))) => {
                let (mut watched, trailer_names_seen) = TrailerWatch::new(s);
                let resp = convert::race_send_with_body(
                    wasip3::http::client::send(wasi_request),
                    w.send_http_body(&mut watched),
                )
                .await?;
                // Review resolution, finding B-1 (refined by fix round 2
                // finding 2): compare the NAMES of trailer fields that
                // actually arrived against the declared ones, not just
                // whether `Trailer:` is present — a header naming the
                // wrong field loses data exactly as if it were absent
                // (measured). Don't return a success that would hide that
                // (see `convert::undeclared_trailers`).
                let undeclared: Vec<http::HeaderName> = trailer_names_seen
                    .lock()
                    .expect("single-threaded guest, never poisoned")
                    .iter()
                    .filter(|name| !declared_trailer_names.contains(*name))
                    .cloned()
                    .collect();
                if !undeclared.is_empty() {
                    return Err(convert::undeclared_trailers(undeclared));
                }
                resp
            }
        };

        let (resp_parts, incoming) = http_from_wasi_response(wasi_response)
            .map_err(convert::wasi_err)?
            .into_parts();
        let out = http::Response::from_parts(resp_parts, Body::from_incoming(incoming));

        // The one event this backend has. `if let Some(..)` on the mark is
        // the gate rather than a second read of `H::WATCHING`: the `Some`
        // above is exactly `H::WATCHING`, and two places that have to
        // agree is one more than is safe.
        //
        // `status` comes off the response this function is about to
        // return, not from anything read separately — the same discipline
        // `http-ng-native`'s `report_head` follows.
        //
        // `version` is `None`, and here that is stronger than a browser's
        // silence: `wasi:http@0.3.0` has no version concept at all, so
        // there is nothing for a host to withhold. `capabilities()` says
        // so with `version_reported: false`, and the `HTTP/1.1` on the
        // response is what `http_from_wasi_response` builds rather than
        // anything observed — see `Head::version` in `http-ng-core`.
        //
        // Nothing is reported on any of the error paths above, including
        // the undeclared-trailers refusal, which is the one that fires
        // *after* a real response arrived. That refusal is this crate
        // saying the exchange did not succeed, and a `Head` beside it
        // would tell a caller counting heads that it did.
        if began.is_some() {
            self.hooks.on(Event::Head(Head {
                id: ConnectionId::UNWATCHED,
                uri: &parts.uri,
                status: out.status(),
                version: None,
                elapsed: hooks::since(began),
            }));
        }
        Ok(out)
    }

    /// Identity, not the default wrapping: `Self::Error` is already
    /// `http_ng_core::Error`, and `convert::wasi_err` has just sorted 39
    /// `ErrorCode` variants into it.
    ///
    /// Without this override `Client::execute` would wrap it again, with
    /// `ErrorKind::Other`, and this whole classification would disappear
    /// for the caller: `is_timeout()`/`is_connect()`/`is_unsupported()`
    /// would all return `false` alike for a DNS failure, a TLS failure, a
    /// connect timeout, and a host rejection, and `Display` would print
    /// the category twice — `Other: Unsupported: wasi:http host rejected
    /// setting 'scheme'`. This was finding B2 of the branch's final
    /// review, and it's fixed right here.
    fn to_error(&self, e: Self::Error) -> Error {
        e
    }

    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
}
