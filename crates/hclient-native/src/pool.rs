//! Connection reuse for [`crate::Native`] (v0.2 W2).
//!
//! # Nobody polls an idle connection, and that is the design
//!
//! hyper's `Connection` is a future someone has to poll or bytes stop
//! moving. Today that someone is the request future itself ([`crate::h1`]'s
//! module doc). Between two pooled requests there is no request future, so
//! either something polls it or nothing does — and this transport is
//! deliberately built **without `Spawn`**, so nothing does.
//!
//! **Why there is no reaper, corrected.** This paragraph used to say that
//! a pool driven by a spawned task "does not compile at all on this seam",
//! because `hclient_rt::Spawn<F>` requires `F: Send + 'static` and this
//! vertical's IO is not `Send`. **That was measured and is false.**
//! `hclient_rt::Spawn<F>` requires nothing — the trait has zero bounds, and
//! its own doc comment says why; `Send + 'static` is added by two *impls*.
//! And a reaper over this pool is `Send` whenever the connection is,
//! because [`Pool`] is an `Arc<Inner<I>>` around a `Mutex`, so it compiles
//! on the shipped `Tokio` and the shipped `Smol` for the shipped
//! connection type — measured against a real socket, with the server
//! observing the close.
//!
//! **How the mistake was made, because it is the reusable part.** The
//! reasoning went from `connect.rs`'s `FakeStream` — a *test* stub holding
//! an `Rc<()>` — to the shipped path. The stub holds that `Rc` precisely
//! to prove that no path here *requires* `Send`; from "the test does not
//! require it" was inferred "production cannot have it". A fixture built
//! to prove an absence is not evidence about presence, and this project
//! writes such things down where they happened.
//!
//! **The reason that does hold, and it is the project's own rule.**
//! [`crate::Native`] is generic over `R`, and not every `R` has a `Spawn`
//! impl at all — `connect.rs`'s own `FakeRt` has none, and W7's embassy
//! survey expects none either. A reaper started by `Native::new` would
//! therefore assume a capability the type parameter does not promise: **a
//! default stronger than the truth**, which is the one thing this crate
//! refuses everywhere else. So there is no reaper *by default* — and there
//! is one on request: [`crate::Native::with_reaper`], bounded on
//! `R: Spawn<Reaper<R, I>>`, so that a runtime which cannot spawn is a
//! compile error where the caller wrote it rather than a reaper that never
//! fires.
//!
//! One piece of it landed before the reaper itself:
//! `hclient_rt_tokio::TokioHandle`, which carries a
//! `tokio::runtime::Handle` instead of reading one out of a thread-local,
//! so its `Spawn` works off a runtime thread — which is where a client is
//! usually constructed. The shipped ZST `Tokio` panics there. That would
//! otherwise be a second default stronger than the truth, hidden inside
//! the first.
//!
//! **And a third one, which no bound can catch: a spawner nobody drives.**
//! `Spawn::spawn` returns `()`. An executor that is never `run` accepts
//! this task, drops it, and cannot report that it did; the sockets then sit
//! open exactly as they would with no reaper, and the type system is happy
//! throughout. **A reaper is at most as good as the executor under it** —
//! written down here and on [`Reaper`] rather than left to be rediscovered
//! from a socket count.
//!
//! Two things the correction does *not* change. Making `Spawn` a
//! *requirement* of `Native` would still lose the property that
//! `tests/h1.rs::works_on_a_bare_futures_executor_with_no_spawn` and
//! `hclient/tests/two_runtimes.rs` exist to hold — hence opt-in rather
//! than mandatory. And the checkout poll below is still what makes the
//! pool correct; a reaper is a resource optimisation on top of it, not a
//! replacement for it.
//!
//! What replaces it *by default* is a **check at checkout**: before a
//! pooled connection is used, [`crate::h1::is_reusable`] polls its
//! `Connection` exactly once and asks `SendRequest::poll_ready`. One poll
//! is enough to see a server that closed the socket while it was idle,
//! because a reactor's readiness is remembered rather than delivered: the
//! `FIN` sits in the kernel and in the reactor's state until somebody
//! reads it, and the first poll with a live waker does. On a bare executor
//! with no reactor at all it is even more direct — the poll reads the
//! socket.
//!
//! Three consequences, all real, all deliberate, none of them hidden:
//!
//! 1. **An idle connection reads nothing.** Between requests we notice
//!    neither a `FIN` nor anything a server might send unbidden. For an
//!    idle HTTP/1 connection the former is the only thing that happens, and
//!    checkout catches it.
//! 2. **By default the idle timeout is a filter, not a reaper.** With
//!    nothing polling in the background, there is nobody to close a
//!    connection when its time is up. [`PoolConfig::idle_timeout`]
//!    therefore means "do not hand out a connection older than this", not
//!    "close a connection older than this": a client that goes quiet for
//!    an hour leaves its sockets open until it makes another request or
//!    the `Native` is dropped. This is the price of the default having no
//!    spawner — not, as this file used to say, of a reaper being
//!    impossible — and [`crate::Native::with_reaper`] is what buys the
//!    second meaning back for a runtime that can spawn. Measured on a real
//!    socket with the server watching its own end close, under a **300 ms**
//!    idle timeout: **299.7 ms** after the response on the shipped `Tokio`
//!    and **300.6 ms** on the shipped `Smol`, against a control differing
//!    in that one call which still held the socket 1200 ms later
//!    (`tests/reaper.rs`).
//!
//!    **This stops applying to a shared entry, and the reason is not a
//!    reaper** ([`crate::Native::multiplexed`], v0.4). A shared entry is a
//!    `SendRequest` clone, and h2 ends a connection's driver when the last
//!    clone of its sender is dropped — so **dropping the pooled entry is
//!    the close**, and [`Pool::take`]'s existing "drop the expired entries
//!    you walk past" therefore closes a socket rather than merely
//!    declining to offer it. It still takes a request to that origin to
//!    walk past them, which is what a reaper is still for. The sentence
//!    this inverts is worth keeping in view: a pooled *exclusive*
//!    connection is inert and a request revives it, where a shared one is
//!    running and the pool entry is its owner.
//! 3. **A race remains.** A server may close between our check and our
//!    write. That window cannot be closed by any HTTP/1 pool — hyper's own
//!    has it too — so it is handled rather than prevented: see the retry in
//!    `Native::execute`.
//!
//!    **How far the retry reaches, corrected twice.** This paragraph used
//!    to end "which is the reason this pool does not make previously
//!    reliable requests fail intermittently", and that is true of the
//!    window the retry covers and false past it. The retry fires on
//!    [`crate::established::Failed::NotSent`] — hyper handing the request
//!    back because not a byte of it reached the wire. If the server's close
//!    lands *after* the request goes out, its kernel answers `RST`, hyper
//!    reports the failure with the request already sent, and the caller
//!    gets an error a client without a pool would not have had. That is
//!    deliberate rather than missing: repeating a request a server may have
//!    acted on is at-least-once, and choosing it selectively would need a
//!    notion of method safety this codebase does not have (see
//!    `docs/h3-research.md` §3.5, which declines the same notion for
//!    0-RTT). Measured, because the correction came from a real failure —
//!    a fixture whose server closes 150 ms after its last response fails
//!    the next request with `Connect / hyper::Error(Io, ConnectionReset)`,
//!    five runs out of five; `docs/v02-acceptance.md` has the whole
//!    run-down, including why `tests/pool.rs` now waits for the server to
//!    say it has closed instead of sleeping and hoping.
//!
//!    **The second correction moved the line, and it moved it in our
//!    favour.** "Not a byte of it reached the wire" is the rule and it
//!    still is; what was wrong was where this crate believed that stopped
//!    being knowable. The window has **three** points, not two: our own
//!    look, hyper's first read with the request still queued, and hyper's
//!    read after it has written. At the middle one hyper refuses to write
//!    at all — a closed read side makes `can_write_head()` false — and
//!    leaves the request whole in its queue, from where `Envelope::drop`
//!    hands it back the moment the connection is dropped.
//!    [`crate::h1::claim_back`] asks for it, so that point is retried like
//!    the first rather than reported like the third. The third is
//!    unchanged, and the paragraph above is about it.
//!    `docs/pooled-reuse-race.md` has both reproductions, the sweep from
//!    outside the client, and why the two expensive fixes named in
//!    `docs/nagle-and-nodelay.md` §6 are still refused.
//!
//! # What is in the key, and why each part is there
//!
//! `PoolKey` is the v0.2 design document's `(scheme, host, port,
//! negotiated ALPN, TLS configuration identity)`, with scheme and TLS
//! identity folded into one field because they are not independent:
//!
//! | design document | here |
//! |---|---|
//! | scheme | [`Security`]'s discriminant — `Plaintext` is `http://`, `Tls` is `https://` |
//! | TLS configuration identity | [`Security::Tls`]'s [`TlsConfigId`] |
//! | host | [`PoolKey::host`], ASCII-lowercased |
//! | port | [`PoolKey::port`], with the scheme's default already applied |
//! | negotiated ALPN | [`Protocol`] |
//!
//! **TLS identity is the one that is a security property rather than a
//! performance one.** Two clients with different roots or different client
//! certificates sharing a socket is a defect, not a missed optimisation.
//! Worth being exact about what this field does today, because it is less
//! than it looks: a pool lives inside one [`crate::Native`], and one
//! `Native` owns exactly one `TlsConnect`, so within a single pool this
//! component is a constant, and two clients with different roots are
//! already kept apart structurally — different transports, different pools.
//! The field is here for the moment that stops being true. A pool shared
//! across clients is the obvious next step for anyone optimising this, and
//! at that moment a key without this component is a silent trust leak with
//! nothing in the type system to catch it. It is much cheaper to have it
//! now, with `TlsConnect::config_id` already answered by every
//! implementation, than to add it after the sharing.
//!
//! **[`Protocol`] keeps the two kinds of connection in separate buckets**
//! — `Native` speaks HTTP/1.1 always and HTTP/2 when ALPN selected it (the
//! `http2` feature, v0.2 W3). The component was added in W2, when it had
//! one variant and the lookup was constant, precisely so that W3 could add
//! the second one without anything else in this file changing; the guard
//! that keeps it honest is in `Native::execute`, which refuses to pool a
//! connection at all when the *negotiated* ALPN is neither of the two
//! protocols this transport speaks.
//!
//! Worth being exact about what it buys, because it is tempting to
//! overstate: it is **not** what stops an HTTP/2 socket being spoken to as
//! HTTP/1. That is impossible for a structural reason instead — a pooled
//! connection is an [`Established`] enum that carries its own protocol,
//! and `established::exchange` both dispatches and shapes the request off
//! that one value, so a connection cannot be addressed as something it is
//! not. This component is a *preference*: it lets checkout ask for the
//! bucket a request would rather have, instead of taking whatever is on
//! top. That is worth two hash lookups, and it is the honest size of the
//! claim.
//!
//! # What an h2 connection is checked out for
//!
//! **One stream at a time by default — an h2 connection is handed out
//! exclusively, exactly as an h1 one is, unless
//! [`crate::Native::multiplexed`] was asked for.** HTTP/2 multiplexes and
//! this pool does not use that by default, which is a decision rather than
//! an omission, and this is where it is written down. The opt-in is v0.4's
//! and is described at the end of this section; everything between here
//! and there is about the default, which did not move.
//!
//! The reason is the same `Spawn` that shapes everything else here.
//! Multiplexing means several requests share one `h2::client::Connection`,
//! and that connection is a future somebody has to poll. With no
//! background task, the only candidates are the in-flight request futures
//! themselves — so the connection would have to be shared behind a lock,
//! with wakers fanned out to every stream on it, and a request whose
//! caller stopped polling would stop driving the connection its
//! *neighbours* are waiting on. That trades HTTP/1's honest cost (one
//! connection per concurrent request) for a failure mode in which one
//! slow consumer wedges unrelated requests. Not worth it here; a client
//! that wants concurrency to the same origin gets a second connection,
//! same as on h1.
//!
//! That argument survives the correction above unchanged, because it never
//! rested on "no spawner can exist" — only on "the default has none". A
//! build that opts into a spawner could multiplex; it would then owe W1's
//! rule an implementation, which today it gets for free.
//!
//! **That sentence has been investigated and then built, and
//! `docs/h2-multiplexing.md` is both.** [`crate::Native::multiplexed`] is
//! the opt-in; what it changes about *this* file is three things.
//!
//! **The verb.** `take` is right for an exclusive connection and wrong for
//! a shared one: a shared entry is **borrowed**, never taken — the pool
//! keeps it and hands out a clone — so [`CheckIn`] is not used on that
//! path at all, and `H2Body::hand_back_to_pool` is a no-op there because
//! `exchange_shared` builds a body with no `reuse` to hand back. That is a
//! subtraction: the whole `Reuse { checkin, sender }` dance in `http2.rs`
//! exists to survive an exclusive check-out. [`Pool::take`] is where both
//! verbs live, decided by the entry rather than by a flag, and
//! [`Pool::forget_shared`] is the other half — a borrowed clone that turns
//! out to be dead leaves its entry behind, and a checkout that only
//! dropped the clone would borrow the same dead connection for ever.
//!
//! **Liveness stops being a checkout-time poll.** The paragraph above says
//! one poll is the only moment a `GOAWAY` is noticed *because nothing
//! polls an idle connection*; on a shared entry something does, and a
//! `GOAWAY` that had arrived 100 µs — 100 ms, measured — earlier is
//! reported by the first `poll_ready` 20 µs later. So
//! `http2::shared_is_reusable` is one `poll_ready` and no `Connection`
//! poll.
//!
//! **A connect becomes single-flight**, and that is not an optimisation:
//! a burst of N concurrent requests to a cold origin has no first request
//! to share from, so without [`Pool::begin_connect`] all N connect and
//! each shared connection is shared with nobody.
//!
//! One sentence above is also very slightly stronger than it needs to be,
//! and it is the one about a lock: while *any* holder is being polled the
//! connection moves, so the lock shape fails only when every caller stalls
//! at once. It is still the wrong shape, for a reason that is about what
//! it cannot do — an idle connection has no holder at all, so it fixes
//! neither the unanswered `PING` nor the unsent `RST_STREAM`; §7 there.
//!
//! Two things follow, and the second one has to be said out loud because
//! it is a consequence of *this policy* rather than of the h2 code:
//!
//! - Concurrency to one origin costs connections, not streams, and
//!   [`PoolConfig::max_idle_per_key`] bounds the idle ones exactly as
//!   before.
//! - W1's rule — cancelling one stream must not tear down the others
//!   sharing its connection — holds trivially, because there are no
//!   others. **That is this policy's guarantee, not `http2.rs`'s.**
//!   Whoever makes check-out non-exclusive takes the rule with them; see
//!   `crate::http2`'s module doc, which says the same thing from the other
//!   end. Under [`crate::Native::multiplexed`] there *are* others, so the
//!   rule stops holding for free and is a test instead —
//!   `tests/http2.rs`'s
//!   `dropping_one_exchange_leaves_a_concurrent_one_alone_on_a_shared_connection`,
//!   which is the same request pair as its exclusive sibling with
//!   `accepted == 1`.
//!
//! # `hclient-h3` does the opposite, and that is not a disagreement
//!
//! v0.3's HTTP/3 transport shares a connection and multiplexes requests on
//! it. Read next to the paragraphs above that looks like two crates
//! answering the same question differently; it is not, and the difference
//! is worth having in both places so that whoever changes one does not
//! import the other's justification along with it.
//!
//! The argument above is conditional, and states its condition: *with no
//! background task*, the only things that can drive a shared connection are
//! the in-flight request futures, so a caller that stops polling wedges its
//! neighbours. `hclient-h3` has a background task — not as an optimisation
//! but because it has no choice. A QUIC connection that nobody polls does
//! not idle, it dies: the PING that resets the peer's idle timer comes from
//! the connection's driver rather than from the kernel, so a pooled h3
//! connection with no driver is a pool that cannot be used. Its driver is
//! therefore spawned, and a driver that is nobody's request future cannot
//! be stalled by any request's polling behaviour.
//!
//! So W1's rule holds in both crates, for opposite reasons: here because
//! there are no neighbours, there because dropping one stream sends
//! `STOP_SENDING` for that stream alone. And the "a build that opts into a
//! spawner could multiplex" sentence above now has a worked example
//! sitting in the workspace — including the price it pays, which is a
//! `R: Spawn` bound on the transport.
use crate::established::Established;
use hclient_core::unversioned::Timer;
use hclient_tls::TlsConfigId;
use hyper::rt::{Read, Write};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

