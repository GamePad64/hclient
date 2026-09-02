//! Tier 2 of h3-research §4's discovery ladder: the HTTPS record this
//! client already knows how to fetch, finally read.
//!
//! `hclient_dns::SvcbEndpoint` has carried `alpn`, `port`, `ipv4hint`,
//! `ipv6hint` and `ech_config_list` since v0.2, and `Resolve::
//! supports` has said which resolvers can answer. Nothing consumed
//! any of it: [`crate::connect`]'s module doc said so in its own words and
//! passed `ech: None`. This module is what consumes it, and
//! [`crate::connect::connect`] is its only caller.
//!
//! # What tier 2 cannot do, and no amount of care here would change it
//!
//! **It cannot choose HTTP/3.** An `alpn` containing `h3` is a fact this
//! crate can read and cannot act on: `hclient-h3` is a different crate
//! with different bounds (`R: UdpBind + Spawn<..>`, `T: QuicTlsConnect`),
//! `Native<R, T, D>` has neither, and a `Client` holds exactly one
//! transport — there is nowhere in **this crate** for "choose between two
//! protocol stacks" to live. That is a transport owning both, and it
//! exists: `hclient-select`'s `Selecting`, which reads the same record and
//! routes to `Native` or `H3`. So the gap is stated here and closed one
//! level up.
//!
//! So [`alpn_offer`] below never returns `h3`, and it is not an oversight
//! to be fixed by adding it to the list: offering a protocol this
//! transport cannot speak would let a server select it, at which point the
//! connection is unusable. The record's `h3` is read, is not offered, and
//! is not treated as an error either — the origin also advertised
//! something we *can* speak, or the endpoint is not for us.
//!
//! # Only `https://`, and only at the scheme's default port
//!
//! Two conditions, each with a reason that is not "we did not get round to
//! it":
//!
//! - **`http://` is excluded** because for a cleartext origin the presence
//!   of an HTTPS RR means something this connector will not do: RFC 9460
//!   §9.5 makes it an instruction to *upgrade the scheme to `https`*.
//!   Taking the record's port and hints while ignoring that is applying
//!   half a rule whose other half is the entire point, and the upgrade
//!   itself is a redirect-shaped decision that belongs to whoever owns the
//!   request, not to a connector.
//! - **A non-default port is excluded** because the record we would be
//!   holding is the wrong one. RFC 9460 §9.5 puts the record for a
//!   non-default port under a prefixed name (`_8443._https.example.com`),
//!   and `Resolve::lookup` is handed the origin host, so what comes
//!   back is the default-port record. Applying it to `https://host:8443/`
//!   would be applying one service's parameters to another's. This
//!   connector does not construct the prefixed name: it would then have to
//!   decide what the A and AAAA lookups are asked for (the prefixed
//!   name has no addresses), and that is a resolver-facing question the
//!   `Resolve` seam does not answer today.
//!
//! # AliasMode is skipped, and skipping it is load-bearing
//!
//! `endpoint_from_binding` in `hclient-dns-system` emits AliasMode records
//! as an endpoint with `priority: 0` and every other field empty (RFC 9460
//! §2.4.1: a recipient MUST ignore the SvcParams of an AliasMode record).
//! Priority 0 is also numerically the *lowest*, so a selection that took
//! the minimum priority without checking would pick the alias every time
//! and act on an endpoint that carries nothing — discovery would look
//! wired up and do nothing at all. Following an alias means restarting
//! resolution at another name, which is not implemented here.
//!
//! # The negative cache, and why it is this crate's rather than Alt-Svc's
//!
//! A record is a claim about an origin that the network may not honour:
//! the port it names can be filtered, the addresses it hints at can be
//! unreachable from here. Without a memory, *every* request to such an
//! origin pays that failed attempt again. **This cache does not belong to
//! Alt-Svc**: the cache of *what was advertised* is Alt-Svc's, the cache
//! of *what failed* is
//! the connector's, and the advertisement's source (a DNS record or a
//! header) does not change what a blocked port costs.
//!
//! So: a connect that used a discovered endpoint and failed marks the
//! origin for [`SVCB_FAILURE_TTL`], and while that mark stands, requests to
//! that origin are made exactly as they were before this module existed.
//! The mark is per-origin and not per-endpoint, because the alternative is
//! not usable: an origin whose record we have just refused to use is an
//! origin whose record we do not fetch, so there is no endpoint left to
//! key on.
//!
//! Two things it deliberately is not. It is **not exponential** — a flat
//! window is what can be defended without a failure counter per origin and
//! a cap on it, and the condition it is waiting out (a stale record, a
//! filtered port) is usually resolved by a DNS change on a timescale of
//! minutes. And it is **not a cache of the lookup**: "this origin has no
//! HTTPS record" is a DNS answer with a TTL of its own, which
//! `SvcbEndpoint` does not carry, and inventing a lifetime for someone
//! else's answer is how a resolver's cache and ours drift apart.

