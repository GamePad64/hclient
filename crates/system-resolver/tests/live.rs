//! The only tests here that need a name server, and therefore the only
//! ones that are `#[ignore]`d.
//!
//! Everything else in this crate is a rule over bytes and runs anywhere.
//! What these add is the half no byte vector can reach: that the seam is
//! still wired to a real resolver, and that the answer that comes back is
//! shaped the way every unit test assumes. A `lookup` that returned an
//! empty `Vec` for everything would leave the whole rest of the suite
//! green.
//!
//! ```text
//! cargo nextest run -p system-resolver --run-ignored all
//! ```

use system_resolver::{Error, Support, lookup, support};

/// RFC 9460 §14.1. Chosen because it is the type this workspace actually
/// depends on, and because it is answerable on every platform here — on
/// Windows it is not one of the types the OS parses into a struct.
const TYPE_HTTPS: u16 = 65;
/// RFC 8659 §4.1. The control for the Windows rule from the other side: no
/// `DNS_TYPE_CAA` exists in the Win32 metadata at all — and, at 257, the
/// type that put musl's ceiling on the map.
const TYPE_CAA: u16 = 257;
/// RFC 1035 §3.3.2. Stands in for `CAA` where a build's ceiling refuses
/// 257: a type nothing publishes, which is what a *this name has none of
/// these* control needs.
const TYPE_HINFO: u16 = 13;

#[test]
#[ignore = "needs a name server"]
fn an_https_record_comes_back_as_svcb_wire_format() {
    let found = lookup("cloudflare.com", TYPE_HTTPS).expect("the lookup succeeds");
    assert!(
        !found.is_empty(),
        "cloudflare.com publishes an HTTPS record"
    );
    for record in &found {
        assert_eq!(record.rtype, TYPE_HTTPS);
        assert_eq!(record.name, "cloudflare.com");
        // RFC 9460 §2.2: a two-octet SvcPriority, then a TargetName. The
        // assertion is deliberately weak on the content and strong on the
        // *shape*: what would fail here is a platform handing back a
        // parsed structure, which is the defect this crate's Windows
        // support matrix exists to prevent, and a struct's first bytes are
        // a pointer rather than a small priority.
        assert!(record.rdata.len() >= 3, "priority and a target name");
        let priority = u16::from_be_bytes([record.rdata[0], record.rdata[1]]);
        assert!(priority < 64, "a published SvcPriority is a small number");
    }
}

/// A type the platform has no special knowledge of, which is the case this
/// crate exists for. On Windows this and the one above take the same path
/// for the same reason; on the others there is no distinction to make.
#[test]
#[ignore = "needs a name server"]
fn a_caa_record_comes_back_as_its_own_rdata() {
    // **musl cannot ask for it at all**, and the refusal is what this test
    // asserts there rather than being skipped: `Support::UpTo(255)` puts
    // 257 on the wrong side of the line, and the whole point of answering
    // a bound instead of `Any` is that a caller is told before a query is
    // spent. A skip would leave that unchecked on the one platform where
    // it happens.
    if !support().allows(TYPE_CAA) {
        assert!(matches!(
            lookup("cloudflare.com", TYPE_CAA),
            Err(Error::UnsupportedType { rtype: TYPE_CAA })
        ));
        return;
    }

    let found = lookup("cloudflare.com", TYPE_CAA).expect("the lookup succeeds");
    assert!(!found.is_empty(), "cloudflare.com publishes CAA records");
    for record in &found {
        // RFC 8659 §4.1: one flags octet, then a length-prefixed tag.
        let tag_len = usize::from(record.rdata[1]);
        let tag = &record.rdata[2..2 + tag_len];
        assert!(
            matches!(tag, b"issue" | b"issuewild" | b"iodef"),
            "an unrecognised CAA tag: {:?}",
            std::str::from_utf8(tag)
        );
    }
}

