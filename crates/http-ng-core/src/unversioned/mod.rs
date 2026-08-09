//! # Semver quarantine
//!
//! The traits in this module are the contract for backend and runtime
//! authors. It has not yet been validated against every backend, so:
//!
//! **Breaking changes in `unversioned` ship in a minor version, not a major.**
//!
//! This trick is borrowed from `ureq`. Without it, 1.0 is unshippable: you
//! can't freeze a trait without having checked it against native, wasi:http,
//! and fetch.

mod timer;
mod transport;

pub use timer::{Discard, Timer};
pub use transport::Transport;