use bytes::Bytes;
use futures_util::StreamExt;
use hclient_dns::{RData, Resolve, SvcbEndpoint, rtype};
use std::collections::HashMap;
use std::fmt::Debug;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long a failed connect through a discovered endpoint keeps this
/// transport away from that origin's HTTPS record.
///
/// Five minutes: long enough that a filtered port is paid for once rather
/// than once per request, short enough that an operator who fixes a record
/// sees clients follow it again within the time a DNS change takes to
/// propagate anyway. It is a constant rather than a setting because a
/// setting would need a caller who knows better than that, and the failure
/// it waits out belongs to the network rather than to the caller.
pub const SVCB_FAILURE_TTL: Duration = Duration::from_secs(300);

/// The origin a discovery attempt is remembered against: what a URI's
/// authority resolves to once the scheme's default port has been applied.
///
/// The host is ASCII-lowercased for the same reason [`crate::pool`]'s key
/// lowercases it — `Example.COM` and `example.com` are one origin, and a
/// key that told them apart would remember a failure under a name the next
/// request does not use.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Origin {
    host: Box<str>,
    port: u16,
}

impl Origin {
    pub(crate) fn new(host: &str, port: u16) -> Self {
        Self {
            host: host.to_ascii_lowercase().into_boxed_str(),
            port,
        }
    }
}

/// The origins whose HTTPS record has just cost a failed connection.
///
/// Cheap to clone (an `Arc` bump), and every clone is the same cache — it
/// lives on [`crate::Native`] and is shared by every request that
/// transport makes, which is the whole point: a memory that lasted one
/// request would be no memory at all.
#[derive(Clone, Default)]
pub(crate) struct NegativeCache {
    /// Origin -> the elapsed time, measured on the owning transport's
    /// `Timer` from the instant that transport was constructed, past which
    /// discovery may be attempted again.
    ///
    /// The same clock and the same origin as [`crate::pool`]'s deadlines,
    /// and for the same reason: `hclient_rt::Timer` is the one seam
    /// through which time reaches this crate, and a `std::time::Instant::
    /// now()` here would disagree with a caller testing under
    /// `tokio::time::pause()`.
    until: Arc<Mutex<HashMap<Origin, Duration>>>,
}

impl Debug for NegativeCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NegativeCache")
            .field(
                "suppressed",
                &self.until.lock().map(|m| m.len()).unwrap_or(0),
            )
            .finish()
    }
}

impl NegativeCache {
    /// Whether discovery is currently suppressed for `origin` — and, as
    /// the same pass, forgetting the entry if its window has closed.
    ///
    /// The expiry is applied here rather than by a background sweep
    /// because this is the only place that asks: an entry nobody looks up
    /// costs one small map slot until the next lookup for that origin, and
    /// a transport is not usually built for one origin it then never
    /// contacts again.
    pub(crate) fn suppressed(&self, origin: &Origin, now: Duration) -> bool {
        let mut until = self.until.lock().expect("svcb negative cache poisoned");
        match until.get(origin) {
            Some(&expires_at) if now < expires_at => true,
            Some(_) => {
                until.remove(origin);
                false
            }
            None => false,
        }
    }

