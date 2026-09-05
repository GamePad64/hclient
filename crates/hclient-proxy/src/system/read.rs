//! Where the settings come from — the environment, the Windows registry,
//! the macOS dynamic store, Android's JVM properties — and the shape they
//! arrive in.
//!
//! # The split this file is built around
//!
//! Every platform reader here is **two functions**: one that asks the OS,
//! which is a handful of lines and cannot run on the machine this
//! workspace is developed on, and one that turns what it said into
//! [`Raw`], which is pure and is tested exhaustively on any host. The
//! second is where every rule lives — `ProxyServer`'s `scheme=host:port`
//! list, `ProxyOverride`'s `<local>`, `nonProxyHosts`' `|`, which keys
//! mean which scheme — so the untestable half holds no decisions.
//!
//! Android is the sharpest case of that split and the reason to read it
//! before adding a fourth platform: its settings are behind a **JVM**, so
//! its untestable half is four JNI calls in `jvm.rs` and its testable
//! half is [`from_jvm_properties`], which takes the lookup as a closure —
//! `platform()` hands it JNI and a test hands it a table, and no rule
//! under test can tell.
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
pub(super) fn platform() -> Raw {
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
pub(super) fn platform() -> Raw {
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

    // **`build` became fallible in `system-configuration` 0.8, and that is
    // a soundness fix rather than a signature change.** 0.6 handed the
    // result of `SCDynamicStoreCreateWithOptions` to
    // `wrap_under_create_rule` with no null check, so a store the OS
    // declined to create was a `SCDynamicStore` that dereferenced null on
    // first use. It is an `Option` now.
    //
    // Both failures take the exit that was already here: the reader has
    // nothing to report either way, and `platform() -> Raw` carries no
    // channel to say which. That collapse is the signature's and not a
    // decision taken here — a reader that could distinguish them would
    // need `Raw` to grow a variant, and nothing above it could act on one.
    let store = SCDynamicStoreBuilder::new("hclient-proxy").build();
    let Some(d) = store.and_then(|s| s.get_proxies()) else {
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
            // The attribute goes **above** the SAFETY comment, not
            // between it and the `unsafe`: clippy's
            // `undocumented_unsafe_blocks` wants the comment adjacent,
            // and an attribute in the gap makes it invisible — a warning
            // nobody saw, because clippy for this target is not something
            // `just lint` runs.
            #[allow(unsafe_code)] // unsafe-code-exception: amendment-C13
            // SAFETY: `ptr` is an element of a CFArray returned by
            // SCDynamicStoreCopyProxies, so it is a valid CF object owned
            // by that array, which outlives this borrow; the Get rule is
            // the right one for a borrow that does not outlive it.
            let value = unsafe { CFType::wrap_under_get_rule(ptr as CFTypeRef) };
            // unsafe-code-exception: amendment-C13
            if let Some(s) = value.downcast::<CFString>() {
                raw.bypass.push(s.to_string());
            }
        }
    }
    raw
}

// --- Android ------------------------------------------------------------

/// The JVM system properties Android fills in from the active network's
/// proxy settings, as [`Raw`].
///
/// **The pure half, and it holds every rule** — the untestable half is
/// four JNI calls that fetch these five strings. `java.net`'s own
/// `DefaultProxySelector` reads exactly these, which is what makes them
/// the right source: a proxy this reads is the proxy every other client
/// in the same process takes.
///
/// `socksProxyHost` is read too, and named rather than dropped:
/// `SystemProxies::from_parts` refuses a mixed configuration by name, and
/// a SOCKS entry that never arrived could not be refused.
///
/// `#[cfg(any(target_os = "android", test))]` for the reason
/// [`from_wininet`] above carries it: the rules are compiled where they
/// are used and where they are tested, and nowhere else — without the
/// `test` arm they would be dead code on every host that runs them.
#[cfg(any(target_os = "android", test))]
pub(super) fn from_jvm_properties(get: impl Fn(&str) -> Option<String>) -> Raw {
    let mut raw = Raw::default();
    for (key, host, port) in [
        ("http", "http.proxyHost", "http.proxyPort"),
        ("https", "https.proxyHost", "https.proxyPort"),
        ("socks", "socksProxyHost", "socksProxyPort"),
    ] {
        let Some(h) = get(host)
            .map(|h| h.trim().to_owned())
            .filter(|h| !h.is_empty())
        else {
            continue;
        };
        // A missing port is the property's own absence rather than an
        // error, and it is left absent here: `from_parts` already reads
        // a bare host as the scheme's default port, which is the one
        // place that rule is written.
        let value = match get(port).and_then(|p| p.trim().parse::<u16>().ok()) {
            Some(p) => format!("{h}:{p}"),
            None => h,
        };
        raw.proxies.push((key.to_owned(), value));
    }
    // `http.nonProxyHosts` is `|`-separated, unlike `NO_PROXY`'s commas —
    // the separator is Java's and so is the wildcard syntax. Splitting is
    // all that happens here; whether a pattern can be honoured is
    // `translate`'s question, and one it already asks for macOS.
    if let Some(list) = get("http.nonProxyHosts") {
        raw.bypass.extend(
            list.split('|')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
        );
    }
    raw
}

