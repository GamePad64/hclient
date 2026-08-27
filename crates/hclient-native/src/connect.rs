//! Connector: Happy Eyeballs (RFC 8305) over TCP, then optional TLS with
//! ALPN.
//!
//! # Where "Resolution Delay" lives here
//!
//! `hclient_dns::Resolve` deliberately returns a `Stream`, not a
//! `Future<Output = Vec<_>>` — the only reason is that RFC 8305 §3
//! requires starting IPv6 attempts without waiting for the IPv4 answer,
//! and `Scheduler` can only react to that if it's fed as results
//! arrive, not as one block after the resolver has finished both
//! families. If this file first collected both streams into a `Vec` and
//! then handed them to `Scheduler::offer_v6`/`offer_v4` in one call with
//! an immediate `mark_v6_done`/`mark_v4_done`, "Resolution Delay" would be
//! dead: no time would pass between `offer` and `mark_done`, and
//! `Scheduler` would never end up in the state "AAAA hasn't arrived yet,
//! but the resolver isn't done either" — the very state it's built as a
//! state machine, rather than a sort over an already-ready list, to
//! handle.
//!
//! `drive` below is the only place that polls `Scheduler`, and it feeds it
//! REAL streams: `connect` passes `dns.lookup_ipv6`/`lookup_ipv4` into it
//! one item at a time, and calls `mark_*_done` only once the stream has
//! actually finished (`None` from `poll_next`), not ahead of time. Since
//! the HTTPS query became concurrent with them the resolver's stream is
//! reached through [`Answers`] rather than handed over directly, and that
//! type exists precisely so that stays true: it replays what
//! arrived early and goes on polling for the rest, where a `Vec` of
//! collected answers would have been the dead-Resolution-Delay shape
//! above. `race_connect` is the second, simpler entry point: it has
//! nothing to resolve (the addresses are already given whole), so it
//! wraps them in `stream::iter` (a stream that yields everything on the
//! first poll and immediately ends) and passes that into the same
//! `drive`. Both entry points go through the same state machine not to
//! save code: it guarantees that a mutation in the interleaving/pacing
//! rules is caught equally by tests through either path, not just one of
//! them.
//!
//! # Why `race_connect` no longer builds the brief's `select_biased!`
//!
//! This task's draft showed `race_connect` with exactly two event sources
//! at the moment of `HeAction::Wait`: attempts (`attempts.next()`) and the
//! timer (`rt.sleep(d)`). `select_biased!`/`select!` from `futures-util`
//! can handle that (FusedFuture on both arms), but `drive` below is fed by
//! TWO MORE sources, the DNS streams, and `select_biased!` doesn't support
//! conditionally-excluded arms — an arm that must go silent forever once
//! its family's stream has finished can't be expressed in the macro's
//! syntax without a separate `enum` wrapper for each arm.
//! `std::future::poll_fn` with an explicit `if !done { poll }` before each
//! source solves the same problem with plain Rust code: a source that
//! currently has nothing to ask (its stream is already done, `attempts`
//! is empty) simply isn't polled this round — and doesn't need a `Waker`,
//! because it objectively cannot produce a new event (see the comment at
//! the `attempts.is_empty()` site below — the same technique as the
//! brief, just generalized from two sources to four).
//!
//! # RFC 6724 (Destination Address Selection) is NOT implemented here — an accepted, named gap
//!
//! `hclient_dns::Resolve`'s doc comment originally named this task as the
//! place where addresses of a SINGLE family must be sorted per RFC 6724
//! §6 before `offer_v4`/`offer_v6`. Checking before implementing it showed
//! this isn't achievable as stated: a full implementation (Rule 1–Rule 10
//! of RFC 6724 §6, some of which require Source Address Selection — i.e.,
//! knowledge of the routing table, which no trait in this vertical
//! provides) is its own, large task, not something to improvise along the
//! way in the connector. Simulating some of the rules without the rest
//! would mean claiming partial compatibility that doesn't actually exist
//! — the same principle that split `RedirectSupport::None`/`Transparent`
//! apart (`a capability that lies about its own state is worse than a
//! capability that simply doesn't exist`, see the doc comments on
//! `hclient_dns::Resolve` and `hclient_tls::TlsInfo`). So: addresses of
//! each family go into `Scheduler::offer_v6`/`offer_v4` in whatever order
//! the resolver handed them over — `Scheduler` documents that sorting
//! isn't its concern, `hclient_dns::Resolve` (updated alongside this
//! finding) now says the same thing from the other end of the seam, and
//! this file doesn't take it on either.
//!
//! This is not an open question: it is an explicit gap, not an oversight
//! for someone to rediscover a third time. Closing it is
//! possible if a separate Source Address Selection capability shows up —
//! until then, neither `Resolve`, nor `Scheduler`, nor this file does any
//! sorting. (For the system resolver specifically, the OS itself often
//! already hands back addresses in RFC 6724 order — see
//! `hclient-dns-system` — but that's a property of a particular backend,
//! not a guarantee of the trait.)
//!
//! # RFC 9460 SVCB/HTTPS is wired up here now
//!
//! This section read "isn't wired up here either" until v0.3 W2, and it
//! was honest: `TlsRequest::ech` and `Resolve::lookup_svcb`/
//! `SvcbEndpoint` both existed, `connect` consumed neither, and
//! it passed `ech: None` rather than pretending it had asked. It asks now.
//! [`crate::discovery`] holds the record-shaped half — which record is
//! chosen, what an `alpn` set means, and the negative cache — and this
//! file holds the connection-shaped half: the port every attempt uses, the
//! addresses Happy Eyeballs starts from, the ALPN list the handshake
//! offers, and whether an ECH config is passed on.
//!
//! **The ECH config is passed on only to a backend that says it applies
//! one** (`TlsConnect::applies_ech`, defaulted to `false`, and `false` for
//! every backend in this workspace today). The alternative is not a
//! stylistic choice: `hclient-tls-rustls` refuses a non-`None` `ech`, so a
//! connector that filled the field from every record would make every
//! origin publishing an ECH config unreachable through the default TLS
//! backend. Measured before it was decided rather than reasoned about —
//! `tests/svcb.rs`'s `an_ech_publishing_origin_is_still_reachable`, which
//! fails with exactly zero bytes on the wire if the guard is removed.
//!
//! What that guard costs is a privacy fact rather than an implementation
//! detail, and it is written where a caller can find it: on
//! `TlsConnect::applies_ech` and on `Native::new`'s `Capabilities`. In
//! one line — with no backend that applies ECH, a connection to an origin that publishes a config is still made,
//! and still sends that origin's name in the clear.
//!
//! # No `expect(dead_code)` here
//!
//! `Native::execute` (`src/lib.rs`) genuinely calls `connect`, not just in
//! tests, so there is no path in this file
//! (`connect`, `Conn`, `host`, `port`, `wants_tls`) that is only alive in
//! test builds (the same conclusion `body.rs`'s doc comment reached for
//! `Inner`/`OutgoingBody` a year earlier in the same vertical).
#![allow(clippy::too_many_arguments)]

use crate::discovery::{self, Endpoint, NegativeCache, Origin, Prefetched};
use crate::{mark, since};
use futures_util::Stream;
use futures_util::stream::{FuturesUnordered, StreamExt};
use hclient_core::unversioned::{Hooks, NoHooks};
use hclient_core::{Error, ErrorKind};
use hclient_dns::{Resolve, ResolvedAddr};
use hclient_proto::happy_eyeballs::{HeAction, HeConfig, Scheduler};
use hclient_rt::{TcpConnect, TcpOpts, Timer};
use hclient_tls::{TlsConnect, TlsInfo, TlsRequest};
use http::Uri;
use hyper::rt::{Read, ReadBufCursor, Write};
use std::future::poll_fn;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

/// A connection: with or without TLS. Both variants are `hyper::rt` IO.
///
/// `pub`, not `pub(crate)`, since v0.2 W2: it appears in the public
/// signature of [`crate::Native`]'s `Transport::Body`
/// (`NativeBody<NativeIo<R, T>>`), because the response body now holds its
/// connection as a concrete type rather than a `Box<dyn Future>` — see
/// `h1.rs`'s module doc comment for why that box had to go. Nothing here
/// is meant to be constructed by a caller; it is nameable because Rust
/// requires the type in a public signature to be nameable, not because it
/// is an API.
#[derive(Debug)]
pub enum Conn<P, T> {
    Plain(P),
    Tls(T),
}

impl<P: Read + Unpin, T: Read + Unpin> Read for Conn<P, T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: ReadBufCursor<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Conn::Plain(p) => Pin::new(p).poll_read(cx, buf),
            Conn::Tls(t) => Pin::new(t).poll_read(cx, buf),
        }
    }
}

impl<P: Write + Unpin, T: Write + Unpin> Write for Conn<P, T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        b: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            Conn::Plain(p) => Pin::new(p).poll_write(cx, b),
            Conn::Tls(t) => Pin::new(t).poll_write(cx, b),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Conn::Plain(p) => Pin::new(p).poll_flush(cx),
            Conn::Tls(t) => Pin::new(t).poll_flush(cx),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Conn::Plain(p) => Pin::new(p).poll_shutdown(cx),
            Conn::Tls(t) => Pin::new(t).poll_shutdown(cx),
        }
    }
}

// --- Typed errors for this module ---------------------------------------
//
// "No silent no-ops": none of the sites below collapse a failure into
// `AllAttemptsFailed`/`ErrorKind::Connect` silently — every distinction
// (the resolver failed / the resolver honestly found zero addresses /
// TCP attempts genuinely happened and all failed / the Happy Eyeballs
// config was outside the RFC-recommended range) stays visible through a
// separate type and a separate `ErrorKind`.

#[derive(Debug, thiserror::Error)]
#[error("all {0} connection attempts failed")]
pub(crate) struct AllAttemptsFailed(pub(crate) usize);

/// No address arrived for either family — and THIS DISTINGUISHES whether
/// the cause was the resolver failing, or the resolver honestly finishing
/// and finding zero records (e.g. `NXDOMAIN`). Collapsing both cases into
/// `AllAttemptsFailed(0)` would be exactly the "resolver error becomes
/// 'no addresses'" this type exists to prevent: it would read as "zero
/// TCP attempts failed," even though there was no TCP attempt at all —
/// not because none were tried, but because there was nothing to try.
/// [`Neither`](Self::Neither) is that second case, named rather than
/// spelled `(None, None)`.
///
/// **Four variants rather than two `Option<Error>` fields — because of
/// `source()`, not for tidiness.** The chain here has to lead to the
/// first family that actually failed, whichever one that is; as a struct
/// that is `v6.or(v4)`, and `thiserror` has no way to say it: `#[source]`
/// marks one field, so marking `v6` would end the chain at `None`
/// whenever only ipv4 failed — a truncation that changes no message and
/// breaks no test written about one. Split into variants, each carries
/// exactly the errors that exist in that case, `#[source]` names the
/// right one in each, and the case with no cause at all has no `#[source]`
/// because there is genuinely nothing to point at.
#[derive(Debug, thiserror::Error)]
enum ResolveErrors {
    #[error("ipv6 lookup failed ({v6}); ipv4 lookup failed ({v4})")]
    Both {
        #[source]
        v6: Error,
        v4: Error,
    },
    #[error("ipv6 lookup failed ({v6}); ipv4 lookup returned no addresses")]
    Ipv6 {
        #[source]
        v6: Error,
    },
    #[error("ipv4 lookup failed ({v4}); ipv6 lookup returned no addresses")]
    Ipv4 {
        #[source]
        v4: Error,
    },
    #[error("resolver returned no addresses for either address family")]
    Neither,
}

impl ResolveErrors {
    /// Whatever each family recorded, if anything — the shape `drive`
    /// accumulates in, folded into the variant that describes it.
    fn from_families(v6: Option<Error>, v4: Option<Error>) -> Self {
        match (v6, v4) {
            (Some(v6), Some(v4)) => Self::Both { v6, v4 },
            (Some(v6), None) => Self::Ipv6 { v6 },
            (None, Some(v4)) => Self::Ipv4 { v4 },
            (None, None) => Self::Neither,
        }
    }

    /// The errors this variant recorded, ipv6 first — the same order
    /// `source()` follows, so the two cannot drift apart.
    fn recorded(&self) -> [Option<&Error>; 2] {
        match self {
            Self::Both { v6, v4 } => [Some(v6), Some(v4)],
            Self::Ipv6 { v6 } => [Some(v6), None],
            Self::Ipv4 { v4 } => [None, Some(v4)],
            Self::Neither => [None, None],
        }
    }

    /// The first recorded resolve error (from either family) whose
    /// `kind()` is NOT `ErrorKind::Resolve`. Wrapping any resolve error
    /// (when `launched == 0`) in a fresh `Error::new(ErrorKind::Resolve,
    /// errs)` and not reading `errs` at all when `launched > 0` erases
    /// `ErrorKind::Cancelled` — the background thread pool shutting down
    /// before the resolve finished — indistinguishably from "this name
    /// doesn't resolve." `Cancelled` is the case that was found,
    /// but the rule is general: ANY `kind()` other than the `Resolve`
    /// this module synthesizes itself carries information the connector
    /// didn't produce and has no right to rename. Called BEFORE both
    /// failure branches in `drive`'s `HeAction::Exhausted`, so neither
    /// `AllAttemptsFailed` nor the synthetic `ErrorKind::Resolve` is
    /// reachable without going through this check — discarding becomes
    /// structurally impossible, not merely handled for the one case that
    /// was found.
    fn distinguishing_error(&self) -> Option<&Error> {
        self.recorded()
            .into_iter()
            .flatten()
            .find(|e| e.kind() != &ErrorKind::Resolve)
    }
}

/// The requested `HeConfig`'s `attempt_delay` is outside the RFC 8305
/// recommended range. `Scheduler::new` silently clamps such a
/// value, because its signature is fixed by the task's interface — `Self`,
/// not `Result`. THIS module's signature isn't fixed by anything, so here
/// it's a typed error rather than the same silent clamp two layers down.
#[derive(Debug, thiserror::Error)]
#[error(
    "attempt_delay {requested:?} is outside the RFC 8305 recommended range and would be \
     silently clamped to {effective:?}; pass a value inside the range instead"
)]
struct InvalidHeConfig {
    requested: Duration,
    effective: Duration,
}
/// Builds a [`Scheduler`], rejecting an out-of-range `attempt_delay` as a
/// typed error — instead of accepting `Scheduler::new`'s silent clamp
/// as-is.
///
/// Detected without knowing the `ATTEMPT_MIN`/`ATTEMPT_MAX` bounds at all
/// (they're private to `hclient_proto::happy_eyeballs` and shouldn't be
/// duplicated here as a second source of truth): the effective value is
/// read back through `Scheduler::config()` and compared against the
/// requested one — exactly the mechanism `Scheduler::new`'s doc comment
/// names directly ("the effective config can always be checked against
/// the requested one via `config()`"), just actually applied by the
/// caller, instead of being left to it as "could, but doesn't have to."
fn build_scheduler(cfg: HeConfig) -> Result<Scheduler, Error> {
    let requested = cfg.attempt_delay;
    let sched = Scheduler::new(cfg);
    let effective = sched.config().attempt_delay;
    if effective != requested {
        return Err(Error::new(
            ErrorKind::Connect,
            InvalidHeConfig {
                requested,
                effective,
            },
        ));
    }
    Ok(sched)
}

