//! The two ways this crate's codecs can refuse a caller's input.
//!
//! [`TemplateError`] is refused before anything is written: an RFC 9298
//! §2 URI template with no place to put the target host or port cannot
//! expand into a usable request path. [`CapsuleError`] is refused after
//! enough bytes have arrived to know something is wrong — a DATAGRAM
//! capsule whose value is too short to hold the context-id varint RFC
//! 9297 requires, or a capsule whose declared length cannot be
//! represented on this platform.

/// A URI template could not be expanded into a CONNECT-UDP request path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TemplateError {
    /// The template has no `{target_host}` variable to substitute.
    #[error("template has no {{target_host}} variable")]
    MissingHost,
    /// The template has no `{target_port}` variable to substitute.
    #[error("template has no {{target_port}} variable")]
    MissingPort,
}

/// A capsule (RFC 9297 §3.2) could not be decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CapsuleError {
    /// A DATAGRAM capsule's value was too short to hold the RFC 9298 §5
    /// context-id varint that must lead it.
    #[error("DATAGRAM capsule value is too short to hold a context id")]
    Malformed,
    /// The capsule's declared length does not fit in this platform's
    /// `usize`, so it can never be read in full.
    #[error("capsule length does not fit in usize")]
    TooLarge,
}

/// The proxy answered a CONNECT or CONNECT-UDP with something other than
/// `2xx`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the MASQUE proxy refused the tunnel with {status}")]
pub struct Refused {
    /// The status it answered.
    pub status: http::StatusCode,
}
