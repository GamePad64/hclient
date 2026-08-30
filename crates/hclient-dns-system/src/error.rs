//! Two failures, and neither of them is "the name does not exist".
//!
//! That absence is the whole shape of this module. A name with no
//! addresses and a name with no HTTPS records are both ordinary answers —
//! an empty stream — and giving either one a variant here is the first
//! step toward a caller reading a name without records as a broken
//! resolver. So what is left is the two ways the *question* failed:
//! `getaddrinfo` refusing it ([`ResolveFailed`]), and a system SVCB
//! lookup that could not produce an RRSet it is willing to stand behind
//! ([`SvcbLookupError`]).
//!
//! **Both keep the platform's own answer rather than a category of ours.**
//! `ResolveFailed` carries the `io::Error` as a `#[source]` so a caller
//! can reach the errno without parsing text, and `SvcbLookupError` keeps
//! the raw `WIN32_ERROR` and the decoder's own `DecodeError` for the same
//! reason: the code is the only thing that tells a timeout from a
//! misconfigured resolver, and inventing a lossy category here would throw
//! that away.
//!
//! Neither type is public and neither becomes so by moving: `ResolveFailed`
//! reaches a caller only through `Error::source`, and `SvcbLookupError` is
//! `pub(crate)`, re-exported from [`crate::sys`] where it has always been.

/// `getaddrinfo` said no, with the name it was asked about kept alongside
/// the reason.
///
/// The name is in the message AND the `io::Error` is a `#[source]`, which
/// is not redundant: `hclient_core::Error` chains `source()`, so a caller
/// that wants the errno downcasts to `std::io::Error` through this without
/// parsing text, while a caller that only logs still sees which name
/// failed. Dropping `#[source]` would leave the second reader with the
/// same message and the first with nothing — see
/// `a_resolve_failure_keeps_the_io_error_reachable_by_downcast`.
#[derive(Debug, thiserror::Error)]
#[error("failed to resolve `{0}`: {1}")]
pub(crate) struct ResolveFailed(pub(crate) String, #[source] pub(crate) std::io::Error);

/// A system SVCB lookup that did not produce records, for a reason that is
/// not "there are none".
///
/// "There are none" is deliberately absent from this enum: it is not an
/// error, it is an empty stream, and giving it a variant here would be the
/// first step toward a caller treating a name without HTTPS records as a
/// broken resolver.
///
/// Same `dead_code` reasoning as `sys`'s own `RawAnswer`: a target with no
/// backend can never reach the variants only `res_query` produces.
///
/// `Clone` and `Eq` are deliberately absent. `Malformed` carries
/// `dns_message_parser::DecodeError`, which is `Debug + PartialEq + Error`
/// and neither. Flattening the decoder's error into a `String` to recover
/// those two derives was rejected: it would break the `source()` chain
/// that lets a caller reach the real cause without parsing a message,
/// which is what `hclient_core::Error` is built around.
///
/// The messages and the `source()` chain are `thiserror`'s now rather than
/// two hand-written impls. `Malformed` carries `#[source]` explicitly and
/// not `#[from]`: an automatic `From<DecodeError>` would make
/// `decode(..)?` compile anywhere in this crate, and the one place that
/// converts a decoder failure is a `map_err` that deserves to stay
/// visible. Every other variant has no source, which is a property with a
/// test of its own — a `{0}` interpolation without `#[source]` reads
/// identically in a message and silently truncates the chain a caller
/// downcasts through.
#[derive(Debug, PartialEq, thiserror::Error)]
#[allow(
    dead_code,
    reason = "each backend constructs a different subset, and a build compiles exactly one backend; repeating the target list of the `#[cfg]` above to narrow this would reintroduce the drift that single `#[cfg]` pair exists to prevent"
)]
pub(crate) enum SvcbLookupError {
    /// The name cannot be handed to a C API — it contains a NUL, or it is
    /// longer than a DNS name may be. A caller bug or a hostile input,
    /// never a resolver problem.
    #[error("`{name}` cannot be used as a DNS query name")]
    NameNotUsable { name: String },
    /// No resolver answered: every configured server timed out, was
    /// unreachable, or the query could not be built.
    #[error("no configured resolver answered")]
    NoResponse,
    /// The resolver answered, with an RCODE that is not a usable result.
    /// RFC 1035 §4.1.1 / RFC 6895 §2.3.
    #[error("resolver answered with RCODE {rcode}")]
    ResponseCode { rcode: u8 },
    /// `TC` was still set on the answer we ended up with. libc retries a
    /// truncated UDP answer over TCP itself, so seeing `TC` here means
    /// that retry did not happen or did not help — and a truncated answer
    /// cannot be read as a complete RRSet.
    #[error("answer was truncated and no complete one was obtained")]
    Truncated,
    /// The answer did not fit in a buffer sized at the largest a DNS
    /// message may be (65535, the width of the length field that frames
    /// one). Either the resolver is misreporting a length or something is
    /// very wrong; both are failures, not empty results.
    #[error("answer exceeds the largest possible DNS message")]
    AnswerTooLarge,
    /// The resolver's header claims answer records, but the call failed
    /// and so returned no length to read them with. Impossible on the
    /// three libcs this crate supports — `res_query` fails a NOERROR
    /// response only when the answer section is empty — and reported
    /// rather than assumed away, because the alternative is silently
    /// returning "no HTTPS records" for a name that has some.
    #[error(
        "resolver reported {ancount} answer record(s) but returned no length to read them with"
    )]
    LengthUnavailable { ancount: u16 },
    /// Fewer than the twelve bytes of a DNS header came back.
    #[error("answer is {got} bytes, shorter than the 12-byte DNS header")]
    HeaderTruncated { got: usize },
    /// The answer arrived and the decoder refused it. RFC 9460 §2.2
    /// requires rejecting the whole RRSet in that case, which is what
    /// `Dns::decode` failing already gives — one bad record fails the
    /// message, so no half-parsed RRSet reaches a caller.
    #[error("malformed answer: {0}")]
    Malformed(#[source] dns_message_parser::DecodeError),
    /// `DnsQuery_UTF8` failed with a status that is not "no records of
    /// this type" and not "no such name" — those two are empty results,
    /// not errors. The raw `WIN32_ERROR` is kept rather than mapped: a
    /// caller that wants to tell a timeout from a misconfigured resolver
    /// needs the code, and inventing a lossy category here would throw
    /// away the only thing that distinguishes them.
    #[error("the Windows DNS client returned status {code}")]
    WindowsDnsError { code: u32 },
    /// RFC 9460 §8: the record's `mandatory` list names a key the record
    /// does not actually carry. Checked here rather than by the decoder,
    /// because it is a statement about the record as a whole and not about
    /// any one parameter's encoding.
    #[error("SvcParamKey {key} is listed as mandatory but is not present in the record")]
    MandatoryKeyAbsent { key: u16 },
}