/// How this transport reuses connections.
///
/// **One setting, not two.** The v0.2 design document is explicit that an
/// idle timeout must live either in `hclient_core::Timeouts` or on the pool
/// and not in both places, and it lives here. `Timeouts` describes phases
/// of *one exchange* and travels with a request through
/// `http::Extensions`; how long a connection may sit idle *after* an
/// exchange is not a property of any request, and two requests carrying
/// different values for it would have no meaning. It would also cost a
/// fourth flag in `TimeoutSupport`, which the two ambient backends would
/// have to answer `false` to for a setting only one backend could ever
/// implement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolConfig {
    /// How long a connection may sit in the pool and still be handed out.
    ///
    /// Read the module doc first: with nothing polling in the background,
    /// this is a filter applied when a connection is taken out, not a timer
    /// that closes it.
    ///
    /// It is measured from when the connection was last **handed out**, not
    /// from when it went idle — the deadline is stamped by the one place
    /// that holds a clock, and the exchange's own duration is therefore
    /// counted against the budget. The error is always in the safe
    /// direction: the deadline is never later than the true idle deadline,
    /// only earlier, and by exactly the time the exchange took.
    pub idle_timeout: Duration,
    /// How many idle connections to keep per `PoolKey`.
    ///
    /// Bounded rather than unbounded: without a reaper (see the module
    /// doc), an unbounded pool that a burst of concurrent requests filled
    /// would hold every one of those sockets open until the process ended.
    /// Reaching the bound drops the *oldest* entry, which is also the one
    /// closest to its deadline.
    pub max_idle_per_key: usize,
}

