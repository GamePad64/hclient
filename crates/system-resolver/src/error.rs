//! Every way a lookup can fail to produce records.
//!
//! One file, by this workspace's convention: a reader asking *what can
//! this crate refuse* reads a file rather than the crate.
//!
//! The ordering below is how far a query got — what is refused before a
//! packet, what the resolver reported, and what came back but could not be
//! read — because that is the order somebody debugging a failure arrives
//! in. Alphabetical order is the order of a type nobody is looking for.

/// Why a [`lookup`](crate::lookup) produced no records.
///
/// **Not `#[non_exhaustive]` here and `#[non_exhaustive]` on
/// [`MalformedAnswer`] below**, and the split is this workspace's rule
/// about who stands on the other side: this enum is what a caller
/// *branches on* — an absent name and a truncated answer send a client in
/// different directions — so a new variant should be a compile error where
/// that decision is made. `MalformedAnswer` is handed back and read, never
/// dispatched on, and a `_` arm there says *the answer was malformed some
/// other way*, which is true.
///
/// `PartialEq` is derived because a consumer's tests compare one of these
/// against an expected value, and every field here is a plain number or a
/// name. `Clone` is deliberately absent: nothing needs a second copy of a
/// failure, and adding it would be a promise about a type that may yet
/// carry a platform handle.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// The name has no wire form: over 255 octets, an empty label, a label
    /// over 63 octets, or an interior NUL.
    ///
    /// Refused before a query rather than passed to the platform, so the
    /// answer is the same on every platform instead of being whatever the
    /// local resolver happens to do with it.
    #[error("`{name}` cannot be used as a DNS query name")]
    NameNotUsable {
        /// The name as it was given.
        name: String,
    },

    /// This build has no backend at all — see **nothing at all**.
    ///
    /// **nothing at all**: crate::Support::None
    #[error("no system resolver backend on this target")]
    Unsupported,

    /// This build cannot be asked for this type — see
    /// the excepted list.
    ///
    /// Costs no query. The platform would have answered *something*; what
    /// it would not have answered is RDATA, and handing a caller a
    /// platform structure's bytes as though they were RDATA is the failure
    /// this refusal exists to prevent.
    ///
    /// the excepted list: crate::Support::AnyExcept
    #[error("this build cannot return RR type {rtype}: the platform parses it into a structure")]
    UnsupportedType {
        /// The type that was asked for.
        rtype: u16,
    },

    /// `NXDOMAIN` — an authority said the name does not exist.
    ///
    /// Deliberately **not** an empty answer: *no records of this type* and
    /// *no such name* send a caller in different directions, and a client
    /// that collapses them will retry a name that will never exist.
    ///
    /// **Not reachable on Apple platforms**, and that is the daemon's
    /// limit rather than a gap here: `DNSServiceQueryRecord` reports both
    /// cases as `kDNSServiceErr_NoSuchRecord` and carries no header to
    /// read an rcode out of, so this crate answers the one it can stand
    /// behind — no records. Measured on macOS 27.
    #[error("the name does not exist")]
    NameDoesNotExist,

    /// Nothing came back: no configured resolver answered, or the call
    /// failed before one could.
    #[error("no configured resolver answered")]
    NoResponse,

    /// The answer did not fit and the resolver's own retry did not replace
    /// it. A truncated answer is not a complete RRSet, so it is refused
    /// rather than returned short.
    #[error("answer was truncated and no complete one was obtained")]
    Truncated,

    /// The resolver answered, with an RCODE that is neither `NOERROR` nor
    /// `NXDOMAIN`.
    #[error("resolver answered with RCODE {rcode}")]
    ResponseCode {
        /// RFC 1035 §4.1.1 / RFC 6895 §2.3.
        rcode: u8,
    },

    /// More than the 65535 octets a DNS message can be framed in.
    #[error("answer exceeds the largest possible DNS message")]
    AnswerTooLarge,

    /// The platform reported failure over a `NOERROR` header that claims
    /// answers, and its failure path returns no length to bound them with.
    ///
    /// Unreachable while `res_query` and `android_res_nresult` behave as
    /// measured — they report failure on a `NOERROR` response only when
    /// the answer section is empty. It is an error rather than a silent
    /// zero because the alternative, on a libc that broke that contract,
    /// is a client that quietly stops discovering anything.
    #[error("resolver reported failure over a header claiming {ancount} answers, with no length")]
    LengthUnavailable {
        /// What the header claimed.
        ancount: u16,
    },

    /// The platform's own status code, for a failure it reported and this
    /// crate has no better word for.
    ///
    /// Kept as a number rather than translated: the vocabulary is the
    /// platform's, it differs between them, and inventing a shared
    /// taxonomy over three of them would lose exactly the detail somebody
    /// reaches for this field to get.
    #[error("the system resolver reported status {code}")]
    Platform {
        /// A `WIN32_ERROR` on Windows; a `DNS_ERROR_*`/`errno` value
        /// elsewhere.
        code: u32,
    },

    /// What came back is not a DNS answer this crate can walk.
    #[error("malformed answer: {0}")]
    Malformed(#[source] MalformedAnswer),
}

/// How a DNS answer failed to walk.
///
/// See [`Error`]'s own note for why this one is `#[non_exhaustive]` and
/// that one is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum MalformedAnswer {
    /// Fewer than the twelve octets RFC 1035 §4.1.1 fixes the header at.
    #[error("answer is {got} bytes, shorter than the 12-byte DNS header")]
    HeaderTruncated {
        /// What arrived.
        got: usize,
    },
    /// A section ended in the middle of a name, a record header, or a
    /// record's data.
    #[error("answer ends inside a record")]
    RecordTruncated,
    /// A compression pointer chain that does not end. Bounded by a jump
    /// budget rather than by a rule about which direction a pointer may
    /// take, because the second is a convention and the first is a proof.
    #[error("compression pointer chain does not terminate")]
    PointerLoop,
    /// A name over RFC 1035 §2.3.4's 255 octets, or a label over 63.
    ///
    /// Reachable through compression from a message whose own names are
    /// each legal, which is why it is checked while a name is assembled
    /// rather than only when one is read.
    #[error("a name in the answer exceeds its wire limit")]
    NameTooLong,
    /// A label that is neither a length nor a pointer — RFC 1035 §4.1.4
    /// reserves the `0b01` and `0b10` forms and nothing defines them.
    #[error("a name in the answer uses a reserved label form")]
    ReservedLabel,
}
