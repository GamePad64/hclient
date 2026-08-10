//! DNS over HTTPS (RFC 8484) behind [`http_ng_dns::Resolve`].
//!
//! ```ignore
//! let doh = Doh::pinned(transport, "https://1.1.1.1/dns-query".parse()?)?;
//! let client = Client::builder(Native::new(rt, tls, doh)).build()?;
//! ```
//!
//! # The bootstrap is the design problem, not the protocol
//!
//! DoH resolves a name by making an HTTP request, and an HTTP request needs
//! a name resolved. Three questions come out of that, and this crate
//! answers all three in its **constructors and its type**, not in prose,
//! because prose is not read at the call site.
//!
//! ## 1. What resolves the DoH server's own name?
//!
//! Whatever resolver the transport you hand to this crate already carries —
//! and which of the four possible shapes that is, you state by picking a
//! constructor:
//!
//! - [`Doh::pinned`] — the endpoint's host is an **IP literal**
//!   (`https://1.1.1.1/dns-query`). No bootstrap exists, because no name is
//!   resolved. The constructor checks it rather than trusting the caller,
//!   so `pinned` is a fact about the URI and not a hope. Its cost is stated
//!   on the method: a pinned address that changes leaves the client with no
//!   DNS at all, and nothing in this crate can notice — so pair it with a
//!   [fallback](Doh::with_fallback) or expect to ship a new address.
//! - [`Doh::bootstrapped`] — the endpoint's host is a **name**, resolved by
//!   the inner transport's own resolver, once per connection it opens.
//!   Pass a transport carrying `SystemDns` and you have shape 2 of
//!   `docs/v03-design.md` §W3 ("the system resolver, once, for the DoH
//!   host"); pass one carrying a resolver of fixed addresses and you have
//!   shape 3 ("caller-supplied bootstrap addresses"). This crate does not
//!   need to distinguish them — both are "the inner transport knows how".
//!
//! The two constructors **partition** the space: `pinned` refuses a name
//! and `bootstrapped` refuses a literal. Which one compiles is therefore a
//! statement about the endpoint, and a URI change that silently turns a
//! bootstrap-free deployment into a bootstrapped one is a runtime error at
//! construction rather than a surprise in production.
//!
//! Shape 4 of §W3 — RFC 9461 `dohpath` discovery — is deliberately not
//! here. Discovering a DoH endpoint by DNS is circular for the first
//! lookup; `http_ng_dns::svcb`'s `RECOGNISED_KEYS` still excludes key 7,
//! and a record that makes it mandatory is still one this client refuses to
//! use.
//!
//! ## 2. What client makes the DoH request? A `Transport`, never a `Client`
//!
//! `C` is an [`http_ng_core::unversioned::Transport`], the seam one level
//! below `http_ng::Client`. That is the whole answer to §W3's *"a
//! resolver's client is not the user's client"*, and it is structural
//! rather than a rule someone has to follow: a cookie jar, a redirect
//! policy and an `Authorization` header are all things `Client` owns and
//! `Transport` has never heard of, so there is no arrangement of this API
//! in which a caller accidentally sends their session cookie to a DNS
//! provider. The shared-`Client` case is not merely awkward here; it does
//! not typecheck.
//!
//! **The cycle §W3 asked about is real, and the type system does refuse
//! it** — but not by any check of ours, and it is worth being exact about
//! the mechanism because the escape hatch is one line away. A DoH resolver
//! whose transport resolves through the same DoH resolver would have the
//! type `Native<R, T, Doh<Native<R, T, Doh<Native<…>>>>>`, which has no
//! finite spelling: writing it needs a type alias that mentions itself, and
//! `rustc` answers `E0072`/`E0391` — a size or cycle error at compile time,
//! never a stack overflow at run time. `tests/no_cycle.rs` carries the
//! recursive definitions in a comment, the four compiler transcripts they
//! actually produce, and next to them the *finite* two-level composition,
//! which does compile and is the shape a caller wants (a DoH resolver over
//! a transport that resolves by IP literal only).
//!
//! **The escape hatch, named where it can be found:** the guard is a
//! property of *not erasing*. `Arc<dyn Resolve>` — or any other boxed
//! resolver — would make every level of that nesting the same type, and the
//! regress a runtime one. This crate stores its transport by value and is
//! generic over it for exactly that reason. Today the hatch is shut twice
//! over, and both halves were measured rather than argued: taking rustc's
//! own `Box` suggestion produces a type that is no longer a `Resolve`
//! (`Box<C>` is not a `Transport`), and `dyn Resolve` cannot be written at
//! all, because `lookup_ipv4` returns an `impl Trait`. **The second is an
//! accident**, not a promise anyone has made — `impl Stream` was chosen for
//! RFC 8305 — so this paragraph is the place to look when someone proposes
//! an object-safe `Resolve` or a blanket `impl Transport for Box<T>`.
//! Neither would be a change to this crate.
//!
//! ## 3. What happens when the DoH server is unreachable?
//!
//! **It fails closed, and the alternative is visible in the type.**
//! `Doh<C>` is `Doh<C, NoFallback>`: a DoH failure is a resolution failure,
//! full stop. [`Doh::with_fallback`] returns a `Doh<C, F>` for the caller's
//! `F: Resolve`, and that type is then written into the transport that
//! holds it — `Native<R, T, Doh<C, SystemDns<R>>>` says on its face that
//! this client will resolve through the system when its DoH server is down.
//!
//! Neither behaviour is a good default for everyone, which is exactly why
//! neither is silent. Failing closed leaves a working network unusable the
//! moment the DoH endpoint is unreachable. Failing open silently defeats
//! the reason someone chose DoH: an attacker who can drop packets to the
//! DoH endpoint can, for the price of that one denial of service, move
//! every subsequent lookup onto the plaintext resolver they were being kept
//! away from. The second is a downgrade attack and it costs one dropped
//! connection to mount, so it is not something to arrive at by accident —
//! but a build with no other resolver at all genuinely wants the first, and
//! neither is ours to pick. See [`Doh::with_fallback`] for the precise
//! rule about when the fallback's answer is used.
//!
//! # What this crate can do that the system resolver often cannot
//!
//! `supports_svcb()` is **`true`**, and unlike a platform resolver it is
//! true on every target: an HTTPS/SVCB query is an ordinary DNS query in an
//! ordinary HTTP body, so nothing about it depends on whether the local
//! stub resolver forwards type 65. That is the point of the crate — see
//! §W3, which names Windows 10, wasm, and anything behind a stub resolver
//! that drops type 65.
//!
//! `ResolvedAddr::ttl` is filled from the record's own TTL, per record
//! rather than per RRset, for the same reason `http-ng-dns-hickory` gives:
//! a caller doing its own caching wants the value the server actually sent.
//!
//! # What it deliberately does not do
//!
//! **No cache.** Every `lookup_*` is an HTTP request. §W3 calls the TTL
//! "the first consumer of a field nothing reads", and filling the field is
//! this crate's job; deciding what to keep and for how long is a caller's,
//! and a cache built in here would be one no caller could turn off.
//!
//! **No total bound on a query.** [`Doh::timeouts`] sets `connect`,
//! `first_byte` and `between_bytes` in the request's extensions, which is
//! everything [`http_ng_core::Timeouts`] can express — there is no `total`
//! there, because in `http-ng` a total budget is enforced by `Client`,
//! which this crate deliberately does not use (question 2 above). A server
//! that answers the head promptly and then dribbles the body one byte per
//! `between_bytes` interval is therefore bounded only by
//! [`MAX_RESPONSE_BYTES`] times that interval. Recorded rather than hidden;
//! closing it needs a `Timer` in this crate, which `Resolve` has no seam
//! for.
//!
//! **POST, not GET.** RFC 8484 §4.1 defines both, and a server must
//! support both. GET carries the query base64url-encoded in `?dns=`, which
//! makes it cacheable by intermediaries — and needs a base64 encoder this
//! workspace does not have and would rather not add for one call site.
//! POST needs none, and an intermediary cache is not obviously something a
//! DNS-over-HTTPS deployment wants anyway.
#![forbid(unsafe_code)]