impl Default for PoolConfig {
    /// 90 seconds and 8 connections per key.
    ///
    /// The idle timeout matches what the ecosystem converged on
    /// (`reqwest`'s `pool_idle_timeout` default) and sits comfortably under
    /// the keep-alive timeouts servers commonly use, so the common case is
    /// that we drop a connection before the server does rather than
    /// discovering its `FIN` at checkout.
    ///
    /// The per-key bound is ours: hyper's own pool leaves it unbounded,
    /// which is defensible when a reaper closes idle connections and is not
    /// defensible here (see [`PoolConfig::max_idle_per_key`]). Eight is
    /// enough that an ordinary burst of concurrent requests to one origin
    /// all find a warm connection on the next round, and small enough that
    /// a client which then goes quiet is not sitting on a hundred open
    /// sockets nobody will close.
    fn default() -> Self {
        Self {
            idle_timeout: Duration::from_secs(90),
            max_idle_per_key: 8,
        }
    }
}

/// Whether TLS was performed for a connection, and under which trust
/// configuration.
///
/// The scheme and the TLS configuration in one field because a plaintext
/// connection has no trust configuration to be identical to — an
/// `Option<TlsConfigId>` alongside a separate `scheme` field would carry
/// the same information with one combination (`https` with no identity)
/// that must never occur and nothing to stop it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Security {
    /// `http://`: no TLS, and nothing about trust to compare.
    Plaintext,
    /// `https://`, under the configuration this identity names — see
    /// [`TlsConfigId`].
    Tls(TlsConfigId),
}

/// The application protocol spoken on a connection.
///
/// Without the `http2` feature there is exactly one variant and the lookup
/// is constant — the guard in `Native::execute` is then what carries the
/// weight, by keeping a connection that negotiated anything else out of
/// the pool entirely. See the module doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Protocol {
    Http11,
    /// Negotiated by ALPN, never assumed: `Native` only speaks HTTP/2 on a
    /// connection whose TLS backend reported `h2`, which is also the only
    /// case in which it offered it (`TlsConnect::reports_alpn`).
    #[cfg(feature = "http2")]
    H2,
}

/// Which connections may serve which requests — see the module doc for the
/// component-by-component justification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PoolKey {
    security: Security,
    /// ASCII-lowercased. `Example.COM` and `example.com` name the same
    /// origin, and a key that told them apart would open two connections
    /// where one would do — a miss, not a defect, but a silly one.
    host: Box<str>,
    /// Explicit, never defaulted here: the scheme's default port is applied
    /// before the key is built, so `http://h/` and `http://h:80/` are one
    /// key rather than two.
    port: u16,
    protocol: Protocol,
    /// Which proxy this connection goes through, `"host:port"`, or `None`
    /// for a direct one.
    ///
    /// A **parameter** of [`PoolKey::new`] rather than something a builder
    /// method adds, so that a new call site cannot forget it.
    ///
    /// **Unreachable today, and kept for exactly the reason `security` is**
    /// — which `docs/v02-acceptance.md` already states about that field: a
    /// proxy is configured on the transport and each transport owns its
    /// own pool, so this is a constant within any one pool, as the TLS
    /// identity is. It must be in the key *before* a pool is shared
    /// between transports, because that is the moment its absence stops
    /// being a redundancy and becomes a security defect — a tunnel handed
    /// to a request routed through a different proxy. No test can reach
    /// it, and saying so is better than a test that pretends to.
    proxy: Option<Box<str>>,
}

