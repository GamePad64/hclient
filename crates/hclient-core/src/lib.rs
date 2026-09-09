//! Plugin contract for hclient: the traits a backend, a runtime or a
//! resolver implements, and the vocabulary types they exchange.
//!
//! # Which door is yours
//!
//! Twelve modules, and a reader needs at most two of them. The split is
//! not by topic but by **audience**, which is the same line the module
//! declarations below are grouped on.
//!
//! **Implementing a backend, a runtime or a resolver** — you write one of
//! these traits:
//!
//! | trait | in | you are writing |
//! |---|---|---|
//! | [`transport::Transport`] | [`transport`] | an HTTP backend; [`transport::SendTransport`] beside it if its futures are `Send` |
//! | [`timer::Timer`] | [`timer`] | a runtime's clock |
//! | [`websocket::WebSocketConnect`] | [`websocket`] | a backend that can open a WebSocket |
//! | [`auth::Auth`] / [`auth::AuthFlow`] | [`auth`] | an authentication scheme — NTLM, Negotiate |
//! | [`hooks::Hooks`] | [`hooks`] | an observer of requests and connections |
//!
//! **Using this crate's vocabulary** — the types those traits exchange:
//! [`body`] for a request body and what may be replayed, [`caps`] for what
//! a transport says it can do, [`req`] for what a caller asked of one
//! request, [`error`] for how anything fails, [`tls`] for what a request
//! asks of a TLS backend, and [`url`] for the one piece of URI syntax
//! every consumer here kept re-deriving.
//!
//! **Neither** — [`erased`] is how `hclient::Client` boxes a transport and
//! a clock, and no backend author writes anything in it: its two traits
//! carry blanket impls.
//!
//! **And a caller of `hclient` needs none of this.** Every type here that
//! a caller meets is re-exported from that crate under a shorter path —
//! `hclient::caps`, `hclient::hooks`, `hclient::Error`. This crate is the
//! door for whoever is *implementing* something, which is why the auth
//! seam lives here rather than in `hclient` — an implementor of a
//! two-method trait should not carry a whole HTTP client's graph. The
//! measurement is in [`auth`]'s own module doc rather than repeated here,
//! because a figure copied to a second place is a figure that goes stale
//! in two.
//!
//! # The `Send` rule
//!
//! **The seam traits declare no `Send`/`Sync` bounds.** `Transport`,
//! `Timer` and the middleware traits leave Send-ness to auto-traits
//! through `impl Future`, because a bound declared where the type is
//! abstract is forced on every backend — including ones that cannot meet
//! it, such as a single-threaded embedded runtime whose connect future
//! holds a `RefCell`.
//!
//! Bounds do appear in three places, and each is a value a caller hands
//! over rather than a demand on an implementor:
//!
//! - [`error::Error`]'s source is `Send + Sync`, or a client could not build an
//!   error from a backend's at all.
//! - [`body::RequestBody`]'s rewind factory and streaming arm.
//! - [`erased`]'s two aliases, which a facade writes at its
//!   own use site to put a transport behind an `Arc`. It is **not a
//!   seam**: a blanket impl covers every `Transport`, so no backend
//!   implements or is taxed by it, and one that cannot meet the bound is
//!   refused at a constructor rather than at a trait.
//!
//! Every such site carries a `send-bound-exception` marker naming the
//! amendment that admits it, and
//! `scripts/no-send-or-sync-in-the-core-surface.sh` fails closed on one
//! that does not. `grep` is therefore the authority on which sites exist;
//! this list says what kind they are.
#![forbid(unsafe_code)]