    /// Records that a connect using this origin's HTTPS record failed.
    ///
    /// Saturating, for the same reason [`crate::Native::checkin_for`]'s
    /// deadline is: an elapsed time near `Duration::MAX` is not a case to
    /// panic on.
    pub(crate) fn record(&self, origin: Origin, now: Duration) {
        let mut until = self.until.lock().expect("svcb negative cache poisoned");
        until.insert(origin, now.saturating_add(SVCB_FAILURE_TTL));
    }
}

/// What one HTTPS record contributes to one connection attempt.
///
/// A type of this crate's own rather than the `SvcbEndpoint` it is built
/// from: `target` is deliberately absent, because this connector does not
/// resolve the target name (see [`lookup`]), and a field carried but never
/// read is how the previous round of this plumbing came to sit unused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Endpoint {
    /// RFC 9460 §7.2 `port`, or `None` when the record does not name one
    /// and the scheme's default stands.
    pub(crate) port: Option<u16>,
    /// §7.3 `ipv4hint`.
    pub(crate) ipv4hint: Vec<Ipv4Addr>,
    /// §7.3 `ipv6hint`.
    pub(crate) ipv6hint: Vec<Ipv6Addr>,
    /// §7.1 `alpn`, as the record gave it — the intersection with what
    /// this transport can speak is [`alpn_offer`]'s job.
    pub(crate) alpn: Vec<Vec<u8>>,
    /// RFC 9849 `ech`. Whether it is offered to the TLS backend is
    /// `TlsConnect::applies_ech`'s answer, not this module's — see
    /// [`crate::connect`].
    pub(crate) ech: Option<Bytes>,
}

impl Endpoint {
    /// `true` when acting on this record changes nothing about the
    /// connection — no port, no hints, no ALPN restriction, no ECH.
    ///
    /// Used to decide whether a failed connect is the *record's* failure,
    /// and the distinction is not cosmetic: marking an origin because a
    /// record that contributed nothing was present would suppress
    /// discovery for an origin whose record never took part in the
    /// attempt, and the next request would behave identically anyway.
    pub(crate) fn is_inert(&self) -> bool {
        self.port.is_none()
            && self.ipv4hint.is_empty()
            && self.ipv6hint.is_empty()
            && self.alpn.is_empty()
            && self.ech.is_none()
    }
}

impl From<SvcbEndpoint> for Endpoint {
    fn from(e: SvcbEndpoint) -> Self {
        Self {
            port: e.port,
            ipv4hint: e.ipv4hint,
            ipv6hint: e.ipv6hint,
            alpn: e.alpn,
            ech: e.ech_config_list,
        }
    }
}

