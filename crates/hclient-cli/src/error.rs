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

#[derive(Debug)]
pub enum Refused {
    /// Named a backend this build does not carry. Never a fallback.
    NotCompiledIn(BackendName),
    /// A build with no backend at all — possible only with
    /// `--no-default-features`, and worth its own message rather than an
    /// empty list in the one above.
    NoneAtAll,
    /// The backend is here and would not start.
    Unavailable { backend: BackendName, cause: String },
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotCompiledIn(b) => {
                write!(f, "this build of hc has no `{b}` backend.\n\nIt carries: ")?;
                write!(f, "{}", available_list())?;
                write!(
                    f,
                    "\n\nA backend is refused rather than silently replaced, which is the \
                     one thing this tool promises over curl's `CURL_SSL_BACKEND`."
                )
            }
            Self::NoneAtAll => write!(
                f,
                "this build of hc carries no backend at all — it was built with \
                 `--no-default-features` and no backend feature. Rebuild with \
                 `--features rustls` or `--features native-tls`."
            ),
            Self::Unavailable { backend, cause } => {
                write!(
                    f,
                    "the `{backend}` backend is compiled in and would not start: {cause}"
                )
            }
        }
    }
}

impl std::error::Error for Refused {}

/// A `%{...}` this build does not know.
#[derive(Debug)]
pub struct Unknown(pub String);

impl std::fmt::Display for Unknown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "unknown --write-out variable `%{{{}}}`.\n\nThis build knows: {}",
            self.0,
            KNOWN.join(", ")
        )
    }
}

impl std::error::Error for Unknown {}
