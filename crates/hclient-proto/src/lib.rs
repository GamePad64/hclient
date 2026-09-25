//! Pure state machines for hclient's protocol layers.
//!
//! Crate invariant: no `async fn`, no runtime dependency, anywhere. Anything
//! that depends on time takes `now` as a parameter. Enforced in CI.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod error;

pub mod backoff;
pub mod encode;
// `#[doc(hidden)]` and not a promise — the reason is the module's own
// first paragraph. A `///` here rather than a `//` would resolve that
// module's `//!` links in *this* scope instead of its own, which is the
// defect this workspace already paid for once when the jar and the cache
// became modules.
#[doc(hidden)]
pub mod field;
pub mod happy_eyeballs;
pub mod head;
pub mod lines;
pub mod link;
pub mod redirect;
pub mod retry;
pub mod sse;
pub mod uri;