/// The HTTPS record this connection should be made under, or `None` when
/// there is none to act on.
///
/// **The lowest ServiceMode priority wins, and nothing else is tried.** RFC
/// 9460 §2.4.2 ranks alternatives by priority and expects a client to fall
/// back through them; this connector uses the first-ranked endpoint and
/// then, if the connection fails, the origin's own addresses — which is
/// the fallback that matters, because it is the one that is still there
/// when every record is wrong. Walking the whole list would multiply a
/// request's connect budget by the number of records an attacker-influenced
/// answer may contain.
///
/// **A lookup error is not fatal, and that is a decision rather than a
/// discarded `Result`.** An HTTPS query is answered with SERVFAIL by a
/// long tail of middleboxes and old resolvers that have never heard of RR
/// type 65; a client that failed the request there would be unable to
/// reach origins it reaches perfectly well today. The lookup's job is to
/// improve a connection, and it either has an answer or it has not. Note
/// what this does *not* swallow: the address lookups below it are
/// untouched, so a resolver that is genuinely broken (or a runtime that is
/// shutting down — `ErrorKind::Cancelled`) still reaches the caller
/// through `connect::drive`'s own machinery, with its kind intact.
///
/// **The target name is not resolved.** RFC 9460 §2.5 lets a ServiceMode
/// record point at another name whose addresses should be used; this
/// connector uses the record's hints and the *origin's* addresses, and
/// nothing else. A record whose target differs from the origin and carries
/// no hints therefore contributes only its port, ALPN and ECH — honest,
/// and one lookup rather than two.
/// **The capability is asked one level up**, in
/// [`crate::connect::discovered_endpoint`], and that is not tidying: "this
/// resolver cannot ask" is *nobody looked*, where this function's `None`
/// is *looked, and there is none*. They are the same instruction to this
/// connector and different ones to a caller holding the answer — see
/// [`Prefetched`]. Asking here as well would make the second unreachable.
pub(crate) async fn lookup<D>(dns: &D, host: &str) -> Option<Endpoint>
where
    D: Resolve,
{
    let mut best: Option<SvcbEndpoint> = None;
    let mut records = std::pin::pin!(dns.lookup(host, rtype::HTTPS));
    while let Some(record) = records.next().await {
        // A record of another type cannot arrive from an HTTPS query, but
        // one method answers every type now, so the filter is written
        // rather than guaranteed — as a filter, not as a branch nobody
        // believes in.
        let Ok(RData::Https(record)) = record.map(|r| r.rdata) else {
            continue;
        };
        // AliasMode. See the module doc: priority 0 sorts *below* every
        // ServiceMode record and carries no parameters at all, so a
        // selection that did not skip it would reliably choose the one
        // endpoint with nothing in it.
        if record.priority == 0 {
            continue;
        }
        if best.as_ref().is_none_or(|b| record.priority < b.priority) {
            best = Some(record);
        }
    }
    best.map(Endpoint::from)
}

/// The ALPN list to offer, restricted to what the record says the endpoint
/// speaks.
///
/// RFC 9460 §7.1: the SVCB-ALPN set is the union of the record's `alpn`
/// values with the scheme's default set, and a client is to offer the
/// protocols it supports *from that set*. For `https` the default set is
/// `http/1.1`, which is why a record listing only `h3` still leaves this
/// transport with something to offer rather than nothing.
///
/// `ours` arrives ranked (h2 before http/1.1, when h2 is on offer at all)
/// and the ranking is preserved: RFC 7301 leaves the choice to the server,
/// but every implementation reads the client's list as a preference order.
///
/// # Two limits, both about what does not cross the `SvcbEndpoint` seam
///
/// **`no-default-alpn` is invisible here.** `hclient-dns-system` parses the
/// parameter and `SvcbEndpoint` has no field for it, so this function
/// cannot tell "the default set applies" from "the record switched it
/// off". It assumes the former, which is the safe direction: assuming the
/// latter would drop `http/1.1` from the offer for every record that did
/// not mention it, and leave a client with nothing to propose.
///
/// **An empty result falls back to `ours`.** It cannot happen while
/// `http/1.1` is in the default set and in every list this crate builds,
/// and the fallback is there for the shape rather than the case: a client
/// that offered an empty ALPN list would meet `no_application_protocol`
/// from a server that would have answered it.
pub(crate) fn alpn_offer<'a>(ours: &[&'a [u8]], record: &'a [Vec<u8>]) -> Vec<&'a [u8]> {
    /// RFC 9460 §7.1.2 — the default set for the `https` scheme.
    const DEFAULT_SET: &[&[u8]] = &[b"http/1.1"];

    let offer: Vec<&[u8]> = ours
        .iter()
        .copied()
        .filter(|p| {
            DEFAULT_SET.contains(p) || record.iter().any(|advertised| advertised.as_slice() == *p)
        })
        .collect();
    if offer.is_empty() {
        ours.to_vec()
    } else {
        offer
    }
}

