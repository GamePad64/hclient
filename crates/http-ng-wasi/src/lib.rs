//! http-ng transport over `wasi:http` 0.3 (the `wasip3` package).
//!
//! Builds under `wasm32-wasip2`. No `wasip3` type appears in this crate's
//! public API: `Body::Error` is `http_ng_core::Error`, which erases the
//! source into `Arc<dyn std::error::Error + Send + Sync>`.
#![forbid(unsafe_code)]

mod body;
mod convert;

pub use body::Body;

use convert::{Payload, TrailerWatch};
use http_ng_core::unversioned::Transport;
use http_ng_core::{
    Capabilities, Error, RedirectSupport, RequestBody, TimeoutSupport, Timeouts, TlsSupport,
    UpgradeSupport,
};
use wasip3::http::types::{ErrorCode, Fields, Request, RequestOptions};
use wasip3::http_compat::{BodyWriter, http_from_wasi_response};

/// Headers the `wasi:http` host refuses to accept from the guest (review
/// resolution, finding B-6): `connection`/`keep-alive` — connection
/// management here is entirely on the host side; `transfer-encoding` — the
/// host itself decides the transfer encoding for the actual body;
/// `upgrade` — no protocol upgrade support (`Capabilities::upgrade` is
/// already `UpgradeSupport::None`); `host` — the host computes it itself
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
#[derive(Debug)]
pub struct WasiHttp {
    caps: Capabilities,
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
        caps.tls_config = TlsSupport::None;
        caps.upgrade = UpgradeSupport::None;
        caps.forbidden_request_headers = FORBIDDEN_REQUEST_HEADERS.as_slice();
        Self { caps }
    }
}

impl Default for WasiHttp {
    fn default() -> Self {
        Self::new()
    }
}

impl Transport for WasiHttp {
    type Body = Body;
    type Error = Error;

    async fn execute(
        &self,
        req: http::Request<RequestBody>,
    ) -> Result<http::Response<Body>, Error> {
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
        Ok(http::Response::from_parts(
            resp_parts,
            Body::from_incoming(incoming),
        ))
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
