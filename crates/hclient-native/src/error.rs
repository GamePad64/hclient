//! Every way this transport refuses, or fails to make, an exchange.
//!
//! **One file, and it is ordered by *when* rather than by which module
//! raises it.** Grouping by subsystem was the other option and it would
//! have rebuilt the thing this move undoes: the errors were already
//! grouped by subsystem — that is what having them in `connect.rs`,
//! `http1.rs`, `proxy.rs` and six more files *was* — and a reader who
//! knows which module to look in did not need the move. What no file
//! could answer before is the question a reader actually arrives with,
//! *how far did my request get before this*, and the answer is an order:
//! a configuration refused before a socket exists, a request with nowhere
//! to go, a connection that was not made, a clock that won, an exchange
//! that broke. Each section below is one of those, and a type's position
//! in the file is a claim about how much of the request had happened.
//!
//! That order is also what makes the near-duplicates readable.
//! [`ResolveTimedOut`], [`ConnectTimedOut`], [`FirstByteTimedOut`] and
//! [`BetweenBytesElapsed`] are one type written four times over, and the
//! only thing that distinguishes them is which phase's bound was in
//! force — which is exactly why they are four types and not one with a
//! `Phase` field: a caller tells them apart with
//! `Error::source().downcast_ref()`. Side by side that reads as a
//! design; scattered across `connect.rs`, `lib.rs` and `idle.rs` it read
//! as four unrelated timeouts.
//!
//! **The two protocol arms keep their own**, and that is the one line
//! drawn against this file. `http2/` and `http3/` are self-contained
//! stacks behind features of their own — the second was a crate of its
//! own until `f4dfe48` — and their failures are about frames and streams
//! rather than about this transport's phases: `http2/error.rs` and
//! `http3/pump.rs` say what `h2` and `h3` mean, which is a different
//! subject from the one above. The rule is mechanical rather than a
//! judgement, so it cannot drift: a type stays where it is if and only if
//! it lives under `src/http2/` or `src/http3/`.
//!
//! No consumer's `use` line moves — each type is re-exported at the path
//! it already had, including `crate::caps::Disagreement` and the two the
//! `proxy` module publishes under its own name as well as at the root.
//! **Nothing became public that was not**, and the whole of what the move
//! cost is one step of widening on what now crosses a file boundary:
//! seven types that were private to their module are `pub(crate)`, with
//! `pub(crate)` fields wherever the module they left still constructs
//! them, and three methods — `ResolveErrors::{from_families,
//! distinguishing_error}` and `Disagreement::new` — for the same reason.
//! `UndeclaredRequestTrailers`'s field is the one on an already-public
//! type, and it is `pub(crate)` rather than `pub`: the accessor
//! [`UndeclaredRequestTrailers::fields`] is still how a caller reads it.

use crate::http1::MINIMUM_MAX_BUF_SIZE;
use hclient_core::{Error, ErrorKind};
use http::HeaderName;
use std::fmt::Debug;
use std::time::Duration;

// ---------------------------------------------------------------------
// Configuration this transport will not accept, refused at the line the
// caller wrote rather than on the first request that trips over it.
// ---------------------------------------------------------------------

/// Turning off the last version this transport could speak.
///
/// Raised by whichever of [`crate::Native::http1`]/[`crate::Native::http2`] would leave
/// nothing, so the refusal is local to the call that caused it rather than
/// deferred to `build()` — the caller learns it at the line they wrote.
#[derive(Debug, thiserror::Error)]
#[error(
    "this would leave the transport unable to speak any HTTP version: \
     `http1` and `http2` cannot both be off"
)]
pub struct NoVersionsLeft;

/// Asking for HTTP/2 in a build that did not compile it.
///
/// A named refusal rather than a silent `false`: the fix is a cargo
/// feature, which is not something a caller can guess from a request that
/// quietly went out over HTTP/1.1.
#[derive(Debug, thiserror::Error)]
#[error("`http2(true)` needs `hclient-native`'s `http2` feature, which this build does not have")]
pub struct Http2NotCompiledIn;

