//! The machine's settings, turned into [`Proxy`] values.
//!
//! # What can go wrong, and why it is an error rather than a shrug
//!
//! A transport holds **one** proxy protocol `P`, which is the limit this
//! crate's root doc records against `Box<dyn Handshake>`. A machine can
//! name a SOCKS proxy and an HTTP one at once, and one transport cannot
//! hold both. Two more things can arrive that no [`Proxy`] can state
//! exactly: a subnet in the bypass list, and a wildcard in the middle of
//! a pattern.
//!
//! Every one of them is a **named refusal**, never a quiet narrowing, and
//! the reason is what the narrowing would do: traffic the machine's owner
//! routed through a proxy would go direct, or traffic they excluded would
//! go through one. Both are changes to where the bytes go, made on
//! somebody's behalf, and invisible from the call site. This workspace
//! has closed the *silently ignored setting* defect four times at the
//! `Capabilities` layer; this is the same defect one layer down, where
//! the setting comes from the machine instead of from the caller.
//!
//! The escape hatch is not a flag on the refusal — it is
//! [`SystemProxies`] itself, which a caller can read and act on however
//! they like.

use crate::error::SystemProxyRefused;

use super::{ProxyKind, Scheme, SystemProxies};
use crate::{HttpConnect, Proxy, ProxyScheme};

/// The HTTP proxies in `sys`, in the order they should be tried.
///
/// Empty is an ordinary answer: most machines have no proxy, and a
/// transport that installs an empty list proxies nothing, which is what
/// it should do.
pub fn http_proxies(sys: &SystemProxies) -> Result<Vec<Proxy<HttpConnect>>, SystemProxyRefused> {
    // Asked first, because it is the one that makes every other answer
    // beside the point: where a script decides, the static entries are
    // WinINET's *fallback* rather than the configuration.
    if let Some(pac) = sys.pac() {
        return Err(SystemProxyRefused::PacScript(pac.into()));
    }
    if let Some(u) = sys.unsupported_bypass().first() {
        return Err(SystemProxyRefused::UnrepresentableBypass(
            u.to_string().into_boxed_str(),
        ));
    }
    // A `*` bypass says *no proxy at all*, which is an answer and not a
    // failure: the honest installation of it is an empty list.
    if sys.bypass_everything() {
        return Ok(Vec::new());
    }

    let mut out = Vec::with_capacity(sys.entries().len());
    for entry in sys.entries() {
        if entry.kind() != ProxyKind::Http {
            return Err(SystemProxyRefused::MixedProtocols {
                kind: entry.kind(),
                host: entry.host().into(),
                port: entry.port(),
            });
        }
        let mut protocol = HttpConnect::new();
        if let Some(c) = entry.credentials() {
            protocol = protocol.basic_auth(c.user(), c.password()).map_err(|_| {
                SystemProxyRefused::UnusableCredential {
                    host: entry.host().into(),
                    port: entry.port(),
                }
            })?;
        }
        let mut proxy = Proxy::new(protocol, entry.host(), entry.port())
            .bypass(sys.bypass().iter().map(|p| p.to_string()));
        if sys.bypass_local() {
            proxy = proxy.bypass_local();
        }
        if let Some(scheme) = entry.applies_to() {
            proxy = proxy.only_for(match scheme {
                Scheme::Http => ProxyScheme::Http,
                Scheme::Https => ProxyScheme::Https,
            });
        }
        out.push(proxy);
    }
    Ok(out)
}

