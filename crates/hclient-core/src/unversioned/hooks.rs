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
//! in it. `crates/hclient-native/tests/hooks_cost.rs` measures it from
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
//! - **No hook is called with a lock held.** `hclient-native` emits from
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
//! The event set is derived from what a connection-owning backend can
//! observe, and backends still to come will have facts this vocabulary has
//! no word for — a QUIC connection migrating, a transfer a background
//! session finished after the process died. Freezing it would freeze one
//! backend's view. See this module's parent for what `unversioned`
//! promises.
use crate::Error;
use core::time::Duration;
use std::fmt::Display;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Somewhere to send what a transport did.
///
/// Implemented by the application rather than by a backend — which makes
/// it the one trait in `unversioned` pointing the other way. A backend
/// *calls* it, and what it owes is written on [`Event`]'s variants.
///
/// **No `Send` bound, declared or implied** (P13, settled by construction
/// in `crates/hclient-core/tests/shape.rs`). A hook is stored in a
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
    fn on(&self, event: &Event<'_>);
}

/// The hook a caller who asked for nothing gets: `WATCHING` is `false`,
/// the type is zero-sized, and `on` has no body.
///
/// The default type parameter of `hclient_native::Native`, so the
/// no-hooks build is the one that needs no words to ask for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NoHooks;

impl Hooks for NoHooks {
    const WATCHING: bool = false;
    fn on(&self, _event: &Event<'_>) {}
}

/// So that a hook can be shared without the caller writing the
/// delegation. `WATCHING` is forwarded rather than defaulted, or an
/// `Arc<NoHooks>` would start watching by being wrapped.
impl<H: Hooks + ?Sized> Hooks for Arc<H> {
    const WATCHING: bool = H::WATCHING;
    fn on(&self, event: &Event<'_>) {
        (**self).on(event);
    }
}

/// A reference is a hook, so `&my_hook` composes and installs like the
/// value — the shape `RedirectPolicy` and `RetryPolicy` already carry for
/// the same reason, and what lets a body borrow the transport's hook where
/// it does not need to own it.
impl<H: Hooks + ?Sized> Hooks for &H {
    const WATCHING: bool = H::WATCHING;
    fn on(&self, event: &Event<'_>) {
        (**self).on(event);
    }
}

/// The single-threaded half of the impl above, and not decoration: it is
/// the shape P13 asks about. A hook behind an `Rc` makes the transport
/// holding it `!Send`, and everything still compiles — see
/// `crates/hclient-core/tests/shape.rs`.
impl<H: Hooks + ?Sized> Hooks for std::rc::Rc<H> {
    const WATCHING: bool = H::WATCHING;
    fn on(&self, event: &Event<'_>) {
        (**self).on(event);
    }
}

/// [`and`](HooksExt::and), kept off [`Hooks`] itself.
///
/// The two seams in this workspace that already compose —
/// `RedirectPolicyExt` and `RetryPolicyExt` — keep it off their traits to
/// stay object-safe, and **that reason does not apply here**: [`Hooks`]
/// has an associated const, so it was never object-safe and never could
/// be. The reasons that do apply are two.
///
/// `Hooks` is blanket-implemented for `Arc<H: ?Sized>` and
/// `Rc<H: ?Sized>`, and `fn and(self, ..) -> And<Self, B>` needs
/// `Self: Sized`. On the trait that is a `where Self: Sized` predicate
/// every implementor reads and none of them wrote; here the bound is
/// stated once, on the extension trait, where it is the whole point of the
/// trait existing.
///
/// And a caller who learned `.and(..)` on a redirect policy should find it
/// spelled the same way here. A third spelling for the same idea is a
/// reader stopping to work out whether the difference means something.
pub trait HooksExt: Hooks + Sized {
    /// Both hooks see every event, `self` first.
    ///
    /// # What does **not** transfer from the policy seams
    ///
    /// `RedirectPolicy::and` and `RetryPolicy::and` compose *verdicts*, so
    /// composing them is a meet on a lattice: the more conservative answer
    /// wins, `Refuse` short-circuits, and the identity of the meet is the
    /// trait's own default — which is why one narrows from *yes* and the
    /// other from a single configured permission. **None of that has a
    /// subject here.** [`Hooks::on`] returns `()`, and `()` has exactly one
    /// value: there is no verdict to combine, nothing to be conservative
    /// about, and nothing that could short-circuit.
    ///
    /// So this is **sequencing**, not a meet — and the difference is
    /// visible, which is the part worth knowing. `redirect`'s own doc says
    /// order is unobservable there and that this is what separates a policy
    /// lattice from middleware. Here order *is* observable: hooks have side
    /// effects, so two of them writing to one log write in an order. `self`
    /// runs first, and that is a promise rather than an artefact of how
    /// `And` happens to be written.
    ///
    /// # A panicking hook, composed
    ///
    /// Unchanged, and one thing is added. The module doc says a panic in
    /// `on` propagates to whoever polled, is deliberately not caught, and
    /// is survivable because no hook is called with a lock held or from a
    /// `Drop`. `And` holds neither a lock nor a `Drop` impl of its own, so
    /// all of that still holds verbatim.
    ///
    /// What composition adds is that **a panic in the first hook means the
    /// second never sees that event**. There is no `catch_unwind` between
    /// them, for the module doc's own reasons — `UnwindSafe` would become a
    /// bound on the caller's type, and it does nothing under
    /// `panic = "abort"`, so isolating them would be a promise that holds
    /// in some builds and not others. A hook that must not be taken down by
    /// its neighbour catches its own panics, which is the only place the
    /// bound can be paid honestly.
    #[must_use]
    fn and<B: Hooks>(self, other: B) -> And<Self, B> {
        And(self, other)
    }
}

impl<T: Hooks + Sized> HooksExt for T {}

/// Two hooks as one. See [`HooksExt::and`].
///
/// A named type rather than `impl Hooks for (A, B)`, and the tuple's cost
/// is what decided it. The ordering promise and the `WATCHING` rule below
/// are properties **of the composition**, and a tuple has nowhere to put
/// them: their only home would be a doc comment on an impl for a
/// primitive, which rustdoc renders on the tuple's own page rather than
/// anywhere a reader of this module will pass. A tuple also needs one impl
/// per arity, or `((a, b), c)`, which reads worse than `a.and(b).and(c)` —
/// and it would make every two-tuple of hooks a hook, which is a coherence
/// commitment on a foreign type taken in exchange for one import.
///
/// That import is what the tuple would have bought, and it is a real cost:
/// `.and(..)` needs [`HooksExt`] in scope, exactly as `.and(..)` on a
/// redirect policy needs `RedirectPolicyExt`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct And<A, B>(pub A, pub B);

impl<A: Hooks, B: Hooks> Hooks for And<A, B> {
    /// **A join, where the verdict lattices meet — and the sign is
    /// load-bearing.**
    ///
    /// [`Hooks::WATCHING`] is a *demand*, not a permission: it says
    /// whether anything reads these events, and a backend skips measuring
    /// when it is `false`. Compose it with `&&` and
    /// `NoHooks.and(my_hook)` would answer `false`, so every backend would
    /// skip the work and `my_hook` would compile, install, and fire never
    /// — a capability that lies, which this workspace treats as worse than
    /// a silent downgrade because a caller can act on it.
    ///
    /// With `||`, [`NoHooks`] is the identity of composition: adding it
    /// changes nothing, and adding anything else to it starts the
    /// measuring. That is the same reason the [`Arc`] impl forwards
    /// `WATCHING` rather than defaulting it.
    const WATCHING: bool = A::WATCHING || B::WATCHING;