/// A [`crate::H1Opts`] value hyper would refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("max_buf_size is {asked}, below hyper's minimum of {MINIMUM_MAX_BUF_SIZE}")]
pub struct MaxBufSizeTooSmall {
    pub asked: usize,
}

/// A proxy and a Unix socket both answer *where does this connection go*,
/// and a precedence rule between them would be one nobody could guess.
#[derive(Debug, thiserror::Error)]
#[error(
    "a proxy and a Unix socket both answer `where does this connection go`; configure at most one"
)]
pub struct ProxyAndUnixSocket;

/// The requested `HeConfig`'s `attempt_delay` is outside the RFC 8305
/// recommended range. `Scheduler::new` silently clamps such a
/// value, because its signature is fixed by the task's interface — `Self`,
/// not `Result`. THIS module's signature isn't fixed by anything, so here
/// it's a typed error rather than the same silent clamp two layers down.
#[derive(Debug, thiserror::Error)]
#[error(
    "attempt_delay {requested:?} is outside the RFC 8305 recommended range and would be \
     silently clamped to {effective:?}; pass a value inside the range instead"
)]
pub(crate) struct InvalidHeConfig {
    pub(crate) requested: Duration,
    pub(crate) effective: Duration,
}

/// Two stacks that cannot be given one honest answer for one field.
///
/// Returned from [`crate::caps::combine`], and therefore from
/// [`Selecting::new`](crate::Native::new) — the same shape as
/// `UnsupportedCapability` at `ClientBuilder::build()`, and for the same
/// reason: the error arrives where the mistake was made, rather than as a
/// surprise on the first request that happens to take the other stack.
#[cfg(feature = "http3")]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "the two stacks disagree on `{field}`, and neither value is true of the pair: the TCP stack says `{tcp}`, the QUIC stack says `{quic}`"
)]
#[non_exhaustive]
pub struct Disagreement {
    /// The `Capabilities` field, by its own name.
    pub field: &'static str,
    /// What `hclient-native` said, formatted with `Debug`.
    pub tcp: String,
    /// What `hclient-h3` said, formatted with `Debug`.
    pub quic: String,
}

#[cfg(feature = "http3")]
impl Disagreement {
    pub(crate) fn new<V: Debug>(field: &'static str, tcp: &V, quic: &V) -> Self {
        Self {
            field,
            tcp: format!("{tcp:?}"),
            quic: format!("{quic:?}"),
        }
    }
}

// ---------------------------------------------------------------------
// A request with nowhere to go: the URI, the scheme, or a name this
// transport cannot resolve into a configuration it holds.
// ---------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
#[error("request URI has no host to connect to")]
pub(crate) struct UriError;

#[derive(Debug, thiserror::Error)]
#[error("unsupported URI scheme: {0:?}")]
pub(crate) struct UnsupportedScheme(pub(crate) String);

/// A plaintext request on a transport that has forbidden HTTP/1.1.
///
/// `http://` carries no ALPN, so HTTP/2 there needs prior knowledge (RFC
/// 9113 §3.4) and this transport does not do it. Serving the request over
/// HTTP/1.1 anyway would ignore the setting; serving it at all would make
/// [`Capabilities::full_duplex`](hclient_core::Capabilities::full_duplex) wrong,
/// since [`crate::Native::http1`] raises
/// that floor on the guarantee that no connection here speaks HTTP/1.1.
#[derive(Debug, thiserror::Error)]
#[error("`http://` needs HTTP/1.1, which this transport has been told not to speak")]
pub struct PlaintextNeedsHttp1;

/// A request named a client identity this TLS backend has not got.
///
/// A refusal rather than a fallback: connecting with the default identity
/// is how one tenant's certificate reaches another tenant's server, and
/// nothing at the call site would show it happened.
#[derive(Debug, thiserror::Error)]
#[error("no client identity named `{0}` in this TLS backend")]
pub(crate) struct UnknownClientIdentity(pub(crate) String);

