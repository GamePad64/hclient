//! The vocabulary every backend translates into, and the three refusals
//! no backend owns.
//!
//! This crate defines seams and implements none of them, so nothing here
//! is a failure that *happened* — there is no socket to reset and no
//! handshake to lose. [`Error`] and [`ErrorKind`] are the shape a backend
//! reports such a failure in, and the rest are the refusals made above
//! every backend, where the fact being refused is portable: a
//! [`Capabilities`](crate::Capabilities) field a transport does not have
//! ([`UnsupportedCapability`]), a
//! [`RequireVersion`](crate::RequireVersion) demand a connection cannot
//! meet ([`VersionNotAvailable`]), and a
//! [`RequestBody`](crate::RequestBody) whose factory contract was broken
//! ([`RewindTooDeep`]).
//!
//! **That is why they are here and not one per backend**, which the two
//! newer ones say in their own words: a caller downcasting on
//! `VersionNotAvailable` must not have to know which transport is
//! underneath, and `MAX_REWIND_DEPTH` exists because four backends
//! answered the same question three different ways. A refusal that is
//! portable has one type.
//!
//! Each is re-exported at the crate root, where it has always been, so no
//! consumer's `use` line moves.

use crate::body::MAX_REWIND_DEPTH;
use std::error::Error as StdError;
use std::fmt::Display;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Phase {
    /// Name resolution alone, which is a phase a caller can distinguish
    /// and a connector mostly cannot — see
    /// [`Timeouts::resolve`](crate::Timeouts::resolve) for what it bounds
    /// and why it is not
    /// [`Connect`](Self::Connect) minus the rest.
    Resolve,
    Connect,
    FirstByte,
    BetweenBytes,
    Total,
}

/// The error's category. Exists so the consumer doesn't have to classify
/// errors by substring-matching on `Display`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorKind {
    Resolve,
    Connect,
    Tls,
    Redirect,
    Timeout(Phase),
    Body,
    Decode,
    Status,
    Unsupported,
    /// The capability behind the failed operation was pulled out from under
    /// it before it could finish — typically, the runtime is shutting down
    /// while the task was still queued (see `hclient_rt::Cancelled`,
    /// returned by `Blocking::run`, `amendment-C5`).
    ///
    /// A separate variant, not `Other`: `Other` is the honest answer for a
    /// GENUINELY opaque backend error (the default `Transport::to_error`,
    /// when the backend has nothing to say about the category). Cancellation
    /// is the opposite of opacity: it's a condition known in advance and
    /// already typed (`Cancelled` is not a string or an OS error code, but a
    /// concrete type), one that EVERY future consumer of the `Blocking`
    /// capability will hit, not a one-off for a single backend. It must not
    /// be mixed with `Other`, and even less with the category of the failed
    /// operation itself (e.g. `Resolve` for a DNS resolver built on
    /// `Blocking`, see `hclient-dns-system`) — for the same reason `Resolve`
    /// and `Other` aren't mixed with each other: the caller must be able to
    /// tell "this attempt failed on its merits" from "this attempt didn't
    /// finish because the runtime is shutting down" without a downcast —
    /// just by comparing `kind()`.
    Cancelled,
    Other,
}

/// `Clone` is deliberate: reqwest's opaque, unclonable error is a source of
/// constant complaints (reqwest#1053).
///
/// `source` must be `Send + Sync` — the one documented exception to the
/// crate invariant "no declared `Send`/`Sync` bound anywhere." Without this
/// bound, `Arc<dyn Error>` erases the source's auto-traits, and `Error`
/// (and with it the future `Client::execute` returns) would be `!Send` for
/// every transport — `tokio::spawn(client.get(u).send())` would never
/// compile. All three v0.1 backends (hyper, wasi:http, browser fetch
/// without `target_feature = "atomics"`) already produce `Send + Sync`
/// errors, so this pins down a fact rather than adding a new restriction;
/// a transport with a fundamentally `!Send` error won't be able to use
/// this wrapper.
#[derive(Debug, Clone)]
pub struct Error {
    kind: ErrorKind,
    source: Arc<dyn StdError + Send + Sync + 'static>, // send-bound-exception: amendment-C1
    /// Whether the transport can say that **no byte of this request
    /// reached a server**.
    ///
    /// Not derivable from [`ErrorKind`], which is why it is a field.
    /// `Connect` looks like it should mean *nothing was sent* and does
    /// not: `hclient-native` classifies a response head that exceeds
    /// `H1Opts::max_headers` as `Connect` too, on hyper's own reasoning
    /// that nothing usable came off the connection — and that happens
    /// **after** the request went out. A retry deciding from the category
    /// alone would resend a request the server had already processed,
    /// which is precisely the guess this workspace exists not to make.
    ///
    /// So it is a claim a transport makes at a site where it knows, and
    /// **`false` is the default**: a backend that says nothing costs a
    /// caller a retry that did not happen, where a wrong `true` costs
    /// them a duplicated request. The understating value is the safe one,
    /// which is `reports_alpn`'s and `SUPPORTS_UNIX`'s rule one seam over.
    unsent: bool,
}

impl Error {
    pub fn new<E>(kind: ErrorKind, source: E) -> Self
    where
        E: StdError + Send + Sync + 'static, // send-bound-exception: amendment-C1
    {
        Self {
            kind,
            source: Arc::new(source),
            unsent: false,
        }
    }

