//! What an in-process app can refuse, and what its body can lose.
//!
//! Both belong to `AppTransport` rather than to the `tower` adapter, and
//! that is the whole of what they share: a `Service` driven over a real
//! socket has an origin and a body error type of its own, where an app
//! served in process has neither until this crate supplies them.

/// A response body failed, with the service's own message.
#[derive(Debug)]
pub(crate) struct BodyFailure(pub(crate) String);

/// A request named an authority this transport does not serve.
#[derive(Debug)]
pub struct WrongAuthority {
    pub expected: String,
    pub actual: String,
}

impl core::fmt::Display for BodyFailure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BodyFailure {}

impl core::fmt::Display for WrongAuthority {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "this app transport serves `{}`; the request named `{}`",
            self.expected, self.actual
        )
    }
}

impl std::error::Error for WrongAuthority {}
