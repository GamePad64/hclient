//! Two types for one fact: what the OS said, and where it said it.
//!
//! Everything this crate can report is WinHTTP's own answer. [`Win32Error`]
//! is the code and nothing else, and [`WinHttpError`] is the place it came
//! from — a named call, an asynchronous completion, a TLS check, a step
//! the state machine did not expect. Neither is a translation: mapping the
//! `ERROR_WINHTTP_*` range onto this workspace's `ErrorKind` variant by
//! variant would be a second vocabulary invented at the boundary, which is
//! what `hclient-fetch` and `hclient-urlsession` both refuse to do, and
//! `session.rs` maps only the handful whose kind is unambiguous.
//!
//! **The split between them is the one thing worth reading twice.** A
//! Win32 code says *what* failed and never *what was being attempted*, and
//! on an asynchronous API those are far apart: the code arrives on a
//! callback, sometimes for a call made three steps earlier. So the
//! surrounding variant carries the call's own name, spelled as WinHTTP's
//! documentation spells it, which is the half a reader can act on.
//!
//! [`WinHttpError`] is re-exported at the crate root, where it has always
//! been, so no consumer's `use` line moves. [`Win32Error`] keeps exactly
//! the reach it had: named in a public field, and not exported.

/// What WinHTTP said went wrong, as a Win32 error code.
///
/// The code and nothing more: WinHTTP's own `FormatMessage` text needs
/// `winhttp.dll` loaded as a message source, and mapping the codes onto
/// this workspace's `ErrorKind` at this layer would be a second
/// vocabulary invented at the boundary — the same reason
/// `hclient-urlsession` reports what Apple said rather than a translation
/// of it. `session.rs` maps the handful that have an unambiguous kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("WinHTTP error {0}")]
pub struct Win32Error(pub u32);

/// What WinHTTP said went wrong.
///
/// The Win32 code is carried rather than translated. `FormatMessage`
/// would give a sentence in the machine's own language, which is a
/// different thing from a code a reader can look up — and mapping the
/// `ERROR_WINHTTP_*` range onto this workspace's [`ErrorKind`](hclient_core::ErrorKind) variant by
/// variant would be a second vocabulary invented at the boundary, which
/// is what `hclient-fetch` and `hclient-urlsession` both refuse to do.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WinHttpError {
    /// A synchronous WinHTTP call returned failure.
    #[error("`{call}` failed: {source}")]
    Call {
        /// The WinHTTP function, spelled as the documentation spells it.
        call: &'static str,
        /// `GetLastError` immediately afterwards.
        source: Win32Error,
    },
    /// The request failed asynchronously: `WINHTTP_CALLBACK_STATUS_
    /// REQUEST_ERROR`, carrying `WINHTTP_ASYNC_RESULT::dwError`.
    #[error("the request failed: {0}")]
    Request(Win32Error),
    /// A TLS failure, with `WINHTTP_CALLBACK_STATUS_SECURE_FAILURE`'s own
    /// flags — which say *which* check failed where the error code that
    /// follows says only that one did.
    #[error("TLS failed; WinHTTP's SECURE_FAILURE flags were {0:#010x}")]
    Tls(u32),
    /// A completion arrived for a call this crate had not made.
    ///
    /// Reported rather than ignored: WinHTTP's asynchronous model is a
    /// sequence, and a step out of order means the state machine here
    /// disagrees with the one in the OS. Continuing would mean guessing.
    #[error("WinHTTP reported `{got}` where `{expected}` was expected")]
    OutOfOrder {
        /// What arrived.
        got: &'static str,
        /// What this crate was waiting for.
        expected: &'static str,
    },
    /// The head WinHTTP handed back is not one this workspace's RFC 9112
    /// §4 parser accepts.
    #[error("the response head did not parse: {0}")]
    Head(#[from] hclient_proto::head::HeadError),
    /// The request cannot be expressed to WinHTTP at all.
    #[error("{0}")]
    Unsupported(String),
}
