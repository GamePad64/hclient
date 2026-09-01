//! Ask **the machine's own resolver** for a DNS record, for record types
//! `getaddrinfo` will not return.
//!
//! ```no_run
//! # fn main() -> Result<(), system_resolver::Error> {
//! // RR type 65, HTTPS (RFC 9460 §14.1).
//! for record in system_resolver::lookup("cloudflare.com", 65)? {
//!     println!("{} ttl {:?} rdata {} bytes", record.name, record.ttl, record.rdata.len());
//! }
//! # Ok(()) }
//! ```
//!
//! # What this is not
//!
//! It is not a resolver. It sends no queries of its own, keeps no cache,
//! implements no retry and validates no signatures. Everything it does,
//! the platform does — this reaches the platform in a way that is the same
//! shape on all five.
//!
//! **Why that is worth a crate.** `hickory-resolver` is an excellent
//! resolver and it is *its own*: it reads `/etc/resolv.conf` and speaks to
//! the servers named there. That is the wrong answer wherever the
//! interesting configuration is not in that file — a VPN's split DNS, a
//! corporate zone, Android's Private DNS, or Windows' per-interface
//! servers, none of which the file describes. `dns-lookup` reaches the
//! platform and offers what `getaddrinfo` does: A and AAAA. So the gap is
//! narrow and real: **the platform's answers, for types the platform's
//! convenience API will not return.**
//!
//! # The answer is records, not a message
//!
//! Four of the five platforms hand over a whole DNS message; Windows hands
//! over a list it has already taken apart. Making the message the common
//! type would mean synthesising one on Windows out of records that carry
//! no header — inventing an rcode and flags nobody reported. Records are
//! what all five genuinely have.
//!
//! What is lost is the header: `AA`, `TC` and the rcode as such. The loss
//! is stated rather than worked around — a caller that needs to tell
//! `NXDOMAIN` from an empty answer gets that from [`Error`], where
//! [`Error::NameDoesNotExist`] and `Ok(vec![])` are different values, and
//! a caller that needs `AA` is doing something this crate is not for.
//!
//! **On Apple that one distinction is not available either**, because the
//! daemon reports a missing name and a missing record type with the same
//! code. It is documented on the variant rather than hidden behind an
//! answer this crate would have had to invent.
//!
//! # [`Record::rdata`] is bytes and stays bytes
//!
//! Interpreting them belongs beside the consumer. A crate that also
//! decoded records would need a type per RFC and would make every consumer
//! wait for the one it needs.
//!
//! # Blocking
//!
//! Every platform call here blocks. A caller that needs otherwise runs
//! this where blocking is allowed; that is what a runtime's blocking pool
//! is for.
//!
//! # What each platform can be asked
//!
//! [`support()`] answers it, and the answer is not the same everywhere —
//! see [`Support`] for the Windows case, which is the only interesting
//! one and the reason this crate has that type at all.

#![doc(html_no_source)]

mod error;
/// Compiled and tested on every host, and reached by four of the five
/// backends: Windows hands over records it has already taken apart, so
/// nothing there walks a message.
///
/// The allowance is `sys`'s, for `sys`'s reason — a build compiles exactly
/// one backend, and narrowing this to the ones that walk a message would
/// mean restating their target list, which is the drift the single `#[cfg]`
/// set in `sys` exists to prevent. Every function here is exercised by this
/// module's own tests on every host, so what the allowance can hide is
/// bounded by that.
#[allow(dead_code, reason = "see this module's own note")]
mod message;
/// The mirror of [`message`]: it decodes a wire message into records, this
/// encodes a record a platform took apart back into RDATA. Compiled and
/// tested on every host for the same reason, and reached by **one**
/// backend — the Windows path that has no `DnsQueryRaw` — which is the
/// exact complement of the four that reach `message`.
#[allow(dead_code, reason = "see `message`'s note; this is its mirror")]
mod rdata;
mod sys;

pub use error::{Error, MalformedAnswer};

use std::time::Duration;

