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
//! | Linux, glibc | `res_query` | **every type** | the header — see the next section |
//! | Linux, musl | `res_query` | every type **up to 255** | the header, **and every type above 255** |
//! | Android >= 29 | `android_res_nquery` + `android_res_nresult` | **every type** | the header |
//! | FreeBSD | `res_query` | **every type** | the header — and see the note below |
//! | macOS, iOS | `DNSServiceQueryRecord` | **every type** | the header, **and** [`Error::NameDoesNotExist`] |
//! | Windows 11, Server 2025 | `DnsQueryRaw` | **every type** | the header |
//! | Windows 10 | `DnsQuery_UTF8` | every type **but sixteen** | the header, sibling records, and see [Windows 10](#windows-10) |
//! | anything else | nothing at all | **nothing at all** | every lookup is [`Error::Unsupported`] |
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
//! **The FreeBSD row has never been run, and it is the only one that says
//! so.** Its symbol is established the way Linux's was — `res_query` is
//! exported from `libc` under `FBSD_1.0`, so it links with no `link_name`
//! and no `-lresolv` — and that half fails loudly if it is wrong. What is
//! read rather than run is the property everything else here depends on:
//! `resolver(3)` calls the implementation thread-safe and `_res` per-thread,
//! and a manual page saying so is what Apple's arm also had before it was
//! measured and moved. `concurrent_lookups_all_answer_where_a_serial_burst_does`
//! is written for exactly that question; running the live suite on a
//! FreeBSD machine is what would settle it.
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
//! header. What it costs a caller is building one, and this workspace paid
//! it: `hclient-dns-system` used to wrap these records in a synthetic
//! message, ninety lines whose whole purpose was to be taken apart again
//! by the next call. It is on `domain` now, which parses a bare field, so
//! the envelope has no subject.
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
//! The one target where [`support()`] does not answer **every type**,
//! and the one where the answer is a **run-time** fact rather than a
//! build-time one. `DnsQueryRaw` arrived in Windows 11 and hands over the
//! wire message; Windows 10 has only `DnsQuery_UTF8`, which hands over
//! records the OS has already taken apart. The same binary is
//! **every type** on one and the excepted list on the other.
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
//! the excepted list, before a query is spent: the DNSSEC signature
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
/// **A range with holes punched in it, rather than an enum of cases**, and
/// that is a reading of what the platforms actually answer rather than a
/// preference. The four answers are three shapes of one thing: *every
/// type* is the full range with nothing excepted; *every type up to 255*
/// is a shorter range; *every type but these sixteen* is the full range
/// with a list; *nothing at all* is an empty range.
///
/// The enum this replaced had to grow a variant the day musl's ceiling was
/// measured. This cannot: a platform with a stranger answer is a different
/// pair of values rather than a different type.
///
/// **The fields are crate-private and [`allows`](Support::allows) is the
/// whole of the public surface**, which is what makes that safe to say — a
/// caller asks whether a type is answerable and cannot come to depend on
/// how the answer is stored. What the enum had and this does not is
/// exhaustiveness: a new variant used to be a compile error at every
/// reader, which is how `UpTo` was caught. What replaces it is that there
/// is now exactly one reader path, so there is nothing to be exhaustive
/// about.
///
/// `Clone` and not `Copy`, because [`RangeInclusive`] is not `Copy` — it
/// is also an iterator.
///
/// [`RangeInclusive`]: core::ops::RangeInclusive
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Support {
    /// The type numbers this build will carry, **inclusive at both ends**.
    /// An empty range is *no backend on this target at all*.
    pub(crate) range: core::ops::RangeInclusive<u16>,
    /// Types inside [`range`](Support::range) the platform still cannot
    /// answer with RDATA, each because it parses that type into a
    /// structure of its own before this crate can see it.
    ///
    /// `&'static [u16]` rather than an enum for the reason
    /// [`Record::rtype`] is a number: the registry gains entries. It is
    /// sixteen long at most today, and it is **derived** where it is
    /// produced rather than written down, so it cannot drift from the
    /// list it is the complement of.
    pub(crate) except: &'static [u16],
}

/// Each backend builds exactly one of these, and a build compiles exactly
/// one backend — so on any single target most of these constructors are
/// unused. Narrowing them by `#[cfg]` would restate `sys`'s target list a
/// second time, which is the drift that module's single `cfg_if!` exists
/// to prevent.
#[allow(dead_code, reason = "see this impl's own note")]
impl Support {
    /// Every type in the registry, including ones nobody has heard of.
    ///
    /// The platform hands over the wire message and this crate walks it,
    /// so a type it does not model is as answerable as `A`.
    pub(crate) fn any() -> Self {
        Self {
            range: 0..=u16::MAX,
            except: &[],
        }
    }

    /// Every type up to `highest`, inclusive, and none above it.
    ///
    /// **This is musl, and it is a fact about the call rather than about
    /// any one type.** Measured against glibc on one host, one name and
    /// one moment: `CAA` (257) answers 515 octets through glibc and `-1`
    /// through musl, while `A`, `AAAA`, `HTTPS` and `ANY` (255) answer
    /// through both. The types it costs are ones callers ask for — `CAA`
    /// at 257, `URI` at 256 — so this is not a corner of the registry
    /// nobody visits.
    pub(crate) fn up_to(highest: u16) -> Self {
        Self {
            range: 0..=highest,
            except: &[],
        }
    }