mod wire;

pub use wire::{DohError, MAX_RESPONSE_BYTES};

use futures_core::Stream;
use futures_util::StreamExt;
use futures_util::stream;
use http::Uri;
use http_body_util::BodyExt;
use http_ng_core::unversioned::Transport;
use http_ng_core::{Error, ErrorKind, RequestBody, Timeouts};
use http_ng_dns::{Resolve, ResolvedAddr, SvcbEndpoint};
use std::net::IpAddr;
use std::time::Duration;
use wire::{Family, Query};

/// `application/dns-message`, RFC 8484 §6. The one media type this crate
/// sends and the one it accepts.
const DNS_MESSAGE: &str = "application/dns-message";

/// The default per-query bounds: 2 s to connect, 5 s to the first response
/// byte, 5 s between body frames.
///
/// Chosen to sit in the same range as a stub resolver's own (`resolv.conf`
/// defaults to a 5 s timeout with 2 attempts), so that a DoH lookup does
/// not silently become the slowest thing in a connection attempt. Override
/// with [`Doh::timeouts`].
const DEFAULT_TIMEOUTS: Timeouts = Timeouts {
    connect: Some(Duration::from_secs(2)),
    first_byte: Some(Duration::from_secs(5)),
    between_bytes: Some(Duration::from_secs(5)),
};