    fn on(&self, event: &Event<'_>) {
        self.0.on(event);
        self.1.on(event);
    }
}

/// What a transport reports.
///
/// **There is deliberately no "request queued" variant.** The original
/// list for this work had one, and `hclient-native` has nothing to put in
/// it: a request that finds no live pooled connection dials a fresh one,
/// there is no per-origin connection limit to wait behind, and an h2
/// connection is checked out of the pool exclusively — one stream at a
/// time — so `SendRequest::poll_ready` never waits for a stream of ours.
/// A variant no code can emit is a capability that lies; this one belongs
/// here once a backend has a queue.
///
/// **`#[non_exhaustive]`, and the compile error it removes is kept in one
/// place rather than lost.**
///
/// A new variant used to be a compile error for every `match` on this
/// enum, which is how `Informational` was caught in `hclient-fetch`'s
/// suite. That property is worth something *inside* this workspace and is
/// a break every release for somebody who wrote a `Hooks` impl against a
/// published version — two audiences wanting opposite things from one
/// enum.
///
/// The resolution is [`Capabilities`]' exactly: mark the type, and keep a
/// single exhaustive match in **this** crate, where the attribute does not
/// apply. `every_event_is_accounted_for` below is that match, so a new
/// variant is still one compile error in one known file — and no break at
/// all for anybody outside.
///
/// [`Capabilities`]: crate::Capabilities
#[derive(Debug)]
#[non_exhaustive]
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
    /// A `1xx` arrived ahead of the response — `100 Continue`,
    /// `103 Early Hints`, or anything else a server sends before the one
    /// answer the caller is waiting for.
    ///
    /// An event rather than a response, because it is not one: a `1xx` is
    /// not the end of the exchange and `Transport::execute` resolves
    /// exactly once. A caller who wants `103`'s preload hints reads them
    /// here, before the head that follows.
    Informational(Informational<'a>),
    /// Octets moved, one direction, one exchange. See [`Progress`] for
    /// which octets, how often they are counted, and why the denominator
    /// is an `Option`.
    Progress(Progress<'a>),
}

/// A `1xx` that arrived before the response.
///
/// # Why there is no `version` here, when [`Head`] has one
///
/// A `1xx` travels on a connection, and the connection's protocol was
/// already reported by the [`Connected`] or [`Reused`] that opened this
/// exchange — both of which carry a plain `Version` rather than an
/// `Option`, because only a transport that owns a connection emits either
/// and owning one means having negotiated its protocol. Repeating it here
/// would be a third place to be wrong about the same fact.
///
/// `Head::version` is an `Option` for the opposite reason: the two
/// backends that own no connection emit `Head` and nothing else, so for
/// them there is no `Connected` to have carried it. Neither of them can
/// emit this event at all.
#[derive(Debug)]
#[non_exhaustive]
pub struct Informational<'a> {
    pub id: ConnectionId,
    /// `1xx`. Which one is the whole of what distinguishes a `100` from a
    /// `103`, so it is not narrowed to an enum: a status this crate has
    /// never heard of is still a status the server sent.
    pub status: http::StatusCode,
    pub headers: &'a http::HeaderMap,
}

/// Octets moved, one direction, one exchange.
///
/// The fact a caller cannot otherwise get at, which is [`Reused`]'s test
/// for whether an event earns its place: a caller holding a response body
/// can count what it reads, and can count nothing at all about what it
/// sent, about a body it never reads, or about the bytes a decompressor
/// consumed to produce the ones it did.
///
/// # Which octets — the encoded ones, off the wire
///
/// [`Self::transferred`] counts **body octets as the transport moved
/// them**, before any content coding is reversed and after any is applied.
/// A `gzip`-encoded response of 1 MiB that decodes to 9 MiB reports 1 MiB.
///
/// That is settled twice over, and the second argument is the one that
/// would decide it even if the first did not.
///
/// **Structurally**, a hook belongs to a transport and `ClientBuilder` has
/// none: `hclient`'s `Decompressed` wrapper sits *above* every emitter
/// there is, so the encoded octets are the only ones an emitter can see.
///
/// **Arithmetically**, the numerator and the denominator have to be in one
/// unit. [`Self::expected`] comes from what the sender stated — a
/// `Content-Length`, or a request body's exact `size_hint` — and
/// RFC 9110 §8.6 makes that the length of the *encoded* body. Count
/// decoded octets against it and the ratio passes 1 on every compressed
/// response, which is a progress bar that overflows rather than one that
/// is merely coarse. `ConnectTiming::tls`'s rule, one event over: a wrong
/// answer is worse than a missing one.
///
/// The decoded count is reachable and is the caller's own — they are
/// holding the frames. This one is not reachable any other way.
///
/// **One backend counts something else and cannot help it.**
/// `hclient-fetch` is handed a stream the browser has already decoded, so
/// what it counts is post-decode octets. It answers `None` for
/// [`Self::expected`] whenever the response carried a `Content-Encoding`,
/// rather than pairing a decoded numerator with an encoded denominator —
/// which is the same rule as above, applied where the unit cannot be
/// chosen.
///
/// # How often — whenever it moved, and the total is cumulative
///
/// An event is emitted when the count has changed since the last one for
/// that direction, and never otherwise: no traffic, no event. In practice
/// that is at most once per poll of the body or of the exchange, which is
/// at most once per frame and often less.
///
/// There is **no threshold knob**, and the reason it costs nothing to
/// refuse one is that [`Self::transferred`] is a running total rather than
/// a delta. A hook that wants one event a second keeps the last value it
/// acted on and compares; it may ignore any number of events and still be
/// exactly right, because the next one it reads carries the whole answer.
/// A delta would make dropping an event a permanent error and force every
/// hook to sum. So sampling is the hook's decision, made where the hook
/// is, rather than a byte or time policy this seam would have to pick for
/// everybody — which is what `Backoff::delay` and `RetryPolicy` mean by
/// keeping a rule pure and leaving the state to whoever owns the
/// operation.
#[derive(Debug)]
#[non_exhaustive]
pub struct Progress<'a> {
    /// The connection carrying it, or [`ConnectionId::UNWATCHED`] where
    /// the transport owns none.
    pub id: ConnectionId,
    /// Which exchange this is about.
    ///
    /// Not redundant beside [`Self::id`], and h2 is why: several requests
    /// share one multiplexed connection, so the id alone cannot say whose
    /// octets these are.
    pub uri: &'a http::Uri,
    pub direction: Direction,
    /// Octets moved in this direction of this exchange **so far** —
    /// cumulative, monotonic, never a delta.
    pub transferred: u64,
    /// What the sender said the whole body would be, in the same unit as
    /// [`Self::transferred`], or `None` where nobody said.
    ///
    /// `Some(n)` is a statement somebody made — a `Content-Length` on the
    /// response, an exact `size_hint` on the request body. `None` is the
    /// absence of one: a chunked response, an unbounded stream.
    ///
    /// **Two states rather than `Discovered`'s three**, and the missing
    /// one is *not consulted*: the transport has the response head before
    /// it counts an incoming octet and the request body before it counts
    /// an outgoing one, so there is no path on which it could have failed
    /// to look. A third variant would be a distinction with one reachable
    /// side, which is what `UpgradeSupport`'s spare variants were deleted
    /// for.
    ///
    /// It is not a promise. A server may send fewer octets than it
    /// declared, or more; this is what was claimed, not what arrived.
    pub expected: Option<u64>,
}