#[derive(Debug, thiserror::Error)]
#[error("request URI has no host to connect to")]
struct UriError;

/// The host from `uri`, regardless of scheme: a URI with no authority
/// (e.g., origin-form `/path`) is rejected right here, before the
/// question "which scheme" even comes up — there's no point asking a URI
/// with nowhere to connect to about TLS.
pub(crate) fn host(uri: &Uri) -> Result<&str, Error> {
    uri.host()
        .ok_or_else(|| Error::new(ErrorKind::Connect, UriError))
}

/// The port from `uri`, defaulted based on the ALREADY-checked scheme
/// (`use_tls` comes from [`wants_tls`], which alone is responsible for
/// rejecting an unsupported scheme) — `https` → 443, `http` → 80. Exactly
/// the same rule `hclient_proto::redirect::port_of` uses for the same
/// purpose: not imported directly from there (that function is private to
/// the `redirect` module), but it has to stay identical as a fact, not
/// just by coincidence — a divergence here would mean a redirect to
/// `https://a:443/` and the original connect to the same address see
/// different ports. Since the scheme is already constrained to
/// `http`/`https` on the way in, a default port always exists — there's
/// no separate "no port" error here anymore.
pub(crate) fn port(uri: &Uri, use_tls: bool) -> u16 {
    uri.port_u16().unwrap_or(if use_tls { 443 } else { 80 })
}

#[derive(Debug, thiserror::Error)]
#[error("unsupported URI scheme: {0:?}")]
struct UnsupportedScheme(String);

/// `true` — TLS is needed (`https`), `false` — plain TCP (`http`). Any
/// other (or missing) scheme is a typed `ErrorKind::Unsupported`, not a
/// silent treatment as `http`.
pub(crate) fn wants_tls(uri: &Uri) -> Result<bool, Error> {
    match uri.scheme_str() {
        Some("http") => Ok(false),
        Some("https") => Ok(true),
        other => Err(Error::new(
            ErrorKind::Unsupported,
            UnsupportedScheme(other.unwrap_or("").to_string()),
        )),
    }
}

/// Happy Eyeballs per RFC 8305, a single state machine for both of this
/// module's entry points (`connect` and `race_connect`) — see the module
/// doc comment for why both of them need exactly this, rather than
/// separate loops.
///
/// **No `spawn`**: attempts live in a `FuturesUnordered`, not in tasks —
/// `spawn` would require `Send + 'static` and would shut out
/// single-threaded runtimes (see the tests
/// `race_connect_never_requires_send_even_through_the_wait_path` and
/// `crates/hclient-native/tests/dual_runtime.rs`, which run this same
/// path through `smol` without a single scheduler thread).
///
/// # What it measures, and only when somebody is watching
///
/// `began` is the instant the caller's connect started — `None` when
/// `H::WATCHING` is `false`, which is how the whole of the timing work
/// below disappears from a build with no hook. The two figures it
/// produces are `Attempted::dns` and `Attempted::tcp`, and each is
/// measured where it happens rather than derived from the other: `dns`
/// ends at the first `HeAction::Start`, which is the first instant an
/// address existed to try, and `tcp` is the winning attempt's own
/// interval, stamped when *that* attempt was launched rather than when
/// the race was. On a staggered race the two differ by the whole
/// stagger, which is precisely the number a caller is trying to find.
async fn drive<R, V6, V4, H>(
    rt: &R,
    mut sched: Scheduler,
    v6_stream: V6,
    v4_stream: V4,
    port: u16,
    opts: &TcpOpts,
    began: Option<R::Instant>,
) -> Result<(R::Stream, Option<Box<Attempted>>), Error>
where
    R: TcpConnect + Timer,
    V6: Stream<Item = Result<ResolvedAddr, Error>>,
    V4: Stream<Item = Result<ResolvedAddr, Error>>,
    H: Hooks,
{
    /// What happened first while we were waiting (`HeAction::Wait`): an
    /// item arrived from one of the DNS streams (or the stream finished —
    /// `None`), one of the connection attempts completed, or the wait
    /// timed out.
    enum Event<T, I> {
        V6(Option<Result<ResolvedAddr, Error>>),
        V4(Option<Result<ResolvedAddr, Error>>),
        /// The address and the instant the attempt was launched travel
        /// with the attempt, because `FuturesUnordered` does not say
        /// which of its futures finished. Both are wanted: the address
        /// for `Connected::remote`, the instant for `Attempted::tcp`.
        Attempt(Option<(SocketAddr, Option<I>, std::io::Result<T>)>),
        TimedOut,
    }

    let mut v6_stream = std::pin::pin!(v6_stream);
    let mut v4_stream = std::pin::pin!(v4_stream);
    let mut v6_done = false;
    let mut v4_done = false;
    // Accumulated as two slots and folded into a `ResolveErrors` variant
    // only at the failure branch below: until then there is no single
    // answer to "which families failed", and a partially-filled error
    // value would be one.
    let mut v6_err: Option<Error> = None;
    let mut v4_err: Option<Error> = None;

    let start = rt.now();
    let mut attempts = FuturesUnordered::new();
    let mut launched = 0usize;
    // The DNS figure, settled at the first `HeAction::Start` and never
    // touched again: it is the wait before any address existed, so a
    // second address arriving later must not move it.
    let mut dns = Duration::ZERO;

    loop {
        let elapsed = rt.elapsed_since(start);
        match sched.poll(elapsed) {
            HeAction::Start(ip) => {
                if launched == 0 {
                    dns = since::<R>(rt, began);
                }
                launched += 1;
                let addr = SocketAddr::new(ip, port);
                // Stamped before the future is built, not inside it: a
                // future in a `FuturesUnordered` is not polled until the
                // collection is, and a mark taken on its first poll would
                // silently leave the scheduling delay out of `tcp` and
                // put it nowhere.
                let at = mark::<H, R>(rt);
                let connecting = rt.connect(addr, opts);
                attempts.push(async move { (addr, at, connecting.await) });
            }
            HeAction::Wait(d) => {
                let sleep_fut = rt.sleep(d);
                let mut sleep_fut = std::pin::pin!(sleep_fut);

                let ev = poll_fn(|cx| {
                    // Each source is polled only while it could, in
                    // principle, still produce a new event. A source that
                    // has already finished (a DNS stream returned `None`)
                    // or never started (`attempts` is empty) is skipped
                    // WITHOUT calling `poll` — not just to avoid violating
                    // the `Stream` contract ("don't poll after `None`"),
                    // but also to avoid repeating the brief's mistake in a
                    // different spot: polling an empty `FuturesUnordered`
                    // returns `Ready(None)` IMMEDIATELY (checked by
                    // reading `futures-util` 0.3.33,
                    // `stream/futures_unordered/mod.rs`: `is_terminated`
                    // starts out `false`, meaning an empty collection
                    // would be polled rather than skipped, and that
                    // `Ready` would win the race against a timer that
                    // hasn't fired yet, every round) — exactly the case
                    // the original `race_connect` brief explicitly
                    // guarded against by checking `attempts.is_empty()`
                    // before the race.
                    if !v6_done && let Poll::Ready(item) = v6_stream.as_mut().poll_next(cx) {
                        return Poll::Ready(Event::V6(item));
                    }
                    if !v4_done && let Poll::Ready(item) = v4_stream.as_mut().poll_next(cx) {
                        return Poll::Ready(Event::V4(item));
                    }
                    if !attempts.is_empty()
                        && let Poll::Ready(item) = Pin::new(&mut attempts).poll_next(cx)
                    {
                        return Poll::Ready(Event::Attempt(item));
                    }
                    if sleep_fut.as_mut().poll(cx).is_ready() {
                        return Poll::Ready(Event::TimedOut);
                    }
                    Poll::Pending
                })
                .await;

                match ev {
                    Event::V6(Some(Ok(addr))) => sched.offer_v6(&[addr.addr]),
                    Event::V6(Some(Err(e))) => v6_err = Some(e),
                    Event::V6(None) => {
                        v6_done = true;
                        sched.mark_v6_done();
                    }
                    Event::V4(Some(Ok(addr))) => sched.offer_v4(&[addr.addr]),
                    Event::V4(Some(Err(e))) => v4_err = Some(e),
                    Event::V4(None) => {
                        v4_done = true;
                        sched.mark_v4_done();
                    }
                    Event::Attempt(Some((remote, at, Ok(s)))) => {
                        return Ok((s, won::<H, R>(rt, remote, dns, at)));
                    }
                    Event::Attempt(Some((_, _, Err(_)))) => {
                        // One attempt failed — no reason to stop the rest
                        // of the race; `Scheduler` itself decides whether
                        // to start another one on the next `poll`.
                    }
                    Event::Attempt(None) => {
                        unreachable!(
                            "poll_next on attempts is only polled when attempts is \
                             non-empty (see the `!attempts.is_empty()` check above), and \
                             FuturesUnordered never returns None for a non-empty collection"
                        );
                    }
                    Event::TimedOut => {}
                }
            }
            HeAction::Exhausted => {
                while let Some((remote, at, res)) = attempts.next().await {
                    if let Ok(s) = res {
                        return Ok((s, won::<H, R>(rt, remote, dns, at)));
                    }
                }
                let errs = ResolveErrors::from_families(v6_err, v4_err);
                // Checked BEFORE both branches below, not as a special
                // case inside one of them — so
                // discarding a differing kind() (in particular,
                // ErrorKind::Cancelled) becomes structurally unreachable,
                // not merely handled for the one case that was found.
                // Returns a CLONE of the original resolver error as-is —
                // without re-wrapping it in Error::new — because it
                // already carries its correct kind() and its own
                // source() chain; wrapping it again would repeat the same
                // categorization mistake in a different way.
                if let Some(distinguishing) = errs.distinguishing_error() {
                    return Err(distinguishing.clone());
                }
                if launched == 0 {
                    // Not a single TCP attempt happened — meaning there
                    // was nothing to try, not that every attempt failed.
                    return Err(Error::new(ErrorKind::Resolve, errs));
                }
                return Err(Error::new(ErrorKind::Connect, AllAttemptsFailed(launched)));
            }
        }
    }
}

/// Happy Eyeballs over an already-resolved address list — a DNS-free
/// primitive. "Resolution Delay" is unreachable in this call not by
/// oversight but structurally: the caller already knows both lists in
/// full, there's genuinely nothing to wait for, and `stream::iter` below
/// honestly reflects that fact (a stream that yields everything on the
/// first poll), not a workaround pretending to be a stream. For real
/// feed-as-it-arrives behavior, see [`connect`] and the module doc
/// comment.
///
/// It reports no timings and takes no hook: every caller is a test in
/// this file, and what a `NoHooks` instantiation here is good for is
/// proving that `drive` still compiles when nobody is watching.
pub(crate) async fn race_connect<R>(
    rt: &R,
    addrs_v6: Vec<IpAddr>,
    addrs_v4: Vec<IpAddr>,
    port: u16,
    opts: &TcpOpts,
    he: HeConfig,
) -> Result<R::Stream, Error>
where
    R: TcpConnect + Timer,
{
    let sched = build_scheduler(he)?;
    let v6 = futures_util::stream::iter(
        addrs_v6
            .into_iter()
            .map(|addr| Ok(ResolvedAddr { addr, ttl: None })),
    );
    let v4 = futures_util::stream::iter(
        addrs_v4
            .into_iter()
            .map(|addr| Ok(ResolvedAddr { addr, ttl: None })),
    );
    drive::<_, _, _, NoHooks>(rt, sched, v6, v4, port, opts, None)
        .await
        .map(|(stream, _)| stream)
}

/// The attempt that won, or `None` when nobody is watching.
///
/// A function rather than a struct literal at the two `return`s, so the
/// three figures are named once and `tcp` cannot silently become `dns`'s
/// twin.
fn won<H: Hooks, R: Timer>(
    rt: &R,
    remote: SocketAddr,
    dns: Duration,
    launched: Option<R::Instant>,
) -> Option<Box<Attempted>> {
    H::WATCHING.then(|| {
        Box::new(Attempted {
            remote: Some(remote),
            dns,
            tcp: since::<R>(rt, launched),
            // Filled in by `attempt`, which is where the handshake is.
            tls: None,
        })
    })
}

/// Everything one [`attempt`] learned about the connection it made — and
/// `None` all the way up when `H::WATCHING` is `false`.
///
/// **Boxed, and that is what the `Option` is for.** `None` costs eight
/// bytes and no allocation, so a request that wants none of this carries
/// a pointer rather than eighty bytes through four nested `async fn`s;
/// `Some` costs one allocation per *connection*, set against a TCP
/// handshake.
///
/// A struct rather than a tuple because three of its four fields are
/// durations and swapping two of them would compile — which is exactly
/// the mutation "a phase duration measured from the wrong start", made
/// unavailable by naming rather than caught after the fact.
#[derive(Debug)]
pub(crate) struct Attempted {
    /// `None` for a connection with no IP address — a Unix-domain socket.
    /// See [`hclient_core::unversioned::Connected::remote`], which this
    /// field feeds and which carries the argument.
    pub(crate) remote: Option<SocketAddr>,
    pub(crate) dns: Duration,
    pub(crate) tcp: Duration,
    /// `None` for a plaintext connection and `Some` for one that
    /// handshook, so the distinction reaches
    /// [`ConnectTiming::tls`](hclient_core::unversioned::ConnectTiming::tls)
    /// as a fact rather than as a zero.
    pub(crate) tls: Option<Duration>,
}