    /// Marks this failure as one where no byte of the request reached a
    /// server.
    ///
    /// Only a transport may say so, and only where it knows — a connect
    /// that never completed, a name that did not resolve, a handshake
    /// that failed, or a dispatcher handing the request back unsent. See
    /// the field's own doc for why `ErrorKind` cannot answer this.
    #[must_use]
    pub fn unsent(mut self) -> Self {
        self.unsent = true;
        self
    }

    /// Whether the transport said no byte of the request reached a
    /// server.
    ///
    /// `false` means *not known to be unsent* rather than *known to have
    /// been sent* — see [`Self::unsent`].
    #[must_use]
    pub fn is_unsent(&self) -> bool {
        self.unsent
    }

    pub fn kind(&self) -> &ErrorKind {
        &self.kind
    }
    pub fn is_timeout(&self) -> bool {
        matches!(self.kind, ErrorKind::Timeout(_))
    }
    pub fn is_redirect(&self) -> bool {
        matches!(self.kind, ErrorKind::Redirect)
    }
    pub fn is_connect(&self) -> bool {
        matches!(self.kind, ErrorKind::Connect)
    }
    pub fn is_unsupported(&self) -> bool {
        matches!(self.kind, ErrorKind::Unsupported)
    }
    pub fn is_cancelled(&self) -> bool {
        matches!(self.kind, ErrorKind::Cancelled)
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.source)
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(&*self.source)
    }
}

/// A [`RequestBody::Rewindable`](crate::RequestBody::Rewindable) whose factory kept handing back another
/// `Rewindable`, past [`MAX_REWIND_DEPTH`](crate::MAX_REWIND_DEPTH).
///
/// The factory contract is being broken rather than a legitimate shape
/// being refused — but a broken contract that overflows the stack is worse
/// than one that returns this.
#[derive(Debug, thiserror::Error)]
#[error(
    "RequestBody::Rewindable factory nested more than {MAX_REWIND_DEPTH} levels deep \
     (each factory call returned another Rewindable instead of a terminal body)"
)]
#[non_exhaustive]
pub struct RewindTooDeep;

/// A [`RequireVersion`](crate::RequireVersion) demand the connection in hand does not satisfy.
///
/// Carries both halves, because "HTTP/2 was required" and "HTTP/1.1 is
/// what this connection negotiated" are separately actionable — the first
/// is the caller's own request coming back, the second is a fact about the
/// server or the TLS configuration.
///
/// One type in this crate rather than one per backend (the shape
/// `hclient_h3::RequestTrailersNotSent` takes), because a caller
/// downcasting on it must not have to know which transport is underneath:
/// the demand is portable, so its refusal is too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "the request required {required:?} and this connection negotiated {negotiated:?}; \
     it was refused before the head was written"
)]
pub struct VersionNotAvailable {
    pub required: http::Version,
    pub negotiated: http::Version,
}

/// A setting the chosen transport cannot honor.
///
/// Returned from `build()` rather than silently ignored. The model is
/// wasi:http itself, whose setters return `request-options-error::not-supported`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("backend `{backend}` does not support `{what}`")]
pub struct UnsupportedCapability {
    pub what: &'static str,
    pub backend: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as StdError;
    use std::fmt::Display;

    #[derive(Debug)]
    struct Src;
    impl Display for Src {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "boom")
        }
    }
    impl StdError for Src {}

    #[test]
    fn preserves_kind_and_source_without_stringifying() {
        let e = Error::new(ErrorKind::Resolve, Src);
        assert_eq!(e.kind(), &ErrorKind::Resolve);
        // The source is available whole — not as a substring of the message.
        let src = StdError::source(&e).unwrap();
        assert!(src.downcast_ref::<Src>().is_some());
    }

    #[test]
    fn is_clone_which_reqwest_error_is_not() {
        let e = Error::new(ErrorKind::Connect, Src);
        let c = e.clone();
        assert_eq!(c.kind(), &ErrorKind::Connect);
        // The clone must share the same source, not copy or lose it: the
        // source pointers of the original and the clone must match.
        let a = StdError::source(&e).unwrap() as *const dyn StdError;
        let b = StdError::source(&c).unwrap() as *const dyn StdError;
        assert!(std::ptr::eq(a, b));
    }

    #[test]
    fn predicates_agree_with_kind() {
        assert!(Error::new(ErrorKind::Timeout(Phase::Connect), Src).is_timeout());
        assert!(Error::new(ErrorKind::Redirect, Src).is_redirect());
        assert!(Error::new(ErrorKind::Connect, Src).is_connect());
        assert!(!Error::new(ErrorKind::Body, Src).is_connect());
        assert!(Error::new(ErrorKind::Unsupported, Src).is_unsupported());
        assert!(!Error::new(ErrorKind::Body, Src).is_unsupported());
        assert!(Error::new(ErrorKind::Cancelled, Src).is_cancelled());
        // Cancellation is neither a DNS failure nor an opaque "other"
        // error: both checks are needed — either alone would be
        // insufficient to catch a regression that confused `Cancelled`
        // with either of these two neighbors.
        assert!(!Error::new(ErrorKind::Resolve, Src).is_cancelled());
        assert!(!Error::new(ErrorKind::Other, Src).is_cancelled());
    }

    // `Error: Send + Sync` (amendment-C1) is asserted in
    // `crates/hclient-core/tests/shape.rs`, not here: a bare
    // `fn _assert<T: Send + Sync>() {}` inside `src` is exactly what the
    // `no-declared-send` guard's pattern matches, so the assertion would
    // need a `send-bound-exception` marker of its own. Outside `src` it
    // needs none, which keeps the guard's blind spot as small as the two
    // lines that genuinely are exceptions.
}
