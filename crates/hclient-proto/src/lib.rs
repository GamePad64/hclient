//! Pure state machines for hclient's protocol layers.
//!
//! Crate invariant: no `async fn`, no runtime dependency, anywhere. Anything
//! that depends on time takes `now` as a parameter. Enforced in CI.
#![forbid(unsafe_code)]

pub mod backoff;
pub mod encode;
pub mod field;
pub mod happy_eyeballs;
pub mod head;
pub mod lines;
pub mod link;
pub mod redirect;
pub mod retry;
pub mod sse;
pub mod uri;
