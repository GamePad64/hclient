//! `--backend`, and the refusal that is the point of it.
//!
//! curl supports several TLS backends **chosen when the binary was
//! built**. Only a `MultiSSL` build honours `CURL_SSL_BACKEND` at runtime,
//! Ubuntu's stock curl is not one, and curl's own man page says that
//! setting a name it does not have "makes curl stay with the default" —
//! a silently ignored setting, which is the defect class this workspace
//! has closed four times over.
//!
//! So the differentiator is not that curl cannot do this. It is that curl
//! *can*, in a build almost nobody has; that when it cannot it says
//! nothing; and that the choice belongs to whoever packaged the binary
//! rather than to whoever runs it. Here a backend this build does not
//! carry is an **error naming it**, next to the list of what is available,
//! and `hc --version` prints that list without being asked.

use crate::args::BackendName;

/// Everything that varies between backends, decided once so the rest of
/// the tool is written for one client.
pub struct Config {
    pub insecure: bool,
    pub resolve: Vec<(String, std::net::IpAddr)>,
    /// `--timeout`, the whole-operation bound. It is applied here rather
    /// than on the request because `ClientBuilder::total_timeout` is where
    /// it lives — `Timeouts` carries `connect`, `first_byte` and
    /// `between_bytes` and deliberately no `total`, since the total is the
    /// one bound that needs a clock to race a sleep against a silent body.
    pub total: Option<std::time::Duration>,
}

/// What this build carries, in the order `--version` prints them.
pub const COMPILED_IN: &[BackendName] = &[
    #[cfg(feature = "rustls")]
    BackendName::Rustls,
    #[cfg(feature = "native-tls")]
    BackendName::NativeTls,
];

/// The one used when `--backend` is absent: the first compiled in, so a
/// build carrying only the platform stack still has a default.
pub fn default_backend() -> Option<BackendName> {
    COMPILED_IN.first().copied()
}

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

pub fn available_list() -> String {
    if COMPILED_IN.is_empty() {
        return "nothing".into();
    }
    COMPILED_IN
        .iter()
        .map(BackendName::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The resolver every backend gets: the system one, with any `--resolve`
/// entries in front of it.
///
/// `Overrides` wraps a `Resolve` rather than replacing it, so an entry
/// answers for its own host and every other name still goes to the
/// system — which is what `curl --resolve` means and is why this composes
/// instead of being a mode.
fn resolver(
    cfg: &Config,
) -> hclient_dns::Overrides<hclient_dns_system::SystemDns<hclient_rt_tokio::Tokio>> {
    let mut o =
        hclient_dns::Overrides::new(hclient_dns_system::SystemDns::new(hclient_rt_tokio::Tokio));
    for (host, addr) in &cfg.resolve {
        o = o.host(host, vec![*addr]);
    }
    o
}

/// Build the client. **Every arm returns the same `hclient::Client`**,
/// which is what makes `--backend` an ordinary `match` and lets the rest
/// of this program be written once — see `.notes/erased-client.md`. A
/// generic client would have made this function's return type name a
/// transport, and the two arms name different ones.
/// Which backend a run will use, decided from what was asked and what the
/// build has — and **taking the available list as an argument**.
///
/// That parameter is the whole point of the function existing. The refusal
/// is this tool's one promise over curl, and in the default build every
/// backend is present, so the arm that refuses is unreachable and a test
/// of `build` cannot fail for a mutation that replaces the refusal with a
/// fallback — measured, that mutation survived all 30 tests. A decision
/// that is a pure function of `(requested, available)` is testable at any
/// feature setting, which is what makes the promise checkable rather than
/// merely written down.
pub fn choose(
    requested: Option<BackendName>,
    available: &[BackendName],
) -> Result<BackendName, Refused> {
    let Some(&first) = available.first() else {
        return Err(Refused::NoneAtAll);
    };
    match requested {
        None => Ok(first),
        Some(want) if available.contains(&want) => Ok(want),
        // Never `Ok(first)`: that is curl's behaviour and the defect this
        // tool exists to not have.
        Some(want) => Err(Refused::NotCompiledIn(want)),
    }
}

pub fn build(which: Option<BackendName>, cfg: &Config) -> Result<hclient::Client, Refused> {
    let which = choose(which, COMPILED_IN)?;
    match which {
        #[cfg(feature = "rustls")]
        BackendName::Rustls => {
            let tls = if cfg.insecure {
                hclient_tls_rustls::Rustls::danger_accept_invalid_certs()
            } else {
                hclient_tls_rustls::Rustls::with_platform_verifier().map_err(|e| {
                    Refused::Unavailable {
                        backend: which,
                        cause: e.to_string(),
                    }
                })?
            };
            finish(
                which,
                cfg,
                hclient_native::Native::new(hclient_rt_tokio::Tokio, tls, resolver(cfg)),
            )
        }
        #[cfg(feature = "native-tls")]
        BackendName::NativeTls => {
            let tls = if cfg.insecure {
                hclient_tls_native_tls::NativeTls::new().danger_accept_invalid_certs()
            } else {
                hclient_tls_native_tls::NativeTls::new()
            };
            finish(
                which,
                cfg,
                hclient_native::Native::new(hclient_rt_tokio::Tokio, tls, resolver(cfg)),
            )
        }
        // `choose` has already refused anything this build lacks, so the
        // arms above are exhaustive for every value that reaches here —
        // and with a backend feature off the compiler needs this arm to
        // say so. It is unreachable rather than a second refusal, which is
        // what keeps one decision in one place.
        #[allow(
            unreachable_patterns,
            reason = "reachable only in a build with a backend feature off, where `choose` has \
                      already returned Err for exactly these values"
        )]
        other => Err(Refused::NotCompiledIn(other)),
    }
}

