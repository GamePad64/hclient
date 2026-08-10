//! The one thing every other test in this crate cannot be: a query to a
//! DoH server nobody here wrote.
//!
//! # Why this file exists
//!
//! Every other test in this crate answers itself. The fixture in
//! `tests/support` and the parser in `src/wire.rs` were written by the same
//! author, from the same reading of the same RFCs, which is precisely the
//! arrangement in which a fixture agrees with a bug. `docs/v03-acceptance.
//! md` recorded that as **"no live DoH endpoint has been queried"** in four
//! separate entries, and the most valuable of them was not about DNS at
//! all: `Doh::pinned` takes an **IP literal**, so every pinned deployment
//! makes a handshake against a certificate presented for an *IP address*,
//! and nothing in this workspace had ever done one.
//!
//! # It is not part of `just test`, and that is the rule rather than a
//! preference
//!
//! A test that needs the internet is a test that goes red for reasons that
//! are not ours. `just test` must stay hermetic, so every test below asks
//! [`live`] first and **returns with a `NOTICE`** unless
//! `HTTP_NG_LIVE_DOH` is set — which `just test-doh-live` sets and nothing
//! else does. `HTTP_NG_REQUIRE_NETWORK` turns every one of those skips into
//! a panic, for a runner that promised connectivity; it is the same shape
//! as `HTTP_NG_REQUIRE_WASMTIME` and `HTTP_NG_REQUIRE_TUNTAP`, and for the
//! same reason — a skip nobody notices is worse than a failure.
//!
//! **Every test that actually ran prints [`RECEIPT`] as its last act**, and
//! `just test-doh-live` refuses a run with fewer than it expects. That is
//! the belt against the failure this repository has been bitten by twice: a
//! gate that returns "skip" for the wrong reason turns the whole file into
//! nine green tests that made no request at all, and nothing in a `PASS`
//! line says otherwise.
//!
//! # Which endpoints, and why more than one
//!
//! **Cloudflare (`1.1.1.1`) and Google (`8.8.8.8`)**, because one
//! operator's behaviour is that operator's, not the protocol's — and the
//! two are independent implementations with independent certificate
//! issuers. **Quad9 (`9.9.9.9`) is a third, used by exactly one test**, and
//! the reason is itself a finding: over HTTP/1.1 it does not answer DNS at
//! all (see `an_endpoint_that_demands_http2_is_a_status_error_and_not_a_
//! parse_error`).
//!
//! All three are public resolvers with published, stable anycast addresses,
//! which is what `Doh::pinned` is for. No test here sends anything a
//! passive observer could tie to this machine beyond the names below, all
//! of which are the operators' own or a public ECH testbed.
//!
//! # What is checked against what
//!
//! Nothing below compares this crate against this crate. The oracles are
//! `dig` (BIND's parser, over plain UDP DNS to the same operator) and, for
//! the TTL, a query built and parsed **by hand** in this file over the same
//! transport — the same rule `tests/support` follows and for the same
//! reason.
#![cfg(not(target_family = "wasm"))]

mod support;

use futures_util::StreamExt;
use http_body_util::BodyExt;
use http_ng_core::RequestBody;
use http_ng_core::unversioned::Transport;
use http_ng_dns::{IpLiteralOnly, Resolve, ResolvedAddr, SvcbEndpoint};
use http_ng_dns_doh::{Doh, DohError};
use http_ng_native::Native;
use http_ng_rt_tokio::Tokio;
use http_ng_tls_rustls::Rustls;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::time::Duration;
use support::{TYPE_A, name_wire};

// ── the gate ────────────────────────────────────────────────────────────

/// Set by `just test-doh-live` and by nothing else. Its absence is what
/// keeps `just test` hermetic.
const OPT_IN: &str = "HTTP_NG_LIVE_DOH";

/// Set by a job that has promised outbound connectivity. Turns every skip
/// below into a panic — the shape `HTTP_NG_REQUIRE_WASMTIME` established.
const REQUIRE_MARKER: &str = "HTTP_NG_REQUIRE_NETWORK";

/// Printed by every test that reached a real server. `just test-doh-live`
/// counts these and fails when there are too few, so that a gate returning
/// "skip" for the wrong reason cannot leave a green run behind.
const RECEIPT: &str = "LIVE-DOH-RAN";

/// How long a TCP connect to an endpoint may take before this environment
/// is declared to have no route to it. Generous: a slow link is not the
/// same as no link, and the distinction this constant draws is between
/// "cannot answer" and "answered wrongly".
const PROBE: Duration = Duration::from_secs(5);

/// Attempts per network operation, in the gate and in every exchange.
///
/// **Measured before it was chosen, on the host that wrote this file.** 240
/// plain TCP connects to these three addresses, in three cadences: **17
/// lost, uniformly spread** — not bursty, not correlated with rate, and
/// nothing to do with DoH, HTTP or this crate. One SYN is therefore not
/// evidence of anything, and a suite of nine tests opening sixteen
/// connections would be red about as often as green on a single attempt.
/// Four attempts with [`BACKOFF`] between them takes a per-exchange ~7% to
/// under one in thirty thousand.
///
/// **It retries the network and never an assertion.** Every `assert!` in
/// this file is outside the loop, on a value the loop has already produced,
/// so a wrong answer fails on the first look and only an exchange that did
/// not happen is tried again — `raw_once` returns the retryable flag from
/// the error's *kind*, and hands every answer a server gave, however
/// unwelcome, back as `Ok`. That distinction is the whole licence for a
/// retry to exist here: a test that hid a *disagreement* behind one would
/// be worthless.
const ATTEMPTS: usize = 4;

/// Between attempts. Not for the server's sake — the loss measured above is
/// uniform, so a pause buys nothing from a rate limiter that is not there.
/// It is for the kernel's: Linux retransmits a SYN at 1 s and 3 s, so a
/// fresh connect started immediately races the same lossy window the last
/// one lost in.
const BACKOFF: Duration = Duration::from_millis(250);

