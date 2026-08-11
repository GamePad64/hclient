//! The observability seam: what a transport did, told to whoever asked.
//!
//! Today the answer to *"why was that request slow"* is "read the source".
//! A caller can see the response and its version and nothing else — not
//! whether a connection was made or reused, not what DNS cost, not why a
//! connection went away underneath them.
//!
//! # It reports; it does not steer
//!
//! [`Hooks::on`] returns `()`, and that is the whole of the contract.
//! There is no verdict a hook can hand back, so there is nothing for the
//! request path to branch on: this cannot grow into a second
//! [`Capabilities`](crate::Capabilities), where a caller's declaration
//! changes what the transport does. A hook that wants to change the
//! request has the request — that is `Client`'s business, one layer up,
//! and a return value here would move the decision to a place where
//! nobody could see it happen.
//!
//! # Zero cost when nobody is watching, and it is [`Hooks::WATCHING`]
//!
//! A hook is a type parameter, not a `Box<dyn Hooks>`, and the type a
//! caller who wants nothing gets is [`NoHooks`] — a zero-sized struct
//! whose `WATCHING` is `false`. Backends read that const **before** they
//! measure anything, so a build with no hook does not read a clock, does
//! not take an id from a counter, and has no branch left that a
//! monomorphised `NoHooks` cannot delete. The const is what makes that
//! structural: a runtime `if self.hooks.is_some()` would still cost a
//! branch, and would already have read the clock to have something to put
//! in it. `crates/http-ng-native/tests/hooks_cost.rs` measures it from
//! outside — a runtime whose clock counts its own reads.
//!
//! It is deliberately all-or-nothing rather than one const per event. A
//! hook that wanted only [`Closed`] would then skip the connect timings,
//! and the seam would gain four booleans that every backend has to read
//! correctly for the timings to stay honest. One const, one rule.
//!
//! # A panicking hook
//!
//! A panic in [`Hooks::on`] propagates to whoever polled the request or
//! the response body. It is deliberately not caught:
//! `std::panic::catch_unwind` needs `UnwindSafe`, which would become a
//! bound on the caller's own type, and it does nothing at all under
//! `panic = "abort"` — so catching would be a promise that holds in some
//! builds and not others. What backends owe instead is that a panic can
//! only unwind *out*, never leave a lock poisoned or a process aborted:
//!
//! - **No hook is called with a lock held.** `http-ng-native` emits from
//!   `Transport::execute` and from its response body, never from inside
//!   the connection pool's mutex. A hook that panics therefore cannot
//!   poison it, and a hook that blocks cannot stall a request that is not
//!   its own.
//! - **No hook is called from a `Drop` impl.** A panic there, during an
//!   unwind already in progress, aborts the process — and an
//!   observability seam that can abort a program is worse than no seam.
//!   The cost is written down where it lands: a connection dropped rather
//!   than finished (a cancelled request; one the pool evicts for age) gets
//!   no [`Closed`] event. That is a hole, and it is the one this rule
//!   buys.
//!
//! # Why `unversioned`
//!
//! One backend implements this today. The event set is derived from what
//! `http-ng-native` can actually observe, and the second and third
//! backends — `http-ng-h3`, and whatever W3 brings — will have facts this
//! vocabulary has no word for (a QUIC connection migrating; a transfer a
//! background session finished after the process died). Freezing it now
//! would freeze one backend's view. See this module's parent for what
//! `unversioned` promises.
use crate::Error;
use core::time::Duration;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};

/// Somewhere to send what a transport did.
///
/// Implemented by the application rather than by a backend — which makes
/// it the one trait in `unversioned` pointing the other way. A backend
/// *calls* it, and what it owes is written on [`Event`]'s variants.
///
/// **No `Send` bound, declared or implied** (P13, settled by construction
/// in `crates/http-ng-core/tests/shape.rs`). A hook is stored in a
/// transport and called from inside a response body's `poll_frame`, which
/// is not the shape any other seam here has: the body outlives
/// `Transport::execute`, so it holds the hook rather than borrowing it,
/// and a `Send` declared anywhere on that path would shut every
/// single-threaded runtime out of observability. It is inferred instead —
/// a transport with a `Send` hook stays `Send`, one with an `Rc` inside
/// its hook does not, and both implement `Transport`.
pub trait Hooks {
    /// Whether anything reads these events.
    ///
    /// Backends must check this before doing work whose only purpose is
    /// an event — reading a clock, taking a [`ConnectionId`]. `false` on
    /// [`NoHooks`] is what makes the whole seam vanish from a build that
    /// does not use it; see the module doc.
    ///
    /// Defaulted to `true`, so a hook that forgets the line gets events
    /// rather than silence. The costly default is the safe one here: the
    /// other way round, a caller's hook would compile and never fire.
    const WATCHING: bool = true;

