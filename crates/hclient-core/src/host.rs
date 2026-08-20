//! Where a URI's authority stops being URI syntax.

/// The host a URI names, with an IPv6 literal's brackets removed.
///
/// `[2001:db8::1]` becomes `2001:db8::1`; every other host — a name, an
/// IPv4 literal, an already-bare v6 address — comes back untouched.
///
/// # Why this exists at all, and why in this crate
///
/// `http::Uri::host()` returns an IPv6 literal **with its brackets**,
/// because that is what the URI says: RFC 3986 §3.2.2 puts `IP-literal =
/// "[" ( IPv6address / IPvFuture ) "]"` in the *authority*'s grammar, not
/// in the host's. Nothing outside a URI wants them, and everything outside
/// a URI is where this workspace kept meeting the same failure:
///
/// * `str::parse::<IpAddr>()` rejects `[::1]`, so a resolver's literal
///   shortcut falls through and asks DNS about a string no zone contains.
/// * `rustls_pki_types::ServerName::try_from` rejects `[::1]` as **both** a
///   DNS name and an address, so a TLS or QUIC handshake fails with
///   `invalid dns name` before a byte of the exchange happens.
///
/// The duty is the **caller's**, not the backend's — see
/// `hclient_tls::TlsRequest::server_name`, whose doc says so at the seam
/// where it matters. A backend that stripped defensively would be the
/// second place normalising, and two places normalising is how they drift;
/// worse, it would have to guess, since a backend cannot tell a host that
/// came from a URI from one a caller built by hand.
///
/// This crate is the home because it is the only one every consumer
/// already has. `hclient-native` and `hclient-h3` both hold a `Uri` and
/// both feed a TLS seam; `hclient-dns` and `hclient-dns-doh` both parse
/// literals; `hclient-tls`, whose doc has to name the duty, depends on
/// this crate and not on any of them. Putting it in `hclient-dns` would
/// make a TLS server name reach through a resolver crate for a fact about
/// URI syntax, and putting it in `hclient-tls` would do the mirror image
/// to a resolver.
///
/// # What it does not do
///
/// It is not a validator and not a parser. `[` alone is not a bracketed
/// host and comes back as `[`; `[]` is a bracketed *empty* host and comes
/// back as the empty string, which every consumer downstream then refuses
/// — `ServerName::try_from("")` and `"".parse::<IpAddr>()` both fail —
/// rather than being quietly patched up here. Percent-encoding, ports and
/// userinfo are `http::Uri`'s business and have already been removed by
/// the time `Uri::host()` has answered.
///
/// # What must NOT be stripped
///
/// The `Host` header and HTTP/2's `:authority` are authority syntax, so
/// they keep their brackets (RFC 9110 §7.2 — `Host = uri-host [ ":" port
/// ]`). Only the step out of URI-land takes them off.
#[must_use]
pub fn bare_host(host: &str) -> &str {
    // Both ends, or neither: `strip_prefix` alone would turn the malformed
    // `[::1` into `::1` and hand a plausible-looking address to a caller
    // that was given a broken URI.
    host.strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(host)
}

#[cfg(test)]
mod tests {
    use super::bare_host;

    /// The case the function exists for, and the four ways of getting it
    /// wrong that a mutation reaches: a strip that fires on every host, a
    /// strip that takes only the prefix, one that trims repeatedly, and one
    /// that slices without checking.
    #[test]
    fn the_brackets_come_off_a_bracketed_host_and_nothing_else() {
        assert_eq!(bare_host("[2001:db8::1]"), "2001:db8::1");
        assert_eq!(bare_host("[::1]"), "::1");

        // Not bracketed: untouched, character for character. `example.com`
        // becoming `xample.co` is the mutation this row is here for.
        assert_eq!(bare_host("example.com"), "example.com");
        assert_eq!(bare_host("127.0.0.1"), "127.0.0.1");
        assert_eq!(bare_host("::1"), "::1");
        assert_eq!(bare_host(""), "");

        // One pair, not as many as there are. A host is bracketed once.
        assert_eq!(bare_host("[[::1]]"), "[::1]");
    }

    /// Half a bracket is not a bracketed host. Both of these are what a
    /// slice-without-checking implementation panics on, and what a
    /// `trim_matches` one silently empties.
    #[test]
    fn a_lone_bracket_is_a_host_like_any_other() {
        assert_eq!(bare_host("["), "[");
        assert_eq!(bare_host("]"), "]");
        assert_eq!(bare_host("[::1"), "[::1");
        assert_eq!(bare_host("::1]"), "::1]");
    }

    /// A bracketed empty host is an empty host, and stays empty rather
    /// than being turned back into `[]` to look harmless. Whoever receives
    /// it refuses it: neither `ServerName::try_from` nor
    /// `str::parse::<IpAddr>` accepts `""`.
    #[test]
    fn an_empty_bracketed_host_is_empty() {
        assert_eq!(bare_host("[]"), "");
        assert!("".parse::<std::net::IpAddr>().is_err());
    }
}