/// The `connect` bound this suite gives its exchanges, replacing `Doh`'s own
/// 2 s default.
///
/// The default is right for a resolver and wrong for this file. Under the
/// ~7% loss measured above, 2 s covers exactly one of the kernel's SYN
/// retransmits (1 s, then 3 s, then 7 s); a lost first SYN therefore times
/// out *inside* the bound rather than being recovered by it, and the suite
/// ends up measuring the uplink instead of the crate. 5 s covers two.
///
/// Set through the public `Doh::timeouts`, which is the knob a deployment on
/// a bad link would reach for too.
const LIVE_TIMEOUTS: http_ng_core::Timeouts = http_ng_core::Timeouts {
    connect: Some(Duration::from_secs(5)),
    first_byte: Some(Duration::from_secs(5)),
    between_bytes: Some(Duration::from_secs(5)),
};

/// A public DoH endpoint, and who runs it.
#[derive(Debug, Clone, Copy)]
struct Endpoint {
    operator: &'static str,
    uri: &'static str,
    addr: &'static str,
}

const CLOUDFLARE: Endpoint = Endpoint {
    operator: "Cloudflare",
    uri: "https://1.1.1.1/dns-query",
    addr: "1.1.1.1:443",
};
const GOOGLE: Endpoint = Endpoint {
    operator: "Google",
    uri: "https://8.8.8.8/dns-query",
    addr: "8.8.8.8:443",
};
/// Quad9. One test only — see its own doc comment.
const QUAD9: Endpoint = Endpoint {
    operator: "Quad9",
    uri: "https://9.9.9.9/dns-query",
    addr: "9.9.9.9:443",
};

/// The two operators every general claim below is made against.
const OPERATORS: [Endpoint; 2] = [CLOUDFLARE, GOOGLE];

/// May this test reach the public internet, and is this endpoint routable?
///
/// `None` means "this environment cannot answer the question" and the
/// caller must `return` — not assert something weaker, and not assert
/// nothing while still printing [`RECEIPT`].
///
/// Under [`REQUIRE_MARKER`] there is no `None`: a runner that promised
/// connectivity and cannot connect is broken, and saying so loudly is the
/// entire purpose of the marker.
#[must_use]
fn live(test: &str, ep: Endpoint) -> Option<Endpoint> {
    let required = std::env::var_os(REQUIRE_MARKER).is_some();
    if !required && std::env::var_os(OPT_IN).is_none() {
        eprintln!(
            "NOTICE: neither `{OPT_IN}` nor `{REQUIRE_MARKER}` is set — skipping the live DoH \
             test `{test}`. `just test` is hermetic on purpose; `just test-doh-live` is the \
             recipe that reaches {} and the other public resolvers.",
            ep.operator
        );
        return None;
    }
    let addr: SocketAddr = ep.addr.parse().expect("a literal address:port");
    let mut failure = None;
    for _ in 0..ATTEMPTS {
        match TcpStream::connect_timeout(&addr, PROBE) {
            Ok(_) => return Some(ep),
            Err(e) => failure = Some(e),
        }
        std::thread::sleep(BACKOFF);
    }
    if let Some(e) = failure {
        assert!(
            !required,
            "no TCP route to {}'s DoH endpoint at {} even though `{REQUIRE_MARKER}` is set \
             (`{test}`): {e}. This runner promised outbound connectivity; the environment is \
             broken, not deliberately offline the way a laptop on a plane is.",
            ep.operator, ep.addr
        );
        eprintln!(
            "NOTICE: no TCP route to {} at {} after {ATTEMPTS} attempts ({e}) — skipping the \
             live DoH test `{test}`.",
            ep.operator, ep.addr
        );
        return None;
    }
    Some(ep)
}

/// The last line of every test that actually spoke to a server.
fn ran(test: &str, what: &str) {
    println!("{RECEIPT} {test}: {what}");
}

// ── the client under test ───────────────────────────────────────────────

/// `Native` over a **real TLS stack with the platform's own verifier**, and
/// `IpLiteralOnly` beneath it because `Doh::pinned`'s endpoint is a literal
/// and a resolver here would be a second thing under test.
///
/// `with_platform_verifier` rather than `with_webpki_roots` is the whole
/// point: the question this file exists to settle is what the **platform**
/// does with a certificate presented for an IP address, which is what a
/// deployed `Doh::pinned` meets.
fn tls_transport() -> Native<Tokio, Rustls, IpLiteralOnly> {
    Native::new(
        Tokio,
        Rustls::with_platform_verifier().expect("the platform trust store is readable"),
        IpLiteralOnly,
    )
}

fn doh(ep: Endpoint) -> Doh<Native<Tokio, Rustls, IpLiteralOnly>> {
    Doh::pinned(
        tls_transport(),
        ep.uri.parse().expect("a valid endpoint uri"),
    )
    .expect("a public resolver's address is an IP literal")
    .timeouts(LIVE_TIMEOUTS)
}

/// An error's whole chain, because `Resolve` reports `ErrorKind::Resolve`
/// for a TLS failure by design and the cause is the interesting part.
fn chain(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut cur = e.source();
    while let Some(s) = cur {
        out.push_str(" <- ");
        out.push_str(&s.to_string());
        cur = s.source();
    }
    out
}

fn expect_addrs(items: Vec<Result<ResolvedAddr, http_ng_core::Error>>) -> Vec<ResolvedAddr> {
    items
        .into_iter()
        .map(|i| i.unwrap_or_else(|e| panic!("a live lookup failed: {}", chain(&e))))
        .collect()
}