/// Routing chose the QUIC arm on a transport that has none.
///
/// Unreachable through `Native::route`, which only ever chooses QUIC
/// after finding an arm — it exists because `over_quic` is also reached
/// from the hedge, and an `unreachable!` in a transport is a panic in a
/// caller's process for a mistake that is ours.
#[cfg(feature = "http3")]
#[derive(Debug, thiserror::Error)]
#[error("this transport has no QUIC arm; see `Native::http3`")]
pub struct NoQuicArm;

// ---------------------------------------------------------------------
// The connection was never made. "No silent no-ops": none of the sites
// that raise these collapse a failure into `AllAttemptsFailed`/
// `ErrorKind::Connect` silently — every distinction (the resolver failed
// / the resolver honestly found zero addresses / TCP attempts genuinely
// happened and all failed) stays visible through a separate type and a
// separate `ErrorKind`.
// ---------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
#[error("all {0} connection attempts failed")]
pub(crate) struct AllAttemptsFailed(pub(crate) usize);

/// No address arrived for either family — and THIS DISTINGUISHES whether
/// the cause was the resolver failing, or the resolver honestly finishing
/// and finding zero records (e.g. `NXDOMAIN`). Collapsing both cases into
/// `AllAttemptsFailed(0)` would be exactly the "resolver error becomes
/// 'no addresses'" this type exists to prevent: it would read as "zero
/// TCP attempts failed," even though there was no TCP attempt at all —
/// not because none were tried, but because there was nothing to try.
/// [`Neither`](Self::Neither) is that second case, named rather than
/// spelled `(None, None)`.
///
/// **Four variants rather than two `Option<Error>` fields — because of
/// `source()`, not for tidiness.** The chain here has to lead to the
/// first family that actually failed, whichever one that is; as a struct
/// that is `v6.or(v4)`, and `thiserror` has no way to say it: `#[source]`
/// marks one field, so marking `v6` would end the chain at `None`
/// whenever only ipv4 failed — a truncation that changes no message and
/// breaks no test written about one. Split into variants, each carries
/// exactly the errors that exist in that case, `#[source]` names the
/// right one in each, and the case with no cause at all has no `#[source]`
/// because there is genuinely nothing to point at.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ResolveErrors {
    #[error("ipv6 lookup failed ({v6}); ipv4 lookup failed ({v4})")]
    Both {
        #[source]
        v6: Error,
        v4: Error,
    },
    #[error("ipv6 lookup failed ({v6}); ipv4 lookup returned no addresses")]
    Ipv6 {
        #[source]
        v6: Error,
    },
    #[error("ipv4 lookup failed ({v4}); ipv6 lookup returned no addresses")]
    Ipv4 {
        #[source]
        v4: Error,
    },
    #[error("resolver returned no addresses for either address family")]
    Neither,
}

impl ResolveErrors {
    /// Whatever each family recorded, if anything — the shape `drive`
    /// accumulates in, folded into the variant that describes it.
    pub(crate) fn from_families(v6: Option<Error>, v4: Option<Error>) -> Self {
        match (v6, v4) {
            (Some(v6), Some(v4)) => Self::Both { v6, v4 },
            (Some(v6), None) => Self::Ipv6 { v6 },
            (None, Some(v4)) => Self::Ipv4 { v4 },
            (None, None) => Self::Neither,
        }
    }

    /// The errors this variant recorded, ipv6 first — the same order
    /// `source()` follows, so the two cannot drift apart.
    fn recorded(&self) -> [Option<&Error>; 2] {
        match self {
            Self::Both { v6, v4 } => [Some(v6), Some(v4)],
            Self::Ipv6 { v6 } => [Some(v6), None],
            Self::Ipv4 { v4 } => [None, Some(v4)],
            Self::Neither => [None, None],
        }
    }