impl PoolKey {
    pub(crate) fn new(
        security: Security,
        host: &str,
        port: u16,
        protocol: Protocol,
        proxy: Option<&str>,
    ) -> Self {
        Self {
            security,
            host: host.to_ascii_lowercase().into_boxed_str(),
            port,
            protocol,
            proxy: proxy.map(|p| p.to_ascii_lowercase().into_boxed_str()),
        }
    }
}

/// Everything an exchange needs in order to hand a connection back once
/// the response body has ended cleanly — or `None` at the call site, which
/// is how "do not reuse this connection" is said.
///
/// Lives here rather than in either protocol module because both hand
/// connections back through it, and because [`CheckIn::put`] is the only
/// way in: the pool's fields are not reachable from `h1.rs` or
/// `http2.rs`, so neither can invent a key or a deadline of its own.
pub(crate) struct CheckIn<I>
where
    I: Read + Write + Unpin,
{
    pool: Pool<I>,
    key: PoolKey,
    expires_at: Duration,
}

impl<I> CheckIn<I>
where
    I: Read + Write + Unpin,
{
    pub(crate) fn new(pool: Pool<I>, key: PoolKey, expires_at: Duration) -> Self {
        Self {
            pool,
            key,
            expires_at,
        }
    }

    /// Hands `est` back under the key and deadline this token was made
    /// with. Consuming, because a connection may only be checked in once.
    pub(crate) fn put(self, est: Established<I>) {
        self.pool.put(self.key, est, self.expires_at);
    }
}

/// A connection waiting to be used again.
struct Idle<I>
where
    I: Read + Write + Unpin,
{
    est: Established<I>,
    /// Elapsed time, measured on the owning transport's `Timer` from the
    /// instant that transport was constructed, past which this connection
    /// is not to be handed out. See [`PoolConfig::idle_timeout`] for why
    /// it is a deadline stamped at hand-out rather than at check-in.
    expires_at: Duration,
}

/// The idle connections of one [`crate::Native`], shared with every
/// response body that might hand one back.
///
/// Cheap to clone: an `Arc` bump. Every clone is the same pool, which is
/// the point — `Native::execute` takes connections out of it and the
/// `NativeBody` it returns puts one back, and the body outlives the call
/// that made it.
pub(crate) struct Pool<I>
where
    I: Read + Write + Unpin,
{
    inner: Arc<Inner<I>>,
}

struct Inner<I>
where
    I: Read + Write + Unpin,
{
    /// `None` — reuse is off. Not a separate `enabled: bool` next to a
    /// config that would then be present but meaningless: there is nothing
    /// to configure about a pool that does not exist, and
    /// `Capabilities::connection_reuse` is derived from this same
    /// `Option` rather than set alongside it, so the capability cannot
    /// drift from the behaviour.
    config: Option<PoolConfig>,
    idle: Mutex<HashMap<PoolKey, Vec<Idle<I>>>>,
    /// Which origins have a connect in flight, and who is waiting for it —
    /// see [`Pool::begin_connect`]. Empty, and never looked at, unless
    /// [`crate::Native::multiplexed`] was asked for.
    ///
    /// A second `Mutex` rather than a second field under the first: the
    /// two are locked at different moments and never together, and one
    /// lock held across both would put a checkout behind a connect that
    /// has not finished.
    #[cfg(feature = "http2")]
    connecting: Mutex<HashMap<PoolKey, Vec<std::task::Waker>>>,
}

/// Hand-written: `#[derive(Clone)]` would demand `I: Clone`, which no IO
/// type satisfies, for a field that is an `Arc`.
impl<I> Clone for Pool<I>
where
    I: Read + Write + Unpin,
{
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<I> std::fmt::Debug for Pool<I>
where
    I: Read + Write + Unpin,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pool")
            .field("config", &self.inner.config)
            .field(
                "idle_keys",
                &self.inner.idle.lock().map(|m| m.len()).unwrap_or(0),
            )
            .finish()
    }
}