/// One address family's answers for one call to [`connect`]: asked once,
/// replayed for every attempt that needs them.
///
/// # Started early, which is what "in parallel" actually means here
///
/// A `Resolve` stream is inert until it is polled — `lookup_ipv4` builds a
/// query, it does not send one. So running the HTTPS query beside the
/// address queries is not a matter of constructing all three and awaiting
/// the first; it is a matter of **polling** the address streams while the
/// record is still outstanding. [`pump`](Self::pump) is that poll, and
/// [`alongside_address_lookups`] is where it is called from.
///
/// # Kept, because the retry must not resolve a second time
///
/// [`connect`] makes up to two attempts — through the record, then on the
/// origin's own terms. Three shapes were available for the addresses the
/// second one needs, and this type is the third:
///
/// - **re-fetch**, which is what the code did while `attempt` owned its
///   own resolution: a second query on any resolver that does not cache,
///   and `hclient-dns-system` caches nothing of its own. The retry exists
///   so that a stale record costs a connect; making it cost a round trip
///   as well is the very expense this type was added to remove.
/// - **collect both families into `Vec`s** and hand those to both
///   attempts. This is the shape the module doc rules out in its opening
///   paragraph: `Scheduler` would then never be in the state "AAAA has not
///   arrived and the resolver is not done either", and RFC 8305's
///   Resolution Delay would be dead code.
/// - **replay** — what has already arrived is handed over at once, what
///   has not is still awaited item by item, and the resolver's stream is
///   polled exactly as many times as one attempt would have polled it.
///
/// The buffer holds one resolution's worth of answers (a `getaddrinfo`
/// reply, in the shipped resolver) and lives for one `connect` call.
struct Answers<S> {
    /// Boxed rather than held by value: this crate forbids `unsafe`, and
    /// projecting a pin into an `impl Stream` field cannot be written
    /// without it. One allocation per family per new connection, set
    /// against a DNS query.
    ///
    /// **`Pin<Box<S>>` and not `Pin<Box<dyn Stream<..>>>`, and the
    /// difference is one auto trait.** The allocation is the same and the
    /// absence of `unsafe` is the same; what changes is that the concrete
    /// stream's type is no longer thrown away, so `Send` reaches
    /// `Native::execute`'s future instead of stopping here. A `dyn` with
    /// no declared auto traits is not neutral — it *removes* them, from
    /// every resolver that had them. Declaring `+ Send` on the `dyn`
    /// instead would have been the other direction: it obliges the seam,
    /// and `Resolve::lookup_ipv4` returns `impl Stream`, which cannot be
    /// named and so cannot be bounded — measured, along with what
    /// converting the seam would cost. See `tests/send_future.rs`.
    inner: Pin<Box<S>>,
    /// Everything the stream has produced, in order — items **and**
    /// errors, because [`drive`] tells a family that failed apart from one
    /// that answered nothing (`ResolveErrors`), and a replay that dropped
    /// the errors would give the second attempt a different diagnosis from
    /// the first.
    seen: Vec<Result<ResolvedAddr, Error>>,
    /// Set once the stream has returned `None`. Polling past that is a
    /// `Stream` contract violation, and both [`pump`](Self::pump) and
    /// [`Replay`] come back for more by design.
    done: bool,
}

impl<S: Stream<Item = Result<ResolvedAddr, Error>>> Answers<S> {
    fn new(stream: S) -> Self {
        Self {
            inner: Box::pin(stream),
            seen: Vec::new(),
            done: false,
        }
    }

    /// Takes whatever the resolver has ready, and leaves a waker behind for
    /// the rest.
    ///
    /// Returns nothing on purpose: nobody here is waiting for a
    /// resolution. This exists so the query is in flight while [`connect`]
    /// waits for the HTTPS record; what it collects is read afterwards, by
    /// [`replay`](Self::replay).
    fn pump(&mut self, cx: &mut Context<'_>) {
        while !self.done {
            match self.inner.as_mut().poll_next(cx) {
                Poll::Ready(Some(item)) => self.seen.push(item),
                Poll::Ready(None) => self.done = true,
                Poll::Pending => return,
            }
        }
    }

    /// This family from its first answer: what has arrived, then whatever
    /// the resolver still has to say.
    fn replay(&mut self) -> Replay<'_, S> {
        Replay { src: self, at: 0 }
    }
}

/// One reader of an [`Answers`], starting at its first item.
///
/// A borrow rather than a copy of the buffer, because the two attempts in
/// [`connect`] are strictly sequential: there is never more than one of
/// these alive, and the borrow is what says so. A reader that could
/// outlive its source would be a second resolution wearing this name.
struct Replay<'r, S> {
    src: &'r mut Answers<S>,
    at: usize,
}