/// The absence of a fallback resolver, as a type.
///
/// `Doh<C>` means `Doh<C, NoFallback>` and fails closed. This is a
/// [`Resolve`] whose every stream is empty, which is what makes the rule in
/// [`Doh::with_fallback`] uniform: "the fallback produced nothing, so the
/// DoH error stands" and "there is no fallback" are the same code path
/// rather than two.
///
/// `supports_svcb()` stays at the trait's `false` default, which is correct
/// and load-bearing: an empty stream from this type means "this resolver
/// cannot", never "asked and found nothing" — the distinction
/// `http_ng_dns`'s module doc draws.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoFallback;

impl Resolve for NoFallback {
    fn lookup_ipv4(&self, _name: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>> {
        stream::empty()
    }
    fn lookup_ipv6(&self, _name: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>> {
        stream::empty()
    }
}

/// A [`Resolve`] that asks a DoH server over the transport `C`.
///
/// Read this crate's module doc before the methods: the three decisions
/// that matter — what resolves the DoH host, what makes the request, and
/// what happens when it fails — are made by which constructor you call and
/// what `F` is, and they are the reason the API looks the way it does.
#[derive(Debug, Clone)]
pub struct Doh<C, F = NoFallback> {
    client: C,
    endpoint: Uri,
    fallback: F,
    timeouts: Timeouts,
}

/// Why an endpoint URI was refused at construction.
///
/// Every variant is a caller mistake that would otherwise show up as a
/// resolution failure at the first lookup, i.e. arbitrarily far from the
/// line that caused it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum EndpointError {
    /// The URI has no host at all — `/dns-query`, or `https:///`.
    #[error("the DoH endpoint `{uri}` has no host")]
    NoHost { uri: String },
    /// [`Doh::pinned`] was given a name.
    #[error(
        "`{host}` is a name, not an IP literal: `Doh::pinned` is the no-bootstrap constructor, \
         use `Doh::bootstrapped` if the inner transport's resolver should look this name up"
    )]
    NotAnIpLiteral { host: String },
    /// [`Doh::bootstrapped`] was given an IP literal.
    #[error(
        "`{host}` is an IP literal, so nothing bootstraps it: use `Doh::pinned`, which says so"
    )]
    IsAnIpLiteral { host: String },
    /// The scheme is neither `https` nor loopback `http`. See
    /// [`Doh::pinned`] for the loopback rule.
    #[error(
        "the DoH endpoint `{uri}` is not https, and its host is not a loopback address: \
         RFC 8484 is DNS over HTTPS, and cleartext DNS to a host that is not this machine \
         is the thing it exists to prevent"
    )]
    NotConfidential { uri: String },
}

