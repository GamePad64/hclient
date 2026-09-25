//! The sans-io half of the client: redirect and retry decisions, backoff,
//! the SSE and line decoders, `Link` headers, the header-field grammar and
//! URI parsing. No function here opens a socket, reads a clock or draws
//! entropy; `Client` drives these over real connections.
//!
//! Private: what a caller configures is re-exported from `redirect`,
//! `retry`, `link` and `sse` at the crate root, and nothing else here is a
//! promise.

// Maintainer notes (not rendered):
//
// These modules were `hclient-proto`'s until the owner's rule that that
// crate is internal and nothing is re-exported from it; `hclient` was the
// only crate using them, so they moved here whole, with their unit tests.
// The public types are declared here and re-exported by this crate's own
// public modules, which makes them `hclient`'s types rather than another
// crate's.

pub(crate) mod backoff;
pub(crate) mod field;
pub(crate) mod lines;
pub(crate) mod link;
pub(crate) mod redirect;
pub(crate) mod retry;
pub(crate) mod sse;
pub(crate) mod uri;
