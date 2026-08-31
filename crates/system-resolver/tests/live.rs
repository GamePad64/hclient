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
/// `DNS_TYPE_CAA` exists in the Win32 metadata at all.
const TYPE_CAA: u16 = 257;

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
    assert!(
        matches!(absent, Err(Error::NameDoesNotExist)),
        "expected NXDOMAIN, got {absent:?}"
    );

    // The control, and the wildcard case above is what makes it worth
    // having: without it, a `lookup` that answered `NameDoesNotExist` for
    // everything would pass the assertion above.
    let present = lookup("www.cloudflare.com", TYPE_CAA).expect("the name exists");
    assert!(present.is_empty(), "no CAA at this name");
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
/// Both directions are asserted, and that matters more than it looks:
/// under `AnyExcept` the earlier tests would pass on either path, because
/// `HTTPS` and `CAA` are answerable on both. Type A is the discriminator.
#[test]
#[ignore = "needs a name server"]
fn support_and_lookup_agree_about_a_type_that_separates_the_two_windows_calls() {
    match support() {
        Support::Any => {
            let found = lookup("cloudflare.com", TYPE_A).expect("A is answerable");
            assert!(!found.is_empty(), "cloudflare.com has A records");
            assert!(found.iter().all(|r| r.rdata.len() == 4), "an IPv4 address");
        }
        Support::AnyExcept(parsed) => {
            assert!(parsed.contains(&TYPE_A), "A is a type Windows parses");
            assert!(matches!(
                lookup("cloudflare.com", TYPE_A),
                Err(Error::UnsupportedType { rtype: TYPE_A })
            ));
        }
        Support::None => {
            assert!(matches!(
                lookup("cloudflare.com", TYPE_A),
                Err(Error::Unsupported)
            ));
        }
    }
}