/// **The distinction that is not an empty answer.** A name that exists
/// with no records of this type and a name that does not exist are
/// different values, and a caller retries only one of them.
#[test]
#[ignore = "needs a name server"]
fn a_name_that_does_not_exist_is_not_an_empty_answer() {
    // RFC 2606 §2 reserves `.invalid` and guarantees it is never
    // delegated, so this is NXDOMAIN by specification rather than by
    // whoever happens to own a zone today. The first version of this test
    // used a made-up label under `cloudflare.com` and failed: that zone
    // answers a wildcard, so the name **exists** and has no HTTPS record —
    // which is `Ok([])`, and correctly so. The test was wrong, not the
    // crate, and the distinction it was written to check is exactly the
    // one it tripped over.
    let absent = lookup("definitely-not-here.invalid", TYPE_HTTPS);
    if cfg!(target_vendor = "apple") {
        // **Apple cannot draw this distinction**, and the test says so
        // rather than accepting either answer: `DNSServiceQueryRecord`
        // reports a missing name and a missing record type with one code
        // and carries no header to read an rcode out of. Written as a
        // branch so that a platform which *can* distinguish and quietly
        // stopped still fails here.
        assert!(
            matches!(absent, Ok(ref none) if none.is_empty()),
            "expected an empty answer on Apple, got {absent:?}"
        );
    } else {
        assert!(
            matches!(absent, Err(Error::NameDoesNotExist)),
            "expected NXDOMAIN, got {absent:?}"
        );
    }

    // The control, and the wildcard case above is what makes it worth
    // having: without it, a `lookup` that answered `NameDoesNotExist` for
    // everything would pass the assertion above.
    //
    // The type has to be one this build can ask for *and* one this name
    // does not have, so it is `CAA` where that is answerable and `HINFO`
    // where it is not — musl refuses 257 before a query, which would make
    // the control assert the wrong thing. RFC 1035 §3.3.2's `HINFO` is a
    // type essentially nothing publishes, which is exactly what a control
    // for *the name exists and has none of these* needs.
    let absent_type = if support().allows(TYPE_CAA) {
        TYPE_CAA
    } else {
        TYPE_HINFO
    };
    let present = lookup("www.cloudflare.com", absent_type).expect("the name exists");
    assert!(present.is_empty(), "no records of type {absent_type} at this name");
}

/// RFC 1035 §3.2.2. The type that separates the two Windows calls: it is
/// one of the forty-three the OS parses, so it is answerable through
/// `DnsQueryRaw` and refused through `DnsQuery_UTF8`. Everywhere else it is
/// simply an ordinary type.
const TYPE_A: u16 = 1;

/// **What [`support`] promises has to be what `lookup` does**, or the
/// capability lies — and this is also how the suite says which of the two
/// Windows paths is live, which nothing else here can.
///
/// **It is written as a biconditional rather than as a match on the
/// answer**, and that is what the struct made possible: [`Support`]'s
/// fields are crate-private, so a caller has exactly one question to ask —
/// *may I ask for this type* — and this asserts that `lookup` agrees with
/// it, for every type, in both directions. The old shape was one arm per
/// variant, which had to be edited on the day musl's ceiling arrived and
/// would have to be again.
///
/// The types are chosen so that no platform answers the same for all
/// three: `A` is the one Windows 10 parses and refuses, `CAA` is above
/// musl's ceiling, and `HTTPS` is answerable everywhere there is a backend
/// at all.
#[test]
#[ignore = "needs a name server"]
fn support_and_lookup_agree_about_every_type_that_separates_a_platform() {
    let caps = support();
    let mut answerable = 0;

    for rtype in [TYPE_A, TYPE_CAA, TYPE_HTTPS] {
        let got = lookup("cloudflare.com", rtype);
        if caps.allows(rtype) {
            answerable += 1;
            // **`Ok` and not merely *not refused by name*.** The weaker
            // assertion was written first and a mutation walked past it:
            // dropping musl's ceiling makes `allows` say yes while the
            // call answers `-1`, which is `NoResponse` — neither
            // `UnsupportedType` nor `Unsupported`, so the weaker form
            // passed. A permitted type has to actually answer, and *no
            // records of this type* is `Ok(vec![])` on every platform
            // here, so this excludes nothing a resolver could legitimately
            // say.
            assert!(
                got.is_ok(),
                "{rtype} is allowed and did not answer: {got:?}"
            );
        } else {
            assert!(
                matches!(
                    got,
                    Err(Error::UnsupportedType { rtype: named }) if named == rtype
                ) || matches!(got, Err(Error::Unsupported)),
                "{rtype} is not allowed and was not refused by name: {got:?}"
            );
        }
    }

    // The control, and it is what keeps the loop above from passing on a
    // build that allows nothing: a backend that exists answers at least
    // `HTTPS`, which is the type this whole crate was written for.
    if caps.allows(TYPE_HTTPS) {
        assert!(answerable >= 1, "a build with a backend answers something");
        let found = lookup("cloudflare.com", TYPE_HTTPS).expect("HTTPS is answerable");
        assert!(!found.is_empty(), "cloudflare.com publishes HTTPS records");
    }
}

