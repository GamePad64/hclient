//! What `Overrides` promises, and the three things about it that are
//! decisions rather than mechanics.

use futures_util::StreamExt;
use hclient_dns::{IpLiteralOnly, Overrides, Resolve, ResolvedAddr};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

fn v4(a: [u8; 4]) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(a[0], a[1], a[2], a[3]))
}

fn collect_v4<D: Resolve>(d: &D, name: &str) -> Vec<IpAddr> {
    futures_executor::block_on(async {
        d.lookup_ipv4(name)
            .filter_map(|r| async { r.ok() })
            .map(|r: ResolvedAddr| r.addr)
            .collect()
            .await
    })
}

fn collect_v6<D: Resolve>(d: &D, name: &str) -> Vec<IpAddr> {
    futures_executor::block_on(async {
        d.lookup_ipv6(name)
            .filter_map(|r| async { r.ok() })
            .map(|r: ResolvedAddr| r.addr)
            .collect()
            .await
    })
}

#[test]
fn an_overridden_name_answers_from_the_table_in_the_order_given() {
    let dns = Overrides::new(IpLiteralOnly).host(
        "staging.example.com",
        [v4([203, 0, 113, 7]), v4([203, 0, 113, 8])],
    );
    assert_eq!(
        collect_v4(&dns, "staging.example.com"),
        vec![v4([203, 0, 113, 7]), v4([203, 0, 113, 8])],
        "order is the caller's; Happy Eyeballs upstream decides what to do with it",
    );
}

/// The control for the test above: without it, an `Overrides` that ignored
/// its table entirely and always delegated would still pass a test that
/// only checked a name it had no entry for.
#[test]
fn a_name_with_no_entry_goes_to_the_resolver_underneath() {
    let dns = Overrides::new(IpLiteralOnly).host("staging.example.com", [v4([203, 0, 113, 7])]);
    // `IpLiteralOnly` answers a literal and refuses a name, so its answer
    // is distinguishable from the table's by construction.
    assert_eq!(
        collect_v4(&dns, "198.51.100.4"),
        vec![v4([198, 51, 100, 4])]
    );
    assert!(
        collect_v4(&dns, "other.example.com").is_empty(),
        "a name the table does not have must reach the resolver, which refuses it",
    );
}

/// **The families are split, not duplicated.** `Resolve` asks two
/// questions and a table entry answers both — an implementation that
/// returned every address to both lookups would hand Happy Eyeballs a v6
/// address in its v4 slot, which is not a thing the connector checks.
#[test]
fn one_entry_answers_both_families_and_each_gets_only_its_own() {
    let six = IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1));
    let dns = Overrides::new(IpLiteralOnly).host("dual.example.com", [v4([203, 0, 113, 7]), six]);

    assert_eq!(
        collect_v4(&dns, "dual.example.com"),
        vec![v4([203, 0, 113, 7])]
    );
    assert_eq!(collect_v6(&dns, "dual.example.com"), vec![six]);
}

/// DNS names are case-insensitive (RFC 4343), and a caller typing
/// `--resolve Example.com:...` on a command line means the same host.
#[test]
fn a_name_matches_whatever_case_it_was_written_in() {
    let dns = Overrides::new(IpLiteralOnly).host("Example.COM", [v4([203, 0, 113, 7])]);
    assert_eq!(collect_v4(&dns, "example.com"), vec![v4([203, 0, 113, 7])]);
    assert_eq!(collect_v4(&dns, "EXAMPLE.com"), vec![v4([203, 0, 113, 7])]);
}

/// **An empty entry is an answer, and a missing one is not.** They are
/// different states and a caller can reach both: `--resolve host:port:`
/// with nothing after it is a host that resolves to nothing, which is not
/// the same as a host nobody overrode.
#[test]
fn an_empty_entry_answers_nothing_rather_than_passing_through() {
    let dns = Overrides::new(IpLiteralOnly).host("198.51.100.4", []);
    assert!(
        collect_v4(&dns, "198.51.100.4").is_empty(),
        "the entry answers, and its answer is no addresses — the literal underneath must not be reached",
    );
}

/// A second entry for one host replaces the first: a table entry is one
/// answer, and a caller who passed the flag twice meant the second.
#[test]
fn a_second_entry_for_one_host_replaces_the_first() {
    let dns = Overrides::new(IpLiteralOnly)
        .host("example.com", [v4([203, 0, 113, 7])])
        .host("example.com", [v4([203, 0, 113, 9])]);
    assert_eq!(collect_v4(&dns, "example.com"), vec![v4([203, 0, 113, 9])]);
}

/// **SVCB passes through, and that is a decision.** An override says where
/// a *name* points; an HTTPS record is the origin's own statement about
/// itself, carrying its own address hints and ALPN. Answering one from a
/// table would be a different feature wearing this name.
#[test]
fn svcb_is_the_resolver_underneath_s_answer_untouched() {
    let dns = Overrides::new(IpLiteralOnly).host("example.com", [v4([203, 0, 113, 7])]);
    assert_eq!(
        dns.supports_svcb(),
        IpLiteralOnly.supports_svcb(),
        "the capability is reported by whoever can actually answer it",
    );
}