/// Which way the octets went, from the client's point of view.
///
/// **Not `#[non_exhaustive]`**, and the split is this workspace's own
/// rule, the one [`ClientCertAsk`] states: a value a caller *branches on*
/// wants exhaustiveness as its mechanism, where a value handed back and
/// only read wants the attribute. A hook that draws an upload bar and a
/// download bar branches, and a third direction — were there one — must
/// be a compile error at every reader rather than a wildcard to be
/// silently mishandled in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    /// The request body, on its way out.
    Sending,
    /// The response body, on its way in.
    Receiving,
}

/// Which connection an event is about.
///
/// Process-wide and monotonic, so a [`Closed`] can be matched to the
/// [`Connected`] that opened the same socket — which is the only reason
/// it exists. Without it a close event says "a connection ended" and a
/// caller holding several has no way to learn which.
///
/// [`ConnectionId::UNWATCHED`] is what an event carries when there was no
/// id to mint for it — because nobody is watching, or because there is no
/// connection. See that constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConnectionId(u64);

static NEXT_CONNECTION_ID: AtomicU64 = AtomicU64::new(1);

impl ConnectionId {
    /// The id an event carries when no id was minted for it.
    ///
    /// Two things produce it, and **a reader can only ever meet the
    /// second**:
    ///
    /// - **Nobody is watching.** [`Hooks::WATCHING`] is `false`, so a
    ///   transport that owns connections leaves the counter alone rather
    ///   than paying an atomic per connection for nobody. That const's
    ///   own question is *whether anything reads these events*, so a
    ///   build producing this value for this reason has, by its own
    ///   declaration, no reader for the event carrying it.
    /// - **There is no connection to name.** `hclient-fetch` and
    ///   `hclient-wasi` own none — the Fetch Standard exposes no
    ///   connection object, and there is no connection resource anywhere
    ///   in `wasi:http@0.3.0` — so their one event, [`Head`], carries
    ///   this.
    ///
    /// To a hook that reads events it therefore means exactly one thing,
    /// *this event names no connection*, and that is something a portable
    /// hook can act on rather than a gap it has to guess at: it is the
    /// only id [`ConnectionId::next`] never returns, so looking it up in
    /// a table of live connections cannot hit one.
    ///
    /// # The name is a producer, not the meaning
    ///
    /// There is deliberately no second constant meaning *this event names
    /// no connection*, distinct from *nobody is watching*: the two would
    /// differ only in a build whose events nobody reads, so no caller
    /// decision turns on the difference — this workspace's test for
    /// whether a distinction earns a name of its own. `UNWATCHED` names
    /// one of the two producers, and any spelling would name one or the
    /// other; read it as the ambient *this event names no connection*.
    pub const UNWATCHED: ConnectionId = ConnectionId(0);

    /// The next id. `Relaxed`: this counter orders nothing, it only has
    /// to hand out distinct numbers.
    ///
    /// It starts at `1`, and that is load bearing rather than tidy: it is
    /// what makes [`UNWATCHED`](Self::UNWATCHED) mean *this event names no
    /// connection* rather than *this event names connection zero*.
    /// `crates/hclient-core/tests/shape.rs` pins it.
    pub fn next() -> Self {
        Self(NEXT_CONNECTION_ID.fetch_add(1, Ordering::Relaxed))
    }

    /// The number itself, for a log line.
    pub fn get(self) -> u64 {
        self.0
    }
}