    /// The first recorded resolve error (from either family) whose
    /// `kind()` is NOT `ErrorKind::Resolve`. Wrapping any resolve error
    /// (when `launched == 0`) in a fresh `Error::new(ErrorKind::Resolve,
    /// errs)` and not reading `errs` at all when `launched > 0` erases
    /// `ErrorKind::Cancelled` — the background thread pool shutting down
    /// before the resolve finished — indistinguishably from "this name
    /// doesn't resolve." `Cancelled` is the case that was found,
    /// but the rule is general: ANY `kind()` other than the `Resolve`
    /// this module synthesizes itself carries information the connector
    /// didn't produce and has no right to rename. Called BEFORE both
    /// failure branches in `drive`'s `HeAction::Exhausted`, so neither
    /// `AllAttemptsFailed` nor the synthetic `ErrorKind::Resolve` is
    /// reachable without going through this check — discarding becomes
    /// structurally impossible, not merely handled for the one case that
    /// was found.
    pub(crate) fn distinguishing_error(&self) -> Option<&Error> {
        self.recorded()
            .into_iter()
            .flatten()
            .find(|e| e.kind() != &ErrorKind::Resolve)
    }
}

/// The proxy sent bytes past the end of its own handshake.
///
/// **The transport's rule rather than any protocol's**, which is why it
/// lives here and not in `hclient-proxy`: a handshake reports faithfully
/// how much of the buffer was its own, and what to make of the rest is a
/// question about what happens next. Nothing the origin might say can have
/// arrived yet — the client has not written to it — so these bytes are the
/// proxy's, and carrying them on would feed them to the TLS handshake, or
/// to hyper, as if the origin had sent them. A refusal to connect rather
/// than a rewind, because the rewind is the quieter failure and the worse
/// one.
#[derive(Debug, thiserror::Error)]
#[error("the proxy sent {0} bytes past its own handshake, before anything was sent to the origin")]
pub struct ProxySpokeFirst(pub usize);

// ---------------------------------------------------------------------
// The clock won. Four types rather than one with a `Phase` field,
// because a caller tells them apart by downcasting — and because each
// names the bound that was actually in force, which no message can be
// relied on to carry.
// ---------------------------------------------------------------------

/// The failure `first_address_within` ends in.
///
/// A named type rather than a string, for the reason
/// [`crate::FirstByteTimedOut`] is one: a caller tells the phases apart
/// with `Error::source().downcast_ref()`, and the point of this bound is
/// that *"DNS is broken"* and *"the origin is unreachable"* stop looking
/// alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("no address from the resolver within the resolve timeout of {0:?}")]
pub struct ResolveTimedOut(pub Duration);

/// The failure [`crate::with_connect_timeout`] ends in when the timer wins the
/// race against `connect::connect`.
#[derive(Debug, thiserror::Error)]
#[error("connect timed out after {0:?}")]
pub(crate) struct ConnectTimedOut(pub(crate) Duration);

/// The failure `Native`'s `first_byte` gate ends in when the timer wins
/// the race against the exchange.
///
/// A named type rather than a string, for the same reason
/// `ConnectTimedOut` is one: a caller must be able to tell the phases
/// apart with `Error::source().downcast_ref()`, and to read the bound
/// that was actually in force.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("no response head within the first_byte timeout of {0:?}")]
pub struct FirstByteTimedOut(pub Duration);

/// The source of an [`ErrorKind::Timeout`]`(`[`Phase::BetweenBytes`](hclient_core::Phase::BetweenBytes)`)`.
///
/// A named type rather than a string, for the same reason
/// `hclient::error::TotalTimeoutElapsed` is one: a caller must be able to tell
/// this apart from any other timeout with
/// `Error::source().downcast_ref()`, and to read the bound that was
/// actually in force rather than parse it out of a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the response body sent nothing for {0:?}, its between_bytes timeout")]
pub struct BetweenBytesElapsed(pub Duration);

