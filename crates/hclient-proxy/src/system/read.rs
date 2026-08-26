//! Where the settings come from — the environment, the Windows registry,
//! the macOS dynamic store — and the shape they arrive in.
//!
//! # The split this file is built around
//!
//! Every platform reader here is **two functions**: one that asks the OS,
//! which is a handful of lines and cannot run on the machine this
//! workspace is developed on, and one that turns what it said into
//! [`Raw`], which is pure and is tested exhaustively on any host. The
//! second is where every rule lives — `ProxyServer`'s `scheme=host:port`
//! list, `ProxyOverride`'s `<local>`, which keys mean which scheme — so
//! the untestable half holds no decisions.
//!
//! That is the same discipline `hclient-dns-system` keeps between its
//! `sys` module and its parsers, and it is why taking a third-party crate
//! for this was weighed and dropped: what it would have carried is
//! `url`, and through it the ICU tables, on exactly the two targets
//! `hclient-idn` exists to keep them off.

/// What a source said, before any of it is interpreted.
///
/// The keys are the platforms' own — `http`, `https`, `socks`, `*` for an
/// unqualified Windows `ProxyServer`, `all` for an `ALL_PROXY` — because
/// the meaning of a key is a rule, and rules belong in
/// [`super::SystemProxies::from_parts`] where they are tested.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Raw {
    pub proxies: Vec<(String, String)>,
    pub bypass: Vec<String>,
    pub exclude_simple: bool,
    /// A proxy auto-config script — see [`super::SystemProxies::pac`].
    pub pac: Option<String>,
}

impl Raw {
    /// Nothing configured. A source that answers this is passed over, so
    /// that an empty environment falls through to the platform rather
    /// than shadowing it.
    fn is_empty(&self) -> bool {
        self.proxies.is_empty() && self.pac.is_none()
    }
}

/// The environment first, the platform where the environment named
/// nothing.
///
/// That order is not ours to choose: it is what curl, reqwest and every
/// other client on the machine does, and an `HTTPS_PROXY` that this
/// client alone ignored would be worse than one it alone honoured.
pub(super) fn read() -> Raw {
    let env = from_env();
    if !env.is_empty() {
        return env;
    }
    platform()
}

// --- the environment ----------------------------------------------------

fn from_env() -> Raw {
    from_pairs(std::env::vars())
}

/// `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, `NO_PROXY`, in either case.
///
/// Any `*_PROXY` is taken, not a fixed list: `FTP_PROXY` arrives as the
/// key `ftp` and is named in [`super::SystemProxies::ignored`] rather
/// than silently dropped, which is only possible because this does not
/// decide what a key means.
fn from_pairs(vars: impl Iterator<Item = (String, String)>) -> Raw {
    let mut raw = Raw::default();
    let mut pairs: Vec<_> = vars
        .filter_map(|(k, v)| {
            let k = k.to_ascii_lowercase();
            k.strip_suffix("_proxy").map(|s| (s.to_owned(), v))
        })
        .collect();
    // The environment has no order, and the order decides which proxy a
    // request takes. Sorting here makes the answer the same twice on one
    // machine; `from_parts` is what puts the scheme-specific entries
    // ahead of the catch-all.
    pairs.sort();
    for (key, value) in pairs {
        let value = value.trim().to_owned();
        if value.is_empty() {
            continue;
        }
        if key == "no" {
            // Both separators, because both are in use and neither is
            // specified: curl splits on commas, some deployments write
            // semicolons, and no host name contains either.
            raw.bypass.extend(
                value
                    .split([',', ';'])
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned),
            );
        } else {
            raw.proxies.push((key, value));
        }
    }
    raw
}

// --- Windows ------------------------------------------------------------

#[cfg(windows)]
fn platform() -> Raw {
    // `HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings`,
    // which is where WinINET keeps what the Internet Options dialog
    // writes and where every other client on the machine reads it. Asked
    // through `windows-registry`, whose API is safe, so this crate keeps
    // `#![forbid(unsafe_code)]`.
    const PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";
    let Ok(key) = windows_registry::CURRENT_USER.open(PATH) else {
        return Raw::default();
    };
    from_wininet(
        key.get_u32("ProxyEnable").unwrap_or(0) != 0,
        key.get_string("ProxyServer").ok().as_deref(),
        key.get_string("ProxyOverride").ok().as_deref(),
        key.get_string("AutoConfigURL").ok().as_deref(),
    )
}