//! # What growing this crate costs, measured rather than promised
//!
//! Every module here is a published surface, so the question that decides
//! the shape of all of it is *what breaks when something is added*. It
//! was simulated rather than argued — a fifth `Timeouts` bound and its
//! `TimeoutSupport` mirror, added on a scratch checkout and taken through
//! the whole workspace and an out-of-tree consumer:
//!
//! - **A caller breaks nowhere.** A consumer crate depending on
//!   `hclient-core` and naming all 64 public items compiles unchanged.
//!   `Timeouts` is `#[non_exhaustive]` with a `const` builder, so a bound
//!   nobody has heard of is `None` and asks for nothing.
//! - **`hclient-core` breaks in exactly two places**, both `E0027`:
//!   [`req::Timeouts::or`] and [`req::Timeouts::support_checks`], which
//!   are the merge and the support gate. Those destructures live here
//!   rather than in `hclient` precisely so that they are compile errors
//!   in the crate that grew the field — under `#[non_exhaustive]` a
//!   consumer would have to write `..`, and a `..` is where a new bound
//!   goes to be silently unchecked and silently dropped.
//! - **Every transport breaks once**, on `TimeoutSupport`'s builder,
//!   because `bon` makes a non-`Option` member required. That is the
//!   intended cost: an unset bound there is a transport *claiming* it does
//!   not enforce something, and a claim nobody wrote is the thing to
//!   refuse.
//! - **`hclient` itself breaks nowhere**, because the gate consumes what
//!   `support_checks` returns rather than destructuring the struct.
//!
//! So the seam is additive for the audience that only reads, and a
//! compile error for the audience that must answer. That split is the
//! whole design, and it is checkable again by repeating the simulation.
//!
pub mod auth;
pub mod body;
pub mod caps;
pub mod error;
pub mod req;
pub mod tls;
pub mod url;

// ── the seams a backend or runtime author implements ────────────────────
//
// **These lived in a `unversioned` module and no longer do.** That module
// was a semver quarantine borrowed from `ureq`: it declared that breaking
// changes to the seams would ship in a *minor* version rather than a
// major, on the stated grounds that the traits had "not yet been
// validated against every backend" — naming native, `wasi:http` and
// fetch.
//
// **That condition is met.** `Transport` is implemented by eight crates
// today, including all three it named — `hclient-native`, `hclient-wasi`
// and `hclient-fetch` — plus `urlsession`, `winhttp`, `tower`, `mock` and
// `otel`. And the seams stopped moving: three breaking changes in the
// last sixty commits, two of them in August.
//
// This read *nine … and `dns-doh`* until it was counted rather than
// recalled: `hclient-dns-doh` **consumes** a transport (`C: SendTransport`,
// so a resolver's client is not the user's client) and implements none.
// The figure was right about the seam being validated and wrong about who
// validated it, which is the direction that matters least — and it is
// still a number in prose, so it is the count rather than the argument
// that will go stale next.
//
// So the quarantine was a promise to be unstable that nothing needed any
// more — and it was the one part of this crate that could not be frozen,
// which is exactly backwards for a crate whose types cross crate
// boundaries. `hclient::Client` is meant to be passable to another
// library by reference, and that only holds while the types in its
// signature are one version in the graph.
//
// Dissolved now rather than after `0.1.0`, because moving a public path
// is free before a stable release and a major version after it.
pub mod erased;
pub mod hooks;
pub mod timer;
pub mod transport;
pub mod websocket;

// ---- the doors ----
//
// **Sixty-one names sat in this crate's root and two modules stood beside
// them.** rustdoc renders one flat alphabetical list, so `RewindTooDeep` —
// a payload nobody constructs, reached only through `Error::source` — sat
// above `Transport`.
//
// **Everything is behind a door now, and nothing is re-exported here.**
// The first draft of this kept the most-used names at the root on the
// grounds that `Error` appears in 130 files and `Transport` in 56. That
// was the wrong test twice over: frequency measures what a move *costs* in
// imports rather than where a name belongs, and it points the wrong way
// here, because the name crossing the most crate boundaries is the one
// that most needs a legible path. A surface where some names are grouped
// and some are not is also two rules a reader has to learn instead of one.
//
// So the rule is uniform: **a name lives in the module its subject names,
// and the root holds modules.** `hclient_core::error::Error`,
// `hclient_core::transport::Transport`, `hclient_core::hooks::Event`.
//
// Nothing about a type changes. What changes is the path a reader types,
// and that a reader landing on this crate meets eleven doors rather than
// an alphabet of sixty-one.