/// Did this lookup fail because the packet never arrived, rather than
/// because the answer was wrong?
///
/// The distinction [`ATTEMPTS`] rests on, and it is made from the **typed**
/// error rather than from its text: only `DohError::Transport` wrapping a
/// connect failure is retried. A `Status`, a `ContentType`, a `Malformed`,
/// a `ResponseCode` — and a TLS failure, which is the *answer* in
/// `an_ipv6_literal_endpoint_…` — are all things the server or the stack
/// said, and asking again would only get them said again.
///
/// **Two kinds, not one, and the second was found by a failing run.** A
/// refused connection is `ErrorKind::Connect`; a connect that runs out of
/// time is `ErrorKind::Timeout(Phase::Connect)`, because `Doh`'s own
/// `DEFAULT_TIMEOUTS` puts a 2 s `connect` bound in the request's
/// extensions and `Native` enforces it. Matching only the first left the
/// commonest lost packet on this network unretried.
fn is_a_lost_packet(e: &http_ng_core::Error) -> bool {
    let Some(DohError::Transport(inner)) =
        std::error::Error::source(e).and_then(|s| s.downcast_ref::<DohError>())
    else {
        return false;
    };
    matches!(
        inner.kind(),
        http_ng_core::ErrorKind::Connect
            | http_ng_core::ErrorKind::Timeout(http_ng_core::Phase::Connect)
    )
}

/// A lookup, up to [`ATTEMPTS`] times, retrying only a lost packet.
async fn lookup_v4(ep: Endpoint, name: &str) -> Vec<Result<ResolvedAddr, http_ng_core::Error>> {
    for attempt in 1..=ATTEMPTS {
        let got: Vec<_> = doh(ep).lookup_ipv4(name).collect().await;
        match got.as_slice() {
            [Err(e)] if is_a_lost_packet(e) && attempt < ATTEMPTS => {
                eprintln!(
                    "NOTICE: attempt {attempt}/{ATTEMPTS} of A `{name}` at {} did not reach the \
                     server ({}) — trying again.",
                    ep.operator,
                    chain(e)
                );
                tokio::time::sleep(BACKOFF).await;
            }
            _ => return got,
        }
    }
    unreachable!("the loop returns on its last iteration")
}

/// The same, for HTTPS records.
async fn lookup_svcb(ep: Endpoint, name: &str) -> Vec<Result<SvcbEndpoint, http_ng_core::Error>> {
    for attempt in 1..=ATTEMPTS {
        let got: Vec<_> = doh(ep).lookup_svcb(name).collect().await;
        match got.as_slice() {
            [Err(e)] if is_a_lost_packet(e) && attempt < ATTEMPTS => {
                eprintln!(
                    "NOTICE: attempt {attempt}/{ATTEMPTS} of HTTPS `{name}` at {} did not reach \
                     the server ({}) — trying again.",
                    ep.operator,
                    chain(e)
                );
                tokio::time::sleep(BACKOFF).await;
            }
            _ => return got,
        }
    }
    unreachable!("the loop returns on its last iteration")
}

// ── DNS by hand: the oracle side ────────────────────────────────────────

/// One RFC 1035 query, built here rather than by the crate under test.
/// Byte-identical to `tests/query_bytes.rs`'s expectation, which is what
/// makes it a fair oracle: the same bytes this crate is asserted to send.
fn query(name: &str, qtype: u16) -> Vec<u8> {
    let mut q = vec![
        0x00, 0x00, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    q.extend(name_wire(name));
    q.extend_from_slice(&qtype.to_be_bytes());
    q.extend_from_slice(&1u16.to_be_bytes());
    q
}

/// One decoded answer record: type, TTL, RDATA. Hand-written, with RFC 1035
/// §4.1.4 name compression handled, because the point of an oracle is that
/// it is not the decoder under test.
#[derive(Debug)]
struct RawRr {
    rtype: u16,
    ttl: u32,
}

/// Walk a name at `at`, returning the offset just past it.
fn skip_name(msg: &[u8], mut at: usize) -> usize {
    loop {
        let len = msg[at];
        if len & 0xC0 == 0xC0 {
            return at + 2; // a pointer ends the name
        }
        at += 1;
        if len == 0 {
            return at;
        }
        at += usize::from(len);
    }
}

fn u16_at(msg: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([msg[at], msg[at + 1]])
}

/// The answer section's records, by hand.
fn answers(msg: &[u8]) -> Vec<RawRr> {
    let qdcount = u16_at(msg, 4);
    let ancount = u16_at(msg, 6);
    let mut at = 12;
    for _ in 0..qdcount {
        at = skip_name(msg, at) + 4;
    }
    let mut out = Vec::new();
    for _ in 0..ancount {
        at = skip_name(msg, at);
        let rtype = u16_at(msg, at);
        let ttl = u32::from_be_bytes([msg[at + 4], msg[at + 5], msg[at + 6], msg[at + 7]]);
        let rdlen = usize::from(u16_at(msg, at + 8));
        at += 10 + rdlen;
        out.push(RawRr { rtype, ttl });
    }
    out
}

/// base64url without padding, RFC 4648 §5 — what RFC 8484 §4.1's `?dns=`
/// takes. Eleven lines here rather than a dependency, because this crate
/// deliberately does not have an encoder and the *reason* it does not is
/// one of the things this file is measuring.
fn base64url(bytes: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..=c.len() {
            out.push(char::from(A[((n >> (18 - 6 * i)) & 0x3F) as usize]));
        }
    }
    out
}

