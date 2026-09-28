//! An unpublished experiment: MASQUE — RFC 9298 CONNECT-UDP, and CONNECT
//! over h2/h3 — built as an `hclient-proxy` egress filter, to prove that
//! crate's datagram and tunnel seams against a proxy protocol neither
//! seam was written against.
//!
//! This crate never reaches crates.io. It exists to answer one question
//! by building the thing rather than arguing about it: can a proxy
//! protocol this workspace did not have in mind when it designed
//! `EgressFilter`, `Dial` and the datagram path actually be written as
//! one, with no seam changed underneath it? What is here so far is the
//! codecs a MASQUE implementation needs before it can dial anything —
//! [`template`] expands RFC 9298 §2's URI template, and [`capsule`] reads
//! and writes RFC 9297 §3.2's capsules, over [`capsule::varint`]'s RFC
//! 9000 §16 varints. The filter itself, and the seams it exercises, are
//! not built yet.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod capsule;
mod error;
pub mod template;

pub use error::{CapsuleError, TemplateError};