impl<I> Pool<I>
where
    I: Read + Write + Unpin,
{
    pub(crate) fn new(config: Option<PoolConfig>) -> Self {
        Self {
            inner: Arc::new(Inner {
                config,
                idle: Mutex::new(HashMap::new()),
                #[cfg(feature = "http2")]
                connecting: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// `None` when reuse is off. The single source of truth for both the
    /// behaviour and `Capabilities::connection_reuse`.
    pub(crate) fn config(&self) -> Option<PoolConfig> {
        self.inner.config
    }

    /// A handle to this pool that does not keep it alive — see
    /// [`WeakPool`].
    pub(crate) fn downgrade(&self) -> WeakPool<I> {
        WeakPool {
            inner: Arc::downgrade(&self.inner),
        }
    }

    /// Drops every connection whose deadline has passed, and reports when
    /// the earliest surviving one falls due — `None` when nothing is left.
    ///
    /// This is the **only** function in this file that closes a connection
    /// nobody asked for; [`Pool::take`] drops expired entries too, but only
    /// the ones it walks past on its way to a live one, and only when a
    /// request is being made. It exists for [`Reaper`], and it takes `now`
    /// from the caller for the same reason `take` does: `hclient_core::
    /// Timer` is the seam through which time enters this crate.
    ///
    /// Returning the next deadline is what lets the reaper sleep exactly
    /// as long as it must instead of polling on a fixed interval — the
    /// difference between closing a connection at its deadline and closing
    /// it up to one interval late.
    pub(crate) fn reap(&self, now: Duration) -> Option<Duration> {
        let mut idle = self.inner.idle.lock().expect("connection pool poisoned");
        let mut next: Option<Duration> = None;
        idle.retain(|_, bucket| {
            // Dropped here, holding the lock, exactly as in `take`:
            // dropping a connection closes a socket, which does not block.
            bucket.retain(|e| e.expires_at > now);
            for e in bucket.iter() {
                next = Some(match next {
                    Some(n) => n.min(e.expires_at),
                    None => e.expires_at,
                });
            }
            !bucket.is_empty()
        });
        next
    }

    /// The freshest connection for `key` that has not passed its deadline,
    /// or `None`.
    ///
    /// LIFO: the most recently returned connection is the one most likely
    /// still to be alive at the far end, and the one furthest from its
    /// deadline. Entries that have passed their deadline are dropped as
    /// they are met — which is the only moment anything in this pool
    /// closes a connection on account of age, see the module doc.
    ///
    /// `now` is elapsed time on the owning transport's `Timer`, measured
    /// from the same instant `expires_at` was measured from. This function
    /// deliberately does not read a clock of its own: `hclient_core::Timer`
    /// is the seam through which time enters this crate, and a
    /// `std::time::Instant::now()` here would quietly disagree with a test
    /// running under `tokio::time::pause()`.
    /// # `take` is the verb for an exclusive connection and `borrow` for a
    /// shared one
    ///
    /// A [`crate::established::Established::H2Shared`] entry is a
    /// `SendRequest` clone, not a connection, so there is nothing
    /// exclusive to hand over: this **leaves the entry where it is** and
    /// returns a clone of it, and no [`CheckIn`] is minted for the way
    /// home because there is no way home to mint. Which of the two happens
    /// is asked of the entry ([`Established::borrowed`]) rather than of a
    /// flag on the pool, so a bucket that holds both kinds — a `Native`
    /// that starts sharing does not retire what an earlier configuration
    /// checked in — cannot be read wrong.
    ///
    /// **A borrowed entry's deadline is restamped**, and that is
    /// [`PoolConfig::idle_timeout`]'s own rule rather than an exception to
    /// it: the deadline is measured *"from when the connection was last
    /// handed out"*, and borrowing is handing out. Without the restamp a
    /// shared connection under continuous load would be dropped one idle
    /// timeout after the first request that ever used it.
    pub(crate) fn take(&self, key: &PoolKey, now: Duration) -> Option<Established<I>> {
        let renewed = self
            .inner
            .config
            .map(|cfg| now.saturating_add(cfg.idle_timeout));
        let mut idle = self.inner.idle.lock().expect("connection pool poisoned");
        let bucket = idle.get_mut(key)?;
        let taken = loop {
            let entry = bucket.last()?;
            if entry.expires_at <= now {
                // Dropped here, holding the lock: dropping a connection
                // closes a socket, which does not block. On a shared entry
                // that drop is the whole of the close — the driver ends
                // when the last `SendRequest` goes — where on an exclusive
                // one it drops a socket nobody was polling.
                bucket.pop();
                continue;
            }
            let Some(borrowed) = entry.est.borrowed() else {
                break bucket.pop().expect("the entry just looked at").est;
            };
            if let Some(expires_at) = renewed {
                bucket
                    .last_mut()
                    .expect("the entry just looked at")
                    .expires_at = expires_at;
            }
            break borrowed;
        };
        if bucket.is_empty() {
            idle.remove(key);
        }
        Some(taken)
    }

    /// Removes the shared entry a borrowed clone came from.
    ///
    /// The other half of borrowing, and it is not optional: a clone that
    /// [`crate::established::is_reusable`] rejects leaves the entry it was
    /// cloned from still in the pool, so a checkout loop that only dropped
    /// the clone would borrow the same dead connection for ever.
    ///
    /// Keyed on [`crate::http2::SharedId`] rather than on
    /// [`hclient_core::unversioned::ConnectionId`], because every
    /// connection in a build with no hook wears the same `UNWATCHED` id —
    /// see `SharedId`'s own doc.
    #[cfg(feature = "http2")]
    pub(crate) fn forget_shared(&self, key: &PoolKey, slot: crate::http2::SharedId) {
        let mut idle = self.inner.idle.lock().expect("connection pool poisoned");
        let Some(bucket) = idle.get_mut(key) else {
            return;
        };
        bucket.retain(|e| e.est.shared_slot() != Some(slot));
        if bucket.is_empty() {
            idle.remove(key);
        }
    }

    /// Offers a connection back to the pool.
    ///
    /// A no-op when reuse is off, so a caller does not have to ask first;
    /// the connection is then dropped, which closes it, which is exactly
    /// what a client without a pool should do with a finished connection.
    pub(crate) fn put(&self, key: PoolKey, est: Established<I>, expires_at: Duration) {
        let Some(cfg) = self.inner.config else {
            return;
        };
        if cfg.max_idle_per_key == 0 {
            return;
        }
        let mut idle = self.inner.idle.lock().expect("connection pool poisoned");
        let bucket = idle.entry(key).or_default();
        bucket.push(Idle { est, expires_at });
        // `>` and a single removal rather than a `while`: the bucket grew by
        // exactly one, so it can be over the bound by at most one.
        if bucket.len() > cfg.max_idle_per_key {
            bucket.remove(0);
        }
    }
}

/// A [`Pool`] the holder does not keep alive.
///
/// The reaper below must not be the reason a pool outlives its
/// [`crate::Native`]: a spawned task holding a `Pool` (an `Arc`) would keep
/// every idle socket open for the life of the process, which is the exact
/// opposite of what a reaper is for. It holds this instead, and the failed
/// upgrade is how it learns that the transport is gone and that it should
/// end.
pub(crate) struct WeakPool<I>
where
    I: Read + Write + Unpin,
{
    inner: std::sync::Weak<Inner<I>>,
}

impl<I> WeakPool<I>
where
    I: Read + Write + Unpin,
{
    fn upgrade(&self) -> Option<Pool<I>> {
        self.inner.upgrade().map(|inner| Pool { inner })
    }
}

/// The background task that closes idle connections when their deadline
/// passes — [`crate::Native::with_reaper`], and nothing else, starts one.
///
/// # Why this is a hand-written struct and not an `async` block
///
/// [`hclient_rt::Spawn<F>`](hclient_rt::Spawn) is hyper's `Executor<Fut>`
/// shape: the future is a type parameter **of the trait**, so a bound has
/// to name it — and an `async` block has no name (`E0308: expected type
/// parameter F, found async block`). That, and not `Send`, is what stood
/// behind the withdrawn claim that a spawned pool task "does not compile
/// on this seam"; see the module doc. Naming the future means writing it
/// out, and writing it out means the sleep has to be a field, which is why
/// [`hclient_core::unversioned::Timer`] carries an associated `Sleep` type.
///
/// # Why the state is behind a `Box`, and the sleep behind a second one
///
/// This workspace forbids `unsafe`, so there is no pin projection to be
/// had: the only way to poll a `!Unpin` field is to own it behind a
/// pointer, and the only way to take `&mut` at a field at all is for the
/// whole future to be `Unpin`. `tokio::time::Sleep` is `!Unpin` (smol's
/// is not, which is exactly why one of the two runtimes must not be
/// allowed to decide the shape), hence `Pin<Box<R::Sleep>>`; and
/// `R::Instant` could be anything at all, hence the outer box, which makes
/// `Reaper` `Unpin` for **every** `R` rather than for those whose private
/// types happen to be. The alternative was `R: Unpin, R::Instant: Unpin`
/// on a public constructor — a promise a caller cannot read off their own
/// runtime's documentation, to buy back one allocation per transport.
///
/// Both boxes hold **concrete** types, so nothing is erased and the auto
/// traits still pass through — the property `h1.rs`'s module doc is about.
///
/// # A reaper is at most as good as the executor under it
///
/// [`Spawn::spawn`](hclient_rt::Spawn::spawn) returns `()`. A spawner
/// whose executor nobody drives — an `async_executor::Executor` with no
/// `run` — accepts this task, drops it on the floor, and has no way of
/// saying so; the connections then sit open exactly as they would with no
/// reaper at all, and nothing anywhere reports it. That is a property of
/// the seam, not something this type can check, and it is written down
/// here because the alternative is someone discovering it from a socket
/// count in production.
pub struct Reaper<R, I>(Box<ReaperState<R, I>>)
where
    R: Timer,
    I: Read + Write + Unpin;

/// [`Reaper`]'s fields, boxed — see its doc comment for why they are not
/// simply its fields.
struct ReaperState<R, I>
where
    R: Timer,
    I: Read + Write + Unpin,
{
    rt: R,
    pool: WeakPool<I>,
    /// The same origin every [`Idle::expires_at`] is measured from — the
    /// owning transport's `epoch`. Passed in rather than read here: a
    /// second origin would make this task's arithmetic disagree with the
    /// pool's by however long the two constructions were apart.
    epoch: R::Instant,
    /// How long to wait when there is nothing in the pool to wait *for*.
    idle_timeout: Duration,
    /// `None` until the first poll. Nothing about this task touches the
    /// runtime before it is polled, which matters for the ZST
    /// `hclient_rt_tokio::Tokio`: its `sleep` reads the ambient runtime out
    /// of a thread-local and panics off a runtime thread, and a client is
    /// usually built off one. Constructing the sleep at first poll puts
    /// that call on whichever thread the executor polls from, which is a
    /// runtime thread by construction.
    sleep: Option<Pin<Box<R::Sleep>>>,
}

/// The shortest interval the reaper will ever wake at.
///
/// Without a floor, `PoolConfig { idle_timeout: Duration::ZERO, .. }` —
/// a legal way to say "never reuse anything" — would give an empty pool a
/// zero-length sleep, and this task would spin a core rather than reap
/// anything. One millisecond is far below any deadline worth reaping and
/// far above any wake rate worth worrying about.
const MIN_REAP_INTERVAL: Duration = Duration::from_millis(1);

impl<R, I> Reaper<R, I>
where
    R: Timer,
    I: Read + Write + Unpin,
{
    pub(crate) fn new(rt: R, pool: WeakPool<I>, epoch: R::Instant, idle_timeout: Duration) -> Self {
        Self(Box::new(ReaperState {
            rt,
            pool,
            epoch,
            idle_timeout,
            sleep: None,
        }))
    }
}

impl<R, I> std::fmt::Debug for Reaper<R, I>
where
    R: Timer,
    I: Read + Write + Unpin,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reaper")
            .field("idle_timeout", &self.0.idle_timeout)
            .field("pool_alive", &self.0.pool.upgrade().is_some())
            .finish()
    }
}

impl<R, I> Future for Reaper<R, I>
where
    R: Timer,
    I: Read + Write + Unpin,
{
    /// `()`, and it is reached exactly once: when the pool this watches is
    /// dropped. A reaper does not fail — there is nothing here that can go
    /// wrong that dropping a socket does not already answer — and it does
    /// not stop for any other reason, so the end of this future is the end
    /// of the transport.
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = &mut *self.get_mut().0;
        loop {
            if this.sleep.is_none() {
                this.sleep = Some(Box::pin(this.rt.sleep(this.idle_timeout)));
            }
            let sleep = this.sleep.as_mut().expect("just set");
            std::task::ready!(sleep.as_mut().poll(cx));

            // The transport is gone, and with it the last strong reference
            // to the pool: whatever was in it has already been dropped, so
            // there is nothing left to reap and nobody left to reap for.
            let Some(pool) = this.pool.upgrade() else {
                return Poll::Ready(());
            };
            let now = this.rt.elapsed_since(this.epoch);
            // Two answers in one call, deliberately: a separate "when is
            // the next deadline" query would read the pool a second time,
            // under a second lock, and could disagree with what the first
            // one had just removed.
            let wake = match pool.reap(now) {
                // Everything left expires later than `now`, so this is
                // positive; the floor is for the branch below.
                Some(next) => next.saturating_sub(now),
                // Nothing to wait for. `idle_timeout` is the soonest
                // anything checked in from here on could expire — near
                // enough: a connection is stamped when it is handed OUT,
                // so an exchange lasting `d` makes its deadline `d`
                // earlier than that, and the reap that follows this sleep
                // is late by at most one exchange.
                None => this.idle_timeout,
            };
            // Dropped before the next sleep so that this task holds no
            // strong reference to the pool while it waits — otherwise
            // "the transport was dropped" could never be observed.
            drop(pool);
            this.sleep = Some(Box::pin(this.rt.sleep(wake.max(MIN_REAP_INTERVAL))));
        }
    }
}

// ── One connect per origin at a time (v0.4, multiplexing only) ──────────

/// The single-flight half of sharing, and it is not an optimisation.
///
/// Sharing a connection means a *second* request finds the first one's
/// connection in the pool — and a burst of N concurrent requests to a cold
/// origin has no first one: all N look, all N find nothing, and all N
/// connect. The pool would then hold N shared connections and share each
/// with nobody, which is today's behaviour with a spawned task added.
///
/// So one request connects and the others wait for it. What they wait on
/// is this: a mark under the origin's `PoolKey`, taken before the
/// connect and released by [`Connecting`]'s `Drop` — so a connect that
/// fails, or a request whose future is dropped mid-connect, releases the
/// herd rather than stranding it.
///
/// **A waiter waits once.** After the wake it looks in the pool again, and
/// if there is still nothing there it takes the mark itself and connects.
/// That bounds the cost of a failed connect to one extra wait per waiter,
/// where a loop would let a permanently unreachable origin hold a request
/// for ever.
#[cfg(feature = "http2")]
impl<I> Pool<I>
where
    I: Read + Write + Unpin,
{
    /// Claim the right to open the connection for `key`, or `None` if
    /// somebody else already has it.
    pub(crate) fn begin_connect(&self, key: &PoolKey) -> Option<Connecting<I>> {
        let mut connecting = self
            .inner
            .connecting
            .lock()
            .expect("connection pool poisoned");
        if connecting.contains_key(key) {
            return None;
        }
        connecting.insert(key.clone(), Vec::new());
        Some(Connecting {
            pool: self.clone(),
            key: key.clone(),
        })
    }

    /// Resolves when the in-flight connect for `key` is over — at once
    /// when there is none, which is what makes this safe to call after a
    /// `begin_connect` that came back `None` and lost its race in between.
    pub(crate) fn wait_for_connect<'a>(&'a self, key: &'a PoolKey) -> WaitForConnect<'a, I> {
        WaitForConnect { pool: self, key }
    }

    /// Releases the mark and wakes everyone waiting on it — [`Connecting`]'s
    /// `Drop`, and nothing else.
    fn finish_connect(&self, key: &PoolKey) {
        let wakers = {
            let mut connecting = self
                .inner
                .connecting
                .lock()
                .expect("connection pool poisoned");
            connecting.remove(key)
        };
        // Woken outside the lock, for the reason every other wake in this
        // workspace is: a waker may run arbitrary code, and running it
        // under a lock a woken task then wants is how a deadlock is
        // written.
        for w in wakers.into_iter().flatten() {
            w.wake();
        }
    }
}

/// The right to be the one opening a connection to an origin — see
/// [`Pool::begin_connect`].
///
/// Held across the connect and dropped as soon as the connection is in the
/// pool, **not** for the rest of the request: what a waiter is waiting for
/// is a connection to exist, and making it wait for somebody else's
/// response as well would turn one slow server into a queue.
#[cfg(feature = "http2")]
pub(crate) struct Connecting<I>
where
    I: Read + Write + Unpin,
{
    pool: Pool<I>,
    key: PoolKey,
}

#[cfg(feature = "http2")]
impl<I> Drop for Connecting<I>
where
    I: Read + Write + Unpin,
{
    fn drop(&mut self) {
        self.pool.finish_connect(&self.key);
    }
}

#[cfg(feature = "http2")]
impl<I> std::fmt::Debug for Connecting<I>
where
    I: Read + Write + Unpin,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connecting")
            .field("key", &self.key)
            .finish()
    }
}