/// One resource record, as the wire carries it.
///
/// `#[non_exhaustive]` because this crate hands one back and a caller only
/// ever reads it — a field added later must not be a compile error at
/// every reader. That is the opposite answer from [`Support`] one type
/// down, and the difference is that a `Support` is *branched on*.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Record {
    /// The record's owner name, without a trailing dot. Empty is the root.
    ///
    /// This is the name the answer is *for*, which after a CNAME chain is
    /// not always the name that was asked about.
    pub name: String,
    /// The RR type, as a number. Not an enum: the registry gains entries,
    /// and a crate whose whole purpose is types it does not model would be
    /// a strange place to enumerate them.
    pub rtype: u16,
    /// The class, always `1` (`IN`). Present so a record reads as a
    /// record; this crate never asks for another class, and Windows does
    /// not report one, so on that platform the value is the class that was
    /// asked for rather than one that was read back.
    pub class: u16,
    /// The resolver's own remaining lifetime for this answer, already
    /// counted down by whatever cached it.
    pub ttl: Duration,
    /// The RDATA, exactly as it appeared. No interpretation.
    pub rdata: Vec<u8>,
}

impl Record {
    /// One record, for a caller building an answer rather than receiving
    /// one — a test, or a double standing in for a resolver.
    ///
    /// **A `#[non_exhaustive]` struct cannot be built with a literal from
    /// outside the crate that defines it**, so without this the type is a
    /// wall to exactly the code that needs it most: this workspace has
    /// already been caught once by a response type with no public
    /// constructor, where a consumer wrote around the gap before finding
    /// the test double that answered it. The constructor costs a line and
    /// keeps the attribute's benefit, which is that a field added later is
    /// not a compile error at every *reader*.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        rtype: u16,
        class: u16,
        ttl: Duration,
        rdata: Vec<u8>,
    ) -> Self {
        Self {
            name: name.into(),
            rtype,
            class,
            ttl,
            rdata,
        }
    }
}

/// `IN`, RFC 1035 §3.2.4 — the only class this crate asks for.
pub const CLASS_IN: u16 = 1;

/// What this build can be asked for.
///
/// Deliberately **exhaustive**, unlike [`Record`] above: this is branched
/// on, so a fourth answer must be a compile error at every reader rather
/// than something a `_` arm quietly mishandles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    /// Any type at all. The platform hands over the wire message and this
    /// crate walks it, so a type nobody has heard of is as answerable as
    /// `A`.
    Any,
    /// Any type **except** these, each of which the platform parses into a
    /// structure of its own before this crate can see it.
    ///
    /// **This is a Windows without `DnsQueryRaw`, and it is a list rather
    /// than a `bool` because the difference is enormous.**
    /// `DnsQuery_UTF8` fills in a `DNS_RECORD` whose data union has no
    /// discriminator; a type the union does not name arrives as the
    /// record's own RDATA, and one it names *may* arrive as that
    /// structure — `SVCB` is the measured counterexample.
    ///
    /// Most of the registry — `CAA`, `HTTPS`, `SVCB`, `SSHFP`,
    /// `OPENPGPKEY`, `CERT`, `LOC`, `URI` — is in the second group and
    /// works there exactly as anywhere else. Of the forty-two in the
    /// first, twenty-six are **re-encoded** from the structure back into
    /// RDATA, which is why `A`, `AAAA`, `MX`, `TXT`, `SRV`, `SOA`, `NS`,
    /// `CNAME`, `PTR`, `DS`, `DNSKEY` and `TLSA` are answerable too. What
    /// is left here is sixteen: the DNSSEC signature and denial records,
    /// protocol machinery like `OPT` and `TSIG`, and Windows' own `WINS`
    /// pair.
    ///
    /// A `bool` would have hidden all of that, and refusing the whole
    /// first group would have refused essentially every record in
    /// everyday use.
    AnyExcept(&'static [u16]),
    /// No backend on this target. [`lookup`] answers
    /// [`Error::Unsupported`] for every type, and says so here rather than
    /// looking like a network failure at the call site.
    None,
}

impl Support {
    /// Whether [`lookup`] can be asked for `rtype` on this build.
    ///
    /// Asking anyway is not undefined — it is [`Error::UnsupportedType`],
    /// naming the type. This exists so a caller can choose a different
    /// route before spending a query rather than after.
    #[must_use]
    pub fn allows(self, rtype: u16) -> bool {
        match self {
            Self::Any => true,
            Self::AnyExcept(parsed) => !parsed.contains(&rtype),
            Self::None => false,
        }
    }
}

