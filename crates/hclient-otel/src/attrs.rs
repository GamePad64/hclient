//! The attribute set of `docs/otel-design.md` §5a, with no socket, no
//! collector and no clock in it.
//!
//! `hclient-proto`'s bargain one crate up: every decision here — the
//! `url.full` redaction, the `{method}` span name, `resend_count` being a
//! sum — is a pure function of an `http::Request` or an `http::Response`,
//! so each is pinned by a unit test rather than by reading a span out of a
//! fixture. Nothing in this module knows which front will record it.

use hclient_core::unversioned::Attempt;
use hclient_core::{ErrorKind, Phase};

/// Everything §5a asks of a request, read once.
///
/// **Read once and carried**, rather than each front reaching back into
/// the `http::Request`, because `execute` gives the request away to the
/// inner transport and a front that wanted an attribute afterwards would
/// have nothing to read. It is the shape `hclient-core`'s `identify` doc
/// already prescribes one seam over for the same reason.
///
/// The borrows are of the request, so this lives only until it is handed
/// over — which is the whole of its life: every front copies what it
/// keeps.
#[derive(Debug)]
pub struct Request<'a> {
    /// `http.request.method`, normalised — and the span's name, which is
    /// the same value. One field rather than two, because two would be
    /// two chances for them to disagree about a thing §5a defines as
    /// identical.
    pub method: &'static str,
    /// `http.request.method_original`, `Some` exactly when `method` is
    /// `_OTHER`.
    pub method_original: Option<&'a str>,
    /// `url.full`, redacted.
    pub url_full: String,
    /// `server.address`.
    pub server_address: Option<&'a str>,
    /// `server.port`, defaulted from the scheme.
    pub server_port: Option<u16>,
    /// `user_agent.original`.
    pub user_agent: Option<&'a str>,
    /// `http.request.resend_count`, which is `hop + resend`.
    pub resend_count: Option<u32>,
    /// The split the sum destroys, for the two fields of our own.
    pub attempt: Option<Attempt>,
}

impl<'a> Request<'a> {
    /// Generic over the body because none of this reads one — which is
    /// what makes every decision in this module testable with an
    /// `http::Request<()>`.
    #[must_use]
    pub fn of<B>(req: &'a http::Request<B>) -> Self {
        let (method, method_original) = method(req.method());
        let uri = req.uri();
        Self {
            method,
            method_original,
            url_full: url_full(uri),
            server_address: uri.host(),
            server_port: server_port(uri),
            user_agent: user_agent(req.headers()),
            resend_count: resend_count(req.extensions()),
            attempt: attempt(req.extensions()),
        }
    }
}

/// Everything §5a asks of a response head.
#[derive(Debug)]
pub struct Head {
    /// `http.response.status_code`.
    pub status: http::StatusCode,
    /// `network.protocol.version`, **already gated** on
    /// `Capabilities::version_reported` by whoever built this — see
    /// [`protocol_version`] for why the gate is not optional.
    pub version: Option<&'static str>,
    /// `error.type`, which for a head is the status number.
    pub error_type: Option<String>,
}

impl Head {
    /// `version_reported` is the caller's to read, and it is a parameter
    /// rather than something this module looks up, because a
    /// `Capabilities` is not a fact about a response and this module has
    /// no transport.
    #[must_use]
    pub fn of<B>(res: &http::Response<B>, version_reported: bool) -> Self {
        let status = res.status();
        Self {
            status,
            version: version_reported
                .then(|| protocol_version(res.version()))
                .flatten(),
            error_type: error_type_for_status(status),
        }
    }
}

/// What an unrecognised method is reported as, spelled by the registry.
const OTHER: &str = "_OTHER";

