//! The one thing this backend refuses that rustls does not.
//!
//! A single type, and a module for it anyway: this workspace keeps a
//! crate's errors in `error.rs`, and a convention with an exception for
//! "only one" is a convention nobody can check. What it says is narrow and
//! worth its own file — every other failure here is rustls' own, arriving
//! through `ErrorKind::Tls` with rustls' text.

/// A client identity this backend was asked for and does not have.
///
/// Reachable only by a consumer that calls this backend directly:
/// `hclient-native` resolves every label through
/// [`TlsIdentity::config_id_for`] before it opens a socket and refuses
/// there, naming the label. Kept private because it is a guard rather
/// than a case a caller distinguishes — the message reaches them through
/// `Error`'s source chain either way.
#[derive(Debug)]
pub(crate) struct UnknownIdentity(pub(crate) String);

impl std::fmt::Display for UnknownIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "no client identity is registered under the name `{}`: refusing rather than \
             connecting with the default one",
            self.0
        )
    }
}

impl std::error::Error for UnknownIdentity {}