impl Display for ConnectionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A connection was established.
#[derive(Debug)]
#[non_exhaustive]
pub struct Connected<'a> {
    pub id: ConnectionId,
    /// The URI whose request paid for this connection, as the transport
    /// received it — absolute, before any protocol rewrote it.
    pub uri: &'a http::Uri,
    /// The address that answered. One of possibly several tried:
    /// RFC 8305 races address families, and this is the winner, not the
    /// first candidate.
    ///
    /// **`None` where the connection has no IP address**, which today
    /// means a Unix-domain socket
    /// (`hclient_native::Native::unix_socket`): there was no name, no
    /// family to race and no port. It is an `Option` rather than a
    /// fabricated `0.0.0.0:0` for `Head::version`'s reason one event over
    /// — a sentinel that is also an ordinary value gives a hook a *wrong*
    /// answer where the absence gives it a missing one, and only the
    /// second can be handled.
    ///
    /// The alternative was to emit no `Connected` at all for such a
    /// connection, and it is worse: the `Closed` that follows would
    /// announce the end of a connection whose beginning was never
    /// announced, which is exactly the defect recorded for building a
    /// `Closed::Failed` out of `wasi:http`'s error codes.
    pub remote: Option<SocketAddr>,
    /// What will be spoken on it, as negotiated — not as offered.
    pub version: http::Version,
    pub timing: ConnectTiming,
    /// The TLS version, as the backend reports it: `"TLSv1.3"`, dotted,
    /// which is the spelling curl prints and OpenSSL uses.
    ///
    /// **`None` has two meanings and they are distinguishable elsewhere.**
    /// Over `http://` there was no handshake at all, and `timing.tls` is
    /// `None` too. Over `https://` it means the backend does not report
    /// it — `hclient-tls-native-tls` is the one that does not, because the
    /// platform stacks expose no getter for it, which that crate's own
    /// module doc says. So the pair `(timing.tls, tls_version)` separates
    /// *no TLS* from *TLS this backend will not describe*.
    ///
    /// A string rather than an enum because the vocabulary is the TLS
    /// registry's and not ours: a backend that grows a version this
    /// workspace has never heard of should be able to say so, where an
    /// enum would force it through an `Other` arm or a wrong variant.
    pub tls_version: Option<&'a str>,
    /// The negotiated cipher suite, by its IANA registry name —
    /// `"TLS_AES_256_GCM_SHA384"`.
    ///
    /// `None` on the same two conditions as [`Self::tls_version`], and a
    /// string for the same reason: the registry gains entries and this
    /// crate is not where they should have to be enumerated.
    pub tls_cipher: Option<&'a str>,
    /// The protocol ALPN selected, as bytes — `b"h2"`, `b"http/1.1"`.
    ///
    /// Bytes rather than a string because ALPN identifiers are octet
    /// sequences in RFC 7301 and nothing obliges a peer to send UTF-8.
    ///
    /// This is **not** a second statement of [`Self::version`]: `version`
    /// is what this transport will actually speak, decided by the same
    /// function that picks the handshake, where this is what the peer
    /// said. They agree on every connection this workspace makes, and a
    /// hook that finds them disagreeing has found something worth
    /// reporting rather than a field to prefer.
    pub alpn: Option<&'a [u8]>,
    /// Whether the server asked for a client certificate, and what for.
    ///
    /// Owned, unlike the three fields above it, because it is not a slice
    /// of anything: the clone happens once per connection and only in the
    /// [`ClientCertAsk::Asked`] arm.
    pub client_cert: ClientCertAsk,
}

/// Whether a server asked for a client certificate — **three answers,
/// because two of them would be a lie by omission.**
///
/// `Option<ClientCertRequest>` was the first shape and it collapses
/// *the server did not ask* into *this backend cannot see whether it
/// did*. Both sides are reachable in one program: `hclient-tls-rustls`
/// observes the `CertificateRequest` by being the resolver, and
/// `hclient-tls-native-tls` cannot, because the platform stacks expose no
/// hook for it — and `hc --backend` picks between them at run time. A
/// caller writing a certificate picker would then see nothing on one
/// backend and conclude that no server ever asks, which is the *silently
/// ignored setting* defect pointed at a credential.
///
/// It is the shape [`Discovered::NoRecord`] against `NotConsulted` has
/// one crate over, and the one `Retry-After` absent against unreadable
/// has: **an answer and the absence of one are different values.**
///
/// [`Discovered::NoRecord`]: https://docs.rs/hclient-native
/// **Not `#[non_exhaustive]`**, unlike [`ClientCertRequest`] beside it,
/// and the split is this workspace's own rule: the payload is a value a
/// library hands back and a caller only reads, where this is a value
/// something *branches on* — `hc -v` prints a different line per arm.
/// Exhaustiveness is the mechanism here, so a fourth state must be a
/// compile error at every reader rather than a wildcard for it to be
/// silently mishandled in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ClientCertAsk {
    /// This backend cannot see whether the server asked.
    ///
    /// **The default, and deliberately the understating value** —
    /// `reports_alpn`'s rule: a backend that says nothing is taken to
    /// know nothing, so silence can never be read as *no server here
    /// wants a certificate*. Reached over `http://` too, where there was
    /// no handshake to observe.
    #[default]
    Unobserved,
    /// The backend watched the handshake and the server did not ask.
    NotAsked,
    /// The server asked. The payload is what it asked for.
    ///
    /// An empty [`ClientCertRequest::authority_names`] here is a fourth
    /// fact and not this variant's absence: RFC 8446 §4.4.2.1 makes an
    /// empty list *send whatever certificate you have*.
    Asked(ClientCertRequest),
}

impl ClientCertAsk {
    /// The request, where there was one.
    ///
    /// For a reader that only wants the payload and has already decided
    /// that *unobserved* and *not asked* mean the same thing to it —
    /// which is a decision, made at the call site, rather than one this
    /// type makes for everybody.
    #[must_use]
    pub fn asked(&self) -> Option<&ClientCertRequest> {
        match self {
            Self::Asked(r) => Some(r),
            _ => None,
        }
    }
}