/// `http.request.method`, normalised, and the original where the two
/// differ.
///
/// **The nine methods RFC 9110 registers are a closed set, and that is
/// what makes the span name safe.** OTel's rule is that an unknown method
/// MUST become `_OTHER`, with the original in
/// `http.request.method_original` — so a caller who invents a method per
/// request cannot put one in a span name, and the name stays one of ten
/// values for ever.
///
/// **`docs/otel-design.md` §5a got this wrong**, and the correction is
/// written here rather than only in the document: it recorded
/// `method_original` as *"only when we normalise a method, which this
/// client does not — so absent, and that is an answer"*. Normalisation is
/// not optional in the specification, it is a MUST, and skipping it is
/// what would have left the span name unbounded — which the same section
/// spends its next paragraph forbidding. The document is corrected.
///
/// The second half is `Some` **if and only if** the first is `_OTHER`,
/// which is the specification's own condition, and it is what a reader of
/// a span needs to tell *a method we do not know* from *a method the
/// caller could not have sent*.
///
/// Matched on the string rather than on `http::Method`'s nine constants,
/// because those are not structural-match constants and cannot appear in
/// a pattern at all — measured, `E0158`. The test beside it enumerates all
/// nine by name for the reason the constants would have given for free: a
/// tenth standard method would otherwise arrive silently.
#[must_use]
pub fn method(m: &http::Method) -> (&'static str, Option<&str>) {
    let known = match m.as_str() {
        "GET" => "GET",
        "HEAD" => "HEAD",
        "POST" => "POST",
        "PUT" => "PUT",
        "DELETE" => "DELETE",
        "CONNECT" => "CONNECT",
        "OPTIONS" => "OPTIONS",
        "TRACE" => "TRACE",
        "PATCH" => "PATCH",
        _ => return (OTHER, Some(m.as_str())),
    };
    (known, None)
}

/// The span's name: `{method}` and nothing else.
///
/// The specification allows `{method} {target}` **only** where the target
/// is low cardinality, and an HTTP client has no route template — it has a
/// URL, which is one distinct name per distinct URL and a cardinality
/// blow-up in whatever aggregates the spans. So the name is the normalised
/// method, which is why the previous function normalises.
#[must_use]
pub fn span_name(m: &http::Method) -> &'static str {
    method(m).0
}

/// The query-string keys whose values are credentials, from the
/// specification's own list.
///
/// **The design document has only the userinfo half**, and this is the
/// other one it owes: a presigned S3 or GCS URL carries its signature in
/// the query, so a span of a request to one is a place a bearer credential
/// travels to a collector — which is exactly the sentence §5a uses to
/// justify redacting userinfo, applied to the commoner case. The key is
/// kept and the value replaced, because a span that cannot say *this URL
/// was presigned* has lost a fact rather than protected one.
const SENSITIVE_QUERY_KEYS: &[&str] = &[
    "X-Amz-Signature",
    "X-Amz-Credential",
    "X-Amz-Security-Token",
    "AWSAccessKeyId",
    "Signature",
    "sig",
    "X-Goog-Signature",
];

/// The literal the specification names, for both halves of the redaction.
const REDACTED: &str = "REDACTED";

/// `url.full`, with the credentials taken out.
///
/// Two redactions, and neither is optional: `https://u:p@host/` becomes
/// `https://REDACTED:REDACTED@host/` (a MUST), and a sensitive query
/// value becomes `key=REDACTED` (a SHOULD, and this workspace's own rule
/// about where credentials may travel makes it one here).
///
/// It builds a `String` rather than borrowing, and that is the cost of the
/// attribute rather than a choice: `http::Uri`'s `Display` is the only
/// thing that reassembles scheme, authority and path-and-query, and the
/// redacted form is by construction not a substring of the original.
#[must_use]
pub fn url_full(uri: &http::Uri) -> String {
    let mut out = String::new();
    if let Some(scheme) = uri.scheme_str() {
        out.push_str(scheme);
        out.push_str("://");
    }
    if let Some(authority) = uri.authority() {
        // `Authority::as_str` keeps the userinfo where the caller wrote
        // one; `host()` and `port()` are what is left after it. Splitting
        // on the LAST `@` rather than the first: RFC 3986 allows a `@` in
        // the userinfo itself, and a host may not contain one.
        let s = authority.as_str();
        match s.rfind('@') {
            Some(at) => {
                out.push_str(REDACTED);
                out.push(':');
                out.push_str(REDACTED);
                out.push('@');
                out.push_str(&s[at + 1..]);
            }
            None => out.push_str(s),
        }
    }
    out.push_str(uri.path());
    if let Some(query) = uri.query() {
        out.push('?');
        redact_query_into(&mut out, query);
    }
    out
}

/// `key=value&key=value`, with the values on the list replaced.
///
/// Deliberately not a parse: a query string is whatever the caller put
/// there, and a form that does not split on `&` and `=` is left alone
/// verbatim rather than being reshaped into one that does. The key
/// comparison is case-insensitive because `sig` and `Sig` are the same
/// parameter to every service that reads one.
fn redact_query_into(out: &mut String, query: &str) {
    for (i, pair) in query.split('&').enumerate() {
        if i > 0 {
            out.push('&');
        }
        match pair.split_once('=') {
            Some((key, _)) if is_sensitive_query_key(key) => {
                out.push_str(key);
                out.push('=');
                out.push_str(REDACTED);
            }
            _ => out.push_str(pair),
        }
    }
}