/// What is already known about an origin's HTTPS record by the time a
/// connection is opened for it.
///
/// **Three states, and the third is the one an `Option<Endpoint>` gets
/// wrong.** "Nobody has looked" and "somebody looked and there is nothing
/// to act on" ask opposite things of [`crate::connect::connect`]: the
/// first is an instruction to make the query, the second is an instruction
/// not to. Collapsed into one `None` they would re-query exactly the
/// origins whose answer cost the most to get — the ones that publish no
/// record, where a resolver has to reach an authoritative answer to say so
/// — and the caller that had already paid for it would pay again.
///
/// Deliberately **not** a cache. It is built from one request's own
/// authority, it lives as long as that request's connect, and nothing
/// remembers it afterwards; the reason there is no memory of a record in
/// this workspace is in this module's doc, and it is unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Prefetched {
    /// Nobody has looked, so the connector looks — what every request did
    /// before this type existed.
    NotConsulted,
    /// [`crate::connect::discovered_endpoint`] has already run, for this
    /// request's own authority, with a resolver that said it could ask,
    /// and this is what it found. `None` is an answer and not an absence.
    Looked(Option<Endpoint>),
}

impl Prefetched {
    /// The endpoint a connection may act on — `None` where a record was
    /// found but contributes nothing to the attempt.
    ///
    /// The inert filter lives here rather than in
    /// [`crate::connect::discovered_endpoint`] so that there is exactly
    /// one of it: a record with an empty ALPN list is still a record to
    /// [`Self::discovered`] — which is what stops a caller reading "this
    /// origin publishes nothing" off an origin that publishes something
    /// dull — and is still nothing to connect *through*, which is what
    /// keeps `connect`'s "was a record in play" test the same question as
    /// "could this attempt have gone differently".
    pub(crate) fn actionable(self) -> Option<Endpoint> {
        match self {
            // Not reachable through `connect`, which replaces this variant
            // with a lookup before asking. `None` rather than a panic
            // because there is genuinely nothing to act on: a record
            // nobody looked for cannot move a connection.
            Self::NotConsulted => None,
            Self::Looked(found) => found.filter(|e| !e.is_inert()),
        }
    }

    /// What this says about the origin's protocols — and nothing about its
    /// routing, see [`Discovered`].
    pub(crate) fn discovered(&self) -> Discovered<'_> {
        match self {
            Self::NotConsulted => Discovered::NotConsulted,
            Self::Looked(None) => Discovered::NoRecord,
            Self::Looked(Some(e)) => Discovered::Record { alpn: &e.alpn },
        }
    }
}

/// What [`crate::Prefetch::prepare`] found, as much of it as anything
/// outside this crate has business reading.
///
/// **The record's port and address hints are deliberately not here.** They
/// say *where to connect*, and the only thing that may decide that is this
/// connector, from an answer its own resolver gave it about this request's
/// own authority. What a caller may read is what the origin said it
/// **speaks** — a fact about protocols rather than about routing, and the
/// one a caller owning a second protocol stack needs in order to know
/// whether to use this transport at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Discovered<'a> {
    /// This transport did not look, so nothing here has been ruled out.
    ///
    /// Three ways to arrive, and none of them is a fact about the origin:
    /// discovery applies to `https://` at the scheme's default port and
    /// nowhere else (this module's doc says why for each); it is held off
    /// entirely while the origin is suppressed by an earlier failed
    /// attempt through its record; and **this transport's own resolver may
    /// say it cannot ask** (`Resolve::supports`), which is a fact
    /// about the resolver. A caller that wants an answer here must get it
    /// for itself — and a caller whose own resolver *can* ask should,
    /// because the question has not been put; a caller that does not want
    /// one may hand this straight back, and the connector behaves exactly
    /// as it does for a request nobody prepared.
    NotConsulted,
    /// Looked, and there is no record to act on: the origin publishes
    /// none, the lookup failed, or every answer was an AliasMode record.
    ///
    /// **Distinct from [`Self::NotConsulted`] on purpose**, and it is the
    /// half a plain `Option` gets wrong: this is an answer, and a caller
    /// holding it knows not to ask again.
    NoRecord,
    /// The first-ranked ServiceMode record's ALPN list, RFC 9460 §7.1, as
    /// the record gave it.
    ///
    /// An empty list is a record that names no protocol, which is not the
    /// same fact as [`Self::NoRecord`]: the origin published something,
    /// and this is what it said.
    Record {
        /// RFC 9460 §7.1 `alpn`, unranked and unfiltered — the
        /// intersection with what a transport can speak is that
        /// transport's business.
        alpn: &'a [Vec<u8>],
    },
}