impl<C> Doh<C, NoFallback> {
    /// A DoH endpoint whose host is an **IP literal**: no bootstrap, no
    /// name to resolve, no cycle to worry about.
    ///
    /// `https://1.1.1.1/dns-query`, `https://[2606:4700:4700::1111]/dns-query`.
    /// The literal is checked here — a name is [`EndpointError::NotAnIpLiteral`],
    /// which is the whole difference between this constructor and
    /// [`Doh::bootstrapped`].
    ///
    /// **The second of those two examples does not work today over
    /// `http-ng-native`, and the defect is not in this crate.** Measured
    /// against `2606:4700:4700::1111`: the TCP connection is made and the
    /// handshake then fails with `Tls: invalid dns name`, before any DNS is
    /// exchanged. `http::Uri::host()` returns an IPv6 literal **with its
    /// brackets**, `http-ng-native`'s connector passes that string to
    /// `TlsRequest::server_name` unchanged, and
    /// `rustls_pki_types::ServerName::try_from` accepts neither `[…]` as a
    /// DNS name nor as an address. This constructor and
    /// `IpLiteralOnly::literal` both strip the brackets, each with a
    /// comment about this exact trap; the TLS name is the one place nobody
    /// does. It is one line in `http-ng-native` or in
    /// `http-ng-tls-rustls`, and it is pinned meanwhile by
    /// `tests/live.rs`'s
    /// `an_ipv6_literal_endpoint_fails_at_tls_today_and_the_defect_is_not_in_this_crate`,
    /// whose failure is the signal to delete this paragraph. An IPv4
    /// literal is unaffected, and a v6 endpoint over a different transport
    /// is untested rather than broken.
    ///
    /// **The cost of pinning, which is real and which this crate cannot
    /// mitigate:** an address that stops answering leaves this resolver
    /// with nothing to ask, and DoH is then not slow but absent. There is
    /// no discovery here to route around it, by construction (see the
    /// module doc on RFC 9461). A deployment that pins should either track
    /// the provider's published addresses or take [`Doh::with_fallback`]
    /// and accept what that means.
    ///
    /// **The certificate that address presents must carry an IP SAN**, and
    /// `docs/v03-design.md` §W3 listed it as unverified whether the
    /// platform verifiers accept one. **On Linux they do** — measured, not
    /// argued: `tests/live.rs`'s
    /// `a_certificate_presented_for_an_ip_address_validates_through_the_platform_verifier`
    /// completes the handshake through `rustls-platform-verifier` against
    /// Cloudflare's `1.1.1.1` and Google's `8.8.8.8` and reads a DNS answer
    /// back from each.
    ///
    /// The question is still open on **macOS and Windows**, and by more
    /// than a missing runner: those two do not use rustls's own webpki path
    /// at all but hand the chain to Security.framework and to CryptoAPI,
    /// which apply their own name-matching rules. Linux is therefore not
    /// evidence about them. Run `just test-doh-live` on either and the
    /// answer is one line of output.
    ///
    /// **`http://` is accepted for a loopback literal, and that is not a
    /// concession to tests.** A local DoH proxy on `127.0.0.1` — the
    /// `dnscrypt-proxy` and `cloudflared` deployment shape — never puts a
    /// query on a network, so the confidentiality TLS buys against a
    /// network observer is not at stake. Any other cleartext endpoint is
    /// [`EndpointError::NotConfidential`].
    pub fn pinned(client: C, endpoint: Uri) -> Result<Self, EndpointError> {
        let host = host_of(&endpoint)?;
        let Some(addr) = ip_literal(host) else {
            return Err(EndpointError::NotAnIpLiteral {
                host: host.to_owned(),
            });
        };
        check_confidential(&endpoint, Some(addr))?;
        Ok(Self::build(client, endpoint))
    }

    /// A DoH endpoint whose host is a **name**, resolved by the resolver
    /// the transport `client` already carries.
    ///
    /// Which of §W3's bootstrap shapes that is depends entirely on what you
    /// pass: a transport over `SystemDns` is "the system resolver, once,
    /// for the DoH host"; a transport over a resolver holding fixed
    /// addresses for that name is "caller-supplied bootstrap addresses".
    /// This crate cannot tell them apart and does not need to.
    ///
    /// **What it can tell apart, and refuses:** a transport whose resolver
    /// is this same `Doh`. Not with a check — with the type system; see the
    /// module doc, and `tests/no_cycle.rs`.
    ///
    /// An IP literal here is [`EndpointError::IsAnIpLiteral`]: it would
    /// work, but it would be a `bootstrapped` that bootstraps nothing, and
    /// the point of having two constructors is that the one you called is
    /// true.
    pub fn bootstrapped(client: C, endpoint: Uri) -> Result<Self, EndpointError> {
        let host = host_of(&endpoint)?;
        if let Some(_addr) = ip_literal(host) {
            return Err(EndpointError::IsAnIpLiteral {
                host: host.to_owned(),
            });
        }
        check_confidential(&endpoint, None)?;
        Ok(Self::build(client, endpoint))
    }

