//! A resolver that answers some names from a table the caller wrote, and
//! passes the rest through — `curl --resolve`, as a [`Resolve`].

use crate::{RData, Record, Resolve, rtype};
use futures_core::Stream;
use hclient_core::Error;
use std::collections::HashMap;
use std::net::IpAddr;
use std::pin::Pin;
use std::task::{Context, Poll};

/// Sends chosen host names to addresses the caller supplied, and asks the
/// resolver underneath for everything else.
///
/// This is `curl --resolve` and `--connect-to`'s address half: staging a
/// request against a host that is not yet in DNS, pinning one of an
/// origin's addresses to reproduce a failure, or reaching a service by the
/// name its certificate carries while the packets go somewhere else.
///
/// ```
/// # use hclient_dns::{IpLiteralOnly, Overrides};
/// # use std::net::{IpAddr, Ipv4Addr};
/// let dns = Overrides::new(IpLiteralOnly)
///     .host("staging.example.com", [IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7))]);
/// # let _ = dns;
/// ```
///
/// # It overrides a **host**, where curl overrides a host and a port
///
/// [`Resolve::lookup`] takes a name and a type and no port — a resolver
/// answers *what addresses does this name have*, and the port belongs to
/// the connection rather than to the question. So `--resolve
/// example.com:443:203.0.113.7` maps onto `Overrides` for the host and
/// loses the `443`: an entry here answers for that name at every port.
///
/// That difference is invisible to the case the flag exists for — one
/// request, one port — and it is stated rather than hidden because the
/// case where it *is* visible is real: a caller who overrides `443` and
/// expects `8443` to resolve normally gets the override on both.
///
/// Widening the seam to carry a port was the alternative and is worse for
/// a reason that outlives this type: it would put a connection's business
/// into a question about names, in every resolver, for a feature that is a
/// diagnostic. `hclient-native`'s `unix_socket` is the shape for anything
/// that genuinely needs to move a *connection* somewhere else.
///
/// # What it does not do
///
/// **It does not touch SVCB.** [`supports`](Resolve::supports)
/// and [`lookup`](Resolve::lookup) pass straight through, so a
/// discovered HTTPS record still comes from the resolver underneath — and
/// with it that record's own address hints, which a connector may use in
/// preference to a lookup. An override is about where a *name* points; a
/// record is the origin's own statement about itself, and overruling one
/// silently from a table would be a different feature wearing this name.
///
/// **It does not sort, filter or deduplicate.** The addresses come back in
/// the order given, split by family, and Happy Eyeballs upstream does what
/// it always does with them.
#[derive(Debug, Clone, Default)]
pub struct Overrides<D> {
    inner: D,
    /// Keyed by the lowercased name: DNS names are case-insensitive
    /// (RFC 4343), and a caller who wrote `Example.com` on a command line
    /// means the same host as `example.com`.
    table: HashMap<String, Vec<IpAddr>>,
}

impl<D> Overrides<D> {
    /// Wraps a resolver, overriding nothing yet.
    pub fn new(inner: D) -> Self {
        Self {
            inner,
            table: HashMap::new(),
        }
    }

    /// Answers `name` with `addrs` instead of asking the resolver.
    ///
    /// A second call for the same name **replaces** the first rather than
    /// appending: a table entry is one answer, and a caller who passed
    /// `--resolve` twice for one host meant the second one. Passing an
    /// empty list is an entry too — the name then resolves to nothing,
    /// which is a real thing to want and different from removing it.
    #[must_use]
    pub fn host(mut self, name: &str, addrs: impl IntoIterator<Item = IpAddr>) -> Self {
        self.table
            .insert(name.to_ascii_lowercase(), addrs.into_iter().collect());
        self
    }

    /// The resolver underneath, for a caller that needs it back.
    pub fn into_inner(self) -> D {
        self.inner
    }

    /// The overriding answers for `name` of type `rtype`, or `None` where
    /// this table says nothing about the name.
    ///
    /// **An override is an address, so only `A` and `AAAA` can be
    /// overridden** — the same rule the two-method shape stated by having
    /// no third method to override, said once now that there is one
    /// method. An HTTPS record carries a port and an ALPN, and minting one
    /// would put values in the answer that nobody supplied.
    fn matching(&self, name: &str, rtype: u16) -> Option<Vec<Record>> {
        if !matches!(rtype, rtype::A | rtype::AAAA) {
            return None;
        }
        let addrs = self.table.get(&name.to_ascii_lowercase())?;
        Some(
            addrs
                .iter()
                .filter(|a| a.is_ipv6() == (rtype == rtype::AAAA))
                .map(|a| Record::new(RData::from(*a)))
                .collect(),
        )
    }
}

/// Either the table's answer or the resolver's, as one type.
///
/// A named enum rather than a boxed `dyn Stream`, so `Send` follows from
/// `S` instead of being chosen here — the same reason every seam in this
/// workspace carries an associated future rather than an RPITIT.
#[derive(Debug)]
pub enum Answer<S> {
    /// The table had this name. Yielded in the order the caller wrote.
    Overridden(std::vec::IntoIter<Record>),
    /// It did not; this is the resolver underneath.
    PassedThrough(S),
}

impl<S> Stream for Answer<S>
where
    S: Stream<Item = Result<Record, Error>> + Unpin,
{
    type Item = Result<Record, Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // `S: Unpin` is on this impl, so a plain `get_mut` and no
        // projection — this crate forbids `unsafe`.
        match self.get_mut() {
            Answer::Overridden(it) => Poll::Ready(it.next().map(Ok)),
            Answer::PassedThrough(s) => Pin::new(s).poll_next(cx),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Answer::Overridden(it) => {
                let n = it.len();
                (n, Some(n))
            }
            Answer::PassedThrough(s) => s.size_hint(),
        }
    }
}

impl<D> Resolve for Overrides<D>
where
    D: Resolve,
    for<'a> D::Records<'a>: Unpin,
{
    type Records<'a>
        = Answer<D::Records<'a>>
    where
        Self: 'a;

    /// The resolver underneath's, unchanged — see the type's own doc for
    /// why an override is not allowed to answer for a type it cannot
    /// supply.
    fn supports(&self, rtype: u16) -> bool {
        self.inner.supports(rtype)
    }

    fn lookup<'a>(&'a self, name: &str, rtype: u16) -> Self::Records<'a> {
        match self.matching(name, rtype) {
            Some(v) => Answer::Overridden(v.into_iter()),
            None => Answer::PassedThrough(self.inner.lookup(name, rtype)),
        }
    }
}