/// What the four registry values mean, as a pure function.
///
/// `enabled` gates the **static** proxy alone: a machine with
/// `ProxyEnable = 0` and an `AutoConfigURL` is configured, and reporting
/// it as unconfigured would send its traffic direct.
///
/// Compiled off Windows **for its tests**, which is the whole point of
/// the split this file is built around: the rules are checked on the
/// machine this workspace is developed on, where the registry is not.
#[cfg(any(windows, test))]
fn from_wininet(
    enabled: bool,
    server: Option<&str>,
    over: Option<&str>,
    auto_config: Option<&str>,
) -> Raw {
    let mut raw = Raw {
        pac: auto_config
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
        ..Raw::default()
    };
    if enabled {
        for entry in server.unwrap_or("").split(';') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            match entry.split_once('=') {
                // `http=a:8080;https=b:8443` — a per-scheme list.
                Some((scheme, host)) => raw
                    .proxies
                    .push((scheme.trim().to_ascii_lowercase(), host.trim().to_owned())),
                // A bare `host:port` is every scheme, which is what the
                // dialog writes when "use the same proxy for all
                // protocols" is ticked.
                None => raw.proxies.push(("*".to_owned(), entry.to_owned())),
            }
        }
    }
    for pattern in over.unwrap_or("").split(';') {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            continue;
        }
        // `<local>` is not a pattern, it is the dotless-host rule — see
        // `Proxy::bypass_local`. It is consumed here rather than passed
        // on, so that the bypass list this crate hands over contains only
        // things that are patterns.
        if pattern.eq_ignore_ascii_case("<local>") {
            raw.exclude_simple = true;
        } else {
            raw.bypass.push(pattern.to_owned());
        }
    }
    raw
}

// --- macOS --------------------------------------------------------------

