//! The one thing this crate can refuse.
//!
//! **A module for a single type**, which is worth a sentence because the
//! count is the finding rather than an argument against the split: this
//! crate holds a seam and the RFC 9460 rules over an already-decoded
//! record, and neither reads a byte, opens a socket or calls an OS API.
//! Every other way name resolution fails belongs to a backend, and each
//! of them has a large enum of its own to say so. So the whole of what
//! *this* crate can refuse is one statement about a record's own
//! self-consistency — and a reader who wants to know that reads a
//! twenty-line file rather than finding it between the parameter decoder
//! and the endpoint builder.
//!
//! [`SvcbRecordError`] is re-exported from [`crate::svcb`], where it has
//! always been, so no consumer's `use` line moves.

/// The one way a well-decoded record can still be malformed as a *record*.
///
/// A single-variant enum rather than a bare `u16`, and its own type rather
/// than a variant of some backend's error: every backend that decodes SVCB
/// has a large error enum of its own describing how its transport can
/// fail, and none of those failures can happen here — this function reads
/// no bytes, opens no socket and calls no OS API. Each backend maps this
/// into its own enum at the call site (`hclient-dns-system` into
/// `SvcbLookupError::MandatoryKeyAbsent`, `hclient-dns-doh` into
/// `DohError::MandatoryKeyAbsent`), which keeps their public taxonomies
/// unchanged by the move.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SvcbRecordError {
    /// RFC 9460 §8: the record's `mandatory` list names a key the record
    /// does not actually carry. Checked here rather than by a decoder,
    /// because it is a statement about the record as a whole and not about
    /// any one parameter's encoding.
    #[error("SvcParamKey {key} is listed as mandatory but is not present in the record")]
    MandatoryKeyAbsent { key: u16 },
}