fn is_sensitive_query_key(key: &str) -> bool {
    SENSITIVE_QUERY_KEYS
        .iter()
        .any(|k| k.eq_ignore_ascii_case(key))
}

/// `server.port`, defaulted from the scheme where the URI does not say.
///
/// The attribute is Required, and a URI that names no port has one all the
/// same — the one the connector will use. Reporting nothing for
/// `https://example.com/` would leave the commonest request in the world
/// without a Required attribute.
#[must_use]
pub fn server_port(uri: &http::Uri) -> Option<u16> {
    uri.port_u16().or_else(|| match uri.scheme_str() {
        Some("https") | Some("wss") => Some(443),
        Some("http") | Some("ws") => Some(80),
        _ => None,
    })
}

/// `http.request.resend_count` — **`hop + resend`, not `resend`.**
///
/// The registry's words are that the count is *"the ordinal number of
/// request resending attempt (for any reason, including redirects)"* and
/// that it is updated *"regardless of what was the cause of the
/// resending (e.g. redirection, authorization failure, 503 Server
/// Unavailable, network issues, or any other)"*.
///
/// [`Attempt`] splits the same total on a line OTel does not draw:
/// `hop` counts redirects and `resend` counts everything else about one
/// hop. Reading `resend` alone is the mapping the field names invite, and
/// it reports `0` for the third hop of a redirect chain — which is exactly
/// the case the attribute exists for.
///
/// `None` where there is no [`Attempt`] at all, which is every transport
/// driven without an `hclient::Client` above it, and `None` at zero,
/// because the attribute's own requirement level is *Recommended: if and
/// only if request was retried*.
#[must_use]
pub fn resend_count(extensions: &http::Extensions) -> Option<u32> {
    let a = extensions.get::<Attempt>()?;
    let total = u32::from(a.hop) + u32::from(a.resend);
    (total > 0).then_some(total)
}

/// The split [`resend_count`] destroys, kept as fields of our own.
///
/// *Third send, first hop* and *first send, third hop* are different
/// failures and the sum cannot tell them apart, so both halves travel
/// beside the standard attribute under names that are visibly not OTel's.
#[must_use]
pub fn attempt(extensions: &http::Extensions) -> Option<Attempt> {
    extensions.get::<Attempt>().copied()
}

/// `network.protocol.version`, as the OTel registry spells it.
///
/// **Only ever called where `Capabilities::version_reported` is true** —
/// the biconditional `Head::version` already established one seam over.
/// `hclient-fetch` and `hclient-wasi` neither select the protocol nor
/// learn it, so the value on their responses is `http`'s builder default
/// standing in for a fact nobody observed, and an attribute set from it
/// would report a browser's h2 and h3 traffic as HTTP/1.1 — a *wrong*
/// answer rather than a missing one.
///
/// `None` for a version this crate has never heard of, rather than a
/// `Debug` rendering: the registry's values are `1.0`, `1.1`, `2`, `3`,
/// and a tenth spelling invented here would be a distinct series in
/// whatever aggregates it.
#[must_use]
pub fn protocol_version(v: http::Version) -> Option<&'static str> {
    Some(match v {
        http::Version::HTTP_09 => "0.9",
        http::Version::HTTP_10 => "1.0",
        http::Version::HTTP_11 => "1.1",
        http::Version::HTTP_2 => "2",
        http::Version::HTTP_3 => "3",
        _ => return None,
    })
}

/// `user_agent.original`, where it is one string this crate can read.
///
/// A header sent more than once, or one that is not UTF-8, reports
/// nothing: the attribute is a string, and inventing a joining rule for a
/// header no server joins would be a value nobody sent.
#[must_use]
pub fn user_agent(headers: &http::HeaderMap) -> Option<&str> {
    headers.get(http::header::USER_AGENT)?.to_str().ok()
}

/// Whether a client span with this status is an error.
///
/// **4xx as well as 5xx**, which is a client-span rule — for a server span
/// the specification makes 4xx MUST-be-unset, because a client's mistake
/// is not the server's failure. It also differs from
/// `hclient::Response::error_for_status`, deliberately: that method exists
/// because a `404` is a normal answer for about half the requests ever
/// made and the caller decides, where a span records what the exchange
/// *was*. Two different questions, and the span is not the place to apply
/// the caller's policy.
#[must_use]
pub fn is_error_status(status: http::StatusCode) -> bool {
    status.as_u16() >= 400
}