/// Waits for whoever holds the mark for one origin to be done with it.
#[cfg(feature = "http2")]
pub(crate) struct WaitForConnect<'a, I>
where
    I: Read + Write + Unpin,
{
    pool: &'a Pool<I>,
    key: &'a PoolKey,
}

#[cfg(feature = "http2")]
impl<I> Future for WaitForConnect<'_, I>
where
    I: Read + Write + Unpin,
{
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let mut connecting = self
            .pool
            .inner
            .connecting
            .lock()
            .expect("connection pool poisoned");
        let Some(wakers) = connecting.get_mut(self.key) else {
            return Poll::Ready(());
        };
        // `will_wake` rather than an unconditional push: a spurious
        // re-poll of this future would otherwise add a second waker for
        // the same task on every wake-up, and the list is only emptied
        // when the connect ends.
        if !wakers.iter().any(|w| w.will_wake(cx.waker())) {
            wakers.push(cx.waker().clone());
        }
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(host: &str) -> PoolKey {
        PoolKey::new(Security::Plaintext, host, 80, Protocol::Http11, None)
    }

    #[test]
    fn a_key_is_case_insensitive_in_the_host_and_nowhere_else() {
        assert_eq!(key("Example.COM"), key("example.com"));
        assert_ne!(key("example.com"), key("example.org"));
        assert_ne!(
            PoolKey::new(Security::Plaintext, "h", 80, Protocol::Http11, None),
            PoolKey::new(Security::Plaintext, "h", 8080, Protocol::Http11, None),
        );
    }

    /// The protocol component, checked here because it cannot be reached
    /// from outside the client — the same situation, and the same answer,
    /// as the TLS identity below.
    ///
    /// One `Native` negotiates the same protocol with a given origin every
    /// time, so no live test can put an `Http11` and an `H2` connection in
    /// one pool and watch them stay apart. What can be checked directly is
    /// the mechanism: that the key tells the two apart at all.
    #[cfg(feature = "http2")]
    #[test]
    fn the_protocol_separates_keys() {
        let of = |p| PoolKey::new(Security::Plaintext, "h", 80, p, None);
        assert_ne!(of(Protocol::Http11), of(Protocol::H2));
        assert_eq!(of(Protocol::H2), of(Protocol::H2));
    }

    /// The component this project would most like to get wrong, since
    /// within one transport it is a constant (see the module doc): two
    /// different TLS configurations must not be one key.
    #[test]
    fn tls_configuration_identity_separates_keys() {
        let a = TlsConfigId::new_unique();
        let b = TlsConfigId::new_unique();
        assert_ne!(
            PoolKey::new(Security::Tls(a), "h", 443, Protocol::Http11, None),
            PoolKey::new(Security::Tls(b), "h", 443, Protocol::Http11, None),
        );
        assert_eq!(
            PoolKey::new(Security::Tls(a), "h", 443, Protocol::Http11, None),
            PoolKey::new(Security::Tls(a), "h", 443, Protocol::Http11, None),
        );
        // And plaintext is not "https with some identity" — the two can
        // never collide, whatever the identity.
        assert_ne!(
            PoolKey::new(Security::Plaintext, "h", 443, Protocol::Http11, None),
            PoolKey::new(Security::Tls(a), "h", 443, Protocol::Http11, None),
        );
    }

    // ── the reaper ──────────────────────────────────────────────────────
    //
    // What lives here is what a network cannot show: that the task ends
    // when its pool is gone, that an empty pool does not make it spin, and
    // that `reap` reports the deadline it will next be woken by. The thing
    // a network CAN show — a real socket closed at its deadline, seen by
    // the server rather than by us — is `tests/reaper.rs`, and it is the
    // reason to believe any of this.

    use std::cell::{Cell, RefCell};
    use std::io;
    use std::rc::Rc;
    use std::task::Waker;

    /// IO that never does anything. Every test below either keeps the pool
    /// empty or parks connections it never speaks on, so a stream that
    /// answers `Pending` to everything is the honest stub: anything it
    /// *did* would be a fact about hyper, not about this file.
    struct NeverIo;

    impl Read for NeverIo {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: hyper::rt::ReadBufCursor<'_>,
        ) -> Poll<io::Result<()>> {
            Poll::Pending
        }
    }

    impl Write for NeverIo {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Pending
        }
        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Pending
        }
        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Pending
        }
    }

    /// A clock that records what it was asked to sleep for and hands out a
    /// sleep that is ready only for the first `ready` requests.
    ///
    /// Recording the *requests* is what makes the reaper's arithmetic
    /// checkable at all: how long it decides to wait is the whole of its
    /// behaviour between two reaps, and it is invisible to a test that only
    /// watches the pool. Rationing readiness is what stops the poll loop —
    /// a sleep that is always ready and a pool that is always there is an
    /// infinite loop by construction, which is exactly the shape the
    /// `MIN_REAP_INTERVAL` test is about.
    #[derive(Clone, Default)]
    struct FakeClock {
        asked: Rc<RefCell<Vec<Duration>>>,
        ready: Rc<Cell<usize>>,
        elapsed: Rc<Cell<Duration>>,
    }

    /// Readiness is decided at **poll** time, out of the shared budget,
    /// not at construction. The reaper's poll is a loop — a sleep that
    /// resolved, a reap, a new sleep — so a budget spent when the sleep is
    /// built is entirely consumed by the first `poll_once`, and a test
    /// could never hand the task a second turn. Deciding on poll lets a
    /// test top the budget up between polls, which is what
    /// `the_reaper_stays_alive_while_the_pool_does` needs in order to be
    /// about the pool at all.
    struct FakeSleep {
        budget: Rc<Cell<usize>>,
    }

    impl Future for FakeSleep {
        type Output = ();
        fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
            let left = self.budget.get();
            self.budget.set(left.saturating_sub(1));
            if left > 0 {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }
    }

    impl Timer for FakeClock {
        /// `()`: this clock's "instants" are all the same one, and the
        /// elapsed time is set by the test instead. A reaper never compares
        /// two instants — it subtracts elapsed durations from the epoch it
        /// was handed — so there is nothing here for a richer instant to
        /// do.
        type Instant = ();
        type Sleep = FakeSleep;
        fn sleep(&self, d: Duration) -> FakeSleep {
            self.asked.borrow_mut().push(d);
            FakeSleep {
                budget: Rc::clone(&self.ready),
            }
        }
        fn now(&self) {}
        fn elapsed_since(&self, _earlier: ()) -> Duration {
            self.elapsed.get()
        }
    }

    impl FakeClock {
        fn ready_for(n: usize) -> Self {
            let c = Self::default();
            c.ready.set(n);
            c
        }
        fn asked(&self) -> Vec<Duration> {
            self.asked.borrow().clone()
        }
    }

    fn poll_once<F: Future + Unpin>(f: &mut F) -> Poll<F::Output> {
        Pin::new(f).poll(&mut Context::from_waker(Waker::noop()))
    }

    /// A handshaken connection over IO that will never answer anything.
    ///
    /// `http1::handshake` finishes without touching the socket — it builds
    /// the sender/connection pair and nothing more — so one poll is enough
    /// and `Pending` here would mean a broken assumption rather than "wait
    /// a little longer", which is why it panics instead of looping.
    fn parked() -> Established<NeverIo> {
        let mut h = Box::pin(crate::h1::handshake(
            NeverIo,
            hclient_core::unversioned::ConnectionId::UNWATCHED,
            crate::h1::H1Opts::default(),
        ));
        match poll_once(&mut h) {
            Poll::Ready(Ok(est)) => Established::H1(est),
            Poll::Ready(Err(e)) => panic!("handshake over inert IO must not fail: {e}"),
            Poll::Pending => panic!("http1::handshake must not touch the socket"),
        }
    }

    fn pool_of(entries: &[u64]) -> Pool<NeverIo> {
        let pool = Pool::new(Some(PoolConfig::default()));
        for ms in entries {
            pool.put(key("h"), parked(), Duration::from_millis(*ms));
        }
        pool
    }

    /// `reap` is the only thing in this file that closes a connection
    /// nobody asked about, and the deadline it reports is what the reaper
    /// sleeps on. Both halves are checked here, because a `reap` that
    /// removed the right entries and reported the wrong deadline would be
    /// invisible from outside until a socket stayed open for an extra
    /// interval.
    #[test]
    fn reap_drops_what_is_past_its_deadline_and_reports_the_next_one() {
        let pool = pool_of(&[100, 200, 300]);

        assert_eq!(
            pool.reap(Duration::from_millis(150)),
            Some(Duration::from_millis(200)),
            "the 100ms entry is gone and 200ms is what to wake for next"
        );
        assert_eq!(
            pool.reap(Duration::from_millis(150)),
            Some(Duration::from_millis(200)),
            "reaping twice at the same instant must change nothing"
        );
        assert_eq!(
            pool.reap(Duration::from_millis(250)),
            Some(Duration::from_millis(300)),
        );
        assert_eq!(
            pool.reap(Duration::from_millis(350)),
            None,
            "everything is past its deadline, so there is nothing to wake for"
        );
    }

    /// An entry exactly at its deadline is expired, which is the same
    /// boundary [`Pool::take`] draws (`expires_at > now` is what it keeps).
    /// Two places reading one rule the opposite way round would put the
    /// reaper and checkout into a disagreement nobody would see until a
    /// connection was handed out one instant after it was closed.
    #[test]
    fn the_deadline_itself_counts_as_expired_for_reap_as_it_does_for_take() {
        let pool = pool_of(&[100]);
        assert_eq!(pool.reap(Duration::from_millis(100)), None);

        let pool = pool_of(&[100]);
        assert!(pool.take(&key("h"), Duration::from_millis(100)).is_none());
    }

    /// An empty pool has no deadline to wait for, so the reaper waits the
    /// idle timeout — the soonest anything checked in from now on could
    /// fall due.
    #[test]
    fn an_empty_pool_makes_the_reaper_wait_the_idle_timeout() {
        let clock = FakeClock::ready_for(1);
        let pool: Pool<NeverIo> = Pool::new(Some(PoolConfig::default()));
        let mut reaper = Reaper::new(
            clock.clone(),
            pool.downgrade(),
            (),
            Duration::from_millis(300),
        );

        assert!(poll_once(&mut reaper).is_pending());
        assert_eq!(
            clock.asked(),
            vec![Duration::from_millis(300), Duration::from_millis(300)],
            "one sleep before the first reap and one after it, both the idle timeout"
        );
    }

    /// The reaper wakes for the earliest deadline rather than on a fixed
    /// interval — which is the difference between closing a connection at
    /// its deadline and closing it up to one interval late.
    #[test]
    fn the_reaper_waits_exactly_until_the_earliest_deadline() {
        let clock = FakeClock::ready_for(1);
        clock.elapsed.set(Duration::from_millis(150));
        let pool = pool_of(&[100, 400]);
        let mut reaper = Reaper::new(
            clock.clone(),
            pool.downgrade(),
            (),
            Duration::from_millis(300),
        );

        assert!(poll_once(&mut reaper).is_pending());
        assert_eq!(
            clock.asked(),
            vec![Duration::from_millis(300), Duration::from_millis(250)],
            "the 100ms entry was reaped at 150ms, and 400ms is 250ms away"
        );
    }

    /// `PoolConfig { idle_timeout: Duration::ZERO, .. }` is a legal way to
    /// say "never hand anything out again". Without a floor it would also
    /// be a way to make this task spin a core: an empty pool, a
    /// zero-length sleep, and a loop with nothing to wait for.
    #[test]
    fn a_zero_idle_timeout_does_not_turn_the_reaper_into_a_spin() {
        let clock = FakeClock::ready_for(3);
        let pool: Pool<NeverIo> = Pool::new(Some(PoolConfig::default()));
        let mut reaper = Reaper::new(clock.clone(), pool.downgrade(), (), Duration::ZERO);

        assert!(poll_once(&mut reaper).is_pending());
        let asked = clock.asked();
        assert_eq!(
            asked.len(),
            4,
            "three ready sleeps and the pending one that ends the poll: {asked:?}"
        );
        for d in &asked[1..] {
            assert!(
                *d >= MIN_REAP_INTERVAL,
                "every wait after the first must be floored: {asked:?}"
            );
        }
    }

    /// The reaper must not be the reason a pool outlives its transport,
    /// and the way it is not is that it holds a `Weak`. When the upgrade
    /// fails the task is over — this is the only thing that ends it.
    #[test]
    fn the_reaper_ends_when_the_pool_it_watches_is_dropped() {
        let clock = FakeClock::ready_for(1);
        let pool: Pool<NeverIo> = Pool::new(Some(PoolConfig::default()));
        let weak = pool.downgrade();
        drop(pool);

        let mut reaper = Reaper::new(clock.clone(), weak, (), Duration::from_millis(300));
        assert_eq!(poll_once(&mut reaper), Poll::Ready(()));
        assert_eq!(
            clock.asked(),
            vec![Duration::from_millis(300)],
            "it must not ask for a second sleep after finding the pool gone"
        );
    }

    /// And it does not end early: a pool that is still there keeps the
    /// task alive across a reap. Without this, the test above would also
    /// pass for a reaper that ended on its first poll whatever it found.
    #[test]
    fn the_reaper_stays_alive_while_the_pool_does() {
        let clock = FakeClock::ready_for(1);
        let pool: Pool<NeverIo> = Pool::new(Some(PoolConfig::default()));
        let mut reaper = Reaper::new(
            clock.clone(),
            pool.downgrade(),
            (),
            Duration::from_millis(300),
        );

        // One reap happened (two sleeps asked for), and the task is still
        // waiting rather than finished.
        assert!(poll_once(&mut reaper).is_pending());
        assert_eq!(clock.asked().len(), 2);

        // The same reaper, the same everything — only the pool is gone.
        drop(pool);
        clock.ready.set(1);
        assert_eq!(poll_once(&mut reaper), Poll::Ready(()));
    }
}
