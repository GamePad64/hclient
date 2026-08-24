//! The four rules that decide where a cookie goes: domain-match
//! (RFC 6265bis §5.1.3), path-match (§5.1.4), the default path (§5.1.4),
//! and what counts as a secure request.
//!
//! Small enough to read in one screen, and every one of them is a *refusal*
//! rule — each says which requests must **not** see a cookie. A version of
//! this file with the boundary checks deleted still returns cookies to the
//! host that set them, which is why `tests/refusals.rs` is written the way
//! it is.

use std::net::IpAddr;

/// RFC 6265bis §5.1.3.
///
/// `host` domain-matches `domain` when they are equal, or when `host` ends
/// with `domain` **at a label boundary** and `host` is not an IP address.
///
/// The label boundary is the whole rule. Without the `.` check,
/// `notexample.com` ends with `example.com` and would receive every cookie
/// scoped to `example.com` — a sibling-domain leak that costs one line to
/// prevent and reads as a redundant condition to anyone simplifying this
/// function.
pub(crate) fn domain_matches(host: &str, domain: &str) -> bool {
    if host == domain {
        return true;
    }
    let Some(prefix_len) = host.len().checked_sub(domain.len()) else {
        return false;
    };
    if prefix_len == 0 || !host.ends_with(domain) {
        return false;
    }
    if host.as_bytes()[prefix_len - 1] != b'.' {
        return false;
    }
    // §5.1.3's last condition. An IP literal has no labels to be a suffix
    // of: `1.2.3.4` must not receive cookies scoped to `2.3.4`.
    !is_ip_literal(host)
}

/// RFC 6265bis §5.1.4.
///
/// A string prefix is not a path prefix. `/foo` matches `/foo`, `/foo/`
/// and `/foo/bar`; it must not match `/foobar`, which is a different
/// resource that merely starts with the same bytes.
pub(crate) fn path_matches(request_path: &str, cookie_path: &str) -> bool {
    if request_path == cookie_path {
        return true;
    }
    if !request_path.starts_with(cookie_path) {
        return false;
    }
    // `Path=/foo/` already ends at a boundary; `Path=/foo` needs the
    // request to continue with `/` rather than with any other byte.
    if cookie_path.ends_with('/') {
        return true;
    }
    request_path.as_bytes()[cookie_path.len()] == b'/'
}

/// RFC 6265bis §5.1.4's "default-path", used when a `Set-Cookie` carries no
/// usable `Path`.
///
/// Everything up to, but not including, the rightmost `/` — so a cookie set
/// by `/a/b/c` defaults to `/a/b`, and one set by `/a` defaults to `/`.
pub(crate) fn default_path(uri_path: &str) -> String {
    if !uri_path.starts_with('/') {
        return "/".to_owned();
    }
    match uri_path.rfind('/') {
        Some(0) | None => "/".to_owned(),
        Some(i) => uri_path[..i].to_owned(),
    }
}

/// The request path a cookie is matched against: the URI's path, with an
/// empty one read as `/`.
pub(crate) fn request_path(uri: &http::Uri) -> &str {
    match uri.path() {
        "" => "/",
        p => p,
    }
}

/// RFC 6265bis §5.1.2's "canonicalized host name", as far as it applies
/// here: lowercased, one trailing dot removed, and an IPv6 literal
/// unwrapped from the brackets `http::Uri` keeps it in.
///
/// No IDN conversion. A non-ASCII host cannot reach an `http::Uri` in the
/// first place, and in this workspace the A-label conversion happens
/// upstream, in `hclient_proto::uri::parse`, where every backend goes
/// through it — doing it a second time here would be a second place for
/// the two answers to differ.
pub(crate) fn canonical_host(uri: &http::Uri) -> Option<String> {
    let host = uri.host()?;
    let host = host.strip_suffix('.').unwrap_or(host);
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if host.is_empty() {
        return None;
    }
    Some(host.to_ascii_lowercase())
}

pub(crate) fn is_ip_literal(host: &str) -> bool {
    host.parse::<IpAddr>().is_ok()
}

/// Whether a `Secure` cookie may be sent to, or stored from, this URI.
///
/// `https` and `wss`, plus the loopback hosts. The loopback part is not
/// decoration: without it every `Secure` cookie silently vanishes against
/// `http://localhost`, which is where a good deal of development happens,
/// and browsers do treat those origins as potentially trustworthy
/// (W3C "Secure Contexts" §3.1).
///
/// What is deliberately *not* implemented from that algorithm: `file:`,
/// `data:`, `blob:`, and the part no library can have — origins an
/// administrator or a browser flag has declared trustworthy. A caller who
/// needs those needs a browser, and a browser owns its own jar
/// (`Capabilities::owns_cookie_jar`).
pub(crate) fn is_secure_request(uri: &http::Uri) -> bool {
    if matches!(uri.scheme_str(), Some("https" | "wss")) {
        return true;
    }
    match canonical_host(uri) {
        Some(host) => is_loopback(&host),
        None => false,
    }
}

fn is_loopback(host: &str) -> bool {
    if host == "localhost" || host.ends_with(".localhost") {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(ip) => ip.is_loopback(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case("example.com", "example.com", true)]
    #[case("www.example.com", "example.com", true)]
    #[case("a.b.example.com", "example.com", true)]
    // The sibling-domain leak: a string suffix that is not a label suffix.
    #[case("notexample.com", "example.com", false)]
    #[case("wwwexample.com", "example.com", false)]
    // The other direction: the cookie's domain is longer than the host.
    #[case("example.com", "www.example.com", false)]
    // An IP literal is only ever equal to itself.
    #[case("1.2.3.4", "2.3.4", false)]
    #[case("1.2.3.4", "1.2.3.4", true)]
    fn domain_match(#[case] host: &str, #[case] domain: &str, #[case] expected: bool) {
        assert_eq!(domain_matches(host, domain), expected);
    }

    #[rstest]
    #[case("/foo", "/foo", true)]
    #[case("/foo/", "/foo", true)]
    #[case("/foo/bar", "/foo", true)]
    #[case("/foo/bar", "/foo/", true)]
    // A string prefix that is not a path prefix.
    #[case("/foobar", "/foo", false)]
    #[case("/foo.html", "/foo", false)]
    // `/` is a prefix of everything, by the trailing-slash arm.
    #[case("/anything/at/all", "/", true)]
    #[case("/", "/foo", false)]
    fn path_match(#[case] request: &str, #[case] cookie: &str, #[case] expected: bool) {
        assert_eq!(path_matches(request, cookie), expected);
    }

    #[rstest]
    #[case("/a/b/c", "/a/b")]
    #[case("/a/b/", "/a/b")]
    #[case("/a", "/")]
    #[case("/", "/")]
    #[case("", "/")]
    #[case("relative", "/")]
    fn defaults(#[case] uri_path: &str, #[case] expected: &str) {
        assert_eq!(default_path(uri_path), expected);
    }
}
