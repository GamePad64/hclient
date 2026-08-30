//! Every value this crate refuses, and every refusal the host hands back.
//!
//! **They are one subject because this backend owns nothing.** There is no
//! socket here, no connector and no pool: `wasi:http` 0.3's `client` is one
//! function, and everything below it belongs to the host. So each of these
//! six types is a boundary report rather than a failure of ours — four are
//! the host refusing a value the transport handed it ([`BadScheme`] before
//! the call, [`TimeoutRejected`], [`Rejected`] and [`FieldsError`] at it),
//! one is a transfer failing on the host's side of the WIT boundary
//! ([`BodyWriteFailed`]), and one is the guard for the case the host
//! reports *nothing at all* ([`UndeclaredTrailers`]).
//!
//! That last one is why they read better together than scattered through
//! `convert.rs`: the interesting question about this backend's errors is
//! not what each says but **which of them the host can say and which it
//! cannot**, and the answer only shows up when they are in one place.
//! [`TimeoutRejected`] carries the host's reason and [`Rejected`] cannot,
//! because those two setters have different WIT signatures; and the
//! silent trailer drop has no error code anywhere in `wasi:http`, so this
//! crate has to notice it after the fact.
//!
//! All six are private and reach a caller only through `Error::source`;
//! the `ErrorKind` each is filed under stays with the conversion function
//! in [`crate::convert`] that chooses it, because that choice is made per
//! call site and in one case per variant.

use wasip3::http::types::{HeaderError, RequestOptionsError};

#[derive(Debug, thiserror::Error)]
#[error("URI scheme must be http or https")]
pub(crate) struct BadScheme;

/// The host's rejection of applying a request timeout option.
///
/// Unlike [`Rejected`], this rejection has a substantive reason —
/// `RequestOptionsError::{NotSupported,Immutable,Other}` — and it isn't
/// flattened into a string: `Display` reports both WHAT was rejected and
/// WHY, and the `RequestOptionsError` itself stays fully reachable through
/// `Error::source()`. The same principle as `convert::wasi_err` for
/// `ErrorCode`: the category (`ErrorKind::Unsupported`) is kept separate
/// from the reason, and the reason isn't a string.
///
/// The field is named `source` on purpose, not by accident of naming:
/// that is what makes it the `Error::source()` of this type, without an
/// attribute — and it must stay so, because the reason is exactly what a
/// caller comes down the chain for.
#[derive(Debug, thiserror::Error)]
#[error("backend `wasi:http` does not support `{what}`: {source}")]
pub(crate) struct TimeoutRejected {
    pub(crate) what: &'static str,
    pub(crate) source: RequestOptionsError,
}

/// The host's rejection of applying `method`/`scheme`/`authority`/
/// `path_with_query`.
///
/// Unlike [`TimeoutRejected`], these `wasi:http` setters return a bare
/// `Result<(), ()>` — the host never sends back any reason at all,
/// there's nothing to wrap in `source`. `what` is all there is to report.
#[derive(Debug, thiserror::Error)]
#[error("wasi:http host rejected setting `{0}`")]
pub(crate) struct Rejected(pub(crate) &'static str);

/// `Fields::from_list` failed: the header is syntactically invalid,
/// forbidden, or exceeds a host limit.
///
/// `#[source]`, not `#[from]`: the category is chosen per variant by
/// `convert::fields_error`, so there is no single `HeaderError -> Error`
/// conversion to hand out.
#[derive(Debug, thiserror::Error)]
#[error("invalid headers: {0}")]
pub(crate) struct FieldsError(#[source] pub(crate) HeaderError);

/// Writing the request body (`BodyWriter::send_http_body`) failed.
///
/// Wraps the whole `wasip3::http_compat::Error`, not just its `Display`:
/// the reason (`HttpBody` — our own frame source failed;
/// `StreamReaderClosed` — the host closed the reading end early;
/// `ResultReaderClosed` / `InvalidTrailers` — a failure at the tail of the
/// transfer) stays reachable through `Error::source()` instead of being
/// flattened into a string. See `convert::resolve_send` for exactly when
/// this error wins and when it yields to the `send` error.
#[derive(Debug, thiserror::Error)]
#[error("failed to write request body: {0}")]
pub(crate) struct BodyWriteFailed(#[source] pub(crate) wasip3::http_compat::Error);

/// The request body emitted trailer field(s) that `Trailer:` didn't name.
///
/// `wasi:http` honestly accepts trailers from `BodyWriter` (they go into
/// `result_writer` regardless of headers — see
/// `wasip3::http_compat::body_writer::send_http_body`), but it's measured
/// on a live host that the host's
/// HTTP/1.1 encoder silently drops them on the wire if the specific field
/// name wasn't declared in advance via `Trailer:` (RFC 9110 §6.5.1: a
/// receiver that chooses not to buffer the body is required to ignore
/// undeclared trailers). What needs comparing is the field NAMES
/// themselves, not just whether the `Trailer:` header is present. A `Trailer:
/// X-Other` header, declaring a different field than the one actually emitted
/// (`x-checksum`), loses data exactly as if the header were absent entirely —
/// measured:
/// `execute` returned `Ok(200)`, while the wire showed `0\r\n\r\n` with no
/// trailer.
///
/// **The error arrives after the fact**: by the time the caller sees this
/// error, the request has
/// already reached the server and gotten a response — the guard only
/// fires AFTER `race_send_with_body` has already succeeded, because
/// trailer field names, generally speaking, are only known once the body
/// has ended, and there's no predicting that before the headers. Don't
/// take this as a sign the request can be blindly retried: for a
/// non-idempotent request, that's a double send.
#[derive(Debug, thiserror::Error)]
#[error(
    "streaming request body emitted trailer field(s) [{}] that the request's \
     `Trailer:` header did not declare — wasi:http's HTTP/1.1 encoder drops undeclared \
     trailer fields silently. This error arrives after the request already reached the \
     server and was answered (race_send_with_body already succeeded) — do not retry \
     blindly, a non-idempotent request may already have taken effect.",
    .0.iter().map(http::HeaderName::as_str).collect::<Vec<_>>().join(", ")
)]
pub(crate) struct UndeclaredTrailers(pub(crate) Vec<http::HeaderName>);
