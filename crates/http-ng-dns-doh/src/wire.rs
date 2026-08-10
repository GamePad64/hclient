//! The DNS message, in and out. RFC 1035 framing, RFC 8484 carriage.
//!
//! **Nothing here parses bytes by hand.** `dns-message-parser` does the
//! decoding, for the reasons `http-ng-dns-system`'s `svcb.rs` wrote down
//! when it chose that crate: no `unsafe` anywhere in its `src`, a
//! `DecodeResult` on every path rather than a panic, and name
//! decompression that terminates by tracking visited offsets instead of
//! trusting a "pointers point backwards" rule. Over a DoH response that
//! matters more than it does there, not less: the bytes come from an HTTP
//! body, so a compromised or hostile endpoint chooses every one of them.
//!
//! **The encode path is new here**, and §W3 listed it as unverified: the
//! decode side is what `http-ng-dns-system` uses, and nothing in this
//! workspace had ever asked `dns-message-parser` to *build* a message.
//! `tests/query_bytes.rs` settles it against a hand-written expected
//! encoding of a query, byte for byte.
//!
//! **What this module refuses, and why each refusal is not an empty
//! answer.** RFC 8484 leaves DNS semantics exactly where they were, so the
//! distinction `Resolve` draws between "asked and found nothing" and
//! "could not ask" has to survive the HTTP layer intact:
//!
//! | condition | result |
//! |---|---|
//! | NOERROR, no records of the type | empty `Vec` — an answer |
//! | NXDOMAIN | empty `Vec` — an answer, from an authority |
//! | any other RCODE | [`DohError::ResponseCode`] |
//! | `QR` clear (a query came back) | [`DohError::NotAResponse`] |
//! | `TC` set | [`DohError::Truncated`] — see below |
//! | the question does not match the one sent | [`DohError::QuestionMismatch`] |
//! | anything the decoder refuses | [`DohError::Malformed`] |
//!
//! `TC` deserves a note because DoH is the one transport where it should
//! never appear: the response travels over TCP-or-better with no 512-byte
//! limit, so a truncated answer means the *server's own* upstream lookup
//! was truncated and it passed that on. There is no retry this crate can
//! make that the server has not already made, so it is an error rather than
//! a partial RRSet — the same call `http-ng-dns-system` makes for the same
//! reason.

use bytes::Bytes;
use dns_message_parser::question::{QClass, QType, Question};
use dns_message_parser::rr::RR;
use dns_message_parser::{Dns, Flags, Opcode, RCode};
use http_ng_dns::svcb::{binding_from_decoded, endpoint_from_binding};
use http_ng_dns::{ResolvedAddr, SvcbEndpoint};
use std::net::IpAddr;
use std::time::Duration;

/// The largest response body this crate will read, in bytes.
///
/// The width of the length field that frames a DNS message over TCP, so no
/// legitimate answer is cut by it — and a bound is needed, because the body
/// length is chosen by a server the client has not yet decided to trust.
pub const MAX_RESPONSE_BYTES: usize = 65_535;

/// The three questions this crate knows how to ask.
///
/// A closed enum rather than a `u16` passed around: it is what makes
/// [`decode_answer`]'s question check total, and it keeps `lookup_ipv4`
/// from being able to ask for `AAAA` by getting an argument wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Query {
    A,
    Aaaa,
    Https,
}

impl Query {
    fn qtype(self) -> QType {
        match self {
            Self::A => QType::A,
            Self::Aaaa => QType::AAAA,
            Self::Https => QType::HTTPS,
        }
    }
}

/// The two questions an *address* lookup can ask.
///
/// A second, narrower enum rather than reusing [`Query`], because the
/// address path and the SVCB path are genuinely different: `lookup_svcb`
/// does not go through `Doh::recover`, so a `recover` taking a `Query` had
/// an `Https` arm nothing could reach. Mutation testing found it — an
/// unreachable arm cannot be killed by any test, which is exactly the kind
/// of line that reads as load-bearing and proves nothing. This type is the
/// fix: the arm is gone rather than covered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Family {
    V4,
    V6,
}

impl Family {
    pub(crate) fn query(self) -> Query {
        match self {
            Self::V4 => Query::A,
            Self::V6 => Query::Aaaa,
        }
    }
}

/// What one exchange produced. Only one of the two is ever non-empty — the
/// other belongs to a question that was not asked.
#[derive(Debug, Default)]
pub(crate) struct Answer {
    pub(crate) addrs: Vec<ResolvedAddr>,
    pub(crate) endpoints: Vec<SvcbEndpoint>,
}

