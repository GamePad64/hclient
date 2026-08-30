//! What an in-process app can refuse, and what its body can lose.
//!
//! Both belong to `AppTransport` rather than to the `tower` adapter, and
//! that is the whole of what they share: a `Service` driven over a real
//! socket has an origin and a body error type of its own, where an app
//! served in process has neither until this crate supplies them.

/// A response body failed, with the service's own message.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub(crate) struct BodyFailure(pub(crate) String);

/// A request named an authority this transport does not serve.
#[derive(Debug, thiserror::Error)]
#[error("this app transport serves `{expected}`; the request named `{actual}`")]
pub struct WrongAuthority {
    pub expected: String,
    pub actual: String,
}