/// A request, together with what this transport has already learned about
/// its origin's HTTPS record.
///
/// # The two travel together, and that is the whole of the design
///
/// A record is evidence about **one** authority. Handing a connector a
/// record and a request as two arguments makes "is this the record for
/// this request" a question — one that can only be answered by a check,
/// and a check needs an origin carried beside the record so that there is
/// something to compare. Here the pairing is made by
/// [`crate::Prefetch::prepare`] out of the request's own URI and cannot be
/// taken apart afterwards: no constructor puts a record beside a request
/// it was not fetched for, and no method replaces the request. The
/// wrong-origin question is not answered — it cannot be asked.
///
/// [`Self::new`] is the other constructor and it asserts **nothing**: it
/// is the state every request is in when it reaches `Transport::execute`,
/// and the connector does its own lookup for it. Both constructors
/// therefore keep one invariant — a `Prepared` never carries a record
/// fetched for another request.
///
/// # Why not a request extension
///
/// That was the other shape considered. Extensions are the **caller's**
/// channel — `Timeouts`, `AllowEarlyData` and `RequireVersion` are all
/// statements a caller makes about their own request, which a transport
/// reads and may refuse — where a record is evidence the transport would
/// otherwise have fetched for itself. An `SvcbEndpoint` carries a port and
/// address hints, so an extension carrying one would let any code that can
/// build a request move the connection to another port and another
/// address. Nothing in this workspace can do that today except DNS.
pub struct Prepared {
    pub(crate) req: http::Request<hclient_core::RequestBody>,
    pub(crate) found: Prefetched,
}

impl Prepared {
    /// A request nothing has been looked up for — exactly what
    /// `Transport::execute` receives, and exactly what it does with it.
    ///
    /// For a caller that prepares some requests and not others: the ones
    /// it did not ask about still go through
    /// [`crate::Prefetch::execute_prepared`], and this is how they get
    /// there, with the connector's own discovery untouched.
    pub fn new(req: http::Request<hclient_core::RequestBody>) -> Self {
        Self {
            req,
            found: Prefetched::NotConsulted,
        }
    }

    /// The request this was made for. There is deliberately no `_mut`: the
    /// record inside was fetched for this URI's authority, and a URI that
    /// could be edited afterwards would be the wrong-origin question
    /// arriving through the back door.
    pub fn request(&self) -> &http::Request<hclient_core::RequestBody> {
        &self.req
    }

    /// What the HTTPS record said — three states, see [`Discovered`].
    pub fn discovered(&self) -> Discovered<'_> {
        self.found.discovered()
    }

    /// The request back, leaving the record behind.
    ///
    /// For the caller that asked, read the answer, and decided to send
    /// this request somewhere else entirely — which is what
    /// `hclient-select` does with a record offering `h3`.
    pub fn into_request(self) -> http::Request<hclient_core::RequestBody> {
        self.req
    }
}

/// Hand-written for [`crate::Native`]'s reason: a derive would print a
/// whole request, and what is worth seeing here is which of the three
/// states the record is in.
impl Debug for Prepared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Prepared")
            .field("uri", &self.req.uri())
            .field("discovered", &self.discovered())
            .finish()
    }
}