/// Everything that can go wrong between "the caller asked for a name" and
/// "there is a decoded answer".
///
/// Every variant is a *failure to ask*, never an empty answer — see the
/// table in this module's doc for where the line is drawn.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum DohError {
    /// The name cannot be put in a DNS question: it is not a valid domain
    /// name, or a label is too long. A caller bug or a hostile input, never
    /// a server problem.
    #[error("`{name}` cannot be used as a DNS query name: {reason}")]
    NameNotUsable { name: String, reason: String },
    /// The query could not be serialised. Structurally unreachable for a
    /// one-question query with no answer sections — kept because the
    /// encoder returns a `Result` and swallowing it would be the
    /// discarded-result defect this workspace has an ast-grep rule about.
    #[error("could not encode the DNS query: {0}")]
    Encode(String),
    /// The transport failed: no connection, a timeout, a TLS failure. The
    /// transport's own classified [`http_ng_core::Error`] is kept whole.
    #[error("the DoH request failed: {0}")]
    Transport(#[source] http_ng_core::Error),
    /// The DoH server answered with something other than 200. RFC 8484 §4.2
    /// gives no other success status.
    #[error("the DoH server answered with HTTP status {status}")]
    Status { status: u16 },
    /// The response was not `application/dns-message` (RFC 8484 §6). A
    /// captive portal's login page, most often.
    #[error("the DoH server answered with content-type `{got}`, not `application/dns-message`")]
    ContentType { got: String },
    /// The response body could not be read to the end, or exceeded
    /// [`MAX_RESPONSE_BYTES`].
    ///
    /// A `String` rather than the body's own error type: `Transport::Body`'s
    /// error is an unconstrained associated type, so there is no type here
    /// to name — and `http_body_util::Limited` wraps it in a
    /// `Box<dyn Error>` of its own anyway, which is not `Clone` and this
    /// enum is.
    #[error("could not read the DoH response body: {0}")]
    Body(String),
    /// The bytes were not a DNS message. RFC 9460 §2.2 requires rejecting
    /// the whole RRSet when any record is malformed, which is what a
    /// whole-message decode failure already gives: no half-parsed answer
    /// reaches a caller.
    #[error("the DoH server's answer is not a valid DNS message: {0}")]
    Malformed(String),
    /// `QR` was clear: what came back is a query, not a response.
    #[error("the DoH server echoed a query rather than answering one")]
    NotAResponse,
    /// `TC` was set. See this module's doc for why this is an error over
    /// DoH specifically.
    #[error("the DoH server's answer was truncated")]
    Truncated,
    /// RCODE was neither NOERROR nor NXDOMAIN. RFC 1035 §4.1.1 / RFC 6895
    /// §2.3.
    #[error("the DoH server answered with RCODE {rcode}")]
    ResponseCode { rcode: u8 },
    /// The response's question section does not echo the question that was
    /// asked.
    ///
    /// Cheap, and worth doing even though HTTP already binds a response to
    /// its request: it catches a server (or a cache in front of one) that
    /// answers the wrong question, which over plain DNS would be a
    /// cache-poisoning primitive and here would be a silent wrong address.
    #[error("the DoH server answered a different question: asked {asked}, got {got}")]
    QuestionMismatch { asked: String, got: String },
    /// RFC 9460 §8: a record's `mandatory` list names a key the record does
    /// not carry. The one client-side malformity no decoder checks — see
    /// `http_ng_dns::svcb`.
    #[error("SvcParamKey {key} is listed as mandatory but is not present in the record")]
    MandatoryKeyAbsent { key: u16 },
}

