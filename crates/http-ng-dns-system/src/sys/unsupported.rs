//! The honest `false`, for every target where a system SVCB lookup has not
//! been established to work.
//!
//! `lookup` here is not `unimplemented!()` and not a panic: it returns an
//! empty result, so a caller that ignores `SUPPORTS_SVCB` and calls it
//! anyway gets the documented shape of an absent capability rather than a
//! crash — and critically not an error either, which would tell that
//! caller its DNS is broken when the truth is only that this build has no
//! backend.
#![forbid(unsafe_code)]

use super::SvcbLookupError;
use http_ng_dns::SvcbEndpoint;

pub(crate) const SUPPORTS_SVCB: bool = false;

pub(crate) fn lookup(_name: &str) -> Result<Vec<SvcbEndpoint>, SvcbLookupError> {
    Ok(Vec::new())
}