/// One HTTP exchange with the endpoint, over the same TLS transport the
/// crate uses, but with the request built here. This is how the file
/// observes what a real server does with things `Doh` never sends.
async fn raw(
    ep: Endpoint,
    method: http::Method,
    target: &str,
    body: Option<Vec<u8>>,
    headers: &[(http::HeaderName, &'static str)],
) -> (http::StatusCode, Option<String>, Vec<u8>) {
    let mut last = String::new();
    for attempt in 1..=ATTEMPTS {
        match raw_once(ep, method.clone(), target, body.clone(), headers).await {
            Ok(got) => return got,
            Err((retryable, e)) => {
                assert!(
                    retryable,
                    "the exchange with {} failed for a reason repeating it cannot change: {e}",
                    ep.operator
                );
                eprintln!(
                    "NOTICE: attempt {attempt}/{ATTEMPTS} of a raw exchange with {} did not \
                     reach the server ({e}) — trying again.",
                    ep.operator
                );
                last = e;
                tokio::time::sleep(BACKOFF).await;
            }
        }
    }
    panic!(
        "all {ATTEMPTS} attempts of a raw exchange with {} failed to reach the server: {last}",
        ep.operator
    )
}

/// One attempt. `Err` is *only* "the exchange did not happen"; every answer
/// a server gave, however unwelcome, comes back as `Ok` for the caller to
/// assert on.
async fn raw_once(
    ep: Endpoint,
    method: http::Method,
    target: &str,
    body: Option<Vec<u8>>,
    headers: &[(http::HeaderName, &'static str)],
) -> Result<(http::StatusCode, Option<String>, Vec<u8>), (bool, String)> {
    let mut req = http::Request::new(match &body {
        Some(b) => RequestBody::Full(bytes::Bytes::from(b.clone())),
        None => RequestBody::Empty,
    });
    *req.method_mut() = method;
    *req.uri_mut() = target.parse().expect("a valid uri");
    for (name, value) in headers {
        req.headers_mut()
            .insert(name, http::HeaderValue::from_static(value));
    }
    // The same bounds `Doh` puts on its own exchanges, so this oracle waits
    // exactly as long as the thing it is an oracle for — and so a
    // black-holed connect ends in seconds rather than in whatever the OS
    // decides.
    req.extensions_mut().insert(LIVE_TIMEOUTS);
    let t = tls_transport();
    let response = t.execute(req).await.map_err(|e| {
        let e = t.to_error(e);
        (
            matches!(
                e.kind(),
                http_ng_core::ErrorKind::Connect
                    | http_ng_core::ErrorKind::Timeout(http_ng_core::Phase::Connect)
            ),
            chain(&e),
        )
    })?;
    let status = response.status();
    let content_type = response
        .headers()
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(';').next().unwrap_or(v).trim().to_ascii_lowercase());
    let bytes = response
        .into_body()
        .collect()
        .await
        .map(|c| c.to_bytes())
        .map_err(|e| {
            (
                false,
                format!("the response body did not read to the end: {e}"),
            )
        })?;
    let _ = ep;
    Ok((status, content_type, bytes.to_vec()))
}

const DNS_MESSAGE: &str = "application/dns-message";
const CT: http::HeaderName = http::header::CONTENT_TYPE;
const ACCEPT: http::HeaderName = http::header::ACCEPT;

// ── 1. the question this file was written for ───────────────────────────

/// **A certificate presented for an IP address, through the platform's own
/// verifier.** `docs/v03-design.md` §W3 and four entries in
/// `docs/v03-acceptance.md` list this as unverified, and it is the one
/// thing `Doh::pinned` cannot work without: its endpoint is an IP literal,
/// so the TLS server name is an address and the certificate must carry an
/// IP SAN.
///
/// Deliberately asserted at the **transport** level rather than through
/// `Doh`: a DNS-layer disagreement and a rejected certificate are different
/// findings, and `From<DohError> for Error` flattens both to
/// `ErrorKind::Resolve`. What is checked here is that the handshake
/// completed and the server answered — the DNS content is every other
/// test's business.
#[tokio::test(flavor = "multi_thread")]
async fn a_certificate_presented_for_an_ip_address_validates_through_the_platform_verifier() {
    const NAME: &str =
        "a_certificate_presented_for_an_ip_address_validates_through_the_platform_verifier";
    for ep in OPERATORS {
        let Some(ep) = live(NAME, ep) else { return };
        let (status, content_type, body) = raw(
            ep,
            http::Method::POST,
            ep.uri,
            Some(query("cloudflare.com", TYPE_A)),
            &[(CT, DNS_MESSAGE), (ACCEPT, DNS_MESSAGE)],
        )
        .await;
        assert_eq!(
            status,
            http::StatusCode::OK,
            "{}'s DoH endpoint answered {status} — the handshake completed, so this is not the \
             IP-SAN question",
            ep.operator
        );
        assert_eq!(content_type.as_deref(), Some(DNS_MESSAGE));
        assert!(
            !answers(&body).is_empty(),
            "{} answered over TLS but with no records",
            ep.operator
        );
        ran(
            NAME,
            &format!(
                "{} at {}: handshake against an IP-SAN certificate completed, {} answered {status}",
                ep.operator, ep.addr, ep.operator
            ),
        );
    }
}

/// **An IPv6-literal endpoint does not work today, and the defect is not in
/// this crate.** `Doh::pinned`'s own doc offers
/// `https://[2606:4700:4700::1111]/dns-query` as an example; over
/// `http-ng-native` + `http-ng-tls-rustls` it fails at the handshake with
/// `Tls: invalid dns name`, before a byte of DNS is exchanged.
///
/// The cause, read after measuring rather than instead of it:
/// `http::Uri::host()` returns an IPv6 literal **with its brackets**, and
/// `connect.rs` hands that string to `TlsRequest::server_name` unchanged,
/// where `rustls_pki_types::ServerName::try_from` rejects `[…]` — it tries
/// `DnsName`, then `IpAddr`, and neither strips a bracket. Both
/// `IpLiteralOnly::literal` and this crate's own `ip_literal` do strip
/// them, each with a comment about this exact trap, so the TCP connection
/// is made and only the TLS name is wrong.
///
/// **This test asserts the defect, on purpose.** Fixing it is a one-line
/// change in `http-ng-native` or `http-ng-tls-rustls`, neither of which is
/// this crate; pinning the current behaviour is what makes the fix visible
/// here — when this test starts failing, the note on `Doh::pinned` and the
/// entry in `docs/v03-acceptance.md` are both stale and should go.
#[tokio::test(flavor = "multi_thread")]
async fn an_ipv6_literal_endpoint_fails_at_tls_today_and_the_defect_is_not_in_this_crate() {
    const NAME: &str =
        "an_ipv6_literal_endpoint_fails_at_tls_today_and_the_defect_is_not_in_this_crate";
    const V6: Endpoint = Endpoint {
        operator: "Cloudflare over IPv6",
        uri: "https://[2606:4700:4700::1111]/dns-query",
        addr: "[2606:4700:4700::1111]:443",
    };
    let Some(ep) = live(NAME, V6) else { return };

    // Through the retrying wrapper, not `doh()` directly: `is_a_lost_packet`
    // retries only a connect that did not happen and hands every answer
    // back untouched, so the error this test is about arrives on the first
    // attempt while a lost SYN does not become a finding. Both these tests
    // failed spuriously before it was used here — the harness bug this line
    // is the fix for.
    let got = lookup_v4(ep, "cloudflare.com").await;
    let [Err(e)] = got.as_slice() else {
        panic!(
            "an IPv6-literal endpoint now works — the bracket defect this test pins has been \
             fixed. Delete this test, the note on `Doh::pinned`, and the entry in \
             docs/v03-acceptance.md. Got: {got:?}"
        );
    };
    let text = chain(e);
    assert!(
        text.contains("invalid dns name"),
        "an IPv6-literal endpoint failed, but not with the bracketed-server-name error this \
         test pins: {text}"
    );
    ran(NAME, &format!("{}: {text}", ep.operator));
}

// ── 2. a real record, against an oracle that is not ours ────────────────

/// **A real HTTPS record, field by field, against `dig`.**
///
/// `crypto.cloudflare.com` rather than `cloudflare.com`, because it is the
/// only one of the two that publishes an `ech` parameter, and the
/// ECHConfigList with RFC 9460 §7.3's redundant length prefix is the single
/// most delicate thing `tests/svcb.rs` builds by hand.
///
/// The oracle is `dig`'s presentation form over plain UDP DNS to the same
/// operator: BIND's decoder, not ours. Where `dig` is absent the structural
/// assertions still run and the comparison says so — under
/// [`REQUIRE_MARKER`] its absence is a failure.
///
/// **What is deliberately not compared: the ECHConfigList's bytes.**
/// Cloudflare rotates that key, and two resolvers hold two snapshots of it
/// — measured, 1.1.1.1 and 8.8.8.8 returned different payloads of identical
/// length in the same minute. Its *length*, its two-byte prefix and the
/// `fe0d` ECHConfig version are stable and are compared; asserting the
/// payload would be asserting the clock.
#[tokio::test(flavor = "multi_thread")]
async fn a_real_https_record_parses_and_every_field_agrees_with_dig() {
    const NAME: &str = "a_real_https_record_parses_and_every_field_agrees_with_dig";
    const OWNER: &str = "crypto.cloudflare.com";
    for ep in OPERATORS {
        let Some(ep) = live(NAME, ep) else { return };
        let got = lookup_svcb(ep, OWNER).await;
        let endpoints: Vec<SvcbEndpoint> = got
            .into_iter()
            .map(|i| i.unwrap_or_else(|e| panic!("live SVCB lookup failed: {}", chain(&e))))
            .collect();
        assert_eq!(
            endpoints.len(),
            1,
            "{OWNER} publishes exactly one HTTPS record; {} returned {endpoints:?}",
            ep.operator
        );
        let e = &endpoints[0];

        // The claim `tests/svcb.rs` makes with bytes it wrote itself.
        let ech = e.ech_config_list.as_ref().unwrap_or_else(|| {
            panic!(
                "{OWNER} publishes an ech parameter; {} dropped it",
                ep.operator
            )
        });
        assert!(
            ech.len() >= 4,
            "an ECHConfigList shorter than its own header: {ech:?}"
        );
        assert_eq!(
            usize::from(u16::from_be_bytes([ech[0], ech[1]])),
            ech.len() - 2,
            "RFC 9460 §7.3's redundant two-byte length prefix is missing or wrong — this is the \
             form rustls parses, and the one the hand-built fixture claims to reproduce"
        );
        assert_eq!(
            &ech[2..4],
            &[0xfe, 0x0d],
            "the ECHConfig version after the prefix is not draft-13 (`fe0d`) — either the prefix \
             was stripped or an extra one was added"
        );

        let Some(oracle) = dig(OWNER, "HTTPS", ep, OWNER) else {
            ran(
                NAME,
                &format!("{}: parsed {e:?} (no dig, no comparison)", ep.operator),
            );
            continue;
        };
        assert_field(&oracle, "priority", &e.priority.to_string(), ep);
        assert_field(&oracle, "target", &e.target, ep);
        assert_field(
            &oracle,
            "alpn",
            &e.alpn
                .iter()
                .map(|a| String::from_utf8_lossy(a).into_owned())
                .collect::<Vec<_>>()
                .join(","),
            ep,
        );
        assert_field(
            &oracle,
            "port",
            &e.port.map(|p| p.to_string()).unwrap_or_default(),
            ep,
        );
        assert_field(&oracle, "ipv4hint", &join(&e.ipv4hint), ep);
        assert_field(&oracle, "ipv6hint", &join(&e.ipv6hint), ep);
        assert_field(&oracle, "ech_len", &ech.len().to_string(), ep);
        ran(
            NAME,
            &format!(
                "{}: all seven fields of {OWNER}'s record agree with dig",
                ep.operator
            ),
        );
    }
}

fn join<T: ToString>(xs: &[T]) -> String {
    let mut v: Vec<String> = xs.iter().map(ToString::to_string).collect();
    v.sort();
    v.join(",")
}

fn assert_field(oracle: &DigRecord, field: &str, ours: &str, ep: Endpoint) {
    let theirs = oracle.field(field);
    assert_eq!(
        ours, theirs,
        "`{field}` disagrees with dig for {}: this crate says `{ours}`, BIND says `{theirs}` \
         (dig line: {})",
        ep.operator, oracle.line
    );
}

/// `dig`'s answer, reduced to the fields `SvcbEndpoint` holds.
struct DigRecord {
    line: String,
    priority: String,
    target: String,
    alpn: String,
    port: String,
    ipv4hint: String,
    ipv6hint: String,
    ech_len: String,
}

impl DigRecord {
    fn field(&self, name: &str) -> &str {
        match name {
            "priority" => &self.priority,
            "target" => &self.target,
            "alpn" => &self.alpn,
            "port" => &self.port,
            "ipv4hint" => &self.ipv4hint,
            "ipv6hint" => &self.ipv6hint,
            "ech_len" => &self.ech_len,
            other => panic!("no such field `{other}`"),
        }
    }
}

/// `dig @<endpoint's address> +short <type> <name>`, parsed.
///
/// The same operator over plain UDP DNS, so the two answers come from the
/// same cache and any disagreement is about decoding rather than about
/// which server was asked.
fn dig(name: &str, rtype: &str, ep: Endpoint, owner: &str) -> Option<DigRecord> {
    let server = ep.addr.rsplit_once(':').expect("addr:port").0;
    let out = match std::process::Command::new("dig")
        .args([&format!("@{server}"), "+short", "+timeout=5", rtype, name])
        .output()
    {
        Ok(out) if out.status.success() => out,
        other => {
            assert!(
                std::env::var_os(REQUIRE_MARKER).is_none(),
                "`dig` is unavailable or failed ({other:?}) even though `{REQUIRE_MARKER}` is \
                 set — this runner promised the oracle as well as the network"
            );
            eprintln!(
                "NOTICE: no usable `dig` — the comparison against BIND's decoder is skipped."
            );
            return None;
        }
    };
    let line = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    let mut words = line.split_whitespace();
    let priority = words.next()?.to_owned();
    // **The one place the two notations genuinely differ, and it took a
    // failing run to find.** `dig` prints the TargetName as it is on the
    // wire, so a ServiceMode record that points at its own owner reads `.`;
    // `SvcbEndpoint::target` carries the **effective** target of RFC 9460
    // §2.5, i.e. the owner name substituted in, because
    // `endpoint_from_binding` does that substitution so no consumer has to
    // know the convention. Mapping `.` to the owner here is therefore not a
    // fudge to make a comparison pass — it is the check that the
    // substitution HAPPENED, on a record nobody here wrote.
    //
    // A first draft of this comment said no fixture in this crate covered
    // the root target. **That was false, and the mutation run said so**:
    // dropping the substitution is killed hermetically by
    // `svcb.rs`'s `a_service_mode_record_with_a_root_target_takes_its_owner
    // _name` as well as by this test (L4). What is added here is not
    // coverage of the rule but coverage of the *reading*: the fixture
    // builds a root target with `name_wire("")` and this reads one a
    // registrar published.
    let target = match words.next()? {
        "." => owner.to_owned(),
        other => other.trim_end_matches('.').to_owned(),
    };
    let mut rec = DigRecord {
        line: line.clone(),
        priority,
        target,
        alpn: String::new(),
        port: String::new(),
        ipv4hint: String::new(),
        ipv6hint: String::new(),
        ech_len: "0".to_owned(),
    };
    for word in words {
        let Some((key, value)) = word.split_once('=') else {
            continue;
        };
        let value = value.trim_matches('"');
        match key {
            "alpn" => rec.alpn = value.to_owned(),
            "port" => rec.port = value.to_owned(),
            "ipv4hint" => rec.ipv4hint = join(&value.split(',').collect::<Vec<_>>()),
            "ipv6hint" => {
                rec.ipv6hint = join(
                    &value
                        .split(',')
                        .map(|a| a.parse::<Ipv6Addr>().expect("dig prints valid v6"))
                        .collect::<Vec<_>>(),
                );
            }
            // dig prints the ECHConfigList base64'd, prefix included.
            "ech" => rec.ech_len = unbase64_len(value).to_string(),
            _ => {}
        }
    }
    Some(rec)
}

/// How many bytes a standard-alphabet base64 string decodes to. Only the
/// length is needed — see the test's doc for why the payload is not
/// compared.
fn unbase64_len(s: &str) -> usize {
    let body = s.trim_end_matches('=');
    let pad = s.len() - body.len();
    s.len() / 4 * 3 - pad
}

// ── 3. what came off the wire ───────────────────────────────────────────

/// **The TTL a caller gets is the number the server sent.**
///
/// `tests/lookup.rs` already pins "per record, not per RRset", with two
/// fabricated TTLs. What a fixture structurally cannot show is that the
/// number is the *server's*: a fixture's 60 and an implementation's
/// hard-coded 60 look exactly alike. Two things here can only be true of a
/// live answer, and neither can be produced by any constant:
///
/// 1. **Against Cloudflare, the value equals the one this file decoded off
///    the wire by hand**, from a query built here and sent seconds earlier.
/// 2. **The two operators disagree**, and by a lot. Each is serving what is
///    left of an 86 400 s TTL out of its own cache, so the remaining time
///    is a fact about when *that* cache last refilled. A crate that
///    invented the number would hand back the same one to both.
///
/// **Why (1) is asserted against Cloudflare and not against Google, which
/// is a measurement rather than a preference.** Two queries a second apart
/// need a coherent cache behind them to be comparable at all. Cloudflare's
/// gave the identical TTL every time it was asked while this file was
/// written (73691/73689, 73129/73129, 72912/72912, 72900/72900,
/// 72387/72387 — the widest gap 2 s). Google's frontends do not share one:
/// the same RRset came back as **703 s and 18 759 s within the same
/// second**, and again as 847/313 and 17765/5893 in an earlier probe. So a
/// cross-query TTL comparison there is a statement about a resolver fleet's
/// architecture, not about this crate, and the honest place to make claim
/// (1) is the operator where it is sound. Google is not let off: it carries
/// the bounds below and half of claim (2).
///
/// The 60 s tolerance on (1) is not slack — a caching recursive decrements
/// once per second and the two queries are seconds apart, so equality would
/// be asserting the clock. It still excludes every wrong answer that is not
/// a clock: `None`, zero, a constant, and the RRset minimum.
#[tokio::test(flavor = "multi_thread")]
async fn the_ttl_a_caller_gets_is_the_one_that_came_off_the_wire() {
    const NAME: &str = "the_ttl_a_caller_gets_is_the_one_that_came_off_the_wire";
    const OWNER: &str = "one.one.one.one";
    /// A week. Nothing is wrong with a long TTL; a value above this is not
    /// a TTL at all, which is what a unit mix-up looks like.
    const SANE: u64 = 7 * 24 * 60 * 60;

    let mut per_operator: Vec<(Endpoint, u64)> = Vec::new();
    for ep in OPERATORS {
        let Some(ep) = live(NAME, ep) else { return };

        let (_, _, body) = raw(
            ep,
            http::Method::POST,
            ep.uri,
            Some(query(OWNER, TYPE_A)),
            &[(CT, DNS_MESSAGE), (ACCEPT, DNS_MESSAGE)],
        )
        .await;
        let oracle: Vec<u32> = answers(&body)
            .iter()
            .filter(|r| r.rtype == TYPE_A)
            .map(|r| r.ttl)
            .collect();
        assert!(
            !oracle.is_empty(),
            "{OWNER} has A records; {} returned none",
            ep.operator
        );

        let ours = expect_addrs(lookup_v4(ep, OWNER).await);
        assert_eq!(
            ours.len(),
            oracle.len(),
            "{} gave a different number of A records",
            ep.operator
        );

        let mut ttls = Vec::new();
        for got in &ours {
            let ttl = got
                .ttl
                .unwrap_or_else(|| {
                    panic!("{} answered with no TTL at all for {OWNER}", ep.operator)
                })
                .as_secs();
            assert!(
                ttl > 0 && ttl < SANE,
                "{ttl}s is not a plausible TTL from {} for {OWNER}",
                ep.operator
            );
            ttls.push(ttl);
        }

        // Claim (1), where a coherent cache makes it sound.
        if ep.uri == CLOUDFLARE.uri {
            for (ttl, wire) in ttls.iter().zip(&oracle) {
                let wire = u64::from(*wire);
                let drift = ttl.abs_diff(wire);
                assert!(
                    drift <= 60,
                    "the TTL this crate reports ({ttl}s) is not the one this file decoded off \
                     the wire ({wire}s) from {} — {drift}s apart, where two queries seconds \
                     apart can differ by seconds",
                    ep.operator
                );
            }
        }

        per_operator.push((ep, ttls[0]));
        ran(
            NAME,
            &format!(
                "{}: TTLs {ttls:?} against the hand-decoded {oracle:?}",
                ep.operator
            ),
        );
    }

    // Claim (2). Reached only when every operator answered, which is what
    // the `return` in the gate above guarantees.
    let [(a, ttl_a), (b, ttl_b)] = per_operator.as_slice() else {
        panic!("expected one TTL per operator, got {per_operator:?}");
    };
    assert_ne!(
        ttl_a, ttl_b,
        "{} and {} report the identical remaining TTL ({ttl_a}s) for {OWNER}. Two independent \
         caches agreeing to the second on what is left of an 86 400 s record is what a \
         fabricated constant looks like, not what two caches look like",
        a.operator, b.operator
    );
}

/// **NXDOMAIN from a real authority is an empty stream, not an error.**
///
/// The line `http_ng_dns`'s module doc draws — "asked and found nothing" is
/// not "could not ask" — checked against a real negative answer rather than
/// a fixture's `0x8183`. `invalid.` is RFC 6761 §6.4's permanently
/// unresolvable name, so this asks nobody's real nameserver a question that
/// could ever be answered differently.
#[tokio::test(flavor = "multi_thread")]
async fn nxdomain_from_a_real_authority_is_an_empty_stream_not_an_error() {
    const NAME: &str = "nxdomain_from_a_real_authority_is_an_empty_stream_not_an_error";
    for ep in OPERATORS {
        let Some(ep) = live(NAME, ep) else { return };
        let got = lookup_v4(ep, "nothing-here.invalid").await;
        assert!(
            got.is_empty(),
            "a real NXDOMAIN from {} produced {got:?} rather than an empty stream — that is the \
             distinction between an answer and a failure to ask",
            ep.operator
        );
        ran(
            NAME,
            &format!("{}: NXDOMAIN is an empty stream", ep.operator),
        );
    }
}

// ── 4. what a real server does with what this crate does not send ───────

/// **`GET` works at both operators, so "POST only" is this crate's choice
/// and not a constraint.**
///
/// RFC 8484 §4.1 defines both forms and requires a server to support both;
/// `docs/v03-acceptance.md` records "no GET" as deliberate — the base64url
/// encoder is a dependency this workspace does not want for one call site.
/// A decision recorded as a trade-off is worth checking is *still* a
/// trade-off: this test builds the GET by hand (eleven lines of base64url,
/// above) and asserts a real answer comes back, so the entry stays honest
/// about what is being given up.
#[tokio::test(flavor = "multi_thread")]
async fn the_get_form_this_crate_does_not_send_is_answered_by_both_operators() {
    const NAME: &str = "the_get_form_this_crate_does_not_send_is_answered_by_both_operators";
    for ep in OPERATORS {
        let Some(ep) = live(NAME, ep) else { return };
        let encoded = base64url(&query("cloudflare.com", TYPE_A));
        let (status, content_type, body) = raw(
            ep,
            http::Method::GET,
            &format!("{}?dns={encoded}", ep.uri),
            None,
            &[(ACCEPT, DNS_MESSAGE)],
        )
        .await;
        assert_eq!(
            status,
            http::StatusCode::OK,
            "{} refused the GET form",
            ep.operator
        );
        assert_eq!(content_type.as_deref(), Some(DNS_MESSAGE));
        assert!(
            answers(&body).iter().any(|r| r.rtype == TYPE_A),
            "{}'s GET answer carries no A record",
            ep.operator
        );
        ran(
            NAME,
            &format!("{}: GET ?dns= answered {status}", ep.operator),
        );
    }
}

/// **The `Content-Type` this crate sends is required; the `Accept` is
/// not.**
///
/// `Doh::exchange` sends both, and RFC 8484 §4.1 makes only the first a
/// MUST — the `Accept` is a SHOULD, which is the kind of line that gets
/// deleted as redundant by someone reading the code and not the wire. What
/// a real server does settles which of the two is carrying weight:
/// measured, dropping `Content-Type` is a refusal at both operators (415 at
/// Cloudflare, 400 at Google) and dropping `Accept` changes nothing.
///
/// The assertion is deliberately about the *pair*, not about the exact
/// status: 415 and 400 are both refusals and neither is this crate's to
/// choose.
#[tokio::test(flavor = "multi_thread")]
async fn the_content_type_is_required_by_a_real_server_and_the_accept_is_not() {
    const NAME: &str = "the_content_type_is_required_by_a_real_server_and_the_accept_is_not";
    for ep in OPERATORS {
        let Some(ep) = live(NAME, ep) else { return };
        let q = query("cloudflare.com", TYPE_A);

        let (without_ct, _, _) = raw(
            ep,
            http::Method::POST,
            ep.uri,
            Some(q.clone()),
            &[(ACCEPT, DNS_MESSAGE)],
        )
        .await;
        assert!(
            without_ct.is_client_error(),
            "{} accepted a DoH POST with no content-type ({without_ct}) — the header this crate \
             sends would then be doing nothing",
            ep.operator
        );

        let (without_accept, ct, body) = raw(
            ep,
            http::Method::POST,
            ep.uri,
            Some(q),
            &[(CT, DNS_MESSAGE)],
        )
        .await;
        assert_eq!(
            without_accept,
            http::StatusCode::OK,
            "{} refused a DoH POST with no accept header",
            ep.operator
        );
        assert_eq!(ct.as_deref(), Some(DNS_MESSAGE));
        assert!(!answers(&body).is_empty());
        ran(
            NAME,
            &format!(
                "{}: no content-type -> {without_ct}, no accept -> {without_accept}",
                ep.operator
            ),
        );
    }
}

/// **Quad9 answers no DNS at all over HTTP/1.1, and this crate reports it
/// as a status rather than as a parse failure.**
///
/// Measured: `9.9.9.9` answers every DoH request over HTTP/1.1 with `505
/// HTTP Version Not Supported` and an HTML body reading *"this server
/// implements RFC 8484 … and requires HTTP/2 in accordance with section 5.2
/// of the RFC"*. §5.2 says HTTP/2 is the minimum RECOMMENDED version; Quad9
/// reads that as a requirement.
///
/// `http-ng-native` speaks HTTP/1.1 unless its `http2` feature is on, so a
/// default build of this workspace **cannot use Quad9 as a DoH resolver at
/// all**. That is worth a test rather than a footnote, and worth two
/// assertions rather than one: the status must be reported as
/// `DohError::Status`, *not* as `DohError::ContentType` or
/// `DohError::Malformed`. The order in `Doh::exchange` — status first, then
/// content type, then decode — is what makes an HTML error page produce a
/// message a reader can act on, and this is the only place in the suite
/// where a real server supplies the HTML.
///
/// The test adapts to the feature rather than assuming it: what HTTP
/// version the connection actually negotiated is read off the response, so
/// the same source is honest in both builds.
#[tokio::test(flavor = "multi_thread")]
async fn an_endpoint_that_demands_http2_is_a_status_error_and_not_a_parse_error() {
    const NAME: &str = "an_endpoint_that_demands_http2_is_a_status_error_and_not_a_parse_error";
    let Some(ep) = live(NAME, QUAD9) else { return };

    let (status, content_type, _) = raw(
        ep,
        http::Method::POST,
        ep.uri,
        Some(query("cloudflare.com", TYPE_A)),
        &[(CT, DNS_MESSAGE), (ACCEPT, DNS_MESSAGE)],
    )
    .await;
    // Through the retrying wrapper, not `doh()` directly: `is_a_lost_packet`
    // retries only a connect that did not happen and hands every answer
    // back untouched, so the error this test is about arrives on the first
    // attempt while a lost SYN does not become a finding. Both these tests
    // failed spuriously before it was used here — the harness bug this line
    // is the fix for.
    let got = lookup_v4(ep, "cloudflare.com").await;

    if status == http::StatusCode::OK {
        // The `http2` feature is on and ALPN got us h2: Quad9 answers.
        assert_eq!(content_type.as_deref(), Some(DNS_MESSAGE));
        let addrs = expect_addrs(got);
        assert!(
            !addrs.is_empty(),
            "Quad9 answered 200 but returned no addresses"
        );
        ran(NAME, &format!("Quad9 answered 200 over HTTP/2: {addrs:?}"));
        return;
    }

    assert_eq!(
        status,
        http::StatusCode::HTTP_VERSION_NOT_SUPPORTED,
        "Quad9 refused with {status} rather than the 505 this test pins"
    );
    assert_ne!(
        content_type.as_deref(),
        Some(DNS_MESSAGE),
        "a 505 carrying a DNS message would make this test's point backwards"
    );
    let [Err(e)] = got.as_slice() else {
        panic!("Quad9 refused the raw request with {status} but the resolver returned {got:?}");
    };
    let text = chain(e);
    assert!(
        text.contains("HTTP status 505"),
        "the 505 reached the caller as `{text}` rather than as a status — an HTML error page \
         decoded as DNS is the confusion `Doh::exchange`'s check order exists to prevent"
    );
    ran(NAME, &format!("Quad9 over HTTP/1.1: {text}"));
}

/// **Two operators, one name, and the addresses have to be usable.**
///
/// The weakest test here and the one that would catch the crudest failure:
/// a decoder that returned the RDATA of the wrong record, or the bytes of a
/// name where an address should be, passes every structural check above and
/// fails this. `one.one.one.one` is Cloudflare's own name for its resolver,
/// so its A records are exactly the addresses the rest of this file
/// connects to — which makes the check independent of what any resolver
/// says.
#[tokio::test(flavor = "multi_thread")]
async fn both_operators_return_the_addresses_this_suite_is_already_talking_to() {
    const NAME: &str = "both_operators_return_the_addresses_this_suite_is_already_talking_to";
    for ep in OPERATORS {
        let Some(ep) = live(NAME, ep) else { return };
        let v4 = expect_addrs(lookup_v4(ep, "one.one.one.one").await);
        let addrs: Vec<IpAddr> = v4.iter().map(|a| a.addr).collect();
        assert!(
            addrs.contains(&IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))),
            "{} resolved one.one.one.one to {addrs:?}, which does not include the address this \
             very suite connects to",
            ep.operator
        );
        ran(
            NAME,
            &format!("{}: one.one.one.one -> {addrs:?}", ep.operator),
        );
    }
}