/// What this build can be asked for; see [`Support`].
///
/// **A function rather than a constant, because on Windows the answer is
/// genuinely a run-time one.** `DnsQueryRaw` arrived in Windows 11 and
/// hands over the wire message, which makes that machine [`Support::Any`];
/// a Windows 10 running the same binary has only `DnsQuery_UTF8` and is
/// [`Support::AnyExcept`]. Everywhere else this is decided by the build and
/// the call is free.
///
/// The answer is resolved once and cached: it cannot change while the
/// process runs.
#[must_use]
pub fn support() -> Support {
    sys::support()
}

/// Asks the system resolver for `name`'s records of type `rtype`.
///
/// Blocking; see the crate root.
///
/// An empty `Vec` is *the name exists and has no records of this type*.
/// Every other outcome is an [`Error`], including the name not existing —
/// which is [`Error::NameDoesNotExist`] and not an empty answer, because
/// the two send a caller in different directions.
///
/// # Errors
///
/// [`Error::UnsupportedType`] where [`support()`] says so, without a query.
/// Otherwise whatever the platform reported, or [`Error::Malformed`] if
/// what came back is not a DNS answer this crate can walk.
pub fn lookup(name: &str, rtype: u16) -> Result<Vec<Record>, Error> {
    if !support().allows(rtype) {
        return Err(match support() {
            Support::None => Error::Unsupported,
            Support::Any | Support::AnyExcept(_) => Error::UnsupportedType { rtype },
        });
    }
    sys::query(name, rtype)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three answers are not interchangeable, and each has a caller
    /// that acts differently on it. Written as a table so a fourth variant
    /// arrives here as a compile error, which is what the enum being
    /// exhaustive is for.
    #[test]
    fn support_answers_for_a_type_rather_than_for_the_platform() {
        assert!(Support::Any.allows(65));
        assert!(Support::AnyExcept(&[1, 15]).allows(65));
        assert!(!Support::AnyExcept(&[1, 15]).allows(15));
        assert!(!Support::None.allows(65));
    }

    /// **Which backend this build selected, checked against a second copy
    /// of the target list.**
    ///
    /// The first copy is `sys`'s `cfg_if!` and decides what compiles; this
    /// one states, in a place a reviewer reads, what that is expected to
    /// mean. It matters more since that selection became ordered arms: an
    /// `if`/`else if` chain cannot compile two backends, but it can
    /// silently fall through to the wrong one, and a build that quietly
    /// answered `None` on Linux would pass every other test in this crate
    /// — they all skip on a platform with no backend.
    ///
    /// CI's three-OS matrix is what makes this a check rather than a
    /// comment.
    #[test]
    fn the_platform_this_was_built_for_selected_the_backend_it_should_have() {
        let wire = cfg!(any(
            target_os = "android",
            all(
                target_os = "linux",
                any(target_env = "gnu", target_env = "musl")
            ),
            target_vendor = "apple",
        ));
        let windows = cfg!(windows) && !wire;
        let got = support();
        // Written as one computed verdict rather than as an `assert!` per
        // arm: each of those compares a `cfg!` against a constant on the
        // target that made it true, which clippy correctly calls a
        // constant assertion. This one is over `support()`'s answer.
        let agrees = match got {
            // Windows is the only platform that can answer either, and
            // which of the two is a fact about the machine rather than the
            // build — so it is checked as *not `None`* and no further.
            Support::Any => wire || windows,
            Support::AnyExcept(_) => windows,
            Support::None => !wire && !windows,
        };
        assert!(
            agrees,
            "this target expects wire={wire} windows={windows} and got {got:?}"
        );
    }

    /// A refused type costs no query, and the refusal names the type
    /// rather than reporting a resolver failure — the *silently ignored
    /// setting* defect avoided one level down, where the setting is which
    /// record was asked for.
    #[test]
    fn a_type_this_build_cannot_ask_for_is_refused_by_name() {
        let Support::AnyExcept(parsed) = support() else {
            // On a platform that answers `Any` or `None` there is nothing
            // to refuse by type; the other two cases are asserted above.
            return;
        };
        let rtype = *parsed.first().expect("the list is not empty");
        assert!(matches!(
            lookup("example.com", rtype),
            Err(Error::UnsupportedType { rtype: got }) if got == rtype
        ));
    }
}
