//! Ask the operating system's own DNS resolver for any record type.
//!
//! `getaddrinfo` only returns A and AAAA. This returns whatever you ask
//! for: HTTPS/SVCB, CAA, TLSA, anything. It calls the platform's own
//! resolver API, so the answer comes from the same place and under the
//! same configuration as every other lookup on the machine.
//!
//! ```no_run
//! # fn main() -> Result<(), system_resolver::Error> {
//! // RR type 65 is HTTPS (RFC 9460 §14.1).
//! for record in system_resolver::lookup("cloudflare.com", 65)? {
//!     println!("{} ttl {:?} rdata {} bytes", record.name, record.ttl, record.rdata.len());
//! }
//! # Ok(()) }
//! ```
//!
//! # Why not a resolver crate
//!
//! `hickory-resolver` and the c-ares bindings are resolvers: they read
//! `/etc/resolv.conf` and send their own queries to the servers listed
//! there. That misses anything configured elsewhere, such as a VPN's split
//! DNS, per-interface servers on Windows, macOS supplemental resolvers or
//! Android's Private DNS, and it does not use the system cache.
//!
//! This crate sends nothing. No socket, no cache, no retries, no config
//! parsing. Whatever your machine already does still happens, because the
//! machine is what answers.
//!
//! # Platforms
//!
//! | target | API used | can be asked for |
//! |---|---|---|
//! | Linux, glibc | `res_query` | every type |
//! | Linux, musl | `res_query` | every type up to 255 |
//! | Android 29+ | `android_res_nquery` | every type |
//! | FreeBSD | `res_query` | every type |
//! | macOS, iOS | `DNSServiceQueryRecord` | every type |
//! | Windows 11 | `DnsQueryRaw` | every type |
//! | Windows 10 | `DnsQuery_UTF8` | every type but sixteen |
//!
//! Anything else compiles and answers [`Error::Unsupported`].
//!
//! The two Windows rows are one binary. `DnsQueryRaw` is resolved at run
//! time, so a build does not know which of the two it is until it runs,
//! which is why [`support()`] is a function and not a constant.
//!
//! # Limits
//!
//! Not every platform can be asked for everything. [`support()`] and its
//! `allows(rtype)` answer before you spend a query, and [`lookup`] returns
//! [`Error::UnsupportedType`] naming the type rather than guessing.
//!
//! - musl cannot pass a type number above 255, so `CAA` (257) and `URI`
//!   (256) are unavailable there.
//! - Windows 10 refuses sixteen types; see below.
//! - Apple reports "no such name" and "no such record" with one code, so
//!   [`Error::NameDoesNotExist`] is unreachable there and an absent name
//!   comes back as an empty answer.
//! - There is no message header, so no `AD` bit and no `TC`. You get
//!   records. Where a caller needs to tell `NXDOMAIN` from *no records of
//!   this type*, [`Error::NameDoesNotExist`] and `Ok(vec![])` are
//!   different values on every platform but Apple.
//! - Every call blocks. Run it from a blocking thread pool.
//! - FreeBSD is compiled and type-checked on every push, but has never
//!   been run on FreeBSD.
//!
//! ## Windows 10
//!
//! `DnsQuery_UTF8` parses 43 record types into structures of its own
//! before this crate can see them. 26 are converted back into RDATA, so
//! `A`, `AAAA`, `MX`, `TXT`, `SRV`, `SOA`, `NS`, `CNAME`, `PTR`, `DS`,
//! `DNSKEY` and `TLSA` work as anywhere else. Sixteen are refused by name:
//! the DNSSEC signature and denial records, `OPT`, `TKEY`, `TSIG`, and
//! `WKS`, `ATMA`, `NULL`, `DHCID`, `WINS`, `WINSR`. Most of the registry
//! is in neither group and arrives as RDATA, `CAA`, `HTTPS`, `SVCB`,
//! `SSHFP`, `OPENPGPKEY`, `CERT`, `LOC` and `URI` among them.
//!
//! Two things are missing rather than altered on that path: records of
//! another type beside the answer, so a CNAME chain is not visible, and
//! the `TC` bit, so a truncated answer cannot be detected.
//!
//! Windows 11 has `DnsQueryRaw`, which hands over the wire message and
//! has none of this. It is resolved with `GetProcAddress` rather than
//! named as an import, because naming a function a machine does not export
//! stops the process from starting at all.
//!
//! # Record data
//!
//! [`Record::rdata`] is the raw RDATA bytes. This crate does not decode
//! them, because that would mean a type per RFC and most callers want one
//! of them.
//!
//! Use a decoder that takes a record type and a byte slice —
//! `hickory-proto`'s `RData::read` or `domain`'s
//! `AllRecordData::parse_rdata` both do. A decoder written for whole
//! messages, such as `dns-message-parser`'s `RR::decode`, wants a record
//! header first, so you would have to build one.
//!
//! Names inside RDATA are expanded before you get them. A compression
//! pointer (RFC 1035 §4.1.4) refers to an offset in the message it arrived
//! in, and a record that has left its message has nothing to point into,
//! so a bare field would otherwise be undecodable out of context.
//!
//! # Notes
//!
//! `docs/system-resolver-design.md` in the repository records what was
//! measured on each platform and why each backend is the one it is,
//! including the two defects that changed the Windows and Apple paths.

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