/// `error.type` for an exchange that produced a response.
///
/// **The design document does not have this half.** §5a's table gives
/// `error.type` one source, [`ErrorKind`]'s variant name, which covers a
/// request that failed before a status existed. The specification's rule
/// has a second arm: *"If response status code was sent or received and
/// status indicates an error according to HTTP span status definition,
/// `error.type` SHOULD be set to the status code number (represented as a
/// string)"*. Without it, a span for a `500` carries `Error` status and no
/// `error.type` at all — and `error.type` is what an aggregation groups
/// by, so the commonest error a client sees would be the one it could not
/// group.
#[must_use]
pub fn error_type_for_status(status: http::StatusCode) -> Option<String> {
    is_error_status(status).then(|| status.as_u16().to_string())
}

/// `error.type` for an exchange that produced no response.
///
/// The variant name of [`ErrorKind`], which is why that type is an enum
/// rather than a string: the attribute's whole requirement is that it be
/// low cardinality, and a `Display` rendering of a DNS failure carries the
/// hostname.
///
/// **`Timeout` carries its [`Phase`]** and the others do not, because the
/// phase is the fact a dashboard is built on — *connects time out* and
/// *bodies stall* are different incidents — and five values is still low
/// cardinality. A caller who wants the phase off the error instead has it,
/// but not after the span has been exported.
#[must_use]
pub fn error_type(kind: &ErrorKind) -> &'static str {
    match kind {
        ErrorKind::Resolve => "Resolve",
        ErrorKind::Connect => "Connect",
        ErrorKind::Tls => "Tls",
        ErrorKind::Redirect => "Redirect",
        ErrorKind::Timeout(p) => match p {
            Phase::Resolve => "Timeout.Resolve",
            Phase::Connect => "Timeout.Connect",
            Phase::FirstByte => "Timeout.FirstByte",
            Phase::BetweenBytes => "Timeout.BetweenBytes",
            Phase::Total => "Timeout.Total",
            // `Phase` is `#[non_exhaustive]`, so this arm is the
            // compiler's demand rather than a case anybody can reach from
            // this crate today. `Timeout` alone is the honest answer for a
            // phase that did not exist when this was written — under-
            // reporting the detail rather than inventing a name for it.
            _ => "Timeout",
        },
        ErrorKind::Body => "Body",
        ErrorKind::Decode => "Decode",
        ErrorKind::Status => "Status",
        ErrorKind::Unsupported => "Unsupported",
        ErrorKind::Cancelled => "Cancelled",
        ErrorKind::Other => "Other",
        // Same shape one enum up, and the same reason it is not
        // `unreachable!`: `ErrorKind` is `#[non_exhaustive]`, so a variant
        // added to `hclient-core` compiles here and must produce a value
        // rather than a panic in a decorator whose whole job is to observe.
        _ => "Other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hclient_core::unversioned::{Attempt, RequestId};

    fn uri(s: &str) -> http::Uri {
        s.parse().expect("test URI")
    }

    #[test]
    fn every_method_http_names_is_known_and_nothing_else_is() {
        // Enumerated by name rather than by iterating, because the point
        // of the list is that it is closed: a tenth `http::Method`
        // constant would arrive silently and this is what says so.
        for m in [
            http::Method::GET,
            http::Method::HEAD,
            http::Method::POST,
            http::Method::PUT,
            http::Method::DELETE,
            http::Method::CONNECT,
            http::Method::OPTIONS,
            http::Method::TRACE,
            http::Method::PATCH,
        ] {
            let (normalised, original) = method(&m);
            assert_eq!(normalised, m.as_str());
            assert_eq!(original, None, "{m} is known and needs no original");
        }
    }

    #[test]
    fn an_unknown_method_becomes_other_and_keeps_the_original() {
        let m = http::Method::from_bytes(b"PROPFIND").expect("valid token");
        assert_eq!(method(&m), ("_OTHER", Some("PROPFIND")));
        assert_eq!(span_name(&m), "_OTHER");
    }

    #[test]
    fn the_span_name_is_the_method_alone() {
        assert_eq!(span_name(&http::Method::GET), "GET");
        // The URL is not in it, at any length. This is the assertion the
        // cardinality argument rests on.
        assert!(!span_name(&http::Method::GET).contains('/'));
    }

    #[test]
    fn userinfo_is_redacted() {
        assert_eq!(
            url_full(&uri("https://alice:hunter2@example.com/x?y=1")),
            "https://REDACTED:REDACTED@example.com/x?y=1"
        );
        // A bare username, no password, is still a credential position.
        assert_eq!(
            url_full(&uri("https://alice@example.com/")),
            "https://REDACTED:REDACTED@example.com/"
        );
        // And an `@` inside the userinfo does not move the split.
        assert_eq!(
            url_full(&uri("https://a@b:p@example.com/")),
            "https://REDACTED:REDACTED@example.com/"
        );
    }

    #[test]
    fn a_url_with_no_credentials_survives_unchanged() {
        for s in [
            "https://example.com/",
            "https://example.com:8443/a/b?c=1&d=2",
            "http://example.com/%20space",
        ] {
            assert_eq!(url_full(&uri(s)), s, "{s}");
        }
    }

    #[test]
    fn a_signed_query_value_is_redacted_and_its_key_is_kept() {
        assert_eq!(
            url_full(&uri(
                "https://b.s3.amazonaws.com/k?X-Amz-Credential=AK&X-Amz-Signature=deadbeef&x=1"
            )),
            "https://b.s3.amazonaws.com/k?X-Amz-Credential=REDACTED&X-Amz-Signature=REDACTED&x=1"
        );
        // Case-insensitively, because a service that reads `sig` reads
        // `Sig`.
        assert_eq!(
            url_full(&uri("https://e.test/?SIG=abc")),
            "https://e.test/?SIG=REDACTED"
        );
        // A query that is not `k=v` at all is left verbatim rather than
        // reshaped into one that is.
        assert_eq!(
            url_full(&uri("https://e.test/?opaque")),
            "https://e.test/?opaque"
        );
    }

    #[test]
    fn the_port_is_defaulted_from_the_scheme() {
        assert_eq!(server_port(&uri("https://e.test/")), Some(443));
        assert_eq!(server_port(&uri("http://e.test/")), Some(80));
        assert_eq!(server_port(&uri("https://e.test:8443/")), Some(8443));
    }

    #[test]
    fn resend_count_is_hop_plus_resend() {
        let mut e = http::Extensions::new();
        assert_eq!(resend_count(&e), None, "no Attempt, nothing to report");

        let a = Attempt::new(RequestId::next());
        e.insert(a);
        assert_eq!(resend_count(&e), None, "first send of the first hop");

        // Three hops of a redirect chain: the third send reports 2, and
        // reading `resend` alone would report nothing at all.
        e.insert(a.next_hop().next_hop());
        assert_eq!(resend_count(&e), Some(2));

        // And the two halves add rather than one shadowing the other.
        e.insert(a.next_hop().resent());
        assert_eq!(resend_count(&e), Some(2));
    }

    #[test]
    fn four_xx_is_an_error_on_a_client_span() {
        assert!(!is_error_status(http::StatusCode::OK));
        assert!(!is_error_status(http::StatusCode::FOUND));
        assert!(is_error_status(http::StatusCode::NOT_FOUND));
        assert!(is_error_status(http::StatusCode::INTERNAL_SERVER_ERROR));
    }

    #[test]
    fn error_type_from_a_status_is_the_number() {
        assert_eq!(
            error_type_for_status(http::StatusCode::NOT_FOUND).as_deref(),
            Some("404")
        );
        assert_eq!(error_type_for_status(http::StatusCode::OK), None);
    }

    #[test]
    fn error_type_from_a_kind_is_low_cardinality_and_keeps_the_phase() {
        assert_eq!(error_type(&ErrorKind::Connect), "Connect");
        assert_eq!(
            error_type(&ErrorKind::Timeout(Phase::Connect)),
            "Timeout.Connect"
        );
        assert_eq!(
            error_type(&ErrorKind::Timeout(Phase::Total)),
            "Timeout.Total"
        );
    }

    #[test]
    fn a_version_this_crate_does_not_know_reports_nothing() {
        assert_eq!(protocol_version(http::Version::HTTP_11), Some("1.1"));
        assert_eq!(protocol_version(http::Version::HTTP_2), Some("2"));
        assert_eq!(protocol_version(http::Version::HTTP_3), Some("3"));
    }

    #[test]
    fn a_user_agent_that_is_not_one_string_reports_nothing() {
        let mut h = http::HeaderMap::new();
        assert_eq!(user_agent(&h), None);
        h.insert(http::header::USER_AGENT, "hclient/0.1".parse().unwrap());
        assert_eq!(user_agent(&h), Some("hclient/0.1"));
        h.insert(
            http::header::USER_AGENT,
            http::HeaderValue::from_bytes(&[0xff, 0xfe]).unwrap(),
        );
        assert_eq!(user_agent(&h), None);
    }
}
