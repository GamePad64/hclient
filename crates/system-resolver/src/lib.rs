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
//! # Platforms, backends and what each costs
//!
//! | target | what is called | [`support()`] | what is not available there |
//! |---|---|---|---|
//! | Linux (glibc, musl) | `res_query` | [`Support::Any`] | the header — see the next section |
//! | Android >= 29 | `android_res_nquery` + `android_res_nresult` | [`Support::Any`] | the header |
//! | macOS, iOS | `DNSServiceQueryRecord` | [`Support::Any`] | the header, **and** [`Error::NameDoesNotExist`] |
//! | Windows 11, Server 2025 | `DnsQueryRaw` | [`Support::Any`] | the header |
//! | Windows 10 | `DnsQuery_UTF8` | [`Support::AnyExcept`], 16 types | the header, sibling records, and see [Windows 10](#windows-10) |
//! | anything else | nothing at all | [`Support::None`] | every lookup is [`Error::Unsupported`] |
//!
//! **The last two Windows rows are one binary**, which is why
//! [`support()`] is a function and not a constant: `DnsQueryRaw` is
//! resolved at run time, so a build does not know which of the two it is
//! until it runs. Every other row is decided by the target.
//!
//! **No row costs a dependency for the call itself.** The Unix symbols are
//! declared where they are used; Windows uses `windows-sys` and
//! `windows-strings`, which no other target resolves.
//!
//! # The answer is records, not a message
//!
//! Three of the six rows above hand over a whole DNS message. Windows 10
//! hands over a list it has already taken apart, and so does Apple's
//! daemon, one record per callback. Making the message the common type
//! would mean synthesising one on those two out of records that carry no
//! header — inventing an rcode and flags nobody reported. Records are what
//! every row genuinely has.
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
//! **Every name inside `rdata` is written out in full, on every target**,
//! and that is what makes a bare RDATA field decodable at all: a
//! compression pointer (RFC 1035 §4.1.4) points at an offset in the
//! message it arrived in, and a record that has left its message has
//! nothing to point into. So the pointers are resolved before a record is
//! handed over. A caller who compares these octets against a packet
//! capture will see that difference and no other.
//!
//! ## What to decode them with
//!
//! What is wanted is a crate that parses RDATA **given its type**, which
//! is a narrower thing than parsing a message — and the difference is the
//! whole of the choice. Read off each crate's published documentation
//! rather than recalled:
//!
//! | crate | entry point | what it takes |
//! |---|---|---|
//! | [`hickory-proto`] 0.26 | `RData::read(&mut BinDecoder, RecordType, Restrict<u16>)` | a bare RDATA field — the direct fit |
//! | [`domain`] 0.12 | `AllRecordData::parse_rdata(Rtype, &mut Parser)` | a bare RDATA field — the direct fit |
//! | [`dns-message-parser`] 0.9 | `RR::decode(Bytes)` | a **whole record**: name, type, class, TTL and length first |
//!
//! The third row is the shape to watch for, and it is not a defect in that
//! crate — a decoder written for messages reasonably expects a record's
//! header. What it costs a caller is building one, and `hclient-dns-system`
//! is a worked example: it wraps these records in a synthetic message and
//! hands that to `Dns::decode`, because it wanted that crate for other
//! reasons.
//!
//! Any other crate is judged by the same question, which is worth asking
//! before the download: *can it be handed a type and some octets?* A
//! decoder that can only start at a message header is usable and is one
//! envelope more expensive.
//!
//! [`hickory-proto`]: https://docs.rs/hickory-proto
//! [`domain`]: https://docs.rs/domain
//! [`dns-message-parser`]: https://docs.rs/dns-message-parser
//!
//! # Windows 10
//!
//! The one target where [`support()`] does not answer [`Support::Any`],
//! and the one where the answer is a **run-time** fact rather than a
//! build-time one. `DnsQueryRaw` arrived in Windows 11 and hands over the
//! wire message; Windows 10 has only `DnsQuery_UTF8`, which hands over
//! records the OS has already taken apart. The same binary is
//! [`Support::Any`] on one and [`Support::AnyExcept`] on the other.
//!
//! **The newer call is resolved with `GetProcAddress` rather than named as
//! an import**, and that is not caution about a missing function: naming
//! one that a machine does not export stops the **process** from starting
//! at all, so a binary importing `DnsQueryRaw` would not run on Windows 10
//! — including the half of it that has nothing to do with DNS.
//!
//! ## What is refused, and why it is a list
//!
//! `DnsQuery_UTF8` fills in a `DNS_RECORD` whose data union carries **no
//! discriminator**. A type the union does not name arrives as the record's
//! own RDATA and needs nothing; a type it names *may* arrive as that
//! structure instead — `SVCB` is the measured counterexample, and reading
//! one as the other is a wrong pointer rather than a wrong answer.
//!
//! So the union's members are answered by reading the structure and
//! writing RDATA back out: twenty-six of them, which is why `A`, `AAAA`,
//! `MX`, `TXT`, `SRV`, `SOA`, `NS`, `CNAME`, `PTR`, `DS`, `DNSKEY` and
//! `TLSA` work here as anywhere. **Sixteen are refused by name** through
//! [`Support::AnyExcept`], before a query is spent: the DNSSEC signature
//! and denial records, whose canonical forms this crate would have to
//! reproduce exactly; protocol machinery — `OPT`, `TKEY`, `TSIG`; and
//! `WKS`, `ATMA`, `NULL`, `DHCID`, `WINS`, `WINSR`. Most of the registry
//! is in neither group and simply arrives as RDATA, `CAA`, `HTTPS`,
//! `SVCB`, `SSHFP`, `OPENPGPKEY`, `CERT`, `LOC` and `URI` among them.
//!
//! The list is **derived** from the union's membership minus the types
//! with a re-encoder, rather than written out a third time.
//!
//! ## What the answers themselves cost
//!
//! For a re-encoded type the octets are *what Windows understood*, not
//! what arrived, and three differences follow. Case is Windows' own — DNS
//! names are case-insensitive and nothing restores what the origin sent.
//! A character-string cannot carry a NUL, because `TXT`, `HINFO`, `X25`,
//! `ISDN` and `NAPTR` strings arrive as C strings — an octet the RFCs
//! permit, truncated by **Windows** before this crate sees it. Names
//! written out in full is the third, and it is not a Windows 10 property:
//! every target does it, for the reason above.
//!
//! Two things are missing rather than altered. **Records of another type
//! beside the answer** — a CNAME chain is visible wherever a message is —
//! because a `DNS_RECORD` of another type is that type's structure and
//! this path would have to know its shape too; ask for `CNAME` directly.
//! And **`TC`**: a truncated answer is refused where the header is
//! readable and is invisible here, so whatever records the OS obtained
//! come back with nothing saying the set is short. [`Error::NameDoesNotExist`]
//! survives, because the API reports it as a status of its own.
//!
//! ## It is best-effort, and that is a statement about evidence
//!
//! There is no Windows 10 machine behind this crate and none in its CI.
//! What stands in for one is that `DnsQuery_UTF8` is present on Windows 11
//! as well and *was* executed there, against `DnsQueryRaw` on the same
//! machine and the same name, with the two answers compared octet for
//! octet. That is the strongest evidence available without the hardware
//! and it is not the same as having run it.
//!
//! # Blocking
//!
//! Every platform call here blocks. A caller that needs otherwise runs
//! this where blocking is allowed; that is what a runtime's blocking pool
//! is for.
//!

