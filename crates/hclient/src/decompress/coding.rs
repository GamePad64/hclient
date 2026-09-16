//! The open seam: what a content coding is, and the four this crate ships
//! as values of it.
//!
//! # Why this is a trait at all, when the set was a `#[cfg]` table
//!
//! It was a `const REGISTRATIONS: &[Registration]` whose entries each sat
//! behind a cargo feature, read through a `BTreeMap` in a `OnceLock`. That
//! shape answers one question — *which codings did this build compile* —
//! and it is the wrong question twice over.
//!
//! **A caller could not narrow it.** Cargo unifies features across a
//! graph, so a library deep in somebody's tree that switches `brotli` on
//! decides what every client in that process asks for, and the
//! application author has no lever: setting `Accept-Encoding` by hand
//! disables decoding **entirely** (see [`negotiate`](super::negotiate)'s
//! caller-set-header branch), so *"ask for gzip only, and still decode
//! it"* was inexpressible. That gap was written down in
//! `.notes/decompression-as-an-attack-surface.md` before it was closed,
//! and the argument that closes it is **CPU under load** rather than
//! binary size or safety, both of which already had answers: size is
//! settled by the features (a compiled-but-unused coding costs nothing at
//! run time), and safety is bounded by
//! [`ClientBuilder::response_limit`](crate::ClientBuilder::response_limit),
//! which counts decoded bytes rather than wire bytes, because the
//! wrapper enforcing it sits outside the one that decodes. What nothing answered is the measured one: at 1900
//! MiB/s gzip decode, a service doing 5000 RPS of 1.7 MiB responses
//! spends **4.5 cores** reversing codings, and there was no way to turn
//! that off for one client.
//!
//! **And a caller could not extend it.** Every other extension point in
//! this workspace — `Resolve`, [`CacheStore`](crate::cache::CacheStore),
//! [`RetryPolicy`](hclient_proto::retry::RetryPolicy),
//! [`RedirectPolicy`](hclient_proto::redirect::RedirectPolicy),
//! [`Auth`](crate::auth::Auth) — is a trait whose implementor may live
//! outside this crate. Content codings were the one closed set, and
//! nothing about them earned the exception: a deployment with an internal
//! coding, a `zstd` tuned with a shared dictionary, or a decoder somebody
//! measured as faster than ours had nowhere to go but a fork.
//!
//! Both halves are one change, because they are one list: the codings a
//! client asks for **are** the codings it can reverse, which is the
//! property the registry existed to keep true and which a `Vec` on the
//! config keeps true just as well.
//!
//! # What replaced the machinery
//!
//! The `BTreeMap`, the `OnceLock`, the `Registration::preference` field
//! and the `Decoders` newtype are gone. What a client holds is a
//! `Vec<Arc<dyn ContentCoding>>` in [`Config`](crate::Config), **in the
//! order it will be advertised**, and the lookups walk it. Four entries
//! never earned a map — the map's own doc said so — and the `preference`
//! field existed only because a `BTreeMap` cannot carry order. A slice
//! can, and its order is the caller's to choose, which is what made the
//! field redundant rather than merely unnecessary.

use super::decoder::Decoder;
use std::sync::Arc;

