//! What name this transport hands QUIC, and what it does with a bracketed
//! host before it asks a resolver anything.
//!
//! `http::Uri::host()` returns an IPv6 literal **with its brackets** —
//! `[::1]`, not `::1` — because they are URI syntax (RFC 3986 §3.2.2).
//! This crate reads that string once and uses it twice, and both uses want
//! it without the brackets: `Endpoint::connect_with`'s server name (which
//! becomes `rustls_pki_types::ServerName`, rejecting `[::1]` as neither a
//! name nor an address) and `resolve`'s literal shortcut (`str::parse::
//! <IpAddr>`, which a bracketed literal fails). See
//! `http_ng_core::bare_host`.
//!
//! **The two are separated by which resolver is in play**, which is the
//! whole reason this file has three tests rather than one:
//!
//! - Under `IpLiteralOnly`, the shortcut is *not* load-bearing: the
//!   resolver strips the brackets itself (`IpLiteralOnly::literal`, which
//!   documents this same trap) and a bracketed host resolves anyway. So
//!   this row can only fail on the server name.
//! - Under a resolver that answers nothing, the shortcut is the only way
//!   to an address at all. That is the arrangement a real deployment is
//!   in: `getaddrinfo("[::1]")` fails, so a literal that reaches the
//!   resolver is a literal that does not connect.
//! - Under a resolver that answers every name, `localhost` must arrive at
//!   QUIC byte for byte — a strip that fires unconditionally would send
//!   `ocalhos`, a perfectly good DNS name and a certificate mismatch.
//!
//! Each assertion is that a real QUIC handshake against a real HTTP/3
//! server completed and the response arrived, not that some argument had
//! some value.
#![cfg(not(target_family = "wasm"))]

mod server;

use futures_util::stream;
use http_body_util::BodyExt;
use http_ng_core::unversioned::Transport;
use http_ng_core::{Error, ErrorKind, RequestBody};
use http_ng_dns::{IpLiteralOnly, Resolve, ResolvedAddr};
use http_ng_h3::H3;
use http_ng_rt_tokio::TokioHandle;
use server::Behaviour;
use std::net::IpAddr;

/// A resolver with no answers and no excuses: every name is an error.
///
/// It stands in for the real thing without being it. `SystemDns` would
/// also fail on `[::1]` — that is the defect — but it would fail
/// *sometimes*, on a machine whose `getaddrinfo` happens to be forgiving,
/// and a test whose sensitivity depends on the host's resolver is not a
/// test of ours. This one cannot answer, so a request that reaches it
/// cannot succeed, and the literal shortcut is the only path left.
#[derive(Debug, Clone, Copy)]
struct NoAnswers;

fn refused(name: &str) -> Result<ResolvedAddr, Error> {
    Err(Error::new(
        ErrorKind::Resolve,
        std::io::Error::other(format!("NoAnswers was asked about `{name}`")),
    ))
}

impl Resolve for NoAnswers {
    fn lookup_ipv4(
        &self,
        name: &str,
    ) -> impl futures_util::Stream<Item = Result<ResolvedAddr, Error>> {
        stream::iter(vec![refused(name)])
    }

    fn lookup_ipv6(
        &self,
        name: &str,
    ) -> impl futures_util::Stream<Item = Result<ResolvedAddr, Error>> {
        stream::iter(vec![refused(name)])
    }
}

/// A resolver that answers every name with one address — the mirror image,
/// for the row where the host is a name rather than a literal.
#[derive(Debug, Clone, Copy)]
struct Pointing(IpAddr);

impl Pointing {
    fn answer(self, this_family: bool) -> Vec<Result<ResolvedAddr, Error>> {
        if this_family {
            vec![Ok(ResolvedAddr {
                addr: self.0,
                ttl: None,
            })]
        } else {
            vec![]
        }
    }
}

impl Resolve for Pointing {
    fn lookup_ipv4(
        &self,
        _: &str,
    ) -> impl futures_util::Stream<Item = Result<ResolvedAddr, Error>> {
        stream::iter(self.answer(self.0.is_ipv4()))
    }

    fn lookup_ipv6(
        &self,
        _: &str,
    ) -> impl futures_util::Stream<Item = Result<ResolvedAddr, Error>> {
        stream::iter(self.answer(self.0.is_ipv6()))
    }
}

/// One HTTP/3 request over a fresh transport, returning the body.
async fn get<D: Resolve>(
    uri: &str,
    cert: &rustls::pki_types::CertificateDer<'static>,
    dns: D,
) -> String {
    let t = H3::new(
        TokioHandle::current().expect("inside a tokio runtime"),
        server::client_tls(cert),
        dns,
    )
    .expect("H3::new does no I/O");
    let req = http::Request::builder()
        .uri(uri)
        .body(RequestBody::Empty)
        .unwrap();
    let resp = t
        .execute(req)
        .await
        .unwrap_or_else(|e| panic!("{uri}: the exchange had to complete, got {e:?} ({e})"));
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.version(), http::Version::HTTP_3);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// The server name: a bracketed authority against a resolver that strips
/// the brackets for us, so only `connect_with` can fail.
#[tokio::test(flavor = "multi_thread")]
async fn an_ipv6_literal_authority_reaches_quic_as_a_server_name() {
    let Some(s) = server::start_on_v6(Behaviour::Echo) else {
        eprintln!("skipped: this host has no IPv6 loopback");
        return;
    };
    // `{addr}` on a v6 `SocketAddr` is `[::1]:port`.
    let body = get(
        &format!("https://{}/one", s.addr),
        &s.cert_der,
        IpLiteralOnly,
    )
    .await;
    assert_eq!(body, "hello over h3");
}

/// The literal shortcut: the same authority against a resolver that
/// answers nothing, so the address can only have come from parsing the
/// host itself.
#[tokio::test(flavor = "multi_thread")]
async fn an_ipv6_literal_authority_never_reaches_the_resolver() {
    let Some(s) = server::start_on_v6(Behaviour::Echo) else {
        eprintln!("skipped: this host has no IPv6 loopback");
        return;
    };
    let body = get(&format!("https://{}/one", s.addr), &s.cert_der, NoAnswers).await;
    assert_eq!(body, "hello over h3");
}

/// A named authority must reach QUIC unchanged, and must still be asked
/// about — the shortcut is for literals only.
#[tokio::test(flavor = "multi_thread")]
async fn a_named_authority_reaches_quic_unchanged() {
    let s = server::start(Behaviour::Echo);
    let body = get(
        &format!("https://localhost:{}/one", s.addr.port()),
        &s.cert_der,
        Pointing(s.addr.ip()),
    )
    .await;
    assert_eq!(body, "hello over h3");
}
