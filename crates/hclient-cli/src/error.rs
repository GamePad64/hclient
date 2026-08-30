//! The two things `hc` refuses before it makes a request.
//!
//! Both are **refusals with their own exit code**, which is what a command
//! line owes a script: a backend this build does not carry is not an
//! unreachable server, and a `%{...}` this build does not know is not a
//! failed request. Everything else `hc` reports is the library's error or
//! the server's status, and neither is ours to name.

use crate::args::BackendName;
use crate::backend::available_list;
use crate::timings::KNOWN;

#[derive(Debug, thiserror::Error)]
pub enum Refused {
    /// Named a backend this build does not carry. Never a fallback.
    ///
    /// The message names what the build *does* carry, so the list is an
    /// expression rather than a field: it is a fact about the build and
    /// there is nothing for a constructor to pass.
    #[error(
        "this build of hc has no `{}` backend.\n\nIt carries: {}\n\nA backend is refused \
         rather than silently replaced, which is the one thing this tool promises over curl's \
         `CURL_SSL_BACKEND`.",
        .0,
        available_list()
    )]
    NotCompiledIn(BackendName),
    /// A build with no backend at all — possible only with
    /// `--no-default-features`, and worth its own message rather than an
    /// empty list in the one above.
    #[error(
        "this build of hc carries no backend at all — it was built with `--no-default-features` \
         and no backend feature. Rebuild with `--features rustls` or `--features native-tls`."
    )]
    NoneAtAll,
    /// The backend is here and would not start.
    #[error("the `{backend}` backend is compiled in and would not start: {cause}")]
    Unavailable { backend: BackendName, cause: String },
}

/// A `%{...}` this build does not know.
#[derive(Debug, thiserror::Error)]
#[error("unknown --write-out variable `%{{{}}}`.\n\nThis build knows: {}", .0, KNOWN.join(", "))]
pub struct Unknown(pub String);