/// One content coding: its name on the wire, and how to start reversing
/// it.
///
/// Implement this to teach a [`Client`](crate::Client) a coding this
/// crate does not ship, and hand it over with
/// [`ClientBuilder::decompression`](crate::ClientBuilder::decompression).
/// The four built in are [`compression::Gzip`](super::compression::Gzip),
/// [`Brotli`](super::compression::Brotli),
/// [`Zstd`](super::compression::Zstd) and
/// [`Deflate`](super::compression::Deflate), each behind the cargo feature
/// of its own name, and each an ordinary value of this trait with no
/// standing the seam does not give it.
///
/// # A decoder is caller-supplied code inside the response body chain
///
/// [`Decode::push`](super::Decode::push) is called with bytes straight off the wire, once per
/// frame, for every response whose `Content-Encoding` this coding claims.
/// Whatever it yields is what the caller reads. So an implementation is
/// not a configuration value: it is code on the hot path of every
/// response, and a defect in it is a defect in this client.
///
/// **The only thing bounding what a decoder yields is
/// [`ClientBuilder::response_limit`]**, which counts decoded bytes
/// because the body wrapper that counts them sits **outside** the one
/// that decodes — a ceiling applied to the wire would pass a
/// decompression bomb by definition, and
/// [`ClientBody`](crate::body::ClientBody) is where that order is
/// written down. It is unset by default.
/// Measured amplification for one GiB of zeros at maximum effort, which
/// is the reason to set one:
///
/// | coding | on the wire | decoded | ratio |
/// |---|---|---|---|
/// | `gzip` | 1,043,656 | 1 GiB | 1:1,028 |
/// | `zstd` | 32,786 | 1 GiB | 1:32,750 |
/// | `br` | **1,681** | 1 GiB | **1:638,751** |
///
/// So 1.6 KB off the network is a gigabyte of memory through brotli, and
/// the codings are not interchangeable from a safety standpoint whoever
/// wrote them. A coding of your own is a fifth row whose ratio nobody
/// here has measured.
///
/// [`ClientBuilder::response_limit`]: crate::ClientBuilder::response_limit
///
/// # This trait declares no auto trait, and that is the house rule
///
/// [`CacheStore`](crate::cache::CacheStore) — the seam this workspace
/// holds up as the model for an open extension point — declares none
/// either, and for the reason the `no-send-or-sync-in-the-core-surface`
/// guard exists: a bound stated on a seam is a demand on every
/// implementor, including one this workspace has never seen. What needs
/// the property is the place the value is **stored**, which is
/// [`SharedContentCoding`], and that is where it is written.
///
/// The practical consequence for an implementor is none at all: write a
/// coding that happens to be `Send + Sync`, which any type holding no
/// `Rc` is, and [`ClientBuilder::decompression`] accepts it. One that is
/// genuinely not gets `E0277` where it is handed over rather than where
/// it is defined.
///
/// **This was tried the other way round first**, and `cargo fmt` settled
/// it rather than an argument: a `send-bound-exception` marker on a
/// `pub trait X: Send {` line is **deleted** by a reflow — reproduced on
/// this trait — so `just fmt-check` and `just invariants` cannot both
/// pass with the bound there. That is the same finding this workspace
/// recorded when `auth`'s two traits met it, arrived at from a third
/// direction.
///
/// [`ClientBuilder::decompression`]: crate::ClientBuilder::decompression
pub trait ContentCoding: std::fmt::Debug {
    /// The canonical token, as it goes out in `Accept-Encoding` and as it
    /// is matched against `Content-Encoding`.
    ///
    /// Matching is ASCII-case-insensitive, as RFC 9110 §8.4.1 requires,
    /// so this may be written in any case and is compared in none. It must
    /// be a `token` in the sense of RFC 9110 §5.6.2 — it goes into a
    /// header field this client writes — and one that is not is refused by
    /// name at [`ClientBuilder::build`](crate::ClientBuilder::build)
    /// rather than panicking where the header is assembled. See
    /// [`InvalidCodingToken`](crate::error::InvalidCodingToken).
    ///
    /// **`&str` borrowed from `&self`, not `&'static str`**, so a coding
    /// whose token is computed or read out of a configuration can keep it
    /// in a field and lend it. That costs nothing: a coding lives behind
    /// an [`Arc`] for the client's whole life, so the borrow always
    /// outlives the call. It is the asymmetry with
    /// [`ClientBody::coding`](crate::body::ClientBody::coding), which
    /// hands back a [`Cow`](std::borrow::Cow) for the opposite reason —
    /// **a body holds its decoder and not the coding that made it**, so
    /// there is nothing there for a `&str` to borrow from.
    fn token(&self) -> &str;

    /// Other spellings this coding answers to in `Content-Encoding`, and
    /// which are **never advertised**.
    ///
    /// Two exist in RFC 9110 and one is reachable here: §8.4.1.3's
    /// `x-gzip`, which [`Gzip`](super::compression::Gzip) carries.
    /// §8.4.1.1's `x-compress` names a coding this crate does not reverse.
    ///
    /// Asked only after every coding's own [`token`](Self::token) has
    /// failed to match, so an alias can never shadow another coding's
    /// canonical name.
    ///
    /// Refused at `build()` under the same rule as
    /// [`token`](Self::token), although an alias never reaches a header
    /// this client writes: a malformed one is dead weight rather than a
    /// panic, because no `Content-Encoding` off the wire can match it —
    /// and one rule refusing both is simpler to state than two rules
    /// differing by where the value ends up.
    fn aliases(&self) -> &[&str] {
        &[]
    }

    /// A fresh decoder for one response body.
    ///
    /// **The factory, not a decoder.** A [`Decode`](super::Decode) is stateful — `push`
    /// and `finish` carry one stream's window and its integrity check — so
    /// a single instance cannot serve two response bodies, and a coding
    /// that handed the same one to both would hand the second a window
    /// full of the first's plaintext. What is registered is the
    /// *constructor*; each body calls it and owns what comes back.
    ///
    /// This is also what makes the seam's own state useful: a coding
    /// holding a shared dictionary or a tuned window builds it once, at
    /// construction, and every decoder it makes reads it through `&self`.
    fn decoder(&self) -> Decoder;
}