/// What a server asked for when it requested a client certificate.
///
/// Read by whoever wants to *choose* one — a rule, or a person.
/// `docs/mtls-design.md` §3.4 has why this leaves the handshake at all:
/// an automatic choice is a synchronous resolver and needs none of it,
/// where an interactive one cannot wait inside a handshake and must
/// observe, abandon, choose and redial.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ClientCertRequest {
    /// The certificate authorities the server said it would accept:
    /// DER-encoded distinguished names, in the order sent, **unparsed**.
    ///
    /// Raw for the reason a peer certificate is: turning a distinguished
    /// name into text needs an X.509 parser, which this workspace
    /// declined to take. A caller who wants names brings one.
    ///
    /// Empty means the server named nobody, which it is entitled to do.
    pub authority_names: Vec<Vec<u8>>,
    /// The signature schemes the server will verify, as IANA codepoints.
    ///
    /// Codepoints rather than an enum, and never a backend's own type:
    /// the registry gains entries, and this seam may no more name
    /// `rustls::SignatureScheme` than [`Connected::tls_cipher`] may be an
    /// enum.
    pub sigschemes: Vec<u16>,
    /// Whether a certificate was presented in answer.
    ///
    /// **This closes a gap neither half closes alone.** Without it a
    /// caller cannot tell *403 because I sent no certificate* from *403
    /// because I am not authorised*, so a picker would fire on the wrong
    /// responses. Hints present with `answered: false` is the exact
    /// signal that a certificate would have been accepted and none was
    /// sent.
    pub answered: bool,
}

impl ClientCertRequest {
    /// An empty record: the server asked and named nobody, and nothing
    /// was sent.
    ///
    /// A builder rather than a literal, because the struct is
    /// `#[non_exhaustive]` and a **backend outside this workspace** is
    /// what fills it — the same pair, and the same reason, as
    /// `hclient_tls::TlsInfo::new` one crate over.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The CAs the server said it would accept, DER, in the order sent.
    #[must_use]
    pub fn authority_names(mut self, names: Vec<Vec<u8>>) -> Self {
        self.authority_names = names;
        self
    }

    /// The signature schemes it will verify, as IANA codepoints.
    #[must_use]
    pub fn sigschemes(mut self, schemes: Vec<u16>) -> Self {
        self.sigschemes = schemes;
        self
    }

    /// Whether a certificate was presented in answer.
    #[must_use]
    pub fn answered(mut self, answered: bool) -> Self {
        self.answered = answered;
        self
    }
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
#[non_exhaustive]
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
#[non_exhaustive]
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
#[non_exhaustive]
pub struct Head<'a> {
    pub id: ConnectionId,
    pub uri: &'a http::Uri,
    pub status: http::StatusCode,
    /// What was spoken — or `None` where the transport could not observe
    /// it.
    ///
    /// `Some` exactly when the transport reports
    /// [`version_reported`](crate::Capabilities::version_reported).
    /// `hclient-native` reads it off the status line, and off ALPN with
    /// the `http2` feature; `hclient-h3` speaks HTTP/3 and nothing else;
    /// both say `true`. `hclient-fetch` and `hclient-wasi` say `false` and
    /// report `None` here: the Fetch Standard's `Response` has no protocol
    /// member, and `wasi:http@0.3.0` has no version concept at all.
    ///
    /// # Why an `Option`, and not `http`'s builder default
    ///
    /// Because `HTTP/1.1` is an ordinary value. Nothing distinguishes it
    /// from an HTTP/1.1 exchange that really happened, so a hook counting
    /// protocol mix records a browser's h2 and h3 traffic as HTTP/1.1 — a
    /// **wrong** answer rather than a missing one. That is
    /// [`ConnectTiming::tls`]'s rule one field over: *a zero would read as
    /// an instant handshake*. `http::Version` has no variant meaning "not
    /// observed" and is not ours to give one, so the `Option` is where the
    /// distinction can live.
    ///
    /// The capability asks the same question and does not answer it in the
    /// same place. [`Capabilities`](crate::Capabilities) is reachable from
    /// whoever built the transport; a [`Hooks`] impl is handed an
    /// [`Event`] and nothing else, and the same hook is written once and
    /// installed on whichever backend the target got. A hook that had to
    /// know which transport it was inside in order not to record a
    /// falsehood is exactly the `#[cfg]` this workspace exists to not
    /// need.
    ///
    /// [`Connected::version`] and [`Reused::version`] stay plain, and that
    /// is structural rather than an oversight: only a transport that owns
    /// a connection emits either of those, and owning one means having
    /// negotiated its protocol.
    pub version: Option<http::Version>,
    /// From the transport receiving the request to the head being read.
    ///
    /// It contains the connect when there was one, which is the point:
    /// the pair (`Head::elapsed`, `ConnectTiming::total`) is what answers
    /// "was it the connection or was it the server".
    pub elapsed: Duration,
}

/// A connection ended.
#[derive(Debug)]
#[non_exhaustive]
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

// ── constructors ────────────────────────────────────────────────────────
//
// Every type above is `#[non_exhaustive]` **and** built by a transport,
// which is the combination that needs both halves. The attribute stops an
// out-of-tree `Hooks` impl breaking on a new field; the constructors stop
// it locking an out-of-tree *transport* out of producing the value at all.
// A backend is a user of this library, and the attribute alone would have
// served one of its two roles at the expense of the other.
//
// The split is the same everywhere: what a value cannot be without goes in
// `new`, and what a backend may not know goes in a setter. So a new field
// that a backend may not know is additive; one it must supply is a break,
// and no attribute has ever protected against that.

impl<'a> Informational<'a> {
    /// Every field is required: an interim response without a status or a
    /// header map is not one.
    #[must_use]
    pub fn new(id: ConnectionId, status: http::StatusCode, headers: &'a http::HeaderMap) -> Self {
        Self {
            id,
            status,
            headers,
        }
    }
}

impl<'a> Progress<'a> {
    /// The three facts an octet count cannot be without: whose exchange,
    /// which way, and how many so far.
    ///
    /// [`Progress::expected`] is a setter, because a backend may not know
    /// it — a chunked response states no length, and neither does an
    /// unbounded request body.
    #[must_use]
    pub fn new(
        id: ConnectionId,
        uri: &'a http::Uri,
        direction: Direction,
        transferred: u64,
    ) -> Self {
        Self {
            id,
            uri,
            direction,
            transferred,
            expected: None,
        }
    }

