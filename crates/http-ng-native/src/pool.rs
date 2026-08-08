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
//! That is not a corner cut for simplicity. `http_ng_rt::Spawn<F>` requires
//! `F: Send + 'static`, and this vertical's IO is deliberately not `Send`
//! (`connect.rs`'s `FakeStream` holds an `Rc<()>` for the sole purpose of
//! proving that no path here requires it). So "a pool driven by a spawned
//! task" is not an option that was weighed and rejected — on this seam it
//! does not compile at all. Making `Spawn` a requirement of `Native` would
//! mean losing the property that `tests/h1.rs::
//! works_on_a_bare_futures_executor_with_no_spawn` and
//! `http-ng/tests/two_runtimes.rs` exist to hold.
//!
//! What replaces it is a **check at checkout**: before a pooled connection
//! is used, [`crate::h1::is_reusable`] polls its `Connection` exactly once
//! and asks `SendRequest::poll_ready`. One poll is enough to see a server
//! that closed the socket while it was idle, because a reactor's readiness
//! is remembered rather than delivered: the `FIN` sits in the kernel and in
//! the reactor's state until somebody reads it, and the first poll with a
//! live waker does. On a bare executor with no reactor at all it is even
//! more direct — the poll reads the socket.
//!
//! Three consequences, all real, all deliberate, none of them hidden:
//!
//! 1. **An idle connection reads nothing.** Between requests we notice
//!    neither a `FIN` nor anything a server might send unbidden. For an
//!    idle HTTP/1 connection the former is the only thing that happens, and
//!    checkout catches it.
//! 2. **The idle timeout is a filter, not a reaper.** With nothing polling
//!    in the background, there is nobody to close a connection when its
//!    time is up. [`PoolConfig::idle_timeout`] therefore means "do not hand
//!    out a connection older than this", not "close a connection older than
//!    this": a client that goes quiet for an hour leaves its sockets open
//!    until it makes another request or the `Native` is dropped. This is
//!    the price of no-spawn, and it is named here rather than discovered.
//! 3. **A race remains.** A server may close between our check and our
//!    write. That window cannot be closed by any HTTP/1 pool — hyper's own
//!    has it too — so it is handled rather than prevented: see the retry in
//!    `Native::execute`, which is the reason this pool does not make
//!    previously reliable requests fail intermittently.
//!
//! # What is in the key, and why each part is there
//!
//! [`PoolKey`] is the v0.2 design document's `(scheme, host, port,
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
//! **One stream at a time — an h2 connection is handed out exclusively,
//! exactly as an h1 one is.** HTTP/2 multiplexes and this pool does not
//! use that, which is a decision rather than an omission, and this is
//! where it is written down.
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
//!   end.
use crate::established::Established;
use http_ng_tls::TlsConfigId;
use hyper::rt::{Read, Write};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How this transport reuses connections.
///
/// **One setting, not two.** The v0.2 design document is explicit that an
/// idle timeout must live either in `http_ng_core::Timeouts` or on the pool
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
    /// How many idle connections to keep per [`PoolKey`].
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
}

impl PoolKey {
    pub(crate) fn new(security: Security, host: &str, port: u16, protocol: Protocol) -> Self {
        Self {
            security,
            host: host.to_ascii_lowercase().into_boxed_str(),
            port,
            protocol,
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
            }),
        }
    }

    /// `None` when reuse is off. The single source of truth for both the
    /// behaviour and `Capabilities::connection_reuse`.
    pub(crate) fn config(&self) -> Option<PoolConfig> {
        self.inner.config
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
    /// deliberately does not read a clock of its own: `http_ng_core::Timer`
    /// is the seam through which time enters this crate, and a
    /// `std::time::Instant::now()` here would quietly disagree with a test
    /// running under `tokio::time::pause()`.
    pub(crate) fn take(&self, key: &PoolKey, now: Duration) -> Option<Established<I>> {
        let mut idle = self.inner.idle.lock().expect("connection pool poisoned");
        let bucket = idle.get_mut(key)?;
        let taken = loop {
            let entry = bucket.pop()?;
            if entry.expires_at > now {
                break entry.est;
            }
            // Dropped here, holding the lock: dropping a connection closes a
            // socket, which does not block.
        };
        if bucket.is_empty() {
            idle.remove(key);
        }
        Some(taken)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn key(host: &str) -> PoolKey {
        PoolKey::new(Security::Plaintext, host, 80, Protocol::Http11)
    }

    #[test]
    fn a_key_is_case_insensitive_in_the_host_and_nowhere_else() {
        assert_eq!(key("Example.COM"), key("example.com"));
        assert_ne!(key("example.com"), key("example.org"));
        assert_ne!(
            PoolKey::new(Security::Plaintext, "h", 80, Protocol::Http11),
            PoolKey::new(Security::Plaintext, "h", 8080, Protocol::Http11),
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
        let of = |p| PoolKey::new(Security::Plaintext, "h", 80, p);
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
            PoolKey::new(Security::Tls(a), "h", 443, Protocol::Http11),
            PoolKey::new(Security::Tls(b), "h", 443, Protocol::Http11),
        );
        assert_eq!(
            PoolKey::new(Security::Tls(a), "h", 443, Protocol::Http11),
            PoolKey::new(Security::Tls(a), "h", 443, Protocol::Http11),
        );
        // And plaintext is not "https with some identity" — the two can
        // never collide, whatever the identity.
        assert_ne!(
            PoolKey::new(Security::Plaintext, "h", 443, Protocol::Http11),
            PoolKey::new(Security::Tls(a), "h", 443, Protocol::Http11),
        );
    }
}
