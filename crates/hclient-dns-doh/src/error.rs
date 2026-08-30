//! Two refusals, at the two moments this resolver can have one.
//!
//! [`EndpointError`] is raised at **construction** and [`DohError`] at a
//! **lookup**, and the split is the crate's bootstrap argument written as
//! types: which constructor compiles is what says whether the DoH server's
//! own name gets resolved, so the mistakes that question admits —
//! a literal handed to `bootstrapped`, a name handed to `pinned`, a
//! cleartext endpoint that is not this machine — are caught on the line
//! that made them rather than at the first lookup, arbitrarily far away.
//!
//! **What unites every [`DohError`] variant is that it is a failure to
//! *ask*, never an empty answer.** A name that does not exist is `Ok` with
//! no addresses; the module doc in `wire.rs` has the table that draws that
//! line. So a caller with a fallback can read this enum as *the question
//! did not get through* without inspecting which variant it got — which is
//! what makes `Doh<C, F>`'s fallback a policy rather than a guess.
//!
//! Both are re-exported at the crate root, where they have always been, so
//! no consumer's `use` line moves.

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
    /// [`Doh::pinned`](crate::Doh::pinned) was given a name.
    #[error(
        "`{host}` is a name, not an IP literal: `Doh::pinned` is the no-bootstrap constructor, \
         use `Doh::bootstrapped` if the inner transport's resolver should look this name up"
    )]
    NotAnIpLiteral { host: String },
    /// [`Doh::bootstrapped`](crate::Doh::bootstrapped) was given an IP literal.
    #[error(
        "`{host}` is an IP literal, so nothing bootstraps it: use `Doh::pinned`, which says so"
    )]
    IsAnIpLiteral { host: String },
    /// The scheme is neither `https` nor loopback `http`. See
    /// [`Doh::pinned`](crate::Doh::pinned) for the loopback rule.
    #[error(
        "the DoH endpoint `{uri}` is not https, and its host is not a loopback address: \
         RFC 8484 is DNS over HTTPS, and cleartext DNS to a host that is not this machine \
         is the thing it exists to prevent"
    )]
    NotConfidential { uri: String },
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
    /// transport's own classified [`hclient_core::Error`] is kept whole.
    #[error("the DoH request failed: {0}")]
    Transport(#[source] hclient_core::Error),
    /// The DoH server answered with something other than 200. RFC 8484 §4.2
    /// gives no other success status.
    #[error("the DoH server answered with HTTP status {status}")]
    Status { status: u16 },
    /// The response was not `application/dns-message` (RFC 8484 §6). A
    /// captive portal's login page, most often.
    #[error("the DoH server answered with content-type `{got}`, not `application/dns-message`")]
    ContentType { got: String },
    /// The response body could not be read to the end, or exceeded
    /// [`MAX_RESPONSE_BYTES`](crate::MAX_RESPONSE_BYTES).
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
    /// `hclient_dns::svcb`.
    #[error("SvcParamKey {key} is listed as mandatory but is not present in the record")]
    MandatoryKeyAbsent { key: u16 },
}