impl<S: Stream<Item = Result<ResolvedAddr, Error>>> Stream for Replay<'_, S> {
    type Item = Result<ResolvedAddr, Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let me = self.get_mut();
        if me.at < me.src.seen.len() {
            let item = me.src.seen[me.at].clone();
            me.at += 1;
            return Poll::Ready(Some(item));
        }
        if me.src.done {
            return Poll::Ready(None);
        }
        match me.src.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(item)) => {
                me.src.seen.push(item.clone());
                me.at = me.src.seen.len();
                Poll::Ready(Some(item))
            }
            Poll::Ready(None) => {
                me.src.done = true;
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

/// `discovery`'s answer, with both address lookups running underneath it.
///
/// The one place where the three queries are concurrent, and it is a
/// hand-written `poll_fn` rather than a `join!` because the address
/// streams are not being *awaited*: nothing here wants their answers yet,
/// only that they are on the wire. So [`Answers::pump`] returns nothing,
/// and this future finishes when `discovery` does — however far along the
/// addresses happen to be, which for a record that arrives first is not far
/// at all.
///
/// **Nothing is spawned, so a dropped connect drops all three queries.**
/// They are branches of one future rather than tasks; this crate has no
/// `Spawn` to hand them to, and a discovery query still running after the
/// request that asked for it is the leak that would be.
async fn alongside_address_lookups<F, S6, S4>(
    v6: &mut Answers<S6>,
    v4: &mut Answers<S4>,
    discovery: F,
) -> F::Output
where
    F: Future,
    S6: Stream<Item = Result<ResolvedAddr, Error>>,
    S4: Stream<Item = Result<ResolvedAddr, Error>>,
{
    let mut discovery = std::pin::pin!(discovery);
    poll_fn(move |cx| {
        // Before `discovery`, and on every round. The addresses must be
        // asked no later than the record is, and a stream nobody polls has
        // not been asked.
        v6.pump(cx);
        v4.pump(cx);
        discovery.as_mut().poll(cx)
    })
    .await
}

/// DNS-consuming connector: consults the origin's HTTPS record where one
/// is to be had (see [`crate::discovery`]), resolves `uri`, runs Happy
/// Eyeballs (feeding [`Scheduler`] as results arrive — see the module doc
/// comment), then optionally runs a TLS handshake with the negotiated ALPN
/// offer. `uri`'s scheme decides whether TLS is needed at all (`https` —
/// yes, `http` — no); any other scheme is `ErrorKind::Unsupported`, not a
/// silent treatment as `http`.
///
/// # The record and the addresses are asked at once
///
/// RFC 9460 §10.3, and it is where this function's cost hides: awaiting
/// the HTTPS query *in front of* the address queries makes every new
/// connection pay one round trip before it starts resolving, on a resolver
/// that answers SVCB — `SystemDns` on Linux does.
///
/// Nothing about an address depends on the record. This connector does not
/// resolve a record's target name (`discovery::lookup` says why), so the
/// origin's own A/AAAA answers are the same answers whatever comes back;
/// what *does* depend on the record — the port, the hints, the ALPN offer,
/// the ECH slot — is needed when a socket is opened, not when a name is
/// resolved. So all three queries go out together and this function waits
/// for whichever it actually needs next.
///
/// The two halves are not merely constructed together, they are **polled**
/// together — see [`Answers`] for why that distinction is the whole
/// mechanism — and neither is spawned, so a dropped connect drops all
/// three.
///
/// # The record may already have been fetched, and then it is not fetched
/// again
///
/// `prefetched` is [`Prefetched::NotConsulted`] for every caller that has
/// not asked, which is the behaviour this function had before the
/// parameter existed. A caller that *has* asked — through
/// [`crate::Prefetch::prepare`], for **this request's own authority**, with
/// this transport's own resolver and this transport's own negative cache —
/// hands the answer over instead, and no query goes out here.
///
/// The two states of "already asked" are both carried: `Looked(Some(..))`
/// and `Looked(None)` are different instructions, and conflating the
/// second with "not asked" would re-query precisely the origins that
/// publish nothing (see [`Prefetched`]).
///
/// # The record is consulted once, and its failure is paid for once
///
/// A discovered endpoint moves the connection: a different port, a
/// different set of addresses to start from. If that connection fails,
/// this function tries again **on the origin's own terms** — no port
/// override, no hints, no ALPN restriction, no ECH — and marks the origin
/// in `discovery` so the next request skips discovery altogether for
/// [`crate::SVCB_FAILURE_TTL`]. Without the retry an origin with a stale
/// record would be unreachable rather than slow; without the mark, every
/// request would pay the failed attempt again.
///
/// **Both attempts share one `Timeouts::connect` budget**, because that
/// deadline wraps this whole future exactly once (`crate::
/// with_connect_timeout`). A retry that could double a caller's bound
/// would not be a bound.
///
/// **The error the caller sees is the second attempt's**, and that is the
/// one about the origin: the first attempt went to an endpoint the caller
/// never named, chosen by this transport from a DNS record, and its
/// failure is recorded in the cache rather than reported as the answer to
/// a request that was still able to proceed. When there was no record, or
/// when it contributed nothing, there is only ever one attempt and one
/// error.
///
/// # The two attempts do not share a DNS figure
///
/// `began` below is taken once and given to the first attempt, whose
/// `dns` therefore covers the HTTPS record *and* the address answers —
/// which is the wait the caller actually experienced. The retry gets a
/// fresh mark, because its addresses are already in hand ([`Answers`]
/// replays them) and measuring its `dns` from the top would report the
/// whole of the failed first attempt as time spent in DNS. Neither
/// figure is a share of a total: see
/// [`ConnectTiming`](hclient_core::unversioned::ConnectTiming), which
/// says so where a caller reads it.
/// Everything a proxy changes, in one place.
///
/// The origin's **name** goes to the proxy and its addresses are never
/// looked up here — an HTTP proxy resolves it from the `CONNECT` target, a
/// SOCKS5 one from `ATYP=0x03 DOMAINNAME`. Happy Eyeballs still runs, over
/// the proxy's own addresses, so a dual-stack proxy is reached the same
/// way a dual-stack origin is.
///
/// Discovery does not run at all: an HTTPS record's address hints name an
/// address nobody will dial, and its port would move the connection
/// somewhere the proxy was not asked about. `Prefetched` is not a
/// parameter here for that reason — there is nothing for it to be.
async fn through_proxy<R, L, P, H>(
    rt: &R,
    dns: &(impl Resolve + ?Sized),
    tls: &L,
    proxy: &crate::proxy::Proxy<P>,
    host: &str,
    use_tls: bool,
    port: u16,
    opts: &TcpOpts,
    alpn: &[&[u8]],
    began: Option<R::Instant>,
) -> Result<
    (
        Conn<R::Stream, L::Stream<R::Stream>>,
        Option<TlsInfo>,
        Option<Box<Attempted>>,
    ),
    Error,
>
where
    R: TcpConnect + Timer,
    L: TlsConnect,
    P: crate::proxy::Handshake + Clone,
    R::Stream: 'static,
    H: Hooks,
{
    let mut v6 = Answers::new(dns.lookup_ipv6(proxy.host()));
    let mut v4 = Answers::new(dns.lookup_ipv4(proxy.host()));
    let sched = build_scheduler(HeConfig::default())?;
    let (tcp, mut attempted) = drive::<_, _, _, H>(
        rt,
        sched,
        v6.replay(),
        v4.replay(),
        proxy.port(),
        opts,
        began,
    )
    .await?;

    let tcp = match proxy.protocol().approach(use_tls) {
        // Nothing to negotiate: an HTTP proxy serving `http://` is an
        // origin server for this request, and what changes is the request
        // line, one layer up.
        crate::proxy::Approach::Absolute => tcp,
        crate::proxy::Approach::Tunnel => {
            // A fresh state machine per connection: the configured
            // protocol is a template — credentials and options — and
            // running a handshake mutates it.
            let mut handshake = proxy.handshake();
            let mut stream = tcp;
            let read_buf = crate::proxy::drive(&mut stream, &mut handshake, host, port).await?;
            // **It must be empty, and this is a check rather than a
            // rewind.** We have not written a byte to the origin, so
            // nothing it might answer can have arrived; anything past the
            // end of the tunnel handshake was invented by the proxy. A
            // `Rewind`-shaped wrapper would carry those bytes into the TLS
            // handshake or into hyper as if the origin had sent them,
            // which is the quieter of the two failures and the worse one.
            if !read_buf.is_empty() {
                return Err(Error::new(
                    ErrorKind::Connect,
                    crate::proxy::ProxySpokeFirst(read_buf.len()),
                ));
            }
            stream
        }
    };

    if !use_tls {
        return Ok((Conn::Plain(tcp), None, attempted));
    }

    // The **origin's** name, never the proxy's: the tunnel is transport,
    // and a certificate is checked against who the caller asked for.
    let req = TlsRequest {
        server_name: hclient_core::bare_host(host),
        alpn,
        // No record was consulted, so there is nothing to apply — see this
        // function's doc comment.
        ech: None,
        early_data: None,
    };
    let handshake_began = mark::<H, R>(rt);
    let (stream, info) = tls.connect(tcp, req).await?;
    if let Some(a) = attempted.as_mut() {
        a.tls = Some(since::<R>(rt, handshake_began));
    }
    Ok((Conn::Tls(stream), Some(info), attempted))
}

/// The TLS half of a Unix-domain connect, which is
/// [`through_proxy`]'s tail and nothing else.
///
/// Split out rather than inlined so the two paths that reach TLS without
/// Happy Eyeballs cannot drift: both must use the **origin's** name, and
/// both hand back an `Attempted` with no address in it — there is no
/// address, which is exactly what the caller should see rather than a
/// fabricated one.
async fn finish_unix<R, L, H>(
    rt: &R,
    tls: &L,
    stream: R::Stream,
    host: &str,
    use_tls: bool,
    alpn: &[&[u8]],
    began: Option<R::Instant>,
) -> Result<
    (
        Conn<R::Stream, L::Stream<R::Stream>>,
        Option<TlsInfo>,
        Option<Box<Attempted>>,
    ),
    Error,
>
where
    R: TcpConnect + Timer,
    L: TlsConnect,
    R::Stream: 'static,
    H: Hooks,
{
    // An `Attempted` **is** produced — `None` here would mean no
    // `Connected` event, and then a `Closed` announcing the end of a
    // connection whose beginning was never announced. `remote` is `None`
    // inside it, which is the honest absence: there is no address. `dns`
    // is zero because nothing was resolved, and `tcp` is the whole of the
    // connect, since `connect_unix` is the only thing that dialled.
    let mut attempted = began.map(|b| {
        Box::new(Attempted {
            remote: None,
            dns: Duration::ZERO,
            tcp: since::<R>(rt, Some(b)),
            tls: None,
        })
    });
    if !use_tls {
        return Ok((Conn::Plain(stream), None, attempted));
    }
    // The name from the URI, because a certificate is checked against who
    // the caller asked for — the socket is transport, exactly as a tunnel
    // is.
    let req = TlsRequest {
        server_name: hclient_core::bare_host(host),
        alpn,
        ech: None,
        early_data: None,
    };
    let handshake_began = mark::<H, R>(rt);
    let (tls_stream, info) = tls.connect(stream, req).await?;
    if let Some(a) = attempted.as_mut() {
        a.tls = Some(since::<R>(rt, handshake_began));
    }
    Ok((Conn::Tls(tls_stream), Some(info), attempted))
}

pub(crate) async fn connect<R, D, L, P, H>(
    rt: &R,
    dns: &D,
    tls: &L,
    proxies: &[crate::proxy::Proxy<P>],
    unix_socket: Option<&std::path::Path>,
    uri: &Uri,
    opts: &TcpOpts,
    alpn: &[&[u8]],
    discovery_cache: &NegativeCache,
    now: Duration,
    prefetched: Prefetched,
    resolve: Option<Duration>,
) -> Result<
    (
        Conn<R::Stream, L::Stream<R::Stream>>,
        Option<TlsInfo>,
        Option<Box<Attempted>>,
    ),
    Error,
>
where
    R: TcpConnect + Timer,
    D: Resolve,
    L: TlsConnect,
    P: crate::proxy::Handshake + Clone,
    R::Stream: 'static,
    H: Hooks,
{
    let began = mark::<H, R>(rt);
    let host = host(uri)?;
    let use_tls = wants_tls(uri)?;
    let port = port(uri, use_tls);

    // Before the resolver and before discovery, because a proxy replaces
    // both rather than layering over them. `prefetched` is not consulted
    // and `discovery_cache` is not touched: whatever either says is about
    // an origin this connection will not dial.
    // `serves` is asked here rather than at `Native::proxy`, because a
    // bypassed origin must take the ordinary path in full — its resolver,
    // its discovery, its Happy Eyeballs — rather than a proxied path with
    // the proxy removed.
    // **Before the proxy and before everything else**, because a Unix
    // socket replaces the whole resolve → discovery → Happy Eyeballs →
    // connect block rather than layering over it: there is no name to
    // resolve, no family to race and no port. `Native::unix_socket`
    // refuses to coexist with a proxy, so the order between these two is a
    // statement about reading rather than a precedence anybody has to
    // learn.
    //
    // There is no bypass list here and there should not be: a bypassed
    // origin would have nowhere to go, since the whole point is that this
    // process reaches the service only through this socket.
    if let Some(path) = unix_socket {
        let stream = rt
            .connect_unix(path)
            .await
            .map_err(|e| Error::new(ErrorKind::Connect, e))?;
        return finish_unix::<R, L, H>(rt, tls, stream, host, use_tls, alpn, began).await;
    }

    // The list is walked here rather than by the caller, and the bypass
    // with it, so a bypassed origin takes the ordinary path *in full* —
    // its resolver, its discovery, its Happy Eyeballs — rather than a
    // proxied path with the proxy removed.
    if let Some(proxy) = crate::proxy::Proxy::choose(proxies, use_tls, host, port) {
        return through_proxy::<R, L, P, H>(
            rt, dns, tls, proxy, host, use_tls, port, opts, alpn, began,
        )
        .await;
    }

    // Both families are started here, at the top, rather than inside
    // `attempt` where they used to live — that placement is the whole of
    // the parallelism, and it is also what leaves the retry below with
    // nothing to re-resolve. See `Answers`.
    let mut v6 = Answers::new(dns.lookup_ipv6(host));
    let mut v4 = Answers::new(dns.lookup_ipv4(host));

    let found = match prefetched {
        // Nobody asked, so this connector asks — and the query goes out
        // beside the address lookups rather than in front of them, which
        // is what `alongside_address_lookups` is for.
        Prefetched::NotConsulted => {
            alongside_address_lookups(
                &mut v6,
                &mut v4,
                discovered_endpoint(dns, host, use_tls, port, discovery_cache, now),
            )
            .await
        }
        // Already asked, for this request's own authority. No second
        // query — and no second chance to ask, because `Looked(None)` is
        // an answer.
        already => already,
    };
    let endpoint = found.actionable();

    // **`Timeouts::resolve`, and it is applied here rather than around
    // anything.** Happy Eyeballs interleaves resolution with connecting on
    // purpose — the streams above are consumed lazily by `attempt` — so
    // there is no instant at which resolution finished for a bound to
    // attach to. What is bounded is the wait for the first address from
    // either family, which is the failure a caller cannot otherwise
    // diagnose: a resolver that hangs is indistinguishable from an origin
    // that will not answer, and only the first is worth a different retry.
    //
    // Nothing is serialised by it. `attempt` cannot connect before an
    // address exists, so this waits for what the next line would wait for
    // anyway; what changes is the error, not the schedule.
    if let Some(bound) = resolve {
        first_address_within::<R, _, _>(rt, bound, &mut v6, &mut v4, endpoint.as_ref()).await?;
    }

    let first = attempt::<R, L, H, _, _>(
        rt,
        tls,
        host,
        use_tls,
        port,
        opts,
        alpn,
        &mut v6,
        &mut v4,
        endpoint.as_ref(),
        began,
    )
    .await;
    match first {
        Ok(conn) => Ok(conn),
        // The discovered endpoint's own failure ends here on purpose: it
        // is recorded in the cache, and the answer to the caller is
        // whatever the origin says next. See this function's doc comment.
        Err(_through_the_record) if endpoint.is_some() => {
            discovery_cache.record(Origin::new(host, port), now);
            let retry_began = mark::<H, R>(rt);
            attempt::<R, L, H, _, _>(
                rt,
                tls,
                host,
                use_tls,
                port,
                opts,
                alpn,
                &mut v6,
                &mut v4,
                None,
                retry_began,
            )
            .await
        }
        Err(e) => Err(e),
    }
}

/// The HTTPS record this connection may act on, with every reason it may
/// not in one place.
///
/// Each condition is `crate::discovery`'s to justify (its module doc has
/// them: the `http://` upgrade rule, the prefixed name a non-default port
/// needs, and the negative cache); what belongs here is that they are
/// checked **before** the lookup rather than after it. A suppressed origin
/// or a non-default port must not cost a DNS query whose answer is then
/// thrown away — that would be the cost of discovery without any of its
/// effect.
///
/// A record that contributes nothing (`Endpoint::is_inert`) is dropped to
/// `None` by [`Prefetched::actionable`], so that the caller's "was a
/// record in play" test is the same question as "could this attempt have
/// gone differently".
///
/// **What it returns is three-valued, and this is the only place the three
/// are told apart.** A condition that stopped the lookup is
/// [`Prefetched::NotConsulted`] — nobody has an answer, and anyone who
/// wants one must ask elsewhere; a lookup that happened is
/// `Looked(..)`, whatever it found. [`crate::Prefetch::prepare`] calls this
/// function rather than a copy of it, so the rule about where discovery
/// applies is written once and cannot drift between the two callers.
pub(crate) async fn discovered_endpoint<D>(
    dns: &D,
    host: &str,
    use_tls: bool,
    port: u16,
    cache: &NegativeCache,
    now: Duration,
) -> Prefetched
where
    D: Resolve,
{
    if !use_tls || port != HTTPS_DEFAULT_PORT {
        return Prefetched::NotConsulted;
    }
    if cache.suppressed(&Origin::new(host, port), now) {
        return Prefetched::NotConsulted;
    }
    // The capability, asked rather than inferred from an empty stream —
    // the distinction `Resolve::supports_svcb` exists to carry (a resolver
    // that cannot ask, and one that asked and found nothing, both return
    // an empty stream, and only the first should stop us asking).
    //
    // **`NotConsulted` and not `Looked(None)`**, and the difference leaves
    // this crate: to the connection they are the same thing, but a caller
    // holding the answer is told *nobody looked* rather than *there is no
    // record* — so a caller whose own resolver can ask still asks. This
    // transport's resolver being unable to answer is not a fact about the
    // origin.
    if !dns.supports_svcb() {
        return Prefetched::NotConsulted;
    }
    Prefetched::Looked(discovery::lookup(dns, host).await)
}

/// The port `https` means when a URI does not say otherwise — and the only
/// port at which the HTTPS record fetched for a bare origin name applies
/// (RFC 9460 §9.5, see [`crate::discovery`]).
pub(crate) const HTTPS_DEFAULT_PORT: u16 = 443;

/// Waits for the first address either family produces, or fails naming
/// the resolver.
///
/// # What it does not wait for
///
/// **Nothing, where the connection does not need it.** An HTTPS record
/// carrying address hints gives `attempt` somewhere to go with no resolver
/// answer at all, so waiting for one would bound a query whose result is
/// not on the path — the same reasoning that keeps discovery from running
/// for an IP literal one layer up.
///
/// # When it stops waiting without an address
///
/// When **both** families are done. A resolver that failed, and one that
/// honestly found nothing, are told apart by `drive`'s `ResolveErrors`,
/// which has the per-family causes this function does not; producing a
/// timeout here for a resolver that already answered *no* would replace a
/// precise diagnosis with a vague one.
async fn first_address_within<R, S6, S4>(
    rt: &R,
    bound: Duration,
    v6: &mut Answers<S6>,
    v4: &mut Answers<S4>,
    endpoint: Option<&Endpoint>,
) -> Result<(), Error>
where
    R: Timer,
    S6: Stream<Item = Result<ResolvedAddr, Error>>,
    S4: Stream<Item = Result<ResolvedAddr, Error>>,
{
    // Hints are addresses. `is_inert` has already dropped a record that
    // contributes nothing, so a `Some` here with hints really is somewhere
    // to go.
    if endpoint.is_some_and(|e| !e.ipv6hint.is_empty() || !e.ipv4hint.is_empty()) {
        return Ok(());
    }
    let mut sleep = core::pin::pin!(rt.sleep(bound));
    core::future::poll_fn(|cx| {
        v6.pump(cx);
        v4.pump(cx);
        // An `Ok` from either family is an address to try. An `Err` is
        // not: a family that failed leaves the other one still worth
        // waiting for, and if both fail `done` below ends the wait.
        fn any<S>(a: &Answers<S>) -> bool {
            a.seen.iter().any(Result::is_ok)
        }
        if any(v6) || any(v4) || (v6.done && v4.done) {
            return Poll::Ready(Ok(()));
        }
        match sleep.as_mut().poll(cx) {
            Poll::Ready(()) => Poll::Ready(Err(Error::new(
                ErrorKind::Timeout(hclient_core::Phase::Resolve),
                ResolveTimedOut(bound),
            ))),
            Poll::Pending => Poll::Pending,
        }
    })
    .await
}

/// The failure `first_address_within` ends in.
///
/// A named type rather than a string, for the reason
/// [`crate::FirstByteTimedOut`] is one: a caller tells the phases apart
/// with `Error::source().downcast_ref()`, and the point of this bound is
/// that *"DNS is broken"* and *"the origin is unreachable"* stop looking
/// alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("no address from the resolver within the resolve timeout of {0:?}")]
pub struct ResolveTimedOut(pub Duration);

/// One connection attempt: Happy Eyeballs over `endpoint`'s hints followed
/// by the origin's own addresses, then TLS if the scheme asks for it.
///
/// **It has no resolver, and that is deliberate.** The addresses arrive as
/// two [`Answers`] that [`connect`] has already asked for, so "the retry
/// does not resolve a second time" is a property of this signature rather
/// than a rule someone has to keep in mind. Until the queries were made
/// concurrent this function called `lookup_ipv6`/`lookup_ipv4` itself,
/// which is exactly why nothing could resolve until the HTTPS query had
/// finished.
///
/// `endpoint: None` is what this function did before v0.3 W2, and it is
/// the shape the retry above uses — the "without the record" path is the
/// same code with the record absent, not a second implementation of
/// connecting.
///
/// **The hints come first and the resolver's answers follow**, rather than
/// replacing them: RFC 9460 §10.3 has the hints as a way to start
/// connecting before the address queries return, not as an alternative to
/// asking. Chaining them onto the front of each family's stream is
/// literally that — `Scheduler` gets the hint on its first poll, and the
/// A/AAAA answers as they arrive, through the same state machine and with
/// the same pacing rules as any other address. Some of them may have
/// arrived already, while the record was still outstanding; the chain is
/// what keeps the hint in front of them anyway.
async fn attempt<R, L, H, S6, S4>(
    rt: &R,
    tls: &L,
    host: &str,
    use_tls: bool,
    port: u16,
    opts: &TcpOpts,
    alpn: &[&[u8]],
    v6: &mut Answers<S6>,
    v4: &mut Answers<S4>,
    endpoint: Option<&Endpoint>,
    began: Option<R::Instant>,
) -> Result<
    (
        Conn<R::Stream, L::Stream<R::Stream>>,
        Option<TlsInfo>,
        Option<Box<Attempted>>,
    ),
    Error,
>
where
    R: TcpConnect + Timer,
    L: TlsConnect,
    H: Hooks,
    S6: Stream<Item = Result<ResolvedAddr, Error>>,
    S4: Stream<Item = Result<ResolvedAddr, Error>>,
{
    let sched = build_scheduler(HeConfig::default())?;
    // RFC 9460 §7.2: the record's port replaces the scheme's default for
    // every attempt in this race, hints and resolved addresses alike —
    // there is one port per race (see `drive`), and an endpoint that named
    // one named it for the service, not for some of its addresses.
    let port = endpoint.and_then(|e| e.port).unwrap_or(port);
    let hint6 = endpoint.map(|e| e.ipv6hint.clone()).unwrap_or_default();
    let hint4 = endpoint.map(|e| e.ipv4hint.clone()).unwrap_or_default();

    let v6_stream = futures_util::stream::iter(hint6.into_iter().map(|a| {
        Ok(ResolvedAddr {
            addr: IpAddr::V6(a),
            ttl: None,
        })
    }))
    .chain(v6.replay());
    let v4_stream = futures_util::stream::iter(hint4.into_iter().map(|a| {
        Ok(ResolvedAddr {
            addr: IpAddr::V4(a),
            ttl: None,
        })
    }))
    .chain(v4.replay());

    let (tcp, mut attempted) =
        drive::<_, _, _, H>(rt, sched, v6_stream, v4_stream, port, opts, began).await?;

    if use_tls {
        // Held outside the `TlsRequest` so the borrowed list outlives it.
        let restricted = endpoint.map(|e| discovery::alpn_offer(alpn, &e.alpn));
        let req = TlsRequest {
            // The one place in this crate where a URI's host stops being
            // URI syntax and becomes a name. `host` is `Uri::host()`'s
            // answer, so an IPv6 literal still wears the brackets RFC 3986
            // §3.2.2 gives the *authority*; `ServerName::try_from` reads
            // them as neither a DNS name nor an address and every
            // `https://[…]/` request failed at the handshake. The duty is
            // the caller's rather than the backend's, and
            // `TlsRequest::server_name`'s own doc is where that is argued.
            //
            // Not stripped anywhere else on purpose: the `Host` header and
            // h2's `:authority` (`established.rs`, `websocket.rs`) are
            // authority syntax and keep their brackets.
            server_name: hclient_core::bare_host(host),
            alpn: restricted.as_deref().unwrap_or(alpn),
            // The whole of the ECH decision, and the reason it is a
            // question rather than an assignment: see the module doc.
            // `applies_ech()` is `false` for every backend here today, so
            // this is `None` today — but it is `None` because a backend
            // said it would not use one, not because nobody asked.
            ech: endpoint
                .filter(|_| tls.applies_ech())
                .and_then(|e| e.ech.as_deref()),
            // Reserved, not used — see `hclient_tls::TlsRequest::
            // early_data`, which explains what a transport has to settle
            // before it may ask for 0-RTT, and why none of that is v0.2's.
            early_data: None,
        };
        // The handshake and nothing else between these two marks: the
        // stream it wraps is already connected, and whatever the caller
        // does with the result afterwards is not TLS.
        let handshake_began = mark::<H, R>(rt);
        let (stream, info) = tls.connect(tcp, req).await?;
        if let Some(a) = attempted.as_mut() {
            a.tls = Some(since::<R>(rt, handshake_began));
        }
        Ok((Conn::Tls(stream), Some(info), attempted))
    } else {
        // `tls` stays `None`, which is not `Some(Duration::ZERO)`: there
        // was no handshake, and a zero would read as an instant one.
        Ok((Conn::Plain(tcp), None, attempted))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::error::Error as StdError;
    use std::net::{Ipv4Addr, Ipv6Addr};
    use std::rc::Rc;
    use std::sync::Arc;
    use std::sync::OnceLock;
    use std::sync::atomic::Ordering;

    fn v6(n: u16) -> IpAddr {
        IpAddr::V6(Ipv6Addr::new(0x20, 0, 0, 0, 0, 0, 0, n))
    }
    fn v4(n: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, n))
    }
    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    // --- FakeRt: deterministic `TcpConnect`+`Timer` with no real network
    // and no real sleeping --------------------------------------------
    //
    // `sleep` doesn't really wait — it SYNCHRONOUSLY advances a shared
    // virtual clock and resolves immediately. This isn't an approximation
    // of the real behavior: `now`/`elapsed_since` read that same clock, so
    // the test sees EXACTLY the time arithmetic `drive` builds, with not a
    // single real millisecond of waiting and no associated jitter that
    // would make an assert on an exact delay value fragile.
    //
    // `log` records `(IpAddr, time_at_start)` for each attempt — this
    // proves either "the pause between starts equals exactly
    // attempt_delay" (staggering), or "addresses alternate between
    // families in the right order" (interleaving), or both at once.
    #[derive(Clone)]
    struct FakeRt {
        clock: Rc<RefCell<Duration>>,
        log: Rc<RefCell<Vec<(IpAddr, Duration)>>>,
        /// `None` — a connection to this address will never complete
        /// (until the test decides otherwise); `Some(true)` — success;
        /// `Some(false)` — failure.
        outcomes: Rc<RefCell<HashMap<IpAddr, bool>>>,
    }

    impl FakeRt {
        fn new(outcomes: impl IntoIterator<Item = (IpAddr, bool)>) -> Self {
            Self {
                clock: Rc::new(RefCell::new(Duration::ZERO)),
                log: Rc::new(RefCell::new(Vec::new())),
                outcomes: Rc::new(RefCell::new(outcomes.into_iter().collect())),
            }
        }
    }

    /// `hyper::rt` IO with not a single real byte — `drive`/`race_connect`
    /// never read from or write to a successfully "connected" stream, they
    /// only hand it back to the caller. Carries an `Rc<()>` ON PURPOSE:
    /// this is the probe for "`Send` is never required" — if
    /// `race_connect` or `drive` required `R::Stream: Send` (or any other
    /// path pulled in `Send` through `FuturesUnordered`/`poll_fn` instead
    /// of an explicit bound), this file wouldn't compile at all. See
    /// `race_connect_never_requires_send_even_through_the_wait_path`
    /// below.
    #[derive(Debug)]
    struct FakeStream(#[allow(dead_code)] Rc<()>);

    impl Read for FakeStream {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: ReadBufCursor<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }
    impl Write for FakeStream {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Poll::Ready(Ok(buf.len()))
        }
        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    /// The virtual clock's sleep, as a named future — and the one place in
    /// this port where naming the type was not a rename.
    ///
    /// **The advance must happen on the first `poll`, not when `sleep` is
    /// called.** The `async fn` this replaces did that for free: an
    /// `async fn` body runs on first poll, not at the call. Happy Eyeballs
    /// creates several of these and polls them selectively, so advancing at
    /// construction moves the clock for a sleep that is never awaited.
    /// Re-measured by mutation rather than left as a recollection —
    /// advancing eagerly in `sleep` is caught by exactly six tests in this
    /// module: `attempt_staggering_delay_is_respected_through_the_
    /// connector`, `connect_returns_success_via_the_wait_branch_not_only_
    /// via_exhausted`, `families_interleave_through_the_connector`,
    /// `race_connect_never_requires_send_even_through_the_wait_path`,
    /// `resolution_delay_is_honored_when_ipv6_is_still_pending`, and
    /// `a_repolled_fake_sleep_advances_the_clock_once` below. Real clocks
    /// are the opposite: `tokio::time::sleep`, `async_io::Timer::after` and
    /// `setTimeout` all fix their deadline at the call, and for them the
    /// RPITIT's laziness was the accident.
    struct FakeSleep {
        clock: Rc<RefCell<Duration>>,
        /// `Some` until the advance has been applied; `take`n on first
        /// poll so a re-poll cannot double-count. Defensive — nothing in
        /// the connector re-polls a completed sleep — and therefore held
        /// by a test of its own (`a_repolled_fake_sleep_advances_the_clock
        /// _once`), because a mutant replacing the `take` with a peek
        /// survived the whole suite otherwise.
        d: Option<Duration>,
    }

    impl Future for FakeSleep {
        type Output = ();
        fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
            let me = self.get_mut();
            if let Some(d) = me.d.take() {
                *me.clock.borrow_mut() += d;
            }
            Poll::Ready(())
        }
    }

    impl Timer for FakeRt {
        type Instant = Duration;
        type Sleep = FakeSleep;
        fn sleep(&self, d: Duration) -> Self::Sleep {
            FakeSleep {
                clock: Rc::clone(&self.clock),
                d: Some(d),
            }
        }
        fn now(&self) -> Duration {
            *self.clock.borrow()
        }
        fn elapsed_since(&self, earlier: Duration) -> Duration {
            *self.clock.borrow() - earlier
        }
    }

    /// Two properties of [`FakeSleep`] in one test, because they are two
    /// halves of the same decision and a mutation of either must fail
    /// somewhere:
    ///
    /// 1. Constructing the sleep must NOT move the clock. Ten tests in
    ///    this module already fail if it does, but they fail for reasons
    ///    about Happy Eyeballs; this one fails for the reason itself.
    /// 2. A second poll must not advance it again. Nothing in the
    ///    connector re-polls a finished sleep, so no other test covers
    ///    this — verified by mutation: replacing `me.d.take()` with a
    ///    non-consuming read survived the entire workspace suite before
    ///    this test existed.
    #[test]
    fn a_repolled_fake_sleep_advances_the_clock_once() {
        let rt = FakeRt::new([]);
        let clock = Rc::clone(&rt.clock);

        let mut s = rt.sleep(Duration::from_millis(250));
        assert_eq!(
            *clock.borrow(),
            Duration::ZERO,
            "constructing the sleep moved the virtual clock; it must only \
             move when the sleep is actually polled"
        );

        let mut cx = Context::from_waker(std::task::Waker::noop());
        assert_eq!(Pin::new(&mut s).poll(&mut cx), Poll::Ready(()));
        assert_eq!(
            *clock.borrow(),
            Duration::from_millis(250),
            "the first poll did not advance the virtual clock"
        );

        // Polling a completed future is a contract violation in general;
        // this one documents that it tolerates it, so that is what is
        // checked.
        assert_eq!(Pin::new(&mut s).poll(&mut cx), Poll::Ready(()));
        assert_eq!(
            *clock.borrow(),
            Duration::from_millis(250),
            "a second poll advanced the virtual clock again"
        );
    }

    impl TcpConnect for FakeRt {
        type Stream = FakeStream;
        // A plain box on purpose: this fixture keeps its log in a
        // `RefCell` and its stream holds an `Rc`, so it is genuinely
        // `!Send` — and the seam's associated type is what lets it say so
        // and still be a `TcpConnect`.
        type Connecting<'a>
            = std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<FakeStream>> + 'a>>
        where
            Self: 'a;

        fn connect<'a>(&'a self, addr: SocketAddr, _opts: &TcpOpts) -> Self::Connecting<'a> {
            Box::pin(async move {
                self.log
                    .borrow_mut()
                    .push((addr.ip(), *self.clock.borrow()));
                let ok = self
                    .outcomes
                    .borrow()
                    .get(&addr.ip())
                    .copied()
                    .unwrap_or(false);
                if ok {
                    Ok(FakeStream(Rc::new(())))
                } else {
                    Err(std::io::Error::other("fake refused"))
                }
            })
        }
    }

    fn he(attempt_delay_ms: u64) -> HeConfig {
        HeConfig {
            attempt_delay: ms(attempt_delay_ms),
            ..Default::default()
        }
    }

    /// `futures_executor::block_on`, but bounded: `FakeRt` never sleeps for
    /// real (its `Timer::sleep` just advances a virtual clock and resolves
    /// immediately), so nothing below should ever take real wall-clock time
    /// — UNLESS a bug (or a mutation, see the mutation-testing notes in
    /// makes `drive`/`race_connect` loop forever without ever advancing
    /// the scheduler to `Exhausted` or `Start`. A test that hangs under
    /// mutation instead of failing wedges CI with no name and no
    /// diagnosis, which is the shape to avoid. A watchdog
    /// thread, not a `Send`-bounded wrapper around `fut` itself: `fut` is
    /// generic with NO `Send` bound here on purpose, because several
    /// tests below deliberately drive `!Send` futures (`FakeStream` holds
    /// an `Rc`) to prove `race_connect`/`drive` impose no such bound — an
    /// `F: Send` bound on this helper would silently defeat that proof
    /// for exactly the tests it matters most for. Only an
    /// `Arc<AtomicBool>` — unrelated to `fut` — crosses the thread
    /// boundary.
    fn bounded_block_on<F: std::future::Future>(fut: F) -> F::Output {
        const BOUND: Duration = Duration::from_secs(10);
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let watchdog_done = done.clone();
        std::thread::spawn(move || {
            std::thread::sleep(BOUND);
            if !watchdog_done.load(Ordering::SeqCst) {
                eprintln!(
                    "bounded_block_on: future did not complete within {BOUND:?} - treating \
                     as a hang (likely an infinite loop in drive/race_connect) instead of \
                     letting the test process wedge CI with no diagnosis"
                );
                std::process::exit(101);
            }
        });
        let result = futures_executor::block_on(fut);
        done.store(true, Ordering::SeqCst);
        result
    }

    #[test]
    fn attempt_staggering_delay_is_respected_through_the_connector() {
        // Two dead addresses of one family: the only reason for a pause
        // between them is the Connection Attempt Delay. A virtual clock,
        // so we check an EXACT value, not "approximately."
        let rt = FakeRt::new([]);
        let out = bounded_block_on(race_connect(
            &rt,
            vec![v6(1), v6(2)],
            vec![],
            81,
            &TcpOpts::default(),
            he(100),
        ));
        assert!(out.is_err(), "both addresses are dead");
        let log = rt.log.borrow().clone();
        assert_eq!(log, vec![(v6(1), ms(0)), (v6(2), ms(100))]);
    }

    #[test]
    fn families_interleave_through_the_connector() {
        let rt = FakeRt::new([]);
        let out = bounded_block_on(race_connect(
            &rt,
            vec![v6(1), v6(2)],
            vec![v4(1), v4(2)],
            81,
            &TcpOpts::default(),
            he(100),
        ));
        assert!(out.is_err());
        let log: Vec<IpAddr> = rt.log.borrow().iter().map(|(a, _)| *a).collect();
        assert_eq!(log, vec![v6(1), v4(1), v6(2), v4(2)]);
    }

    #[test]
    fn all_attempts_failed_reports_connect_kind_with_the_launched_count() {
        let rt = FakeRt::new([]);
        let err = bounded_block_on(race_connect(
            &rt,
            vec![v6(1)],
            vec![v4(1), v4(2)],
            81,
            &TcpOpts::default(),
            he(100),
        ))
        .expect_err("all three addresses are dead");
        assert_eq!(err.kind(), &ErrorKind::Connect);
        assert_eq!(rt.log.borrow().len(), 3);
    }

    #[test]
    fn a_successful_attempt_short_circuits_the_remaining_race() {
        let rt = FakeRt::new([(v4(1), true)]);
        let stream = bounded_block_on(race_connect(
            &rt,
            vec![v6(1)],
            vec![v4(1)],
            81,
            &TcpOpts::default(),
            he(100),
        ));
        assert!(stream.is_ok());
    }

    #[test]
    fn he_config_out_of_range_is_a_typed_error_not_a_silent_clamp() {
        let rt = FakeRt::new([]);
        let err = bounded_block_on(race_connect(
            &rt,
            vec![v6(1)],
            vec![],
            81,
            &TcpOpts::default(),
            he(1), // below ATTEMPT_MIN (100ms) — would be clamped silently
        ))
        .expect_err("an out-of-range attempt_delay must be rejected");
        assert_eq!(err.kind(), &ErrorKind::Connect);
        // No attempt was launched: the failure happened BEFORE the race.
        assert!(rt.log.borrow().is_empty());
    }

    /// A stream carrying an `Rc` (`FakeStream`) genuinely goes through the
    /// `Wait` branch (not just an immediate success): a dead address, then
    /// a live one — the same shape as the integration test
    /// `falls_over_from_a_dead_address_to_a_live_one`, just on `FakeRt`.
    /// If `drive`/`race_connect` (through `FuturesUnordered`, `poll_fn`,
    /// or `TcpConnect`'s signature) required `Send` anywhere, this file
    /// wouldn't compile: `FakeStream` is `!Send` by construction.
    #[test]
    fn race_connect_never_requires_send_even_through_the_wait_path() {
        let rt = FakeRt::new([(v4(1), true)]);
        let stream = bounded_block_on(race_connect(
            &rt,
            vec![v6(1), v6(2)],
            vec![v4(1)],
            81,
            &TcpOpts::default(),
            he(100),
        ));
        assert!(stream.is_ok());
        // RFC 8305 interleaving (first_family_count defaults to 1) makes
        // v4(1) the SECOND attempt (v6(1), v4(1), v6(2), ...) — a success
        // on it stops the race before v6(2)'s turn ever comes up. Both
        // attempts that did start (v6(1), dead, and v4(1), live) went
        // through `drive`'s Wait branch exactly, not just an immediate
        // success on the very first `Start` — that's what this test is
        // about.
        assert_eq!(rt.log.borrow().len(), 2);
    }

    // --- Virtual DNS streams for `drive` --------------------------------
    //
    // `poll_next` checks against the SAME virtual clock as `FakeRt::Timer`
    // (the same `Rc<RefCell<Duration>>`), so "AAAA arrives after N ms"
    // isn't a real delay, it's a point on the shared time axis that
    // `FakeRt::sleep` advances.
    struct AtVirtualTime {
        clock: Rc<RefCell<Duration>>,
        resolve_at: Duration,
        item: Option<IpAddr>,
        yielded: bool,
    }

    impl futures_util::Stream for AtVirtualTime {
        type Item = Result<ResolvedAddr, Error>;
        fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            let this = self.get_mut();
            if *this.clock.borrow() < this.resolve_at {
                return Poll::Pending;
            }
            if this.yielded {
                return Poll::Ready(None);
            }
            this.yielded = true;
            match this.item {
                Some(addr) => Poll::Ready(Some(Ok(ResolvedAddr { addr, ttl: None }))),
                None => Poll::Ready(None),
            }
        }
    }

    #[test]
    fn resolution_delay_is_honored_when_ipv6_is_still_pending() {
        // AAAA "arrives" at 80ms — later than the default Resolution Delay
        // (50ms). A "arrives" immediately (0ms). If `drive` didn't honor
        // Resolution Delay (i.e., if the code that collected the whole
        // stream into a Vec before calling Scheduler had come back), IPv4
        // would start instantly, before 50ms; correct behavior is to wait
        // exactly 50ms, then go with IPv4 (RFC 8305 §3: wait the
        // Resolution Delay, not the whole resolver).
        let rt = FakeRt::new([(v4(9), true)]);
        let clock = rt.clock.clone();
        let v6s = AtVirtualTime {
            clock: clock.clone(),
            resolve_at: ms(80),
            item: Some(v6(9)),
            yielded: false,
        };
        let v4s = AtVirtualTime {
            clock,
            resolve_at: ms(0),
            item: Some(v4(9)),
            yielded: false,
        };
        let sched = build_scheduler(HeConfig::default()).unwrap();
        let out = bounded_block_on(drive::<_, _, _, NoHooks>(
            &rt,
            sched,
            v6s,
            v4s,
            81,
            &TcpOpts::default(),
            None,
        ));
        assert!(out.is_ok(), "IPv4 must have been tried after waiting");
        let log = rt.log.borrow();
        assert_eq!(
            log.len(),
            1,
            "AAAA only arrived at 80ms — already after IPv4 had won"
        );
        assert_eq!(log[0].0, v4(9));
        assert_eq!(
            log[0].1,
            ms(50),
            "IPv4's start must happen EXACTLY after the Resolution Delay (50ms), \
             not earlier (wouldn't have waited for AAAA) and not later (would have \
             waited for the whole resolver instead of a fixed pause)"
        );
    }

    #[test]
    fn late_ipv6_arrival_after_resolution_delay_is_still_attempted() {
        // RFC 8305 §3: a late AAAA is still taken into account, not
        // dropped — the same scenario as `Scheduler`'s own
        // `late_ipv6_arrival_after_resolution_delay_is_still_attempted`
        // (happy_eyeballs.rs), but here through a real DNS stream and a
        // real `drive`, not direct calls to `offer_v6`/`poll`.
        let rt = FakeRt::new([]); // all dead: IPv4 doesn't answer, neither does IPv6
        let clock = rt.clock.clone();
        let v6s = AtVirtualTime {
            clock: clock.clone(),
            resolve_at: ms(300),
            item: Some(v6(7)),
            yielded: false,
        };
        let v4s = AtVirtualTime {
            clock,
            resolve_at: ms(0),
            item: Some(v4(7)),
            yielded: false,
        };
        let sched = build_scheduler(HeConfig::default()).unwrap();
        let out = bounded_block_on(drive::<_, _, _, NoHooks>(
            &rt,
            sched,
            v6s,
            v4s,
            81,
            &TcpOpts::default(),
            None,
        ));
        assert!(out.is_err());
        let log = rt.log.borrow();
        assert_eq!(
            log.iter().map(|(a, _)| *a).collect::<Vec<_>>(),
            vec![v4(7), v6(7)],
            "a late AAAA must be tried, not dropped"
        );
    }

    #[test]
    fn a_resolve_error_on_one_family_surfaces_as_resolve_kind_when_nothing_else_is_found() {
        struct ErrOnce(bool);
        impl futures_util::Stream for ErrOnce {
            type Item = Result<ResolvedAddr, Error>;
            fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
                let this = self.get_mut();
                if this.0 {
                    this.0 = false;
                    Poll::Ready(Some(Err(Error::new(
                        ErrorKind::Resolve,
                        std::io::Error::other("dns down"),
                    ))))
                } else {
                    Poll::Ready(None)
                }
            }
        }
        let rt = FakeRt::new([]);
        let sched = build_scheduler(HeConfig::default()).unwrap();
        let err = bounded_block_on(drive::<_, _, _, NoHooks>(
            &rt,
            sched,
            ErrOnce(true),
            futures_util::stream::empty(),
            81,
            &TcpOpts::default(),
            None,
        ))
        .expect_err("no address arrived for either family");
        assert_eq!(
            err.kind(),
            &ErrorKind::Resolve,
            "the resolver failed — this is NOT 'all TCP attempts failed', it's 'nothing to try'"
        );
        assert!(
            rt.log.borrow().is_empty(),
            "there should not have been a single TCP attempt"
        );
    }

    #[test]
    fn zero_addresses_without_any_resolver_error_also_surfaces_as_resolve_kind() {
        // An NXDOMAIN-like case: the resolver finished honestly, found
        // zero addresses, never returned an Err. Still
        // ErrorKind::Resolve, not Connect with launched=0 — "0 TCP
        // attempts failed" would sound as though we tried at all.
        let rt = FakeRt::new([]);
        let sched = build_scheduler(HeConfig::default()).unwrap();
        let err = bounded_block_on(drive::<_, _, _, NoHooks>(
            &rt,
            sched,
            futures_util::stream::empty(),
            futures_util::stream::empty(),
            81,
            &TcpOpts::default(),
            None,
        ))
        .expect_err("both families are empty");
        assert_eq!(err.kind(), &ErrorKind::Resolve);
    }

    /// Like `ErrOnce` (see above), but with a configurable `ErrorKind` —
    /// needed to simulate `ErrorKind::Cancelled` (the background thread
    /// pool shut down before `getaddrinfo` finished), not just
    /// `ErrorKind::Resolve`.
    struct ErrOnceWithKind(bool, ErrorKind);
    impl futures_util::Stream for ErrOnceWithKind {
        type Item = Result<ResolvedAddr, Error>;
        fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            let this = self.get_mut();
            if this.0 {
                this.0 = false;
                Poll::Ready(Some(Err(Error::new(
                    this.1.clone(),
                    std::io::Error::other("boom"),
                ))))
            } else {
                Poll::Ready(None)
            }
        }
    }

    /// Loss path A, "flattened to Resolve": wrapping ANY resolve error
    /// when `launched == 0` into a fresh
    /// `Error::new(ErrorKind::Resolve, errs)` discards the original
    /// `kind()`. `ErrorKind::Cancelled` exists
    /// specifically so a caller can tell "the runtime is shutting down"
    /// apart from "this name doesn't resolve" without downcasting, just
    /// by comparing `kind()` — and flattening into `Resolve` erased that
    /// distinction.
    #[test]
    fn a_cancelled_resolve_error_is_not_flattened_to_resolve_kind_when_nothing_launched() {
        let rt = FakeRt::new([]);
        let sched = build_scheduler(HeConfig::default()).unwrap();
        let err = bounded_block_on(drive::<_, _, _, NoHooks>(
            &rt,
            sched,
            ErrOnceWithKind(true, ErrorKind::Cancelled),
            futures_util::stream::empty(),
            81,
            &TcpOpts::default(),
            None,
        ))
        .expect_err("v6 resolver was cancelled, v4 found nothing");
        assert_eq!(
            err.kind(),
            &ErrorKind::Cancelled,
            "the caller must see 'the runtime is shutting down', not 'this name doesn't \
             resolve' — otherwise a circuit breaker keyed on ErrorKind::Resolve would \
             wrongly blacklist a live host during an ordinary shutdown"
        );
        assert!(
            rt.log.borrow().is_empty(),
            "there should not have been a single TCP attempt"
        );
    }

    /// Loss path B, "silently discarded" — the same principle, but with
    /// `launched > 0`: v6 was cancelled, v4 found one dead address and
    /// genuinely tried it. Returning
    /// `AllAttemptsFailed`/`ErrorKind::Connect` in this branch leaves `errs`
    /// (holding the Cancelled signal) was never read at all — not in
    /// `.source()`, nowhere. This is the worse of the two loss paths: an
    /// error that isn't even in the source chain can't be recovered by
    /// any caller code, however careful.
    #[test]
    fn a_cancelled_resolve_error_is_not_discarded_when_the_other_family_launched_and_failed() {
        let rt = FakeRt::new([]); // the single v4 address isn't in outcomes -> failure
        let sched = build_scheduler(HeConfig::default()).unwrap();
        let err = bounded_block_on(drive::<_, _, _, NoHooks>(
            &rt,
            sched,
            ErrOnceWithKind(true, ErrorKind::Cancelled),
            futures_util::stream::iter([Ok(ResolvedAddr {
                addr: v4(1),
                ttl: None,
            })]),
            81,
            &TcpOpts::default(),
            None,
        ))
        .expect_err("the single v4 address is dead");
        assert_eq!(
            err.kind(),
            &ErrorKind::Cancelled,
            "the v6 cancellation must stay visible even when v4 genuinely started and failed"
        );
        assert_eq!(
            rt.log.borrow().len(),
            1,
            "the single (dead) v4 address really was tried"
        );
    }

    // --- `ResolveErrors`: the chain, not just the message ---------------
    //
    // `ResolveErrors` is the only error in this module that a caller is
    // expected to WALK rather than read: it says which family failed, and
    // its `source()` hands back that family's own `Error` — kind, source
    // chain and all — instead of a copy of its `Display`. The four tests
    // below pin all four shapes it can take, because a `source()` that
    // returns `None` where it used to return the resolver's error still
    // compiles, still prints the same message, and still passes every
    // assertion written about that message.

    /// A resolver stream that fails once with `ErrorKind::Resolve` and a
    /// caller-chosen message, then ends. `ErrOnceWithKind` above hardcodes
    /// `"boom"` for both families, so a test that uses it twice cannot
    /// tell WHICH family's error came out the far end of the chain — and
    /// that is exactly the question here.
    struct NamedResolveErr(bool, &'static str);
    impl futures_util::Stream for NamedResolveErr {
        type Item = Result<ResolvedAddr, Error>;
        fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            let this = self.get_mut();
            if this.0 {
                this.0 = false;
                Poll::Ready(Some(Err(Error::new(
                    ErrorKind::Resolve,
                    std::io::Error::other(this.1),
                ))))
            } else {
                Poll::Ready(None)
            }
        }
    }

    /// The `ResolveErrors` one `source()` hop below what `drive` returned.
    fn resolve_errors(err: &Error) -> &ResolveErrors {
        StdError::source(err)
            .expect("Error::new(ErrorKind::Resolve, ..) always has a source")
            .downcast_ref::<ResolveErrors>()
            .expect("the source of a resolve failure is ResolveErrors itself")
    }

    /// The family failure `ResolveErrors::source()` leads to — the hop a
    /// caller walks to find out WHY the lookup failed. `None` means the
    /// chain stops at `ResolveErrors`.
    fn recorded_family_error(errs: &ResolveErrors) -> Option<&Error> {
        StdError::source(errs).map(|s| {
            s.downcast_ref::<Error>().expect(
                "the recorded family failure must stay the resolver's own Error, \
                 not a stringified copy of it",
            )
        })
    }

    #[test]
    fn a_failed_ipv6_lookup_stays_reachable_through_the_source_chain() {
        let rt = FakeRt::new([]);
        let sched = build_scheduler(HeConfig::default()).unwrap();
        let err = bounded_block_on(drive::<_, _, _, NoHooks>(
            &rt,
            sched,
            NamedResolveErr(true, "v6 lookup exploded"),
            futures_util::stream::empty(),
            81,
            &TcpOpts::default(),
            None,
        ))
        .expect_err("v6 failed and v4 found nothing");
        let errs = resolve_errors(&err);
        assert_eq!(
            errs.to_string(),
            "ipv6 lookup failed (Resolve: v6 lookup exploded); \
             ipv4 lookup returned no addresses"
        );
        let recorded = recorded_family_error(errs)
            .expect("the ipv6 failure must stay reachable, not be flattened into the message");
        assert_eq!(recorded.kind(), &ErrorKind::Resolve);
        assert_eq!(recorded.to_string(), "Resolve: v6 lookup exploded");
    }

    /// The asymmetric case, and the one a `source()` that only ever reads
    /// the ipv6 slot gets wrong: ipv6 recorded nothing at all, so the only
    /// failure there is to hand back is ipv4's. Truncating the chain here
    /// would leave the message unchanged and the cause unreachable.
    #[test]
    fn a_failed_ipv4_lookup_stays_reachable_even_though_ipv6_recorded_nothing() {
        let rt = FakeRt::new([]);
        let sched = build_scheduler(HeConfig::default()).unwrap();
        let err = bounded_block_on(drive::<_, _, _, NoHooks>(
            &rt,
            sched,
            futures_util::stream::empty(),
            NamedResolveErr(true, "v4 lookup exploded"),
            81,
            &TcpOpts::default(),
            None,
        ))
        .expect_err("v4 failed and v6 found nothing");
        let errs = resolve_errors(&err);
        assert_eq!(
            errs.to_string(),
            "ipv4 lookup failed (Resolve: v4 lookup exploded); \
             ipv6 lookup returned no addresses"
        );
        let recorded = recorded_family_error(errs).expect(
            "ipv4's failure must not vanish just because ipv6 recorded nothing — \
             a source() that reads only the ipv6 slot ends the chain right here",
        );
        assert_eq!(recorded.to_string(), "Resolve: v4 lookup exploded");
    }

    #[test]
    fn when_both_lookups_fail_the_message_names_both_and_the_chain_leads_to_ipv6() {
        let rt = FakeRt::new([]);
        let sched = build_scheduler(HeConfig::default()).unwrap();
        let err = bounded_block_on(drive::<_, _, _, NoHooks>(
            &rt,
            sched,
            NamedResolveErr(true, "v6 lookup exploded"),
            NamedResolveErr(true, "v4 lookup exploded"),
            81,
            &TcpOpts::default(),
            None,
        ))
        .expect_err("both families failed");
        let errs = resolve_errors(&err);
        assert_eq!(
            errs.to_string(),
            "ipv6 lookup failed (Resolve: v6 lookup exploded); \
             ipv4 lookup failed (Resolve: v4 lookup exploded)"
        );
        let recorded = recorded_family_error(errs).expect("both were recorded");
        assert_eq!(
            recorded.to_string(),
            "Resolve: v6 lookup exploded",
            "with both families recorded the chain follows the first one, ipv6"
        );
    }

    /// The distinction the whole type exists for, seen from the chain: an
    /// NXDOMAIN-like empty answer is NOT a failure of anything, so there
    /// is no cause to hand back. `source()` returning `None` here is the
    /// correct answer, and is what tells this case apart from the three
    /// above without reading any prose.
    #[test]
    fn zero_addresses_and_no_resolver_error_ends_the_chain_instead_of_inventing_a_cause() {
        let rt = FakeRt::new([]);
        let sched = build_scheduler(HeConfig::default()).unwrap();
        let err = bounded_block_on(drive::<_, _, _, NoHooks>(
            &rt,
            sched,
            futures_util::stream::empty(),
            futures_util::stream::empty(),
            81,
            &TcpOpts::default(),
            None,
        ))
        .expect_err("both families are empty");
        let errs = resolve_errors(&err);
        assert_eq!(
            errs.to_string(),
            "resolver returned no addresses for either address family"
        );
        assert!(
            recorded_family_error(errs).is_none(),
            "nothing failed — an empty answer must not acquire a fabricated cause"
        );
    }

    // --- `connect` plumbing: URI -> host/port/scheme, TLS/plain ---------

    struct StaticResolve {
        v6: Vec<IpAddr>,
        v4: Vec<IpAddr>,
    }
    impl Resolve for StaticResolve {
        fn lookup_ipv6(
            &self,
            _: &str,
        ) -> impl futures_util::Stream<Item = Result<ResolvedAddr, Error>> {
            futures_util::stream::iter(
                self.v6
                    .clone()
                    .into_iter()
                    .map(|addr| Ok(ResolvedAddr { addr, ttl: None })),
            )
        }
        fn lookup_ipv4(
            &self,
            _: &str,
        ) -> impl futures_util::Stream<Item = Result<ResolvedAddr, Error>> {
            futures_util::stream::iter(
                self.v4
                    .clone()
                    .into_iter()
                    .map(|addr| Ok(ResolvedAddr { addr, ttl: None })),
            )
        }
    }

    /// [`StaticResolve`] that also answers SVCB, for the one property
    /// below that cannot be watched from a socket.
    struct SvcbResolve {
        v4: Vec<IpAddr>,
        records: Vec<hclient_dns::SvcbEndpoint>,
    }

    impl Resolve for SvcbResolve {
        fn lookup_ipv6(
            &self,
            _: &str,
        ) -> impl futures_util::Stream<Item = Result<ResolvedAddr, Error>> {
            futures_util::stream::iter(Vec::new())
        }
        fn lookup_ipv4(
            &self,
            _: &str,
        ) -> impl futures_util::Stream<Item = Result<ResolvedAddr, Error>> {
            futures_util::stream::iter(
                self.v4
                    .clone()
                    .into_iter()
                    .map(|addr| Ok(ResolvedAddr { addr, ttl: None }))
                    .collect::<Vec<_>>(),
            )
        }
        fn supports_svcb(&self) -> bool {
            true
        }
        fn lookup_svcb(
            &self,
            _: &str,
        ) -> impl futures_util::Stream<Item = Result<hclient_dns::SvcbEndpoint, Error>> {
            futures_util::stream::iter(self.records.clone().into_iter().map(Ok).collect::<Vec<_>>())
        }
    }

    /// The one claim of this item that a peer socket cannot make, and the
    /// reason is in `tests/svcb.rs`'s module doc: the retry after a
    /// discovered endpoint fails goes to the origin's *own* endpoint,
    /// which for a default-port `https` URI is port 443 — where an
    /// unprivileged test process cannot put a listener. So it is watched
    /// here instead, on the one observer that does see every attempt:
    /// `FakeRt`'s log.
    ///
    /// Three attempts for one call. The hint first (the record moved where
    /// we start), then the origin's own address, then — after the whole
    /// race failed — the origin's address again, on a second race run
    /// **without** the record. Without the retry the third entry is
    /// missing and a stale record is an outage rather than a slow request;
    /// without the discovery the first is.
    #[test]
    fn a_failed_discovered_endpoint_is_retried_without_the_record() {
        let hint = std::net::Ipv4Addr::new(10, 0, 0, 7);
        let origin = v4(1);
        let dns = SvcbResolve {
            v4: vec![origin],
            records: vec![hclient_dns::SvcbEndpoint {
                priority: 1,
                target: "example.invalid".to_string(),
                alpn: Vec::new(),
                port: None,
                ipv4hint: vec![hint],
                ipv6hint: Vec::new(),
                ech_config_list: None,
            }],
        };
        // Every attempt fails, so the discovered race and the plain one
        // both run to `Exhausted` — which is the case this is about.
        let rt = FakeRt::new([]);
        let uri: Uri = "https://example.invalid/".parse().unwrap();
        let cache = NegativeCache::default();

        let _ = bounded_block_on(super::connect::<_, _, _, crate::proxy::NoProxy, NoHooks>(
            &rt,
            &dns,
            &NoOpTls,
            &[],
            None,
            &uri,
            &TcpOpts::default(),
            &[],
            &cache,
            Duration::ZERO,
            Prefetched::NotConsulted,
            None,
        ))
        .expect_err("nothing answers");

        let tried: Vec<IpAddr> = rt.log.borrow().iter().map(|(ip, _)| *ip).collect();
        assert_eq!(
            tried,
            vec![IpAddr::V4(hint), origin, origin],
            "the hint, then the origin, then the origin again without the record"
        );
    }

    /// A record that sets no parameter at all changes nothing about the
    /// connection, so its presence must not be mistaken for its taking
    /// part: `Endpoint::is_inert` drops it, and this is what that costs if
    /// it does not.
    ///
    /// One race, not two. Without the check the empty record counts as a
    /// discovered endpoint, the (identical) attempt "fails with the
    /// record", and the retry runs the same race a second time — twice the
    /// connect budget for an origin whose record said nothing, and a
    /// negative-cache entry for a discovery that never happened.
    #[test]
    fn a_record_that_sets_nothing_does_not_buy_a_second_race() {
        let origin = v4(1);
        let dns = SvcbResolve {
            v4: vec![origin],
            records: vec![hclient_dns::SvcbEndpoint {
                priority: 1,
                target: "example.invalid".to_string(),
                alpn: Vec::new(),
                port: None,
                ipv4hint: Vec::new(),
                ipv6hint: Vec::new(),
                ech_config_list: None,
            }],
        };
        let rt = FakeRt::new([]);
        let uri: Uri = "https://example.invalid/".parse().unwrap();

        let _ = bounded_block_on(super::connect::<_, _, _, crate::proxy::NoProxy, NoHooks>(
            &rt,
            &dns,
            &NoOpTls,
            &[],
            None,
            &uri,
            &TcpOpts::default(),
            &[],
            &NegativeCache::default(),
            Duration::ZERO,
            Prefetched::NotConsulted,
            None,
        ))
        .expect_err("nothing answers");

        let tried: Vec<IpAddr> = rt.log.borrow().iter().map(|(ip, _)| *ip).collect();
        assert_eq!(tried, vec![origin], "an inert record is not an endpoint");
    }

    // --- the record and the addresses, asked at once --------------------

    /// A stream that takes one poll to be *asked* and one to *answer*,
    /// writing both into a shared log.
    ///
    /// The pending poll wakes itself instead of parking. That is what makes
    /// a connector which serialises its queries fail with a **wrong log**
    /// rather than with a deadlock: the test then says, in a millisecond,
    /// which order it saw, instead of being killed ten seconds later by
    /// [`bounded_block_on`]'s watchdog with nothing to read. A busy-spin is
    /// affordable here because every poll in this fixture makes progress.
    struct Staged<T> {
        log: Rc<RefCell<Vec<&'static str>>>,
        asked: &'static str,
        answered: &'static str,
        items: std::vec::IntoIter<T>,
        started: bool,
    }

    impl<T: Unpin> Stream for Staged<T> {
        type Item = Result<T, Error>;

        fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            let me = self.get_mut();
            if !me.started {
                me.started = true;
                me.log.borrow_mut().push(me.asked);
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
            match me.items.next() {
                Some(item) => {
                    me.log.borrow_mut().push(me.answered);
                    Poll::Ready(Some(Ok(item)))
                }
                None => Poll::Ready(None),
            }
        }
    }

    /// A resolver that reports, from outside this connector, **when** each
    /// of its queries was created, sent and answered.
    ///
    /// Nine possible events, one shared log, and no clock at all: whether
    /// two queries overlap is a claim about the order of four of those
    /// events, not about how long anything took. That distinction is the
    /// whole reason this fixture exists rather than a timing assertion —
    /// `crates/hclient-h3/tests/` has a fresh example of a timing assertion
    /// missing its window by 0.22 ms.
    #[derive(Clone)]
    struct WatchedResolve {
        log: Rc<RefCell<Vec<&'static str>>>,
        v4: Vec<IpAddr>,
        records: Vec<hclient_dns::SvcbEndpoint>,
    }

    impl WatchedResolve {
        fn new(v4: Vec<IpAddr>, records: Vec<hclient_dns::SvcbEndpoint>) -> Self {
            Self {
                log: Rc::new(RefCell::new(Vec::new())),
                v4,
                records,
            }
        }

        fn events(&self) -> Vec<&'static str> {
            self.log.borrow().clone()
        }

        fn note(&self, what: &'static str) {
            self.log.borrow_mut().push(what);
        }
    }

    impl Resolve for WatchedResolve {
        fn lookup_ipv6(&self, _: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>> {
            self.note("aaaa:call");
            Staged {
                log: Rc::clone(&self.log),
                asked: "aaaa:ask",
                answered: "aaaa:answer",
                items: Vec::<ResolvedAddr>::new().into_iter(),
                started: false,
            }
        }

        fn lookup_ipv4(&self, _: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>> {
            self.note("a:call");
            Staged {
                log: Rc::clone(&self.log),
                asked: "a:ask",
                answered: "a:answer",
                items: self
                    .v4
                    .iter()
                    .map(|&addr| ResolvedAddr { addr, ttl: None })
                    .collect::<Vec<_>>()
                    .into_iter(),
                started: false,
            }
        }

        fn supports_svcb(&self) -> bool {
            true
        }

        fn lookup_svcb(
            &self,
            _: &str,
        ) -> impl Stream<Item = Result<hclient_dns::SvcbEndpoint, Error>> {
            self.note("https:call");
            Staged {
                log: Rc::clone(&self.log),
                asked: "https:ask",
                answered: "https:answer",
                items: self.records.clone().into_iter(),
                started: false,
            }
        }
    }

    fn at(events: &[&'static str], what: &str) -> usize {
        events
            .iter()
            .position(|e| *e == what)
            .unwrap_or_else(|| panic!("{what} never happened; the log was {events:?}"))
    }

    /// RFC 9460 §10.3: the HTTPS query and the address queries go out
    /// together. Watched from the resolver's side, where it is a statement
    /// about the order of four events and about nothing else.
    ///
    /// Two claims, and each rules out one of the two ways to be serial:
    ///
    /// - the A query was **sent before the record was answered** — false
    ///   for a connector that awaits the record first, which is what this
    ///   one did until the queries were made concurrent;
    /// - the HTTPS query was **sent before the addresses were answered** —
    ///   false for a connector that resolves first and asks afterwards.
    ///
    /// Neither claim alone is the property. Together they say the two
    /// queries were outstanding at the same time.
    #[test]
    fn the_record_and_the_addresses_are_asked_at_once() {
        let dns = WatchedResolve::new(
            vec![v4(1)],
            vec![hclient_dns::SvcbEndpoint {
                priority: 1,
                target: "example.invalid".to_string(),
                alpn: Vec::new(),
                port: Some(8443),
                ipv4hint: Vec::new(),
                ipv6hint: Vec::new(),
                ech_config_list: None,
            }],
        );
        let rt = FakeRt::new([]);
        let uri: Uri = "https://example.invalid/".parse().unwrap();

        let _ = bounded_block_on(super::connect::<_, _, _, crate::proxy::NoProxy, NoHooks>(
            &rt,
            &dns,
            &NoOpTls,
            &[],
            None,
            &uri,
            &TcpOpts::default(),
            &[],
            &NegativeCache::default(),
            Duration::ZERO,
            Prefetched::NotConsulted,
            None,
        ))
        .expect_err("nothing answers");

        let events = dns.events();
        assert!(
            at(&events, "a:ask") < at(&events, "https:answer"),
            "the address query must be on the wire before the record comes \
             back, or the record's round trip is paid in front of it: {events:?}"
        );
        assert!(
            at(&events, "https:ask") < at(&events, "a:answer"),
            "and the record's query must be on the wire before the addresses \
             come back, or the two are serialised the other way round: {events:?}"
        );
    }

    /// The retry on the origin's own terms reuses the resolution the first
    /// attempt raced; it does not ask again.
    ///
    /// `attempt` has no resolver to ask with (see its doc comment), so this
    /// is structural — but the structure is only worth having if something
    /// notices when it is undone, and the cost of undoing it is invisible
    /// on a resolver that caches. Here nothing caches: a second `lookup_*`
    /// call shows up in the log as a second `call`/`ask` pair.
    ///
    /// The attempt log is asserted alongside on purpose. Without it "one
    /// lookup" would also pass for a connector that never retried at all,
    /// which is a different regression with the same symptom.
    #[test]
    fn the_retry_without_the_record_does_not_resolve_again() {
        let hint = std::net::Ipv4Addr::new(10, 0, 0, 7);
        let origin = v4(1);
        let dns = WatchedResolve::new(
            vec![origin],
            vec![hclient_dns::SvcbEndpoint {
                priority: 1,
                target: "example.invalid".to_string(),
                alpn: Vec::new(),
                port: None,
                ipv4hint: vec![hint],
                ipv6hint: Vec::new(),
                ech_config_list: None,
            }],
        );
        let rt = FakeRt::new([]);
        let uri: Uri = "https://example.invalid/".parse().unwrap();

        let _ = bounded_block_on(super::connect::<_, _, _, crate::proxy::NoProxy, NoHooks>(
            &rt,
            &dns,
            &NoOpTls,
            &[],
            None,
            &uri,
            &TcpOpts::default(),
            &[],
            &NegativeCache::default(),
            Duration::ZERO,
            Prefetched::NotConsulted,
            None,
        ))
        .expect_err("nothing answers");

        let tried: Vec<IpAddr> = rt.log.borrow().iter().map(|(ip, _)| *ip).collect();
        assert_eq!(
            tried,
            vec![IpAddr::V4(hint), origin, origin],
            "the record's endpoint failed and the origin's own was tried \
             again — without that second race there is nothing to check"
        );

        let events = dns.events();
        for (call, ask) in [("a:call", "a:ask"), ("aaaa:call", "aaaa:ask")] {
            assert_eq!(
                events.iter().filter(|e| **e == call).count(),
                1,
                "{call} happened more than once across two attempts: {events:?}"
            );
            assert_eq!(
                events.iter().filter(|e| **e == ask).count(),
                1,
                "{ask} happened more than once across two attempts: {events:?}"
            );
        }
    }

    // --- cancellation ---------------------------------------------------

    /// Whatever stream it was given, plus a note of when it was dropped.
    struct Recorded<S> {
        name: &'static str,
        dropped: Rc<RefCell<Vec<&'static str>>>,
        inner: S,
    }

    impl<S: Stream + Unpin> Stream for Recorded<S> {
        type Item = S::Item;
        fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            Pin::new(&mut self.get_mut().inner).poll_next(cx)
        }
    }

    impl<S> Drop for Recorded<S> {
        fn drop(&mut self) {
            self.dropped.borrow_mut().push(self.name);
        }
    }

    /// A resolver whose address queries answer nothing at once and whose
    /// HTTPS query never answers at all — and which says which of the three
    /// were dropped.
    ///
    /// **The address families end rather than hang, and that is the
    /// difference between a test and a wedged CI job.** The first version
    /// of this fixture hung all three, and the mutation "discovery is never
    /// consulted" then sent `connect` straight into `drive` with two
    /// streams that never finish and a `FakeSleep` that resolves at once —
    /// an infinite loop *inside a single poll*, which no watchdog in this
    /// module can interrupt (`bounded_block_on`'s exists for exactly this
    /// shape, and this test does not use it). With the families empty the
    /// same mutation makes `connect` return `Ready(Err)` on the first poll,
    /// and the assertion below is what fails — measured: red in 10 ms.
    struct HangingResolve {
        dropped: Rc<RefCell<Vec<&'static str>>>,
    }

    impl HangingResolve {
        fn record<S>(&self, name: &'static str, inner: S) -> Recorded<S> {
            Recorded {
                name,
                dropped: Rc::clone(&self.dropped),
                inner,
            }
        }
    }

    impl Resolve for HangingResolve {
        fn lookup_ipv6(&self, _: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>> {
            self.record("aaaa", futures_util::stream::empty())
        }
        fn lookup_ipv4(&self, _: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>> {
            self.record("a", futures_util::stream::empty())
        }
        fn supports_svcb(&self) -> bool {
            true
        }
        fn lookup_svcb(
            &self,
            _: &str,
        ) -> impl Stream<Item = Result<hclient_dns::SvcbEndpoint, Error>> {
            self.record("https", futures_util::stream::pending())
        }
    }

    /// A connect that is dropped takes all three queries with it.
    ///
    /// Worth its own test now that discovery is a concurrent branch rather
    /// than something awaited in front: the shape that would leak — spawn
    /// the HTTPS query, keep it, hand it to the next request — is exactly
    /// what a caller with a `connect` timeout would then be unable to
    /// cancel. This crate has no `Spawn` to leak through and the query
    /// borrows both the resolver and the host, so the property is also
    /// structural; the test is what makes it observable.
    ///
    /// One poll gets all three queries created and in flight
    /// (`alongside_address_lookups` pumps both address streams and then
    /// polls discovery, all on the first round), and the HTTPS one never
    /// answers, so the future is still pending when it is dropped.
    #[test]
    fn a_dropped_connect_drops_the_record_query_with_the_address_ones() {
        let dropped = Rc::new(RefCell::new(Vec::new()));
        let dns = HangingResolve {
            dropped: Rc::clone(&dropped),
        };
        let rt = FakeRt::new([]);
        let uri: Uri = "https://example.invalid/".parse().unwrap();
        let cache = NegativeCache::default();
        let opts = TcpOpts::default();

        {
            let mut fut =
                std::pin::pin!(super::connect::<_, _, _, crate::proxy::NoProxy, NoHooks>(
                    &rt,
                    &dns,
                    &NoOpTls,
                    &[],
                    None,
                    &uri,
                    &opts,
                    &[],
                    &cache,
                    Duration::ZERO,
                    Prefetched::NotConsulted,
                    None,
                ));
            let mut cx = Context::from_waker(std::task::Waker::noop());
            assert!(
                fut.as_mut().poll(&mut cx).is_pending(),
                "the HTTPS query never answers, so the connect cannot have finished"
            );
        }

        let mut seen = dropped.borrow().clone();
        seen.sort_unstable();
        assert_eq!(
            seen,
            vec!["a", "aaaa", "https"],
            "every query must be dropped with the connect that started it"
        );
    }

    /// Doesn't encrypt anything — it only proves that `connect` genuinely
    /// calls `TlsConnect::connect` for `https` and doesn't call it for
    /// `http`. The same technique as `NoOpTls` in `hclient_tls`'s own
    /// tests.
    struct NoOpTls;
    impl hclient_tls::TlsIdentity for NoOpTls {
        /// One stub, one configuration, therefore one identity — drawn
        /// once into a `OnceLock` rather than freshly on every call, which
        /// is the contract `TlsIdentity::config_id` states and which a
        /// `NoOpTls::new_unique()` here would quietly break for anyone who
        /// copied this stub.
        fn config_id(&self) -> hclient_tls::TlsConfigId {
            static ID: OnceLock<hclient_tls::TlsConfigId> = OnceLock::new();
            *ID.get_or_init(hclient_tls::TlsConfigId::new_unique)
        }
    }

    impl TlsConnect for NoOpTls {
        type Stream<S>
            = S
        where
            S: Read + Write + Unpin;

        async fn connect<S>(&self, io: S, req: TlsRequest<'_>) -> Result<(S, TlsInfo), Error>
        where
            S: Read + Write + Unpin,
        {
            Ok((
                io,
                TlsInfo {
                    alpn: req.alpn.first().map(|p| p.to_vec()),
                    ..Default::default()
                },
            ))
        }
    }

    fn live_listener_addr() -> SocketAddr {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            let _ = l.accept();
        });
        addr
    }

    /// Every other `connect()` test uses a single-address resolver, so
    /// success always returns via the
    /// `Exhausted` branch's post-drain loop — a mutation that broke ONLY
    /// the `Wait` branch's `Event::Attempt(Some(Ok(s))) => return Ok(s)`
    /// (see `drive`) would be invisible through `connect()`, even though
    /// `race_connect`'s own
    /// `race_connect_never_requires_send_even_through_the_wait_path`
    /// already catches that exact mutation. Same shape as that test: two
    /// v6 addresses plus one live v4 address. RFC 8305 interleaving starts
    /// v6(1) first, then v4 (second, not third) — so the second v6
    /// address is STILL queued when the v4 attempt succeeds,
    /// `Scheduler::poll`'s `Exhausted` condition
    /// (`v6.is_empty() && v4.is_empty() && ..`) is therefore not met yet,
    /// and the success can only be observed via `Wait`.
    #[test]
    fn connect_returns_success_via_the_wait_branch_not_only_via_exhausted() {
        let live = live_listener_addr();
        let dns = StaticResolve {
            v6: vec![v6(1), v6(2)],
            v4: vec![live.ip()],
        };
        let uri: Uri = format!("http://example.invalid:{}/", live.port())
            .parse()
            .unwrap();
        let rt = FakeRt::new([(live.ip(), true)]);
        let (conn, _info, _facts) =
            bounded_block_on(super::connect::<_, _, _, crate::proxy::NoProxy, NoHooks>(
                &rt,
                &dns,
                &NoOpTls,
                &[],
                None,
                &uri,
                &TcpOpts::default(),
                &[],
                &NegativeCache::default(),
                Duration::ZERO,
                Prefetched::NotConsulted,
                None,
            ))
            .expect("the v4 address must win the race");
        assert!(matches!(conn, Conn::Plain(_)));
        // v6(2) never got its turn — the race stopped as soon as v4 won.
        assert_eq!(
            rt.log.borrow().len(),
            2,
            "v6(1) (dead), then a live v4 — v6(2) never started"
        );
    }

    #[test]
    fn connect_uses_plain_for_http_and_reports_no_tls_info() {
        let addr = live_listener_addr();
        let dns = StaticResolve {
            v6: vec![],
            v4: vec![addr.ip()],
        };
        let uri: Uri = format!("http://example.invalid:{}/", addr.port())
            .parse()
            .unwrap();
        let (conn, info, _facts) =
            bounded_block_on(super::connect::<_, _, _, crate::proxy::NoProxy, NoHooks>(
                &FakeRt::new([(addr.ip(), true)]),
                &dns,
                &NoOpTls,
                &[],
                None,
                &uri,
                &TcpOpts::default(),
                &[],
                &NegativeCache::default(),
                Duration::ZERO,
                Prefetched::NotConsulted,
                None,
            ))
            .expect("connect");
        assert!(matches!(conn, Conn::Plain(_)));
        assert!(info.is_none());
    }

    #[test]
    fn connect_uses_tls_for_https_and_returns_tls_info() {
        let addr = live_listener_addr();
        let dns = StaticResolve {
            v6: vec![],
            v4: vec![addr.ip()],
        };
        let uri: Uri = format!("https://example.invalid:{}/", addr.port())
            .parse()
            .unwrap();
        let alpn: [&[u8]; 1] = [b"h2"];
        let (conn, info, _facts) =
            bounded_block_on(super::connect::<_, _, _, crate::proxy::NoProxy, NoHooks>(
                &FakeRt::new([(addr.ip(), true)]),
                &dns,
                &NoOpTls,
                &[],
                None,
                &uri,
                &TcpOpts::default(),
                &alpn,
                &NegativeCache::default(),
                Duration::ZERO,
                Prefetched::NotConsulted,
                None,
            ))
            .expect("connect");
        assert!(matches!(conn, Conn::Tls(_)));
        assert_eq!(info.unwrap().alpn.as_deref(), Some(b"h2".as_slice()));
    }

    #[test]
    fn connect_rejects_an_unsupported_scheme() {
        let dns = StaticResolve {
            v6: vec![],
            v4: vec![],
        };
        let uri: Uri = "ftp://example.invalid/".parse().unwrap();
        let err = bounded_block_on(super::connect::<_, _, _, crate::proxy::NoProxy, NoHooks>(
            &FakeRt::new([]),
            &dns,
            &NoOpTls,
            &[],
            None,
            &uri,
            &TcpOpts::default(),
            &[],
            &NegativeCache::default(),
            Duration::ZERO,
            Prefetched::NotConsulted,
            None,
        ))
        .expect_err("ftp isn't supported");
        assert_eq!(err.kind(), &ErrorKind::Unsupported);
    }

    #[test]
    fn connect_defaults_the_port_from_the_scheme_when_absent() {
        // No port given explicitly in the URI — the scheme's default is
        // used (https -> 443). There's no real server on 443 in the test
        // environment and there doesn't need to be: we're only checking
        // that `connect` GOT AS FAR AS attempting a connection on the
        // right port, not that it stopped earlier on "can't determine a
        // port." `FakeRt` refusing any address is enough — we care about
        // the attempt log, not success.
        let dns = StaticResolve {
            v6: vec![],
            v4: vec![IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1))],
        };
        let uri: Uri = "https://example.invalid/".parse().unwrap();
        let rt = FakeRt::new([]);
        let _ = bounded_block_on(super::connect::<_, _, _, crate::proxy::NoProxy, NoHooks>(
            &rt,
            &dns,
            &NoOpTls,
            &[],
            None,
            &uri,
            &TcpOpts::default(),
            &[],
            &NegativeCache::default(),
            Duration::ZERO,
            Prefetched::NotConsulted,
            None,
        ));
        let log = rt.log.borrow();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].0, IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1)));
    }

    #[test]
    fn connect_rejects_a_uri_without_a_host() {
        let dns = StaticResolve {
            v6: vec![],
            v4: vec![],
        };
        // An `http::Uri` with no authority at all (origin-form).
        let uri: Uri = "/just/a/path".parse().unwrap();
        let err = bounded_block_on(super::connect::<_, _, _, crate::proxy::NoProxy, NoHooks>(
            &FakeRt::new([]),
            &dns,
            &NoOpTls,
            &[],
            None,
            &uri,
            &TcpOpts::default(),
            &[],
            &NegativeCache::default(),
            Duration::ZERO,
            Prefetched::NotConsulted,
            None,
        ))
        .expect_err("no host — nowhere to connect to");
        assert_eq!(err.kind(), &ErrorKind::Connect);
    }
}