#[cfg(target_os = "android")]
pub(super) fn platform() -> Raw {
    from_jvm_properties(crate::system::jvm::system_property)
}

#[cfg(not(any(windows, target_vendor = "apple", target_os = "android")))]
pub(super) fn platform() -> Raw {
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

    /// The JVM properties an Android device would have, as a lookup.
    ///
    /// A closure rather than a fixture type, because that is the seam:
    /// `platform()` passes JNI and this passes a table, and the rules
    /// under test cannot tell them apart — which is the whole reason the
    /// untestable half is four calls long.
    fn jvm(props: &[(&str, &str)]) -> Raw {
        from_jvm_properties(|name| {
            props
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_owned())
        })
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

    /// **Each scheme's pair becomes one entry**, which is the whole of
    /// what this reader does.
    #[test]
    fn each_scheme_becomes_a_host_and_port() {
        assert_eq!(
            jvm(&[
                ("http.proxyHost", "p.example"),
                ("http.proxyPort", "8080"),
                ("https.proxyHost", "s.example"),
                ("https.proxyPort", "8443"),
            ])
            .proxies,
            vec![
                ("http".to_owned(), "p.example:8080".to_owned()),
                ("https".to_owned(), "s.example:8443".to_owned()),
            ]
        );
    }

    /// **A host with no port is left without one**, rather than being
    /// given 80 or 443 here.
    ///
    /// `from_parts` already reads a bare host as the scheme's default,
    /// and that rule belongs in one place — this is `hclient-dns-system`'s
    /// split applied to a port: the reader reports, the translator
    /// decides.
    #[test]
    fn a_missing_or_unreadable_port_leaves_a_bare_host() {
        assert_eq!(
            jvm(&[("http.proxyHost", "p.example")]).proxies,
            vec![("http".to_owned(), "p.example".to_owned())]
        );
        // Not a number, so not a port. Dropping the proxy instead would
        // send traffic direct that the device routed.
        assert_eq!(
            jvm(&[("http.proxyHost", "p.example"), ("http.proxyPort", "-")]).proxies,
            vec![("http".to_owned(), "p.example".to_owned())]
        );
    }

    /// **A port with no host is nothing**, because a port cannot be
    /// connected to on its own — and an entry built from it would be a
    /// proxy at the empty host.
    #[test]
    fn a_port_without_a_host_is_not_an_entry() {
        assert!(jvm(&[("http.proxyPort", "8080")]).proxies.is_empty());
        // An empty or blank host is the same case: Android clears these
        // by setting them empty rather than by removing them.
        assert!(jvm(&[("http.proxyHost", "  ")]).proxies.is_empty());
    }

    /// **`socksProxyHost` is read and named**, not dropped.
    ///
    /// A transport holds one proxy protocol, so a device naming both is
    /// refused by name in `from_parts` — and a SOCKS entry that never
    /// reached it could not be refused. This is the reader's half of
    /// `SystemProxyRefused::MixedProtocols`.
    #[test]
    fn a_socks_proxy_is_reported_rather_than_dropped() {
        assert_eq!(
            jvm(&[("socksProxyHost", "s.example"), ("socksProxyPort", "1080")]).proxies,
            vec![("socks".to_owned(), "s.example:1080".to_owned())]
        );
    }

    /// **`|`, not `,`.** The separator is Java's, and a reader that split
    /// on commas would read one pattern where the device wrote three.
    #[test]
    fn non_proxy_hosts_split_on_the_bar() {
        assert_eq!(
            jvm(&[("http.nonProxyHosts", "localhost|127.*| *.example.com |")]).bypass,
            vec![
                "localhost".to_owned(),
                "127.*".to_owned(),
                "*.example.com".to_owned()
            ]
        );
    }

    /// Nothing set is nothing read, which is what lets the environment
    /// keep its precedence on a device that has one.
    #[test]
    fn an_unconfigured_device_reads_as_empty() {
        assert_eq!(jvm(&[]), Raw::default());
    }
}