/// How many threads and how many queries each: the shape of the
/// measurement that found the Apple defect, so a platform that fails the
/// same way fails the same number.
const THREADS: usize = 8;
const PER_THREAD: usize = 8;

/// One query, answered or not. Deliberately narrow — this test counts
/// answers and says nothing about which, because the tests above already
/// pin the content and a second copy of that would rot.
///
/// [`TYPE_HTTPS`] rather than [`TYPE_A`], and the difference is Windows 10:
/// `A` is one of the forty-three types that platform parses, so a burst of
/// them would be [`Error::UnsupportedType`] there rather than evidence
/// about threads.
fn answered() -> bool {
    matches!(lookup("cloudflare.com", TYPE_HTTPS), Ok(ref found) if !found.is_empty())
}

/// **The requirement `sys/mod.rs` names, asserted for the first time.**
/// That module says a `res_query` backend needs "a libc whose resolver
/// state is per-thread", and until this test nothing checked it: the
/// property was established by reading exported symbols, which is how
/// Apple's arm shipped for two verticals unable to serve a client —
/// **64/64 answered serially and 12/64 from eight threads**, measured on
/// macOS 27, with 46 of the failures returning before anything went out.
///
/// **Why threads at all, when every call here blocks.** Because a blocking
/// call is run on a blocking *pool*. `hclient-dns-system` wraps each of
/// `Resolve`'s three methods in its own `Blocking::run`, and a single
/// request asks for A, AAAA and the HTTPS record at once — so three of
/// these are in flight on three pool threads before a connection is
/// opened, and a server handling many requests has many more. Concurrency
/// is not a thing a caller opts into here; it is the ordinary shape, and
/// it is what made the Apple defect a failure rather than a curiosity.
///
/// **The serial burst is the control**, and it is what keeps this from
/// blaming threads for a resolver that is simply down: only a serial pass
/// beside a concurrent failure is evidence about the backend, and the
/// message says which half broke so a reader does not have to guess.
///
/// **`cargo test`, not just nextest.** nextest runs each test in its own
/// process, so tests running "in parallel" are parallel *processes* and
/// exercise nothing about shared state — which is why the four tests above
/// passing concurrently says nothing here, and why this one spawns its own
/// threads instead of relying on the runner.
///
/// This is also the instrument any new platform is established with. The
/// symbol a backend links against fails loudly; this property fails
/// quietly, and quietly is what it did.
#[test]
#[ignore = "needs a name server"]
fn concurrent_lookups_all_answer_where_a_serial_burst_does() {
    if !support().allows(TYPE_HTTPS) {
        // Nothing to establish about a build with no backend — and asking
        // through `allows` rather than for a named variant is the whole
        // point of the type: this file is outside the crate, so the fields
        // are not its to read.
        return;
    }

    let total = THREADS * PER_THREAD;
    let serial = (0..total).filter(|_| answered()).count();

    let concurrent: usize = std::thread::scope(|scope| {
        let running: Vec<_> = (0..THREADS)
            .map(|_| scope.spawn(|| (0..PER_THREAD).filter(|_| answered()).count()))
            .collect();
        running
            .into_iter()
            .map(|t| t.join().expect("a lookup thread panicked"))
            .sum()
    });

    assert_eq!(
        serial, total,
        "{serial}/{total} answered one at a time, so this machine cannot \
         resolve rather than cannot resolve concurrently"
    );
    assert_eq!(
        concurrent, total,
        "{serial}/{total} answered one at a time and {concurrent}/{total} from \
         {THREADS} threads: this platform's resolver state is shared, and a \
         caller running these on a blocking pool loses answers"
    );
}