    /// Something happened. Called synchronously, on the task driving the
    /// request.
    fn on(&self, event: Event<'_>);
}

/// The hook a caller who asked for nothing gets: `WATCHING` is `false`,
/// the type is zero-sized, and `on` has no body.
///
/// The default type parameter of `http_ng_native::Native`, so the
/// no-hooks build is the one that needs no words to ask for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NoHooks;

impl Hooks for NoHooks {
    const WATCHING: bool = false;
    fn on(&self, _event: Event<'_>) {}
}

/// So that a hook can be shared without the caller writing the
/// delegation. `WATCHING` is forwarded rather than defaulted, or an
/// `Arc<NoHooks>` would start watching by being wrapped.
impl<H: Hooks + ?Sized> Hooks for std::sync::Arc<H> {
    const WATCHING: bool = H::WATCHING;
    fn on(&self, event: Event<'_>) {
        (**self).on(event);
    }
}

/// The single-threaded half of the impl above, and not decoration: it is
/// the shape P13 asks about. A hook behind an `Rc` makes the transport
/// holding it `!Send`, and everything still compiles — see
/// `crates/http-ng-core/tests/shape.rs`.
impl<H: Hooks + ?Sized> Hooks for std::rc::Rc<H> {
    const WATCHING: bool = H::WATCHING;
    fn on(&self, event: Event<'_>) {
        (**self).on(event);
    }
}

/// What a transport reports.
///
/// Not `#[non_exhaustive]`, for `Message`'s reason (see
/// [`Message`](crate::unversioned::Message)): nothing here is published,
/// so a new variant costs a rebase inside this workspace, and a compile
/// error is what an implementer of [`Hooks`] should get when the
/// vocabulary grows — rather than a silently ignored fact.
///
/// **There is deliberately no "request queued" variant.** The original
/// list for this work had one, and `http-ng-native` has nothing to put in
/// it: a request that finds no live pooled connection dials a fresh one,
/// there is no per-origin connection limit to wait behind, and an h2
/// connection is checked out of the pool exclusively — one stream at a
/// time — so `SendRequest::poll_ready` never waits for a stream of ours.
/// A variant no code can emit is a capability that lies, which is the
/// defect this workspace has caught four times; it belongs here when a
/// backend has a queue, and `http-ng-urlsession` (W3) will.
#[derive(Debug)]
pub enum Event<'a> {
    /// A connection was made. See [`Connected`] for what each duration
    /// means and, more to the point, what it does not.
    Connected(Connected<'a>),
    /// A connection somebody else already made is being used again.
    Reused(Reused<'a>),
    /// The response head arrived.
    Head(Head<'a>),
    /// A connection ended, and why.
    Closed(Closed<'a>),
}

/// Which connection an event is about.
///
/// Process-wide and monotonic, so a [`Closed`] can be matched to the
/// [`Connected`] that opened the same socket — which is the only reason
/// it exists. Without it a close event says "a connection ended" and a
/// caller holding several has no way to learn which.
///
/// [`ConnectionId::UNWATCHED`] is what a transport uses when
/// [`Hooks::WATCHING`] is `false`: the counter is not touched, because
/// touching it would be an atomic per connection bought for nobody.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConnectionId(u64);

static NEXT_CONNECTION_ID: AtomicU64 = AtomicU64::new(1);

impl ConnectionId {
    /// The id of a connection nobody asked about. Never returned by
    /// [`ConnectionId::next`], so an event carrying it is unambiguous
    /// rather than colliding with the first real connection.
    pub const UNWATCHED: ConnectionId = ConnectionId(0);

    /// The next id. `Relaxed`: this counter orders nothing, it only has
    /// to hand out distinct numbers.
    pub fn next() -> Self {
        Self(NEXT_CONNECTION_ID.fetch_add(1, Ordering::Relaxed))
    }

    /// The number itself, for a log line.
    pub fn get(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A connection was established.
#[derive(Debug)]
pub struct Connected<'a> {
    pub id: ConnectionId,
    /// The URI whose request paid for this connection, as the transport
    /// received it — absolute, before any protocol rewrote it.
    pub uri: &'a http::Uri,
    /// The address that answered. One of possibly several tried:
    /// RFC 8305 races address families, and this is the winner, not the
    /// first candidate.
    pub remote: SocketAddr,
    /// What will be spoken on it, as negotiated — not as offered.
    pub version: http::Version,
    pub timing: ConnectTiming,
}

/// What each phase of a connect cost.
///
/// **The three phases are three measurements, not a decomposition.**
/// `dns + tcp + tls <= total` always holds — they are disjoint intervals
/// inside the whole — but the remainder is real time belonging to none of
/// them: RFC 8305 staggers connection attempts, so a winner that started
/// 250 ms into the race spent that stagger in no phase at all, and a
/// first attempt made through an HTTPS record's hints and failed is time
/// the second attempt's phases do not contain either.
///
/// Every duration is measured on the transport's own `Timer` — the same
/// clock its timeouts and its pool deadlines use, so a test under
/// `tokio::time::pause()` sees one consistent story rather than two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectTiming {
    /// From the start of the connect to the moment the first address
    /// could be tried.
    ///
    /// A DNS figure in the honest sense — everything waited for is a DNS
    /// answer — but not only an A/AAAA one: an HTTPS record (RFC 9460) is
    /// looked up beside the addresses and its hints are tried first, so a
    /// slow record delays the first attempt and shows up here.
    pub dns: Duration,
    /// The winning TCP attempt, from the moment it was launched to the
    /// moment it connected. Not the race: an attempt the scheduler
    /// started earlier and that lost is not in this number.
    pub tcp: Duration,
    /// The TLS handshake, or `None` for a connection that has none —
    /// which is the honest answer for `http://`, where a zero would read
    /// as an instant handshake.
    pub tls: Option<Duration>,
    /// The whole connect, from the transport asking for a connection to
    /// having one: DNS, every attempt including those that failed, and
    /// TLS.
    pub total: Duration,
}

/// A connection was taken from the pool instead of being made.
///
/// The counterpart of [`Connected`], and the fact a caller cannot
/// otherwise get at: two requests to one origin either cost one
/// connection or two, and only the transport knows which happened.
#[derive(Debug)]
pub struct Reused<'a> {
    /// The id this connection was given when it was made — so the
    /// [`Connected`] it belongs to is findable.
    pub id: ConnectionId,
    pub uri: &'a http::Uri,
    /// What is spoken on it. A pooled connection is keyed on its
    /// protocol, so this is what the connection negotiated when it was
    /// made, not a guess.
    pub version: http::Version,
}

/// The response head arrived.
#[derive(Debug)]
pub struct Head<'a> {
    pub id: ConnectionId,
    pub uri: &'a http::Uri,
    pub status: http::StatusCode,
    pub version: http::Version,
    /// From the transport receiving the request to the head being read.
    ///
    /// It contains the connect when there was one, which is the point:
    /// the pair (`Head::elapsed`, `ConnectTiming::total`) is what answers
    /// "was it the connection or was it the server".
    pub elapsed: Duration,
}

/// A connection ended.
#[derive(Debug)]
pub struct Closed<'a> {
    pub id: ConnectionId,
    pub reason: CloseReason<'a>,
}

/// Why a connection ended.
///
/// Three, because three is what the code can tell apart. It deliberately
/// does not include "the caller dropped it": that would have to be
/// reported from a `Drop` impl, and the module doc says why no hook is
/// ever called from one.
#[derive(Debug)]
pub enum CloseReason<'a> {
    /// The exchange finished it: the peer closed the connection after the
    /// response, or the response said `Connection: close`. Nothing went
    /// wrong — there is simply no second request to be had on it.
    Ended,
    /// It was taken from the pool for a new request and turned out to
    /// have been closed by the peer while it sat idle.
    ///
    /// The reason a request that did nothing wrong sometimes pays for a
    /// connect: this is the event that explains the [`Connected`]
    /// following it.
    Stale,
    /// It failed, and here is the failure.
    Failed(&'a Error),
}