#[cfg(target_vendor = "apple")]
fn platform() -> Raw {
    use core_foundation::array::CFArray;
    use core_foundation::base::{CFType, TCFType};
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::number::CFNumber;
    use core_foundation::string::CFString;
    use core_foundation_sys::base::CFTypeRef;
    use system_configuration::dynamic_store::SCDynamicStoreBuilder;

    /// The schema keys are string constants in `SCSchemaDefinitions.h`
    /// (`kSCPropNetProxiesHTTPEnable` *is* `CFSTR("HTTPEnable")`), so
    /// naming them as strings costs nothing and avoids dereferencing a
    /// `CFStringRef` static, which would be `unsafe` for no gain.
    fn number(d: &CFDictionary<CFString, CFType>, key: &'static str) -> Option<i32> {
        d.find(CFString::from_static_string(key))
            .and_then(|v| v.downcast::<CFNumber>())
            .and_then(|v| v.to_i32())
    }
    fn string(d: &CFDictionary<CFString, CFType>, key: &'static str) -> Option<String> {
        d.find(CFString::from_static_string(key))
            .and_then(|v| v.downcast::<CFString>())
            .map(|v| v.to_string())
    }
    fn enabled(d: &CFDictionary<CFString, CFType>, key: &'static str) -> bool {
        number(d, key).unwrap_or(0) == 1
    }

    let store = SCDynamicStoreBuilder::new("hclient-proxy").build();
    let Some(d) = store.get_proxies() else {
        return Raw::default();
    };

    let mut raw = Raw::default();
    // One arm per protocol, and the port is part of the value rather than
    // a field: `from_parts` parses `host:port` for every source, so a
    // reader that split them would be the one place with a second
    // spelling.
    for (key, enable, host, port) in [
        ("http", "HTTPEnable", "HTTPProxy", "HTTPPort"),
        ("https", "HTTPSEnable", "HTTPSProxy", "HTTPSPort"),
        // macOS's SOCKS is SOCKS5 — the dialog's own word, and what
        // Chromium and Firefox both read it as. That is *unlike*
        // Windows's unversioned `socks=`, which `ProxyKind::Socks4`
        // documents, so the key here is deliberately not `socks`.
        ("socks5", "SOCKSEnable", "SOCKSProxy", "SOCKSPort"),
        ("ftp", "FTPEnable", "FTPProxy", "FTPPort"),
    ] {
        if !enabled(&d, enable) {
            continue;
        }
        let Some(h) = string(&d, host) else { continue };
        let value = match number(&d, port) {
            Some(p) => format!("{h}:{p}"),
            None => h,
        };
        raw.proxies.push((key.to_owned(), value));
    }

    raw.exclude_simple = enabled(&d, "ExcludeSimpleHostnames");
    if enabled(&d, "ProxyAutoConfigEnable") {
        raw.pac = string(&d, "ProxyAutoConfigURLString");
    }
    // **The one `unsafe` in this workspace's proxy code**, and it is here
    // rather than avoided because there is no safe path: `core-foundation`
    // implements `ConcreteCFType` for `CFArray<*const c_void>` alone, so a
    // dictionary value can be downcast to an *untyped* array and to no
    // other, and its elements arrive as raw pointers. Checked in 0.9 and
    // 0.10; `objc2-core-foundation` has the same wall one level up, at the
    // dictionary. Skipping the list instead is not an option worth having:
    // **every Mac ships one** — `*.local` and `169.254/16` are the
    // defaults — so a reader that dropped it would silently proxy the
    // traffic its owner excluded.
    //
    // What is assumed is only that the pointer is a valid CF object, which
    // is what a `CFArray` from `SCDynamicStoreCopyProxies` holds. **Which**
    // class it is, is checked rather than assumed: `downcast::<CFString>()`
    // compares the type id, so an array of something else yields nothing
    // instead of reading a string out of it.
    if let Some(list) = d
        .find(CFString::from_static_string("ExceptionsList"))
        .and_then(|v| v.downcast::<CFArray>())
    {
        for ptr in list.get_all_values() {
            // SAFETY: `ptr` is an element of a CFArray returned by
            // SCDynamicStoreCopyProxies, so it is a valid CF object owned
            // by that array, which outlives this borrow; the Get rule is
            // the right one for a borrow that does not outlive it.
            #[allow(unsafe_code)] // unsafe-code-exception: amendment-C13
            let value = unsafe { CFType::wrap_under_get_rule(ptr as CFTypeRef) };
            // unsafe-code-exception: amendment-C13
            if let Some(s) = value.downcast::<CFString>() {
                raw.bypass.push(s.to_string());
            }
        }
    }
    raw
}