/// The same, for a caller who must not fail — and it hands back what it
/// could not install rather than swallowing it.
///
/// # Why there are two of these
///
/// Because a refusal is only useful to somebody who can act on it, and
/// that is not everybody. [`http_proxies`] refuses because its caller
/// **asked** for the machine's configuration and can decide what to do
/// about a part of it this client cannot express. `Client::new` did not
/// ask — it is a convenience constructor that reads the settings so that
/// a client is a good citizen by default — and a refusal there would mean
/// **a client that will not construct** on a network with WPAD, or on a
/// machine whose owner also configured a SOCKS proxy. That is a worse
/// answer than proxying what we can.
///
/// So: the explicit call refuses, the implicit one degrades and reports.
/// Nothing is silent at the API level; what a caller does with the report
/// is theirs.
///
/// # What it does with each awkward configuration
///
/// - **A PAC script and static entries beside it.** The static ones are
///   installed, which is not a fallback of ours but the machine's own:
///   WinINET keeps `ProxyServer` as exactly that when a script is
///   configured. Better than direct, and better than what a client that
///   cannot see the script at all would do.
/// - **A PAC script alone.** Direct — which is what curl and reqwest do
///   on the same machine, neither of them being able to see it. The
///   difference is that this one can, and says so in the report.
/// - **A SOCKS proxy.** Dropped, because a transport holds one proxy
///   protocol. Where SOCKS was the *only* proxy this means going direct
///   on a machine that wanted a proxy, which is the one degradation here
///   that loses something real — and is why the strict call exists.
/// - **A bypass pattern this matcher cannot state** — a wildcard that is
///   not a leading `*.`. The pattern is dropped and the proxy is kept,
///   which is what every other implementation does; dropping the *proxy*
///   over one odd exclusion would be the larger surprise.
pub fn http_proxies_lossy(
    sys: &SystemProxies,
) -> (Vec<Proxy<HttpConnect>>, Vec<SystemProxyRefused>) {
    let mut dropped = Vec::new();

    if let Some(pac) = sys.pac() {
        dropped.push(SystemProxyRefused::PacScript(pac.into()));
    }
    for u in sys.unsupported_bypass() {
        dropped.push(SystemProxyRefused::UnrepresentableBypass(
            u.to_string().into_boxed_str(),
        ));
    }
    if sys.bypass_everything() {
        return (Vec::new(), dropped);
    }

    let mut out = Vec::with_capacity(sys.entries().len());
    for entry in sys.entries() {
        if entry.kind() != ProxyKind::Http {
            dropped.push(SystemProxyRefused::MixedProtocols {
                kind: entry.kind(),
                host: entry.host().into(),
                port: entry.port(),
            });
            continue;
        }
        let mut protocol = HttpConnect::new();
        if let Some(c) = entry.credentials() {
            match protocol.clone().basic_auth(c.user(), c.password()) {
                Ok(with_auth) => protocol = with_auth,
                Err(_) => {
                    // The proxy is installed **without** the credential
                    // rather than dropped: a proxy that answers `407` is
                    // a diagnosable failure, where going direct past a
                    // proxy the machine named is not.
                    dropped.push(SystemProxyRefused::UnusableCredential {
                        host: entry.host().into(),
                        port: entry.port(),
                    });
                }
            }
        }
        let mut proxy = Proxy::new(protocol, entry.host(), entry.port())
            .bypass(sys.bypass().iter().map(|p| p.to_string()));
        if sys.bypass_local() {
            proxy = proxy.bypass_local();
        }
        if let Some(scheme) = entry.applies_to() {
            proxy = proxy.only_for(match scheme {
                Scheme::Http => ProxyScheme::Http,
                Scheme::Https => ProxyScheme::Https,
            });
        }
        out.push(proxy);
    }
    (out, dropped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system::testing::system_proxies;

    /// Where a request to `host` under `use_tls` would go, asked through
    /// the chooser a transport uses rather than by reading fields back: a
    /// proxy installed but never *chosen* would satisfy every assertion
    /// that reads fields and fail every request.
    fn route(list: &[Proxy<HttpConnect>], use_tls: bool, host: &str, port: u16) -> Option<String> {
        Proxy::choose(list, use_tls, host, port).map(|p| format!("{}:{}", p.host(), p.port()))
    }

    #[test]
    fn an_https_proxy_and_an_http_one_each_take_their_own_scheme() {
        // The case the whole feature exists for: the ordinary corporate
        // pair at different hosts, which one `Option` could never hold.
        let sys = system_proxies(
            &[("http", "plain.corp:8080"), ("https", "secure.corp:8443")],
            &[],
            false,
        );
        let list = http_proxies(&sys).expect("installs");

        assert_eq!(
            route(&list, false, "example.com", 80).as_deref(),
            Some("plain.corp:8080")
        );
        assert_eq!(
            route(&list, true, "example.com", 443).as_deref(),
            Some("secure.corp:8443")
        );
    }

    #[test]
    fn a_catch_all_proxy_does_not_shadow_a_scheme_specific_one() {
        // `choose` is first-match-wins and the platform hands its settings
        // over as an unordered map, so if the ordering imposed by
        // `SystemProxies` were lost, every `https://` request would go to
        // the wrong host. This is that ordering asserted where it has a
        // consequence.
        let sys = system_proxies(
            &[("*", "everything.corp:3128"), ("https", "secure.corp:8443")],
            &[],
            false,
        );
        let list = http_proxies(&sys).expect("installs");

        assert_eq!(
            route(&list, true, "example.com", 443).as_deref(),
            Some("secure.corp:8443")
        );
        assert_eq!(
            route(&list, false, "example.com", 80).as_deref(),
            Some("everything.corp:3128")
        );
    }

    #[test]
    fn the_bypass_list_takes_a_host_direct() {
        let sys = system_proxies(
            &[("*", "proxy.corp:8080")],
            &["internal.example.com", "*.corp.example.com"],
            false,
        );
        let list = http_proxies(&sys).expect("installs");

        assert_eq!(route(&list, true, "internal.example.com", 443), None);
        // `*.corp.example.com` translated to `.corp.example.com`: the host
        // and everything under it.
        assert_eq!(route(&list, true, "build.corp.example.com", 443), None);
        assert_eq!(route(&list, true, "corp.example.com", 443), None);
        assert_eq!(
            route(&list, true, "example.com", 443).as_deref(),
            Some("proxy.corp:8080")
        );
    }

    #[test]
    fn exclude_simple_hostnames_takes_a_dotless_host_direct() {
        // macOS ships with this on, so it is the common case rather than
        // a corner: without it, every `http://intranet/` on a Mac behind a
        // proxy would go through the proxy the machine's owner told it not
        // to use for exactly those names.
        let sys = system_proxies(&[("*", "proxy.corp:8080")], &[], true);
        let list = http_proxies(&sys).expect("installs");

        assert_eq!(route(&list, false, "intranet", 80), None);
        assert_eq!(
            route(&list, false, "10.0.0.5", 80).as_deref(),
            Some("proxy.corp:8080")
        );
    }

    #[test]
    fn without_the_local_rule_a_dotless_host_is_proxied() {
        // The control: a `bypass_local` that was always on would pass
        // every assertion in the test above.
        let sys = system_proxies(&[("*", "proxy.corp:8080")], &[], false);
        let list = http_proxies(&sys).expect("installs");
        assert_eq!(
            route(&list, false, "intranet", 80).as_deref(),
            Some("proxy.corp:8080")
        );
    }

    #[test]
    fn a_socks_proxy_is_refused_by_name_rather_than_dropped() {
        // Installing the HTTP entries and discarding this one would send
        // traffic direct that the machine routes through SOCKS —
        // invisibly, from a call site that asked for the machine's
        // configuration.
        let sys = system_proxies(&[("all", "socks5://socks.corp:1080")], &[], false);
        let err = http_proxies(&sys).expect_err("not installable here");
        let rendered = err.to_string();
        assert!(rendered.contains("socks.corp"), "{rendered}");
        assert!(rendered.contains("1080"), "{rendered}");
    }

    #[test]
    fn a_bypass_pattern_that_cannot_be_stated_exactly_is_refused_by_name() {
        // The mirror failure: honouring a pattern approximately would put
        // a host on the proxy that the machine excluded from it.
        let sys = system_proxies(&[("*", "proxy.corp:8080")], &["192.168.1.*"], false);
        let err = http_proxies(&sys).expect_err("a mid-pattern wildcard is not statable");
        assert!(err.to_string().contains("192.168.1.*"), "{err}");
    }

    #[test]
    fn the_default_macos_exceptions_install_rather_than_refuse() {
        // What every Mac ships: a leading-label wildcard and an
        // abbreviated subnet. Both are statable, so the common case is an
        // installation rather than the refusal this used to be.
        let sys = system_proxies(
            &[("*", "proxy.corp:8080")],
            &["*.local", "169.254/16"],
            true,
        );
        let list = http_proxies(&sys).expect("the platform's own default installs");

        assert_eq!(route(&list, true, "printer.local", 443), None);
        assert_eq!(route(&list, true, "169.254.3.4", 443), None);
        assert_eq!(route(&list, true, "intranet", 80), None);
        assert_eq!(
            route(&list, true, "example.com", 443).as_deref(),
            Some("proxy.corp:8080")
        );
    }

    // --- the lenient path ------------------------------------------

    #[test]
    fn the_lenient_path_installs_a_pac_machines_static_entries() {
        // WinINET's own fallback, not an invention of ours: with a script
        // configured, `ProxyServer` is what it falls back to. Honouring
        // it beats direct, and beats what a client blind to the script
        // would do.
        use crate::system::testing::system_proxies_with_pac;
        let sys = system_proxies_with_pac(
            &[("*", "fallback.corp:8080")],
            &[],
            false,
            "http://wpad.corp/proxy.pac",
        );
        let (list, dropped) = http_proxies_lossy(&sys);

        assert_eq!(
            route(&list, true, "example.com", 443).as_deref(),
            Some("fallback.corp:8080")
        );
        // Degraded, never silent: the script is in the report.
        assert!(matches!(dropped[0], SystemProxyRefused::PacScript(_)));
    }

    #[test]
    fn the_lenient_path_takes_a_pac_only_machine_direct_and_says_so() {
        use crate::system::testing::system_proxies_with_pac;
        let sys = system_proxies_with_pac(&[], &[], false, "http://wpad.corp/proxy.pac");
        let (list, dropped) = http_proxies_lossy(&sys);

        assert!(list.is_empty());
        assert_eq!(dropped.len(), 1);
    }

    #[test]
    fn the_lenient_path_keeps_the_http_proxy_beside_a_socks_one() {
        // The common Windows shape — `http=` and `socks=` in one
        // `ProxyServer` — where dropping the SOCKS entry costs this
        // client nothing: it is the HTTP one that serves our traffic.
        let sys = system_proxies(
            &[("http", "plain.corp:8080"), ("socks", "socks.corp:1080")],
            &[],
            false,
        );
        let (list, dropped) = http_proxies_lossy(&sys);

        assert_eq!(
            route(&list, false, "example.com", 80).as_deref(),
            Some("plain.corp:8080")
        );
        assert!(matches!(
            dropped[0],
            SystemProxyRefused::MixedProtocols { .. }
        ));
    }

    #[test]
    fn the_lenient_path_goes_direct_where_socks_was_the_only_proxy() {
        // The one degradation that loses something real, and the reason
        // the strict call exists. Asserted rather than left implied.
        let sys = system_proxies(&[("all", "socks5://socks.corp:1080")], &[], false);
        let (list, dropped) = http_proxies_lossy(&sys);

        assert!(list.is_empty());
        assert!(matches!(
            dropped[0],
            SystemProxyRefused::MixedProtocols { .. }
        ));
    }

    #[test]
    fn the_lenient_path_drops_an_unstatable_pattern_and_keeps_the_proxy() {
        let sys = system_proxies(&[("*", "proxy.corp:8080")], &["192.168.1.*"], false);
        let (list, dropped) = http_proxies_lossy(&sys);

        assert_eq!(
            route(&list, true, "example.com", 443).as_deref(),
            Some("proxy.corp:8080")
        );
        assert!(matches!(
            dropped[0],
            SystemProxyRefused::UnrepresentableBypass(_)
        ));
    }

    #[test]
    fn an_ordinary_machine_installs_the_same_list_either_way() {
        // The two paths may not disagree about a configuration both can
        // express — otherwise `Client::new` and an explicit call would
        // proxy differently on the same machine.
        for sys in [
            system_proxies(&[], &[], false),
            system_proxies(&[("*", "proxy.corp:8080")], &["a.com", "*.b.com"], true),
            system_proxies(
                &[("http", "p:8080"), ("https", "s:8443")],
                &["10.0.0.0/8"],
                false,
            ),
        ] {
            let strict = http_proxies(&sys).expect("expressible");
            let (lossy, dropped) = http_proxies_lossy(&sys);
            assert!(dropped.is_empty());
            let key = |l: &[Proxy<HttpConnect>]| {
                l.iter().map(|p| (p.key(), p.scheme())).collect::<Vec<_>>()
            };
            assert_eq!(key(&strict), key(&lossy));
        }
    }

    #[test]
    fn a_pac_machine_is_refused_rather_than_taken_direct() {
        // The sharpest of the four refusals: ignoring a PAC script means
        // going direct on a machine whose owner routed its traffic
        // through a proxy — a policy violation, and on a network with no
        // direct egress a failure nobody can explain from here.
        use crate::system::testing::system_proxies_with_pac;
        let sys = system_proxies_with_pac(&[], &[], false, "http://wpad.corp/proxy.pac");
        let err = http_proxies(&sys).expect_err("a script is not a proxy list");
        assert!(err.to_string().contains("wpad.corp/proxy.pac"), "{err}");
    }

    #[test]
    fn a_pac_url_beside_static_entries_still_refuses() {
        // WinINET keeps `ProxyServer` as a *fallback* when a script is
        // configured, so installing those entries would be honouring the
        // machine's second choice while ignoring its first.
        use crate::system::testing::system_proxies_with_pac;
        let sys = system_proxies_with_pac(
            &[("*", "fallback.corp:8080")],
            &[],
            false,
            "http://wpad.corp/proxy.pac",
        );
        assert!(http_proxies(&sys).is_err());
    }

    #[test]
    fn a_machine_with_no_proxy_installs_none() {
        // Not an error: most machines are this one.
        let list = http_proxies(&system_proxies(&[], &[], false)).expect("ordinary");
        assert!(list.is_empty());
    }

    #[test]
    fn a_star_bypass_installs_nothing_at_all() {
        // `NO_PROXY=*` is *use no proxy*, which is an answer rather than a
        // failure — and the honest installation of it is an empty list,
        // not a refusal and not a proxy nobody asked for.
        let sys = system_proxies(&[("*", "proxy.corp:8080")], &["*"], false);
        assert!(http_proxies(&sys).expect("installs").is_empty());
    }

    #[test]
    fn credentials_from_a_proxy_url_reach_the_connector() {
        // `http://user:pass@proxy` is how the environment carries a proxy
        // credential, and a proxy installed without it authenticates
        // against nothing and gets a 407 the caller cannot explain.
        use crate::Handshake;
        let sys = system_proxies(&[("*", "http://alice:hunter2@proxy.corp:8080")], &[], false);
        let list = http_proxies(&sys).expect("installs");

        let header = list[0]
            .protocol()
            .proxy_authorization()
            .expect("a credential reached the connector");
        // RFC 7617: `alice:hunter2` base64-encoded.
        assert_eq!(header.as_bytes(), b"Basic YWxpY2U6aHVudGVyMg==");
        // And it is marked sensitive, so it does not reach a log through
        // a `Debug`.
        assert!(header.is_sensitive());
    }
}
