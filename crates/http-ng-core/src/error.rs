use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
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
    /// while the task was still queued (see `http_ng_rt::Cancelled`,
    /// returned by `Blocking::run`, vertical 2, Task 1, `amendment-C5`).
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
    /// `Blocking`, see `http-ng-dns-system`) — for the same reason `Resolve`
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
    source: Arc<dyn std::error::Error + Send + Sync + 'static>, // send-bound-exception: amendment-C1
}

impl Error {
    pub fn new<E>(kind: ErrorKind, source: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static, // send-bound-exception: amendment-C1
    {
        Self {
            kind,
            source: Arc::new(source),
        }
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

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.source)
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&*self.source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Src;
    impl std::fmt::Display for Src {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "boom")
        }
    }
    impl std::error::Error for Src {}

    #[test]
    fn preserves_kind_and_source_without_stringifying() {
        let e = Error::new(ErrorKind::Resolve, Src);
        assert_eq!(e.kind(), &ErrorKind::Resolve);
        // The source is available whole — not as a substring of the message.
        let src = std::error::Error::source(&e).unwrap();
        assert!(src.downcast_ref::<Src>().is_some());
    }

    #[test]
    fn is_clone_which_reqwest_error_is_not() {
        let e = Error::new(ErrorKind::Connect, Src);
        let c = e.clone();
        assert_eq!(c.kind(), &ErrorKind::Connect);
        // The clone must share the same source, not copy or lose it: the
        // source pointers of the original and the clone must match.
        let a = std::error::Error::source(&e).unwrap() as *const dyn std::error::Error;
        let b = std::error::Error::source(&c).unwrap() as *const dyn std::error::Error;
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

    // `Error: Send + Sync` (spec amendment-C1) — moved to
    // `crates/http-ng-core/tests/shape.rs` per amendment-C3: a bare
    // `fn _assert<T: Send + Sync>() {}` inside `src` matches the
    // `no-declared-send` guard's own pattern. Fix round 1 for Task 12
    // dropped this file's blanket exclusion from that guard in favour of
    // per-line `send-bound-exception` markers, which turned this
    // previously-shielded compile-time assertion into a false positive.
    // Relocating it (rather than marking it) shrinks the guard's blind
    // spot instead of growing it — the assertion needs zero exception
    // once it's not sharing a file with the two lines that actually are
    // the exception.
}