    /// What the sender said the whole body would be. `None` is the honest
    /// answer where nobody said, and never a `0` standing in for one.
    #[must_use]
    pub fn expected(mut self, expected: Option<u64>) -> Self {
        self.expected = expected;
        self
    }
}

impl<'a> Connected<'a> {
    /// A connection that was opened. `remote`, `timing` and the TLS
    /// facts are what a backend may not have — a Unix socket has no peer
    /// address, an ambient host reports no phases, and a plaintext
    /// connection has no handshake — so they are setters.
    #[must_use]
    pub fn new(id: ConnectionId, uri: &'a http::Uri, version: http::Version) -> Self {
        Self {
            id,
            uri,
            remote: None,
            version,
            timing: ConnectTiming::new(),
            tls_version: None,
            tls_cipher: None,
            client_cert: ClientCertAsk::Unobserved,
            alpn: None,
        }
    }

    /// The peer's address, where there is one. `None` is not a gap to be
    /// filled with `0.0.0.0:0`: a Unix socket genuinely has no address, and
    /// a fabricated one is a wrong answer where the absence is a missing
    /// one.
    #[must_use]
    pub fn remote(mut self, remote: Option<SocketAddr>) -> Self {
        self.remote = remote;
        self
    }

    /// The TLS facts, all of them at once.
    ///
    /// One setter rather than three, because they come from one place —
    /// a backend either read the handshake's outcome or it did not — and
    /// three would let a caller set two and forget the third, which is
    /// the shape `Native::hooks` dropping the `1xx` installer while
    /// keeping its capability already cost this workspace once.
    pub fn tls(
        mut self,
        version: Option<&'a str>,
        cipher: Option<&'a str>,
        alpn: Option<&'a [u8]>,
        client_cert: ClientCertAsk,
    ) -> Self {
        self.tls_version = version;
        self.tls_cipher = cipher;
        self.alpn = alpn;
        self.client_cert = client_cert;
        self
    }

    pub fn timing(mut self, timing: ConnectTiming) -> Self {
        self.timing = timing;
        self
    }
}

impl<'a> Reused<'a> {
    /// Every field is required: a reused connection has an identity, a
    /// target and a negotiated version, or it is not one.
    #[must_use]
    pub fn new(id: ConnectionId, uri: &'a http::Uri, version: http::Version) -> Self {
        Self { id, uri, version }
    }
}

impl<'a> Head<'a> {
    /// `version` is deliberately not here: it is `Option` because a
    /// backend that does not learn the protocol must say so rather than
    /// answer `HTTP/1.1`, and a required parameter would invite exactly
    /// that. [`Head::version`] is the setter, and
    /// `Capabilities::version_reported` is the biconditional it answers.
    #[must_use]
    pub fn new(
        id: ConnectionId,
        uri: &'a http::Uri,
        status: http::StatusCode,
        elapsed: Duration,
    ) -> Self {
        Self {
            id,
            uri,
            status,
            version: None,
            elapsed,
        }
    }

    /// The protocol this response arrived over. Set it exactly when
    /// `Capabilities::version_reported` is `true`.
    #[must_use]
    pub fn version(mut self, version: Option<http::Version>) -> Self {
        self.version = version;
        self
    }
}

impl<'a> Closed<'a> {
    /// Both fields are required: a close names the connection and says
    /// why.
    #[must_use]
    pub fn new(id: ConnectionId, reason: CloseReason<'a>) -> Self {
        Self { id, reason }
    }
}

impl ConnectTiming {
    /// All phases zero, which is what a backend that measures none
    /// reports.
    #[must_use]
    pub fn new() -> Self {
        Self {
            dns: Duration::ZERO,
            tcp: Duration::ZERO,
            tls: None,
            total: Duration::ZERO,
        }
    }

    /// Time spent resolving.
    #[must_use]
    pub fn dns(mut self, dns: Duration) -> Self {
        self.dns = dns;
        self
    }

    /// Time spent on the TCP connect.
    #[must_use]
    pub fn tcp(mut self, tcp: Duration) -> Self {
        self.tcp = tcp;
        self
    }

    /// Time spent on the TLS handshake, or `None` where there was none.
    #[must_use]
    pub fn tls(mut self, tls: Option<Duration>) -> Self {
        self.tls = tls;
        self
    }

    /// The whole connect, which is **not** the sum of the phases above: a
    /// connector that races two families spends wall-clock time no single
    /// phase accounts for.
    #[must_use]
    pub fn total(mut self, total: Duration) -> Self {
        self.total = total;
        self
    }
}

impl Default for ConnectTiming {
    fn default() -> Self {
        Self::new()
    }
}

/// Read a clock, but only if a hook is watching.
///
/// **The gate is the point, not the line of code.** `H::WATCHING` is what
/// keeps a `NoHooks` build from paying for a feature it does not use, and
/// the discipline it needs is subtle enough to have produced a defect:
/// `hclient-fetch` once carried a second `H::WATCHING` test beside this
/// one, and a mutation that removed the *other* gate survived the whole
/// suite — a `NoHooks` build read the clock and cloned a `Uri` on every
/// request while the cost test still read zero. One `Option`, produced in
/// one place, closes that; four crates each writing their own is four
/// chances to reopen it.
///
/// Generic over the clock read rather than over a clock **type**, because
/// the four callers genuinely disagree about what a clock is: a `Timer`'s
/// `Instant` on native, `performance.now()`'s `f64` in a browser, a
/// `std::time::Instant` under WASI. What they never disagree about is
/// this.
#[must_use]
pub fn mark<H: Hooks, T>(now: impl FnOnce() -> T) -> Option<T> {
    H::WATCHING.then(now)
}

/// How long since [`mark`], or zero where nothing was marked.
///
/// `Duration::ZERO` rather than an `Option`: an unwatched request has no
/// elapsed time to report and no hook to report it to, so the absence has
/// nowhere to travel.
#[must_use]
pub fn since<T>(at: Option<T>, elapsed: impl FnOnce(T) -> Duration) -> Duration {
    at.map_or(Duration::ZERO, elapsed)
}

