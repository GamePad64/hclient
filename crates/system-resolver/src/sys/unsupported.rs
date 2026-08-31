//! No backend on this target.
//!
//! An honest absence rather than a stub that pretends: [`support`] is
//! [`Support::None`], so a caller can see there is nothing here before it
//! spends a lookup, and [`query`] refuses with a reason that does not look
//! like a network failure.
//!
//! [`Support::None`]: crate::Support::None

use crate::error::Error;
use crate::{Record, Support};

pub(crate) fn support() -> Support {
    Support::None
}

/// Unreachable through [`crate::lookup`], which checks [`support`] first.
/// Written anyway, because the module has to define the pair and a
/// `unreachable!()` in a resolver costs a process where an error costs a
/// line.
pub(crate) fn query(_name: &str, _rtype: u16) -> Result<Vec<Record>, Error> {
    Err(Error::Unsupported)
}