/// The half that is the same for every backend, so an arm above is one
/// expression and the two cannot drift.
///
/// The bound is `BoxedTransport`, which `hclient-core` carries a blanket
/// impl of — so this names no transport type and neither do the arms'
/// return values. That is the erasure paying for itself: with a generic
/// `Client` this function's signature would have had to name one
/// transport, and the arms build different ones.
#[allow(
    dead_code,
    reason = "unreachable in a build carrying no backend feature at all"
)]
fn finish<T>(backend: BackendName, cfg: &Config, transport: T) -> Result<hclient::Client, Refused>
where
    T: hclient_core::unversioned::erased::BoxedTransport + Send + Sync + 'static, // send-bound-exception: amendment-C12
{
    // `build()` refuses a client setting the transport cannot honour, and
    // this program sets none of them here — the cookie jar and the cache
    // are added by `run.rs` on the client it gets back. So a failure would
    // mean the transport disagreed with a default, which is a bug rather
    // than a user error, and it is reported as one.
    let mut b = hclient::Client::builder(transport);
    if let Some(total) = cfg.total {
        b = b.total_timeout(hclient_rt_tokio::Tokio, total);
    }
    b.build().map_err(|e| Refused::Unavailable {
        backend,
        cause: e.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The promise, stated as a test rather than as a paragraph: a name
    /// this build has not got is refused, **never** replaced by the
    /// default. curl's own man page says `CURL_SSL_BACKEND` with an
    /// unknown name "makes curl stay with the default", and that is the
    /// behaviour this asserts the absence of.
    #[test]
    fn a_backend_the_build_lacks_is_refused_and_never_silently_replaced() {
        let only_rustls = [BackendName::Rustls];
        let e = choose(Some(BackendName::NativeTls), &only_rustls).unwrap_err();
        match e {
            Refused::NotCompiledIn(b) => assert_eq!(b, BackendName::NativeTls),
            other => panic!("expected a refusal naming the backend, got {other:?}"),
        }
    }

    #[test]
    fn the_message_names_what_was_asked_for_and_what_is_available() {
        // A refusal a caller cannot act on is barely better than a silent
        // fallback, so both halves are asserted.
        let e = Refused::NotCompiledIn(BackendName::NativeTls).to_string();
        assert!(e.contains("native-tls"), "{e}");
        assert!(e.contains("It carries:"), "{e}");
    }

    #[test]
    fn a_backend_the_build_has_is_used_and_no_request_means_the_first() {
        let both = [BackendName::Rustls, BackendName::NativeTls];
        assert_eq!(
            choose(Some(BackendName::NativeTls), &both).unwrap(),
            BackendName::NativeTls
        );
        assert_eq!(choose(None, &both).unwrap(), BackendName::Rustls);
        // The default is the first of what is there rather than a constant,
        // so a build carrying only the platform stack still has one.
        let only_native = [BackendName::NativeTls];
        assert_eq!(choose(None, &only_native).unwrap(), BackendName::NativeTls);
    }

    #[test]
    fn a_build_with_no_backend_at_all_says_so_rather_than_showing_an_empty_list() {
        assert!(matches!(choose(None, &[]), Err(Refused::NoneAtAll)));
        assert!(matches!(
            choose(Some(BackendName::Rustls), &[]),
            Err(Refused::NoneAtAll)
        ));
    }

    /// The list `choose` is given in production is not empty, which is what
    /// makes every test above about a real configuration rather than a
    /// hypothetical one.
    #[test]
    fn this_build_carries_at_least_one_backend() {
        assert!(!COMPILED_IN.is_empty(), "built with no backend feature");
    }
}