    fn build(client: C, endpoint: Uri) -> Self {
        Self {
            client,
            endpoint,
            fallback: NoFallback,
            timeouts: DEFAULT_TIMEOUTS,
        }
    }
}

impl<C, F> Doh<C, F> {
    /// Fail **open** to `fallback` when the DoH query fails.
    ///
    /// The rule, exactly: a lookup that fails at the DoH layer — no
    /// connection, a non-200, a body that is not a DNS message, a SERVFAIL
    /// — is retried against `fallback`, and **whatever the fallback's
    /// stream yields is the answer, including its errors**. Only if the
    /// fallback yields *nothing at all* does the DoH error surface, which
    /// is what makes [`NoFallback`] (whose streams are always empty) mean
    /// "fail closed" without a second code path.
    ///
    /// That last clause has one consequence worth stating, because it is a
    /// deliberate conflation and not an oversight: if DoH fails and the
    /// fallback genuinely has no record of that family, the caller sees the
    /// **DoH error**, not an empty stream. It is the honest answer — DoH
    /// failed, so nobody established that the family is absent — but it
    /// does mean an empty answer from the fallback is not distinguishable
    /// here from a fallback that could not answer.
    ///
    /// A DoH answer that *succeeded* is never second-guessed: NXDOMAIN and
    /// an empty answer section are answers, and asking the fallback for a
    /// second opinion on them would be the downgrade the module doc
    /// describes, on every lookup rather than only under attack.
    ///
    /// The type changes — `Doh<C, NoFallback>` becomes `Doh<C, F>` — and
    /// that is the point: it is written into the type of every transport
    /// that holds this resolver.
    pub fn with_fallback<F2: Resolve>(self, fallback: F2) -> Doh<C, F2> {
        Doh {
            client: self.client,
            endpoint: self.endpoint,
            fallback,
            timeouts: self.timeouts,
        }
    }

    /// Replace the per-query [`Timeouts`] put into each request's
    /// extensions.
    ///
    /// These are the transport's to enforce, and a transport whose
    /// `Capabilities::timeouts` says it does not enforce them will ignore
    /// them — this crate declares nothing on their behalf. There is no
    /// `total` here to set; see the module doc's last section for what that
    /// leaves unbounded.
    #[must_use]
    pub fn timeouts(mut self, timeouts: Timeouts) -> Self {
        self.timeouts = timeouts;
        self
    }

    /// The endpoint this resolver queries. Useful mostly for asserting in a
    /// test that the constructor kept what it was given.
    #[must_use]
    pub fn endpoint(&self) -> &Uri {
        &self.endpoint
    }
}

/// `http::Uri::host()` returns an IPv6 literal WITH its brackets (`[::1]`),
/// which is not what `IpAddr::from_str` parses — the same trap
/// `IpLiteralOnly::literal` documents in `http-ng-dns`.
fn ip_literal(host: &str) -> Option<IpAddr> {
    let bare = host
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(host);
    bare.parse::<IpAddr>().ok()
}

fn host_of(uri: &Uri) -> Result<&str, EndpointError> {
    uri.host().ok_or_else(|| EndpointError::NoHost {
        uri: uri.to_string(),
    })
}

/// `https`, or `http` to a loopback literal. See [`Doh::pinned`].
fn check_confidential(uri: &Uri, host: Option<IpAddr>) -> Result<(), EndpointError> {
    if uri.scheme_str() == Some("https") {
        return Ok(());
    }
    if uri.scheme_str() == Some("http") && host.is_some_and(|a| a.is_loopback()) {
        return Ok(());
    }
    Err(EndpointError::NotConfidential {
        uri: uri.to_string(),
    })
}