#[cfg(not(any(windows, target_vendor = "apple")))]
fn platform() -> Raw {
    // No platform store to ask. The environment is the whole of it, which
    // is the same answer curl gives on these targets — and it has already
    // been read by the time this is called.
    Raw::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn env(vars: &[(&str, &str)]) -> Raw {
        from_pairs(vars.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())))
    }

    #[test]
    fn the_usual_variables_are_read_in_either_case() {
        let raw = env(&[
            ("HTTP_PROXY", "http://a:8080"),
            ("https_proxy", "http://b:8443"),
            ("PATH", "/usr/bin"),
        ]);
        assert_eq!(
            raw.proxies,
            [
                ("http".to_owned(), "http://a:8080".to_owned()),
                ("https".to_owned(), "http://b:8443".to_owned())
            ]
        );
    }

    #[test]
    fn the_environment_has_no_order_so_this_imposes_one() {
        // Which proxy a request takes depends on the order, so an answer
        // that varied between runs on one machine would be a client that
        // proxies differently on Tuesday.
        let a = env(&[("HTTPS_PROXY", "b"), ("HTTP_PROXY", "a")]);
        let b = env(&[("HTTP_PROXY", "a"), ("HTTPS_PROXY", "b")]);
        assert_eq!(a.proxies, b.proxies);
    }

    #[rstest]
    #[case("a.com,b.com", &["a.com", "b.com"])]
    #[case("a.com;b.com", &["a.com", "b.com"])]
    #[case(" a.com , b.com ", &["a.com", "b.com"])]
    #[case("a.com,,b.com", &["a.com", "b.com"])]
    fn no_proxy_splits_on_both_separators(#[case] value: &str, #[case] want: &[&str]) {
        assert_eq!(env(&[("NO_PROXY", value)]).bypass, want);
    }

    #[test]
    fn an_empty_variable_names_no_proxy() {
        // `HTTP_PROXY=` is how a shell script turns one off, and reading
        // it as a proxy at the empty host would fail every request.
        assert!(env(&[("HTTP_PROXY", "")]).proxies.is_empty());
        assert!(env(&[("HTTP_PROXY", "   ")]).proxies.is_empty());
    }

    #[test]
    fn a_variable_this_client_cannot_use_is_kept_for_naming_later() {
        // Not dropped here: `from_parts` is what decides a key has no
        // meaning for an HTTP client, and it says so in `ignored`.
        assert_eq!(
            env(&[("FTP_PROXY", "ftp-proxy:2121")]).proxies,
            [("ftp".to_owned(), "ftp-proxy:2121".to_owned())]
        );
    }

    #[test]
    fn a_per_scheme_proxy_server_string_is_split_by_scheme() {
        let raw = from_wininet(
            true,
            Some("http=a:8080;https=b:8443;socks=c:1080"),
            None,
            None,
        );
        assert_eq!(
            raw.proxies,
            [
                ("http".to_owned(), "a:8080".to_owned()),
                ("https".to_owned(), "b:8443".to_owned()),
                ("socks".to_owned(), "c:1080".to_owned()),
            ]
        );
    }

    #[test]
    fn a_bare_proxy_server_string_serves_every_scheme() {
        let raw = from_wininet(true, Some("proxy.corp:8080"), None, None);
        assert_eq!(
            raw.proxies,
            [("*".to_owned(), "proxy.corp:8080".to_owned())]
        );
    }

    #[test]
    fn proxy_enable_zero_turns_off_the_static_proxy_and_nothing_else() {
        // The half that matters: a machine with the static proxy off and
        // an auto-config URL set is *configured*, and reading it as
        // unconfigured would send its traffic direct.
        let raw = from_wininet(false, Some("proxy.corp:8080"), None, Some("http://w/p.pac"));
        assert!(raw.proxies.is_empty());
        assert_eq!(raw.pac.as_deref(), Some("http://w/p.pac"));
    }

    #[test]
    fn the_local_token_becomes_the_rule_rather_than_a_pattern() {
        let raw = from_wininet(true, Some("p:8080"), Some("<local>;a.com; b.com "), None);
        assert!(raw.exclude_simple);
        // `<local>` does not survive into the pattern list, where nothing
        // could match it.
        assert_eq!(raw.bypass, ["a.com", "b.com"]);
    }

    #[test]
    fn the_local_token_is_matched_regardless_of_case() {
        assert!(from_wininet(true, None, Some("<LOCAL>"), None).exclude_simple);
    }

    #[test]
    fn an_absent_value_is_not_an_empty_one() {
        let raw = from_wininet(true, None, None, None);
        assert!(raw.proxies.is_empty());
        assert!(raw.bypass.is_empty());
        assert!(raw.pac.is_none());
        assert!(!raw.exclude_simple);
    }

    #[test]
    fn an_empty_auto_config_url_is_no_auto_config_url() {
        // The registry keeps the value after the checkbox is cleared, so
        // an empty string is the ordinary shape of *not configured* and
        // refusing on it would refuse on most machines.
        assert!(from_wininet(true, None, None, Some("")).pac.is_none());
        assert!(from_wininet(true, None, None, Some("  ")).pac.is_none());
    }

    #[test]
    fn a_source_with_only_a_pac_url_is_not_empty() {
        // What decides whether the platform is consulted at all: a PAC
        // machine whose environment names nothing must not be read as an
        // environment that named a proxy.
        let raw = from_wininet(false, None, None, Some("http://w/p.pac"));
        assert!(!raw.is_empty());
        assert!(Raw::default().is_empty());
    }
}