#![doc(html_no_source)]

mod error;
/// Compiled and tested on every host, and reached by **three** of the five
/// backends — `res_query`, Android's pair and `DnsQueryRaw`. The other two
/// hand over records that were taken apart before this crate saw them, so
/// nothing there walks a message: Windows 10's `DnsQuery_UTF8`, and
/// Apple's daemon, which calls back once per record.
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
/// backend — the Windows path that has no `DnsQueryRaw`.
///
/// It is not the complement of the three that reach [`message`], and the
/// gap is Apple: that daemon hands over RDATA already, so it needs neither
/// module. Reading the two counts as a partition is the mistake this
/// sentence exists to stop, and it was one this crate's own docs made for
/// as long as Apple's arm was `res_query`.
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
    /// **This is a Windows without `DnsQueryRaw`**, and today it is the
    /// only producer. Which sixteen types, and why those, is the crate
    /// root's [Windows 10](crate#windows-10) section — said once there
    /// rather than twice.
    ///
    /// **What belongs here is why it is a list and not a `bool`**, because
    /// that is a fact about this type: the refused set is sixteen against
    /// a registry that mostly arrives untouched, so a `bool` would have
    /// reported a platform that answers `A`, `HTTPS`, `MX`, `TXT` and
    /// `CAA` as one that answers nothing. It is `&'static [u16]` rather
    /// than an enum for the reason [`Record::rtype`] is a number: the
    /// registry gains entries.
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
/// genuinely a run-time one** — one binary, two answers, depending on
/// whether the machine it is running on exports `DnsQueryRaw`. The crate
/// root's [Windows 10](crate#windows-10) section is why. Everywhere else
/// this is decided by the build and the call is free.
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