/// A running octet count for one direction of one exchange, and the last
/// value reported for it.
///
/// **It exists only when somebody is watching**, which is the whole of the
/// gate: [`meter`] is the only constructor and it hands back `None` under
/// [`NoHooks`], so a build with no hook has no counter, no atomic and no
/// branch — the discipline [`mark`] already enforces for clocks, and for
/// the same recorded reason. Four crates each writing `if H::WATCHING`
/// around their own counter is four chances to reopen the defect where one
/// of them forgets.
///
/// The count and the report are in different places on every backend here
/// — the request body increments it and the transport emits, the response
/// body does both — so the two halves are `&self` methods over atomics
/// rather than `&mut self`, and the type is shared through an [`Arc`]
/// where they are far apart.
#[derive(Debug)]
pub struct Meter {
    expected: Option<u64>,
    moved: AtomicU64,
    told: AtomicU64,
}

/// A [`Meter`], but only if a hook is watching. See [`Meter`] and [`mark`].
#[must_use]
pub fn meter<H: Hooks>(expected: Option<u64>) -> Option<Meter> {
    H::WATCHING.then(|| Meter {
        expected,
        moved: AtomicU64::new(0),
        told: AtomicU64::new(0),
    })
}

impl Meter {
    /// Another `n` octets went past.
    ///
    /// `Relaxed`, for [`ConnectionId::next`]'s reason: this counter orders
    /// nothing. Saturating, because a count that wrapped would be a
    /// progress bar running backwards, and 2^64 octets is not a number any
    /// exchange reaches by accident.
    pub fn add(&self, n: u64) {
        let _ = self
            .moved
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |t| {
                Some(t.saturating_add(n))
            });
    }

    /// What the sender stated, unchanged since construction.
    #[must_use]
    pub fn expected(&self) -> Option<u64> {
        self.expected
    }

    /// Octets so far.
    #[must_use]
    pub fn transferred(&self) -> u64 {
        self.moved.load(Ordering::Relaxed)
    }

    /// Emit a [`Progress`] if the count has moved since the last one.
    ///
    /// **The "if" is the whole policy, and it is not a threshold.** A
    /// caller of this may poll as often as it likes; what it cannot do is
    /// produce an event for traffic that did not happen. Callers that poll
    /// a body per frame therefore emit per frame, and one that polls a
    /// future many times between two frames emits once — both correct,
    /// because [`Progress::transferred`] is cumulative.
    pub fn report<H: Hooks>(
        &self,
        hooks: &H,
        id: ConnectionId,
        uri: &http::Uri,
        direction: Direction,
    ) {
        let now = self.moved.load(Ordering::Relaxed);
        if self.told.swap(now, Ordering::Relaxed) == now {
            return;
        }
        hooks.on(&Event::Progress(
            Progress::new(id, uri, direction, now).expected(self.expected),
        ));
    }
}

/// A response body that counts what it yields, and reports it.
///
/// **It is the outermost thing a backend hands back, and innermost of the
/// wrappers a client adds** — which is the whole of where the number comes
/// from. `hclient`'s `Decompressed` sits above every transport's body, so
/// what this sees is the encoded octets; see [`Progress`] for why that is
/// the right number rather than merely the available one.
///
/// It also carries the **send** meter, where there is one, and reports
/// that too. That is not a second job: on a protocol with a duplex request
/// body the upload is still running after the head, and the response body
/// is then the only thing the caller is polling — so it is the only place
/// left that can notice the upload moving.
///
/// A pass-through with no allocation and no branch worth naming when
/// nobody is watching: [`meter`] hands back `None`, so no [`http::Uri`] is
/// cloned and no atomic exists.
#[derive(Debug)]
pub struct Counting<B, H> {
    inner: B,
    hooks: H,
    id: ConnectionId,
    /// The clone and the counter are produced **together or not at all**.
    ///
    /// Two `Option`s that have to agree is one more than is safe: a second
    /// `H::WATCHING` test beside this one is exactly the shape whose
    /// mutation survived `hclient-fetch`'s whole suite, leaving a
    /// `NoHooks` build cloning a `Uri` per request while the cost test
    /// still read zero.
    recv: Option<(http::Uri, Meter)>,
    /// Written by the request body, read here. `None` when nobody is
    /// watching, and also when the request had no body to count.
    sent: Option<Arc<Meter>>,
}

impl<B, H> Counting<B, H>
where
    B: http_body::Body,
    H: Hooks,
{
    /// Wrap a body. `expected` is read off the body's own
    /// [`http_body::Body::size_hint`] **once, here** — the exact hint, or
    /// `None`.
    ///
    /// That one rule gives the right answer on every backend because each
    /// body's `size_hint` already carries its own honesty argument.
    /// `hclient-fetch`'s, for one, refuses to trust a `Content-Length`
    /// beside a `Content-Encoding`, because the browser hands over a
    /// stream it has already decoded — the same unit rule [`Progress`]
    /// states, reached independently and written down one field over
    /// before this event existed.
    /// # `uri: None` is *do not count here*, not *no uri*
    ///
    /// It is the caller's statement that this wrapper is not the counter
    /// for this body, and there are two ways to mean it. Nobody is
    /// watching, which is `hclient-fetch`'s case — the `Uri` it still holds
    /// after the request was consumed lives in the same `Option` as its
    /// clock mark, so the gate travels rather than being read twice. Or
    /// something underneath already counts: `hclient-native` wraps here for
    /// its TCP protocols and passes `None` for a body that came up through
    /// the QUIC arm, which `hclient_native::H3` has already wrapped.
    ///
    /// The type is the same either way, which is the point — `Transport::Body`
    /// has to name one type, and a second one for the uncounted case would
    /// be a `Transport` impl per route.
    #[must_use]
    pub fn new(
        inner: B,
        hooks: H,
        id: ConnectionId,
        uri: Option<&http::Uri>,
        sent: Option<Arc<Meter>>,
    ) -> Self {
        let recv =
            uri.and_then(|uri| meter::<H>(inner.size_hint().exact()).map(|m| (uri.clone(), m)));
        Self {
            inner,
            hooks,
            id,
            recv,
            sent,
        }
    }
}