// ---------------------------------------------------------------------
// The exchange broke. Everything here happened on a connection that
// existed, which is what separates the first two from the connect
// failures above: a caller reading one of these knows a socket was open.
// ---------------------------------------------------------------------

/// A pooled connection that turned out to be finished at the last moment
/// before its request was handed to hyper — the server closed it while
/// nothing was polling it, and the checkout poll one instant earlier had
/// not seen it yet.
#[derive(Debug, thiserror::Error)]
#[error("the pooled connection was closed before the request was sent")]
pub(crate) struct ConnectionWentAwayBeforeTheRequest;

/// The residual race the pool cannot close: the connection ended between
/// the request being handed to hyper and hyper writing it. Named rather
/// than folded into a generic connect error, because a caller reading it
/// should be able to tell it apart from a connect that never happened.
#[derive(Debug, thiserror::Error)]
#[error("the connection ended while the request was still queued on it")]
pub(crate) struct ConnectionEndedWithTheRequestQueued;

#[derive(Debug, thiserror::Error)]
#[error("the server answered {0} rather than 101 Switching Protocols")]
pub struct NotSwitchingProtocols(pub http::StatusCode);

#[derive(Debug, thiserror::Error)]
#[error("the connection ended before the handshake response arrived")]
pub struct EndedBeforeTheResponse;

/// The request body carried trailer field(s) the request never declared,
/// on a connection speaking HTTP/1.1.
///
/// **Some of the request may already have gone**, and the error says so
/// rather than leaving a caller to assume otherwise. How much is a fact
/// about the caller's own body, measured both ways in
/// `tests/request_trailers.rs`: a body that pends between its last data
/// frame and its trailers — the shape of any real streaming producer —
/// has had the head and every preceding chunk flushed to the socket by
/// then, while one that answers `Ready` throughout is drained inside a
/// single `Dispatcher::poll_write` and dies with the head still in
/// hyper's write buffer, leaving the server with a connection and no
/// request at all.
///
/// What the refusal prevents in both cases is the *last-chunk marker*:
/// the message is aborted instead of completed without the caller's
/// trailers, so no server ever treats it as a well-formed request whose
/// trailers happened to be absent. A server that did receive the prefix
/// may still have acted on it, so this is not a signal to retry blindly.
///
/// The fix is the caller's and is one header: `Trailer:` naming each field
/// the body will emit (RFC 9110 §6.6.2, and hyper's
/// `proto/h1/encode.rs` enforces it). The same request over HTTP/2 needs
/// no such header and is unaffected — which is why
/// [`Capabilities::request_trailers`](hclient_core::Capabilities::request_trailers)
/// is `true` for this transport: it sends them on both protocols it
/// speaks, and a request that omits the declaration HTTP/1.1 requires is
/// malformed rather than unsupported.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "the request body emitted trailer field(s) [{}] that the request's `Trailer:` header did \
     not declare, and this connection speaks HTTP/1.1, where hyper's encoder drops an \
     undeclared trailer field silently (RFC 9110 §6.6.2) — send `Trailer: {}` with the \
     request head. The message was aborted rather than finished without them, so the server \
     never saw a complete request; how much of it had already been flushed depends on \
     whether the body pended before its trailers, and a non-idempotent request that had may \
     already have taken effect — do not retry blindly",
    .0.iter().map(HeaderName::as_str).collect::<Vec<_>>().join(", "),
    .0.iter().map(HeaderName::as_str).collect::<Vec<_>>().join(", "),
)]
pub struct UndeclaredRequestTrailers(pub(crate) Vec<HeaderName>);

impl UndeclaredRequestTrailers {
    /// The field names that were emitted and not declared, in the order
    /// they appeared in the trailers frame.
    pub fn fields(&self) -> &[HeaderName] {
        &self.0
    }
}
