//! Plugin contract for hclient: the traits a backend, a runtime or a
//! resolver implements, and the vocabulary types they exchange.
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

pub mod auth;
pub mod body;
pub mod caps;
pub mod identity;
pub mod error;
pub mod host;

// ── the seams a backend or runtime author implements ────────────────────
//
// **These lived in a `unversioned` module and no longer do.** That module
// was a semver quarantine borrowed from `ureq`: it declared that breaking
// changes to the seams would ship in a *minor* version rather than a
// major, on the stated grounds that the traits had "not yet been
// validated against every backend" — naming native, `wasi:http` and
// fetch.
//
// **That condition is met.** `Transport` is implemented by nine crates
// today, including all three it named, plus `urlsession`, `winhttp`,
// `tower`, `mock`, `otel` and `dns-doh`. And the seams stopped moving:
// three breaking changes in the last sixty commits, two of them in
// August.
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