impl<B, H> http_body::Body for Counting<B, H>
where
    B: http_body::Body + Unpin,
    H: Hooks + Unpin,
{
    type Data = B::Data;
    type Error = B::Error;

    fn poll_frame(
        self: core::pin::Pin<&mut Self>,
        cx: &mut core::task::Context<'_>,
    ) -> core::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        use bytes::Buf as _;
        let this = self.get_mut();
        let out = core::pin::Pin::new(&mut this.inner).poll_frame(cx);
        let Some((uri, m)) = &this.recv else {
            return out;
        };
        if let core::task::Poll::Ready(Some(Ok(frame))) = &out
            && let Some(data) = frame.data_ref()
        {
            m.add(data.remaining() as u64);
        }
        // Both directions on every poll, and unconditionally: `report` is
        // what decides whether anything moved, so a caller of it never has
        // to know.
        m.report(&this.hooks, this.id, uri, Direction::Receiving);
        if let Some(sent) = &this.sent {
            sent.report(&this.hooks, this.id, uri, Direction::Sending);
        }
        out
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

/// A body that counts what it yields into a shared [`Meter`], and reports
/// nothing.
///
/// The counting half on its own, for the **request** side, where the
/// thing that counts and the thing that reports are not the same object:
/// the request body is handed to hyper, to `h2`'s pump or to the browser,
/// and what reports is [`Reporting`] or [`Counting`] back where the hook
/// is. That is why the meter is an [`Arc`] and this type names no `H`.
///
/// **`hclient-native` deliberately does not use it**, and its own
/// `OutgoingBody` says why in a sentence written before this existed:
/// that type is *the one place every frame of every request body passes
/// through on its way to hyper*, and *a wrapper would be a second thing to
/// remember to put on*. It carries the meter as a field for exactly that
/// reason. The two ambient backends have no such single type — a
/// `wasi:http` request body is a buffered arm and a streaming arm, and a
/// browser's is three — so for them the wrapper is the choke point.
#[derive(Debug)]
pub struct Metered<B> {
    inner: B,
    meter: Option<Arc<Meter>>,
}

impl<B> Metered<B> {
    /// `None` is a pass-through, which is what an unwatched build gets.
    #[must_use]
    pub fn new(inner: B, meter: Option<Arc<Meter>>) -> Self {
        Self { inner, meter }
    }
}

impl<B: http_body::Body + Unpin> http_body::Body for Metered<B> {
    type Data = B::Data;
    type Error = B::Error;

    fn poll_frame(
        self: core::pin::Pin<&mut Self>,
        cx: &mut core::task::Context<'_>,
    ) -> core::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        use bytes::Buf as _;
        let this = self.get_mut();
        let out = core::pin::Pin::new(&mut this.inner).poll_frame(cx);
        if let Some(m) = &this.meter
            && let core::task::Poll::Ready(Some(Ok(frame))) = &out
            && let Some(data) = frame.data_ref()
        {
            m.add(data.remaining() as u64);
        }
        out
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

/// A future that reports what a request body has sent, each time it is
/// polled.
///
/// **The upload has no body of its own for a caller to poll**, which is
/// what this exists for: the request body belongs to hyper, to `h2`'s pump
/// or to `h3`'s, and the only thing running while it drains is the
/// exchange future. So the exchange future is what looks.
///
/// The granularity that falls out is *whenever this was polled and the
/// count had moved* — never an event for traffic that did not happen, and
/// never a threshold this seam had to pick for everybody. [`Progress`] says
/// why a cumulative total makes that affordable.
///
/// `F: Unpin` rather than a `Box::pin`, so the caller writes
/// `std::pin::pin!(fut)` and pays no allocation: a `Pin<&mut F>` is both.
#[derive(Debug)]
pub struct Reporting<F, H> {
    inner: F,
    hooks: H,
    id: ConnectionId,
    /// One `Option`, for [`Counting::recv`]'s reason.
    watch: Option<(http::Uri, Arc<Meter>)>,
}

impl<F, H> Reporting<F, H> {
    /// `meter` is already gated — [`meter`] hands back `None` when nobody
    /// is watching — so this clones the [`http::Uri`] exactly when there
    /// is something to report it against.
    #[must_use]
    pub fn new(
        inner: F,
        hooks: H,
        id: ConnectionId,
        uri: &http::Uri,
        meter: Option<Arc<Meter>>,
    ) -> Self {
        Self {
            inner,
            hooks,
            id,
            watch: meter.map(|m| (uri.clone(), m)),
        }
    }
}

impl<F, H> core::future::Future for Reporting<F, H>
where
    F: core::future::Future + Unpin,
    H: Hooks + Unpin,
{
    type Output = F::Output;

    fn poll(
        self: core::pin::Pin<&mut Self>,
        cx: &mut core::task::Context<'_>,
    ) -> core::task::Poll<Self::Output> {
        let this = self.get_mut();
        let out = core::pin::Pin::new(&mut this.inner).poll(cx);
        if let Some((uri, m)) = &this.watch {
            m.report(&this.hooks, this.id, uri, Direction::Sending);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Every variant is accounted for, and adding one is a compile error
    /// here.**
    ///
    /// This is the whole of what `#[non_exhaustive]` on [`Event`] gives
    /// up, bought back in one place: the attribute stops an out-of-tree
    /// `Hooks` impl from breaking on a new variant, and this match stops
    /// the variant from being added without anybody noticing. Written with
    /// no `_` arm on purpose — the attribute is inert inside the defining
    /// crate, which is the only reason this can be here at all.
    ///
    /// It asserts nothing about behaviour. It does not need to: the value
    /// is that it fails to compile, which is the same value the 28
    /// scattered matches in the backends' test suites used to provide
    /// between them.
    #[test]
    fn every_event_is_accounted_for() {
        fn name(e: &Event<'_>) -> &'static str {
            match e {
                Event::Connected(_) => "connected",
                Event::Reused(_) => "reused",
                Event::Head(_) => "head",
                Event::Closed(_) => "closed",
                Event::Informational(_) => "informational",
                Event::Progress(_) => "progress",
            }
        }
        // One live value, so the function is not merely compiled but
        // reached — a `match` no test calls is checked by the compiler and
        // by nothing else, which is enough here and cheap to improve on.
        let err = crate::Error::new(crate::ErrorKind::Other, std::io::Error::other("x"));
        let closed = Closed {
            id: ConnectionId::UNWATCHED,
            reason: CloseReason::Failed(&err),
        };
        assert_eq!(name(&Event::Closed(closed)), "closed");
    }
}