/// A content coding as the client stores it.
///
/// **`Arc<dyn ..>` rather than `&'static dyn ..`**, and the four built-in
/// codings being zero-sized is exactly why the question came up. A ZST
/// needs no allocation and could have been a `&'static`; a third-party
/// coding carrying a dictionary or a tuned window cannot be, without
/// making its author leak it. One allocation per coding at
/// [`build()`](crate::ClientBuilder::build) — four, in the default build —
/// against a seam that would otherwise be open only to codings that hold
/// nothing.
///
/// `Arc` rather than `Box` because [`Config`](crate::Config) is `Clone`:
/// `Client::total_timeout` hands back a second handle by cloning it, and
/// a `Box` would deep-copy a list that is read-only after `build()`. Same
/// answer as [`SharedRedirectPolicy`](crate::redirect::SharedRedirectPolicy)
/// and [`SharedRetryPolicy`](crate::retry::SharedRetryPolicy) one field
/// over.
pub type SharedContentCoding = Arc<dyn ContentCoding + Send + Sync>; // send-bound-exception: amendment-C12

/// The coding a `Content-Encoding` names, if this client carries one.
///
/// **A walk, where this was a `BTreeMap` behind a `OnceLock`.** The map's
/// own doc argued that four entries do not earn `phf` or a hand-written
/// binary search; they do not earn a `BTreeMap` either, and once the list
/// became per-client it could not be a `static` at all. What the walk
/// costs is a handful of `eq_ignore_ascii_case` against a list a caller
/// wrote, once per response that carries the header.
///
/// **Canonical tokens are tried before any alias**, in two passes rather
/// than one, and that ordering is the whole of what keeps an alias from
/// shadowing a name: a caller who supplies a coding calling itself
/// `x-gzip` beside this crate's [`Gzip`](super::compression::Gzip) — which
/// lists `x-gzip` among its aliases — gets their own coding for that
/// token, because the first pass finds it. One pass would hand it to
/// whichever came first in the list.
///
/// A duplicate token is answered by the **first** coding that claims it,
/// which is the only answer a list can give and is the same rule as the
/// `Accept-Encoding` order one function down: the caller wrote the list.
pub(crate) fn lookup<'a>(
    codings: &'a [SharedContentCoding],
    token: &str,
) -> Option<&'a SharedContentCoding> {
    codings
        .iter()
        .find(|c| c.token().eq_ignore_ascii_case(token))
        .or_else(|| {
            codings
                .iter()
                .find(|c| c.aliases().iter().any(|a| a.eq_ignore_ascii_case(token)))
        })
}

/// The `Accept-Encoding` value for a list of codings, or `None` when there
/// is nothing to ask for.
///
/// **The slice order is the wire order**, which is the whole reason
/// `Registration::preference` is gone: a caller who wants `gzip` offered
/// ahead of `br` writes them in that order, and nothing here reorders
/// them. Aliases are never advertised — `x-gzip` is a spelling this
/// client *accepts*, and RFC 9110 §8.4.1.3 deprecates it, so offering it
/// would invite a server to use it.
///
/// # The `expect` here is the one this change had to earn back
///
/// It read *"infallible: every token is a compile-time ASCII constant
/// from a `Registration`, and the separator is `", "`. Nothing here comes
/// from the network or the caller"* — which was true of a closed set and
/// **dies with it**: a caller's coding answering `"my coding"` or a
/// non-ASCII token would turn that line into a panic on third-party data,
/// once per request.
///
/// What replaces the justification is not a weaker line here but a
/// refusal earlier: [`validate`] is run at
/// [`ClientBuilder::build`](crate::ClientBuilder::build), so a list that
/// reaches this function has had every token checked against RFC 9110
/// §5.6.2's `token` production — which is a strict subset of what a
/// `HeaderValue` accepts, and excludes the separator. Silently skipping a
/// bad coding was the alternative and is the *silently ignored setting*
/// defect this workspace has closed four times: a caller who asked for a
/// coding would get a client that never asks for it and never says so.
pub(crate) fn accept_encoding(codings: &[SharedContentCoding]) -> Option<http::HeaderValue> {
    if codings.is_empty() {
        return None;
    }
    let value = codings
        .iter()
        .map(|c| c.token())
        .collect::<Vec<_>>()
        .join(", ");
    Some(http::HeaderValue::from_str(&value).expect("`validate` ran at `build()`"))
}