impl<C, F> Doh<C, F>
where
    C: Transport,
    C::Error: Send + Sync, // send-bound-exception: amendment-C1
    <C::Body as http_body::Body>::Error: std::error::Error + Send + Sync + 'static, // send-bound-exception: amendment-C1
    F: Resolve,
{
    /// One RFC 8484 exchange: encode a query, POST it, read the answer
    /// back.
    async fn exchange(&self, name: &str, query: Query) -> Result<wire::Answer, DohError> {
        let body = wire::encode_query(name, query)?;

        let mut req = http::Request::new(RequestBody::Full(body));
        *req.method_mut() = http::Method::POST;
        *req.uri_mut() = self.endpoint.clone();
        req.headers_mut().insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static(DNS_MESSAGE),
        );
        // RFC 8484 §4.1: a client SHOULD say what it can read back. A
        // server that answers with anything else is refused below rather
        // than parsed hopefully.
        req.headers_mut().insert(
            http::header::ACCEPT,
            http::HeaderValue::from_static(DNS_MESSAGE),
        );
        req.extensions_mut().insert(self.timeouts);

        let response = self
            .client
            .execute(req)
            .await
            .map_err(|e| DohError::Transport(self.client.to_error(e)))?;

        let status = response.status();
        if status != http::StatusCode::OK {
            return Err(DohError::Status {
                status: status.as_u16(),
            });
        }
        // Checked before the body is read, not after: a proxy's HTML error
        // page returned with a 200 is the case this catches, and decoding
        // it as DNS would either fail with a confusing message or — worse —
        // succeed on some prefix.
        let content_type = response
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.split(';').next().unwrap_or(v).trim().to_ascii_lowercase());
        if content_type.as_deref() != Some(DNS_MESSAGE) {
            return Err(DohError::ContentType {
                got: content_type.unwrap_or_default(),
            });
        }

        // `Limited` before `collect`: the body length is chosen by the
        // server, and a DoH endpoint is a thing a client talks to before it
        // has decided to trust anything. `MAX_RESPONSE_BYTES` is the
        // largest a DNS message can be, so no legitimate answer is cut.
        let collected = http_body_util::Limited::new(response.into_body(), MAX_RESPONSE_BYTES)
            .collect()
            .await
            .map_err(|e| DohError::Body(e.to_string()))?;

        wire::decode_answer(collected.to_bytes(), name, query)
    }

    /// The whole of one family's lookup, as the `Vec` the stream is built
    /// from.
    ///
    /// A `Vec` rather than a genuinely incremental stream because one DoH
    /// query is one HTTP request: every address arrives in the same
    /// response, so there is nothing to hand out early. The `Stream` in
    /// `Resolve` is there so that A and AAAA can proceed independently
    /// (RFC 8305), and they do — these are two separate requests — but
    /// *within* one family there is no partial answer to stream.
    async fn addrs(&self, name: &str, family: Family) -> Vec<Result<ResolvedAddr, Error>> {
        // An IP literal is not a name and there is nothing to ask about it.
        //
        // **This is not an optimisation; without it a `Doh`-backed client
        // cannot reach `https://192.0.2.1/` at all.**
        // `http_ng_native::connect` hands `Uri::host()` to the resolver
        // unconditionally — there is no literal shortcut above this seam,
        // which is why `IpLiteralOnly` exists as a `Resolve` rather than as
        // a branch in the connector. So a DoH resolver that queried for the
        // "name" `192.0.2.1` would get NXDOMAIN from any honest server, and
        // every IP-literal URL would fail to connect. Found by reading
        // `connect.rs` while looking for what the mutation table was not
        // covering, not by a bug report.
        //
        // The rule is `IpLiteralOnly`'s, deliberately: the literal goes to
        // its own family's stream and the other family gets an empty one,
        // because "there is no AAAA for this v4 literal" is a true and
        // unremarkable answer rather than a failure.
        if let Some(addr) = ip_literal(name) {
            return match (addr, family) {
                (IpAddr::V4(_), Family::V4) | (IpAddr::V6(_), Family::V6) => {
                    vec![Ok(ResolvedAddr { addr, ttl: None })]
                }
                _ => Vec::new(),
            };
        }
        match self.exchange(name, family.query()).await {
            Ok(answer) => answer.addrs.into_iter().map(Ok).collect(),
            Err(e) => self.recover(name, family, e).await,
        }
    }

    /// [`Doh::with_fallback`]'s rule, in one place for both families.
    ///
    /// Takes a [`Family`] rather than a [`Query`], and that is a correction
    /// rather than a tidy-up. With a `Query` there was a third arm, for
    /// `Https`, that nothing could reach — `lookup_svcb` deliberately does
    /// not come through here — and a mutation of that arm survived the
    /// whole suite, because an unreachable arm cannot be killed by any
    /// test. The type now says what both callers already meant, and the
    /// arm is gone rather than covered.
    async fn recover(
        &self,
        name: &str,
        family: Family,
        failure: DohError,
    ) -> Vec<Result<ResolvedAddr, Error>> {
        let recovered: Vec<Result<ResolvedAddr, Error>> = match family {
            Family::V4 => self.fallback.lookup_ipv4(name).collect().await,
            Family::V6 => self.fallback.lookup_ipv6(name).collect().await,
        };
        if recovered.is_empty() {
            vec![Err(failure.into())]
        } else {
            recovered
        }
    }
}