/// One RFC 8484 query, ready to be a request body.
///
/// **The ID is zero, deliberately** (RFC 8484 §4.1: "the DNS ID SHOULD be
/// 0"). Over UDP the ID is what binds an answer to its question against an
/// off-path attacker; over HTTP that binding is the HTTP exchange itself,
/// and a varying ID only makes two identical queries look different to a
/// cache. The question echoed in the response is checked instead — see
/// [`DohError::QuestionMismatch`].
///
/// `rd` is set: this is a stub resolver asking a recursive one.
pub(crate) fn encode_query(name: &str, query: Query) -> Result<Bytes, DohError> {
    // No bracket stripping here, deliberately. `Uri::host()` brackets an
    // IPv6 literal (`[::1]`) and that is the string `Resolve` receives —
    // but a literal never reaches this function, because `Doh::addrs`
    // answers it from the string itself and makes no query at all. What
    // could still arrive bracketed is a thing that is *not* a literal, and
    // turning `[foo]` into a query for `foo` would be inventing a name the
    // caller did not give.
    let domain_name =
        name.parse().map_err(
            |e: dns_message_parser::DomainNameError| DohError::NameNotUsable {
                name: name.to_owned(),
                reason: e.to_string(),
            },
        )?;

    let dns = Dns {
        id: 0,
        flags: Flags {
            qr: false,
            opcode: Opcode::Query,
            aa: false,
            tc: false,
            rd: true,
            ra: false,
            ad: false,
            cd: false,
            rcode: RCode::NoError,
        },
        questions: vec![Question {
            domain_name,
            q_class: QClass::IN,
            q_type: query.qtype(),
        }],
        answers: Vec::new(),
        authorities: Vec::new(),
        additionals: Vec::new(),
    };

    Ok(dns
        .encode()
        .map_err(|e| DohError::Encode(e.to_string()))?
        .freeze())
}

/// A response body, as the records a caller may act on.
pub(crate) fn decode_answer(body: Bytes, name: &str, query: Query) -> Result<Answer, DohError> {
    let dns = Dns::decode(body).map_err(|e| DohError::Malformed(e.to_string()))?;

    if !dns.is_response() {
        return Err(DohError::NotAResponse);
    }
    if dns.flags.tc {
        return Err(DohError::Truncated);
    }
    check_question(&dns, name, query)?;
    match dns.flags.rcode {
        RCode::NoError => {}
        // The name does not exist, said by an authority. A definitive
        // answer — there are no records of any type — and not a failure to
        // ask, so it is an empty `Vec` rather than an error, exactly as
        // `http-ng-dns-system` treats the same RCODE.
        RCode::NXDomain => return Ok(Answer::default()),
        rcode => {
            return Err(DohError::ResponseCode { rcode: rcode as u8 });
        }
    }

    let mut answer = Answer::default();
    for rr in &dns.answers {
        // Records of a type that was not asked for — a CNAME chain, most
        // commonly — are stepped over rather than rejected. The owner name
        // is deliberately NOT compared against the question: a CNAME means
        // the addresses are legitimately owned by a different name, and
        // requiring a match would break every aliased host.
        match (query, rr) {
            (Query::A, RR::A(a)) => answer.addrs.push(ResolvedAddr {
                addr: IpAddr::V4(a.ipv4_addr),
                ttl: Some(Duration::from_secs(u64::from(a.ttl))),
            }),
            (Query::Aaaa, RR::AAAA(a)) => answer.addrs.push(ResolvedAddr {
                addr: IpAddr::V6(a.ipv6_addr),
                ttl: Some(Duration::from_secs(u64::from(a.ttl))),
            }),
            (Query::Https, RR::HTTPS(binding)) => {
                if let Some(endpoint) = endpoint_from_binding(&binding_from_decoded(binding))
                    .map_err(|e| match e {
                        http_ng_dns::svcb::SvcbRecordError::MandatoryKeyAbsent { key } => {
                            DohError::MandatoryKeyAbsent { key }
                        }
                    })?
                {
                    answer.endpoints.push(endpoint);
                }
            }
            _ => {}
        }
    }
    Ok(answer)
}

/// The response must echo exactly the question that was asked.
///
/// Compared case-insensitively on the name, because a server is free to
/// apply DNS 0x20 randomisation or to echo the name in a different case,
/// and neither is a different question.
fn check_question(dns: &Dns, name: &str, query: Query) -> Result<(), DohError> {
    let asked = format!("{}/{:?}", name.trim_end_matches('.'), query.qtype());
    let Some(question) = dns.questions.first() else {
        return Err(DohError::QuestionMismatch {
            asked,
            got: "no question section".to_owned(),
        });
    };
    let got_name = question.domain_name.to_string();
    let got_name = got_name.strip_suffix('.').unwrap_or(&got_name);
    let got = format!("{got_name}/{:?}", question.q_type);
    if !got_name.eq_ignore_ascii_case(name.trim_end_matches('.'))
        || question.q_type != query.qtype()
        || question.q_class != QClass::IN
    {
        return Err(DohError::QuestionMismatch { asked, got });
    }
    Ok(())
}