    /// Every type but these.
    ///
    /// **This is a Windows without `DnsQueryRaw`**, and today it is the
    /// only producer. Which sixteen types, and why those, is the crate
    /// root's [Windows 10](crate#windows-10) section — said once there
    /// rather than twice. What belongs here is why it is a list and not a
    /// `bool`: the refused set is sixteen against a registry that mostly
    /// arrives untouched, so a `bool` would have reported a platform that
    /// answers `A`, `HTTPS`, `MX`, `TXT` and `CAA` as one that answers
    /// nothing.
    pub(crate) fn any_except(except: &'static [u16]) -> Self {
        Self {
            range: 0..=u16::MAX,
            except,
        }
    }

    /// No backend on this target: [`lookup`] answers [`Error::Unsupported`]
    /// for every type.
    ///
    /// The empty range is written once, here, rather than at each site
    /// that means it — `1..=0` reads as a mistake everywhere except beside
    /// this sentence.
    #[allow(
        clippy::reversed_empty_ranges,
        reason = "an empty range is the point: `1..=0` is how *no type at all* is spelled, and the constructor exists so that it is written once, here, beside the sentence saying so"
    )]
    pub(crate) fn none() -> Self {
        Self {
            range: 1..=0,
            except: &[],
        }
    }

    /// Whether [`lookup`] can be asked for `rtype` on this build.
    ///
    /// Asking anyway is not undefined — it is [`Error::UnsupportedType`],
    /// naming the type. This exists so a caller can choose a different
    /// route before spending a query rather than after.
    #[must_use]
    pub fn allows(&self, rtype: u16) -> bool {
        self.range.contains(&rtype) && !self.except.contains(&rtype)
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
    let caps = support();
    if !caps.allows(rtype) {
        // **An empty range is the whole build refusing, anything else is
        // this one type**, and the two are different errors because a
        // caller does different things with them: one reaches for another
        // resolver, the other for another record. Reading it off the range
        // rather than off a variant is what the struct buys — there is one
        // question and one place that answers it.
        return Err(if caps.range.is_empty() {
            Error::Unsupported
        } else {
            Error::UnsupportedType { rtype }
        });
    }
    sys::query(name, rtype)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each shape the platforms actually produce, and the boundary of
    /// every one of them.
    ///
    /// **The boundaries are what this is for.** A range is inclusive at
    /// both ends and an off-by-one there would be a type answerable on one
    /// build and refused on the next, which is invisible from any single
    /// platform — so 255 and 256 are both asserted, and so is the type
    /// exactly on the excepted list beside the one just past it.
    #[test]
    fn support_answers_for_a_type_rather_than_for_the_platform() {
        assert!(Support::any().allows(65));
        assert!(Support::any().allows(u16::MAX));

        assert!(Support::any_except(&[1, 15]).allows(65));
        assert!(!Support::any_except(&[1, 15]).allows(15));

        assert!(Support::up_to(255).allows(255), "inclusive at the top");
        assert!(!Support::up_to(255).allows(256));
        assert!(Support::up_to(255).allows(0), "and at the bottom");

        assert!(!Support::none().allows(65));
        assert!(!Support::none().allows(0), "an empty range holds nothing");
    }

    /// **The two halves compose**, which the enum could not express: a
    /// build could be bounded *and* have a parsed-type list, and nothing
    /// in the type says otherwise. No platform answers this today, and
    /// asserting it is what keeps the fields independent rather than a
    /// tagged union wearing a struct's clothes.
    #[test]
    fn a_ceiling_and_an_exception_list_are_independent() {
        let both = Support {
            range: 0..=255,
            except: &[15],
        };
        assert!(both.allows(1));
        assert!(!both.allows(15), "excepted below the ceiling");
        assert!(!both.allows(257), "above the ceiling and not excepted");
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
            all(target_os = "linux", target_env = "gnu"),
            target_os = "freebsd",
            target_vendor = "apple",
        ));
        // musl hands over a message like the others and still cannot be
        // asked for every type, so it is its own row rather than part of
        // `wire`: what separates the arms here is the *answer*, not the
        // shape of what the platform returns.
        let capped = cfg!(all(target_os = "linux", target_env = "musl"));
        let windows = cfg!(windows) && !wire && !capped;
        let got = support();
        // Written as one computed verdict rather than as an `assert!` per
        // arm: each of those compares a `cfg!` against a constant on the
        // target that made it true, which clippy correctly calls a
        // constant assertion. This one is over `support()`'s answer.
        //
        // The three shapes are read off the fields rather than off a
        // variant, which is the same reading `lookup` makes: an empty
        // range is *no backend*, a non-empty `except` is Windows 10's
        // parsed list, and a range short of `u16::MAX` is musl's ceiling.
        // Windows is the only platform that can answer either of two, and
        // which is a fact about the machine rather than the build, so it
        // is checked as *not empty* and no further.
        let nothing = got.range.is_empty();
        let excepts = !got.except.is_empty();
        let bounded = !nothing && *got.range.end() < u16::MAX;
        let agrees = if nothing {
            !wire && !windows && !capped
        } else if excepts {
            windows
        } else if bounded {
            capped
        } else {
            wire || windows
        };
        assert!(
            agrees,
            "this target expects wire={wire} capped={capped} windows={windows} and got {got:?}"
        );
    }

    /// A refused type costs no query, and the refusal names the type
    /// rather than reporting a resolver failure — the *silently ignored
    /// setting* defect avoided one level down, where the setting is which
    /// record was asked for.
    #[test]
    fn a_type_this_build_cannot_ask_for_is_refused_by_name() {
        let caps = support();
        let Some(&rtype) = caps.except.first() else {
            // On a build whose `except` is empty there is no type to
            // refuse *by name* — a ceiling refuses by number and an empty
            // range refuses everything, and both are asserted above.
            return;
        };
        assert!(matches!(
            lookup("example.com", rtype),
            Err(Error::UnsupportedType { rtype: got }) if got == rtype
        ));
    }
}