impl<C, F> Resolve for Doh<C, F>
where
    C: Transport,
    C::Error: Send + Sync, // send-bound-exception: amendment-C1
    <C::Body as http_body::Body>::Error: std::error::Error + Send + Sync + 'static, // send-bound-exception: amendment-C1
    F: Resolve,
{
    fn lookup_ipv4(&self, name: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>> {
        let name = name.to_owned();
        stream::once(async move { self.addrs(&name, Family::V4).await }).flat_map(stream::iter)
    }

    fn lookup_ipv6(&self, name: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>> {
        let name = name.to_owned();
        stream::once(async move { self.addrs(&name, Family::V6).await }).flat_map(stream::iter)
    }

    /// **`true`, and this is the reason the crate exists.**
    ///
    /// An HTTPS/SVCB query is an ordinary DNS query carried in an ordinary
    /// HTTP body, so this answer does not depend on the platform, on the
    /// local stub resolver forwarding type 65, or on a libc exposing a raw
    /// query API. `http-ng-dns-system` can only say `true` where its
    /// backend can (`res_query` on Unix, `DnsQuery_UTF8` on Windows), and
    /// `IpLiteralOnly` and `wasi:http` can never say it at all.
    ///
    /// The pair with [`Self::lookup_svcb`] is the one `http_ng_dns`'s
    /// module doc requires: both overridden together, so an empty stream
    /// from here means "asked, found none" and nothing else.
    fn supports_svcb(&self) -> bool {
        true
    }

    fn lookup_svcb(&self, name: &str) -> impl Stream<Item = Result<SvcbEndpoint, Error>> {
        let name = name.to_owned();
        stream::once(async move {
            // An IP literal has no owner name to carry an HTTPS record, so
            // there is nothing to ask — the same rule as `Self::addrs`, and
            // the same reason. Empty rather than an error, because with
            // `supports_svcb() == true` an empty stream reads as "asked,
            // found none", and for a literal there is genuinely none.
            if ip_literal(&name).is_some() {
                return Vec::new();
            }
            match self.exchange(&name, Query::Https).await {
                Ok(answer) => answer.endpoints.into_iter().map(Ok).collect(),
                // Deliberately NOT routed through `recover`: a fallback
                // resolver is a `Resolve`, and asking it for SVCB when it
                // reports `supports_svcb() == false` would turn its
                // honest "I cannot" into our empty "there are none". The
                // DoH error stands.
                Err(e) => vec![Err(Error::from(e))],
            }
        })
        .flat_map(stream::iter)
    }
}

impl From<DohError> for Error {
    /// Always [`ErrorKind::Resolve`], including when the cause was a
    /// connect failure or a timeout inside the DoH transport.
    ///
    /// The category names *which operation failed for the caller*, and the
    /// caller asked this object to resolve a name. A DoH server that cannot
    /// be connected to is not the user's connection failing — the user's
    /// connection has not been attempted, and reporting `Connect` would
    /// send anyone reading `kind()` looking at the wrong host entirely. The
    /// transport's own classified error is kept as the `source`, where the
    /// detail belongs.
    fn from(e: DohError) -> Self {
        Error::new(ErrorKind::Resolve, e)
    }
}
