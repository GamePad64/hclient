//! Deliberately empty: this crate exists only to hold the pair-property
//! test in `tests/pair_property.rs` — proof that the runtime capabilities
//! seam (Task 4 review, verdict item A) is real: the same client-shaped
//! code body runs on tokio and on smol without a single `#[cfg]`.
#![forbid(unsafe_code)]