/// Is `s` a `token` in the sense of RFC 9110 §5.6.2?
///
/// ```text
/// token          = 1*tchar
/// tchar          = "!" / "#" / "$" / "%" / "&" / "'" / "*"
///                / "+" / "-" / "." / "^" / "_" / "`" / "|" / "~"
///                / DIGIT / ALPHA
/// ```
///
/// Written out rather than delegated to `http::HeaderName` or
/// `HeaderValue`: a `HeaderValue` accepts a space and a comma, which are
/// exactly the two characters that would let one coding's token be read
/// as two, and `HeaderName` lower-cases and would answer about a name
/// where this is a value. `hclient_proto`'s parsers are the other
/// candidate and the wrong direction — this validates a value this crate
/// is about to *write*, which is a check rather than a parse.
fn is_token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

/// Every token and alias in `codings` is a token, or the first one that is
/// not.
///
/// **At [`ClientBuilder::build`](crate::ClientBuilder::build), which is
/// where this workspace refuses a configuration it cannot honour.** The
/// precedent is `config::check_supported`, which turns a setting the
/// transport cannot keep into an error naming it rather than a value
/// quietly dropped; this is the same gesture one step out, where what
/// cannot be honoured is a coding whose own name will not go in a header.
///
/// Both halves of a coding are checked although only the token is
/// written: see [`ContentCoding::aliases`] for why one rule is easier to
/// state than two.
pub(crate) fn validate(
    codings: &[SharedContentCoding],
) -> Result<(), crate::error::InvalidCodingToken> {
    for c in codings {
        let token = c.token();
        if !is_token(token) {
            return Err(crate::error::InvalidCodingToken {
                token: token.to_owned(),
                coding: token.to_owned(),
            });
        }
        for alias in c.aliases() {
            if !is_token(alias) {
                return Err(crate::error::InvalidCodingToken {
                    token: (*alias).to_owned(),
                    coding: token.to_owned(),
                });
            }
        }
    }
    Ok(())
}

/// What a build with no configuration asks for.
///
/// **Assembled from the built-in structs rather than from a table**, so
/// there is one statement of which codings exist and it is the `#[cfg]` on
/// each `push` below. Measured before this was decided: 40 test files in
/// this crate call `Client::builder` and **none** mentions a coding,
/// because the compiled-in set applied silently. Removing the default
/// outright would have forced an explicit list at every one of those call
/// sites and at every consumer's, and would have changed behaviour for
/// everyone who upgrades — so what disappeared is the machinery and not
/// the behaviour. A caller who says nothing gets exactly what they got
/// before; a caller who calls
/// [`decompression`](crate::ClientBuilder::decompression) replaces the
/// list.
///
/// # The order, which is the one decision this function makes
///
/// Densest first, so a server picking the first token it recognises picks
/// the coding that costs the fewest bytes — and `deflate` **last**,
/// deliberately: it is the one coding whose wire format RFC 9110 §8.4.1.2
/// leaves ambiguous (`deflate`'s module doc has the sniffing this crate
/// has to do about it), so this client would rather be offered any other.
/// That decision used to be a `preference: u8` field on every
/// registration, for the sole reason that a `BTreeMap` orders by key and
/// would have advertised `br, deflate, gzip, zstd` — alphabetical, a
/// preference nobody chose. A slice carries its own order, so the field
/// had nothing left to say.
pub(crate) fn builtin() -> Vec<SharedContentCoding> {
    // **`push` rather than a `vec![..]` literal**, because each element is
    // behind its own `#[cfg]` and an attribute on an expression inside
    // `vec![]` is not something the macro accepts. The empty build is then
    // an ordinary empty `Vec` rather than a case anything has to say
    // anything about, which is what the `#[cfg]`-per-entry `const
    // REGISTRATIONS` array bought before it, one shape over.
    #[allow(
        clippy::vec_init_then_push,
        reason = "each push carries its own `#[cfg]`, which a `vec![]` literal cannot express"
    )]
    #[allow(
        unused_mut,
        reason = "a build with no coding features pushes nothing, and the empty list is the correct answer there rather than a case to special-case"
    )]
    {
        let mut v: Vec<SharedContentCoding> = Vec::new();
        #[cfg(feature = "zstd")]
        v.push(Arc::new(super::compression::Zstd));
        #[cfg(feature = "brotli")]
        v.push(Arc::new(super::compression::Brotli));
        #[cfg(feature = "gzip")]
        v.push(Arc::new(super::compression::Gzip));
        #[cfg(feature = "deflate")]
        v.push(Arc::new(super::compression::Deflate));
        v
    }
}
