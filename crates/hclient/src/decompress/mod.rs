//! Response decompression: asking for a content coding, and reversing the
//! one the server chose.
//!
//! # Why this is in `Client` and not a `tower` layer
//!
//! It is a test rather than an argument: a layer wrapping the transport
//! changes the CLIENT's type,
//! so `struct App { http: Client }` stops compiling, and
//! `tests/deadline_client_type.rs` plus
//! `tests/compression_client_type.rs` pin exactly that.
//! Decompressing here changes only the response BODY's type, which is
//! already generic over the transport — a `Client` is still a `Client`
//! whether or not it decodes anything.
//!
//! # The order of the two wrappers, and why it is this way round
//!
//! `Client::execute` hands back
//! [`Decompressed`]`<`[`Deadline`](crate::deadline::Deadline)`<T::Body, Tm>>` — the
//! deadline INSIDE, wrapped directly around the transport's own body, and
//! the decoder outside it. Reversed, the bound would be walked around by
//! the very traffic it exists to bound.
//!
//! [`Deadline`](crate::deadline::Deadline) is checked on every poll of itself (it
//! holds no sleep of its own — see its doc comment for why it cannot).
//! With the decoder INSIDE the deadline, one `Deadline::poll_frame` can
//! turn into an unbounded number of polls of the socket: the decoder's
//! loop below keeps pulling compressed frames and feeding them to the
//! decoder, and only RETURNS to its caller when a frame decodes to some
//! output. A server sending highly compressible padding — a megabyte of
//! zeroes is a few hundred compressed bytes, and the reverse arrangement
//! also exists — can therefore stay inside a single outer poll for as long
//! as it likes, and the clock is never consulted. Put the other way round,
//! as it is here, every compressed frame off the wire passes through the
//! deadline check before it reaches the decoder, so the bound is measured
//! against the stream that actually arrives.
//!
//! The same order answers the mirror-image question — a decompression bomb
//! — the same way: the bound is on the bytes the server sent, which is the
//! only quantity a client can hold a server to.
//!
//! The per-coding arguments moved with their code: the `deflate`
//! sniffing rule is in [`deflate`], the `zstd` window cap is in
//! [`zstd`], and the dispatch over all four is in [`decoder`].
//!
//! # What is NOT here
//!
//! - **Request-body compression.** Response only; out of scope for W5.
//! - **`compress`/`x-compress`.** RFC 9110 §8.4.1.1's LZW coding. No
//!   decoder, so it is never advertised and never matched.
//! - **A `q`-value on `Accept-Encoding`.** The header is a plain list in
//!   preference order (see [`Decoders::PREFERENCE`]); RFC 9110 §12.5.3
//!   allows weights and nothing here needs one, because no answer in the
//!   set is worse than no answer at all.
//! - **Telling a caller which `deflate` arrived.** See above: the wire
//!   does not distinguish them, so neither does the accessor.
//! - **Tidying the headers of a transport that decoded for us.** Under
//!   [`DecompressionSupport::Internal`] the response may still carry a
//!   `Content-Encoding` and a `Content-Length` describing the wire rather
//!   than the body handed over — `fetch` does exactly that, and
//!   `hclient-fetch`'s `Body::size_hint` is built around it. This module
//!   strips those two headers only where it decoded the body ITSELF, and
//!   leaves them alone otherwise: a `Client` that rewrote headers over
//!   bytes it never saw would be making a claim on the transport's behalf.
//!   The trigger to revisit is a portable consumer that reads
//!   `Content-Encoding` off a response and gets a different answer per
//!   target for the same server; nothing in this workspace does yet.

mod decoder;
#[cfg(feature = "deflate")]
mod deflate;
#[cfg(feature = "zstd")]
mod zstd;

use crate::error::DecodeFailed;
use decoder::Decoder;
use std::collections::BTreeMap;
use std::sync::OnceLock;

use crate::response::classify_body_error;
use bytes::Bytes;
use hclient_core::{Capabilities, DecompressionSupport, Error, ErrorKind};
use std::error::Error as StdError;
use std::fmt::Debug;
use std::pin::Pin;
use std::task::{Context, Poll};

/// One entry in the [registry](registry): a coding's identity and how to
/// start decoding it.
///
/// **The factory, not a decoder.** A `Decode` is stateful — `push` and
/// `finish` carry one stream's window and its integrity check — so a
/// single instance cannot serve two response bodies, and a registry of
/// decoders would hand the same window to both. What is registered once
/// is the *constructor*; each body calls it and owns what comes back.
/// That is also why the map's value is a `fn()` rather than a `dyn
/// Decode`: a `dyn` is unsized and could not sit in a map by value
/// anyway.
pub(crate) struct Registration {
    /// The token as it appears in `Content-Encoding` and
    /// `Accept-Encoding` — and the key this is registered under.
    pub(crate) token: &'static str,
    /// Aliases this token is also known by, matched
    /// ASCII-case-insensitively like the token itself.
    ///
    /// **Two exist in RFC 9110 and one is reachable here**: §8.4.1.3's
    /// `x-gzip`. §8.4.1.1's `x-compress` names a coding this client does
    /// not reverse. Inventing a third — an `x-deflate`, say — would be
    /// this client deciding what a token nobody specified means, on the
    /// one coding whose wire format it already has to guess at.
    pub(crate) aliases: &'static [&'static str],
    /// Where this coding sits in `Accept-Encoding`, lowest first.
    ///
    /// **Explicit, because the map cannot carry it.** A `BTreeMap` orders
    /// by key, so walking it would ask for `br, deflate, gzip, zstd` —
    /// alphabetical, which puts `deflate` above `gzip` and states a
    /// preference nobody chose. The wire order is a decision (the
    /// densest coding first, and the one whose wire format has to be
    /// guessed at last), so it is a field rather than an accident of
    /// spelling.
    pub(crate) preference: u8,
    /// A fresh decoder for one response body.
    pub(crate) new: fn() -> Decoder,
}

/// Every coding this build can reverse, keyed by its token.
///
/// # Why a registry, and what it replaced
///
/// This was an `enum Coding` with four variants and four `match`es over
/// it — `token`, `decoder`, `coding` and `has` — plus a `Decoders` struct
/// of four `bool`s and a `PREFERENCE` array. Adding a fifth coding meant
/// touching all six, and the compiler could only catch the `match`es: a
/// coding missing from `PREFERENCE` compiled and silently never appeared
/// in `Accept-Encoding`.
///
/// Here a coding is **one [`Registration`]**, declared beside its
/// decoder, and the map is what everything else reads. What the compiler
/// no longer checks — that the list is complete — is checked by
/// `every_registration_is_reachable` instead, which is the trade: a test
/// where there were exhaustive matches.
///
/// # `OnceLock` rather than a `const` table
///
/// The map is built once, on the first response that has a
/// `Content-Encoding` or the first request that sets `Accept-Encoding`,
/// and never again. A `const` table would need `phf` or a sorted-slice
/// binary search written by hand; four entries do not earn either, and
/// `BTreeMap` is what makes the lookup a lookup rather than a chain of
/// `eq_ignore_ascii_case`.
fn registry() -> &'static BTreeMap<&'static str, &'static Registration> {
    static REGISTRY: OnceLock<BTreeMap<&'static str, &'static Registration>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut m = BTreeMap::new();
        for r in REGISTRATIONS {
            // A duplicate token would mean two codings answering to one
            // name, and the second silently winning. `insert` returning
            // `Some` is that, and there is no configuration in which it
            // is not a bug in this file.
            assert!(
                m.insert(r.token, r).is_none(),
                "two decoders registered the same token"
            );
        }
        m
    })
}

/// The codings compiled into this build, in the order they are declared.
///
/// **One `#[cfg]` per coding and no `not(any(..))` anywhere**: a build
/// with no coding features has an empty array, which is an ordinary
/// value, where the enum this replaced had no variants and needed three
/// `match *self {}` arms to say so.
const REGISTRATIONS: &[Registration] = &[
    #[cfg(feature = "zstd")]
    Registration {
        token: "zstd",
        aliases: &[],
        preference: 0,
        new: || Box::new(zstd::ZstdStream::new()),
    },
    #[cfg(feature = "brotli")]
    Registration {
        token: "br",
        aliases: &[],
        preference: 1,
        new: || Box::new(decoder::Brotli::new()),
    },
    #[cfg(feature = "gzip")]
    Registration {
        token: "gzip",
        // RFC 9110 §8.4.1.3's deprecated alias.
        aliases: &["x-gzip"],
        preference: 2,
        new: || Box::new(decoder::Gzip::new()),
    },
    #[cfg(feature = "deflate")]
    Registration {
        token: "deflate",
        aliases: &[],
        // Last, and that is a decision rather than an ordering accident:
        // it is the one coding whose wire format RFC 9110 §8.4.1.2 leaves
        // ambiguous, so this client would rather be offered any other.
        preference: 3,
        new: || Box::new(deflate::DeflateStream::new()),
    },
];

/// The registration a `Content-Encoding` names, if this build has one.
///
/// Matching is ASCII-case-insensitive, as RFC 9110 §8.4.1 requires, and
/// the map's own key is the fast path — an alias costs a walk, which four
/// entries make free and which is the only place a token that is not a
/// token gets looked at twice.
fn lookup(token: &str) -> Option<&'static Registration> {
    let reg = registry();
    if let Some(r) = reg.get(token) {
        return Some(r);
    }
    reg.values().copied().find(|r| {
        r.token.eq_ignore_ascii_case(token)
            || r.aliases.iter().any(|a| a.eq_ignore_ascii_case(token))
    })
}

/// The content codings this build can actually reverse.
///
/// **One value, three readers** — what goes into `Accept-Encoding`, what
/// a `Content-Encoding` is matched against, and which decoder is
/// constructed all come from here, so a client cannot advertise a coding
/// it will not reverse or reverse one it did not ask for.
///
/// It is a `bool` beside the [registry](registry) rather than a set of
/// its own: which codings *exist* is the registry's answer and is fixed
/// at compile time, and the only thing that varies per request is whether
/// this client may decode **at all** — which
/// [`Capabilities::response_decompression`] decides. Carrying a subset
/// would be a second statement of what the registry already says, and the
/// enum-plus-four-`bool`s this replaced was exactly that: `Decoders`
/// re-derived from cargo features what `Coding::decoder` re-derived
/// again, and a test existed to pin that the two agreed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Decoders(bool);

impl Decoders {
    /// What this build can reverse — everything registered, which is what
    /// the coding features decided when they included their
    /// [`Registration`].
    pub(crate) const fn compiled_in() -> Self {
        Self(true)
    }

    /// Nothing may be reversed — what the capability gate returns for a
    /// transport that decodes for us.
    pub(crate) const fn none() -> Self {
        Self(false)
    }

    pub(crate) fn is_empty(self) -> bool {
        !self.0 || REGISTRATIONS.is_empty()
    }

    /// The `Accept-Encoding` value to send, or `None` when there is
    /// nothing to ask for.
    ///
    /// Assembled from the registry rather than from a table of literals,
    /// so the set asked for and the set understood cannot drift, and
    /// neither can their spelling. The order is
    /// [`Registration::preference`] — see there for why it is a field and
    /// not the map's own.
    pub(crate) fn accept_encoding(self) -> Option<http::HeaderValue> {
        if !self.0 {
            return None;
        }
        let mut regs: Vec<&'static Registration> = registry().values().copied().collect();
        regs.sort_by_key(|r| r.preference);
        let value = regs.iter().map(|r| r.token).collect::<Vec<_>>().join(", ");
        if value.is_empty() {
            return None;
        }
        // Infallible: every token is a compile-time ASCII constant from a
        // `Registration`, and the separator is `", "`. Nothing here comes
        // from the network or the caller.
        Some(http::HeaderValue::from_str(&value).expect("content coding tokens are ASCII"))
    }

    /// The decoder for what a `Content-Encoding` names, if this build has
    /// one and this client may decode.
    ///
    /// A single token only, never a LIST (`gzip, br` — two codings
    /// applied in order): reversing one layer of two and then declaring
    /// the body decoded would corrupt it, and no server sends a list to a
    /// client that asked for a single coding. `identity` and an empty
    /// value are the ordinary "not encoded" answers, and neither is
    /// registered.
    pub(crate) fn decoder(self, value: &http::HeaderValue) -> Option<Decoder> {
        if !self.0 {
            return None;
        }
        let token = value.to_str().ok()?.trim();
        Some((lookup(token)?.new)())
    }
}

/// Decides what this client may do about compression for one request, and
/// sets `Accept-Encoding` when it may ask for something.
///
/// Returns the codings that may be reversed on the response — empty means
/// "hand the body through untouched".
///
/// **The gate is [`Capabilities::response_decompression`] and nothing
/// else.** In particular it is NOT
/// [`Capabilities::forbidden_request_headers`], even though the one
/// transport in this workspace that decodes internally also forbids
/// `Accept-Encoding`: those are different claims that coincide there by
/// accident (`DecompressionSupport`'s doc comment says so at the seam).
/// The two are read here for two different purposes, and the third branch
/// below is what keeps them apart — a transport that forbids the header
/// while decoding nothing gets no header from us AND still gets its
/// response decoded, because a `Content-Encoding` the server applied
/// unbidden is still ours to reverse.
///
/// The `forbidden_request_headers` check covers only the header this
/// function itself would add. Filtering a header the CALLER set is a
/// different job, still unimplemented (see `RequestBuilder::headers`),
/// and doing half of it here would be worse than doing none.
pub(crate) fn negotiate(
    headers: &mut http::HeaderMap,
    caps: &Capabilities,
    available: Decoders,
) -> Decoders {
    // The transport already decodes, and chose what to ask for. Decoding
    // again would corrupt every compressed response, and an
    // `Accept-Encoding` of ours could only contradict the one it sent.
    if caps.response_decompression == DecompressionSupport::Internal {
        return Decoders::none();
    }
    if available.is_empty() {
        return Decoders::none();
    }
    // The caller did their own negotiating. Their header stands untouched
    // and their body is handed over as it arrives: a caller asking for
    // `zstd`, or for `identity`, means it, and silently decoding on top of
    // an answer to a question we did not ask is the same class of surprise
    // as overriding the header itself. reqwest makes the same call.
    if headers.contains_key(http::header::ACCEPT_ENCODING) {
        return Decoders::none();
    }
    if !caps
        .forbidden_request_headers
        .contains(&http::header::ACCEPT_ENCODING)
        && let Some(v) = available.accept_encoding()
    {
        headers.insert(http::header::ACCEPT_ENCODING, v);
    }
    available
}

/// The decoder for a response, if its `Content-Encoding` names a coding
/// `allowed` covers — and the two headers that stop being true the moment
/// one is built.
///
/// `Content-Encoding` is removed because the body handed on is no longer
/// encoded, and `Content-Length` because it counts the bytes on the wire,
/// not the ones the caller will read. Leaving either in place is the
/// `size_hint` trap `hclient-fetch`'s `body.rs` documents at length,
/// reproduced one layer up.
pub(crate) fn decoder_for(parts: &mut http::response::Parts, allowed: Decoders) -> Option<Decoder> {
    // The decoder is built BEFORE the headers are touched, so that the
    // only way to lose those two headers is to have something that will
    // actually reverse the coding. Failing the other way round would leave
    // a compressed body labelled as plaintext.
    let decoder = allowed.decoder(parts.headers.get(http::header::CONTENT_ENCODING)?)?;
    parts.headers.remove(http::header::CONTENT_ENCODING);
    parts.headers.remove(http::header::CONTENT_LENGTH);
    Some(decoder)
}

/// The response body with its `Content-Encoding` reversed.
///
/// Always in the type, whether or not anything is being decoded — the same
/// decision [`Deadline`](crate::deadline::Deadline) documents, and for the same reason: a type
/// cannot appear and disappear with a runtime value. When there is nothing
/// to decode the cost is one enum test per frame and every call is
/// forwarded unchanged.
pub(crate) struct Decompressed<B> {
    inner: B,
    state: State,
}

enum State {
    /// Nothing to reverse: frames are forwarded exactly as they arrive.
    Through,
    /// A coding is being reversed. `fed` records whether a single
    /// compressed byte has arrived — see [`Decompressed::poll_frame`] for
    /// why an empty body must not be run through the integrity check.
    Decoding { decoder: Decoder, fed: bool },
    /// The decoded stream has ended, cleanly or with an error.
    Ended,
}

impl<B> Decompressed<B> {
    pub(crate) fn new(inner: B, decoder: Option<Decoder>) -> Self {
        Self {
            inner,
            state: match decoder {
                Some(decoder) => State::Decoding {
                    decoder,
                    fed: false,
                },
                None => State::Through,
            },
        }
    }

    /// The body underneath. For a response out of [`crate::Client`] that
    /// is the [`Deadline`](crate::deadline::Deadline) wrapper, whose own accessors report on
    /// the whole-operation bound — which is how a caller reaches
    /// `total_timeout()`/`is_expired()` through this one.
    pub fn get_ref(&self) -> &B {
        &self.inner
    }

    /// The coding being reversed, as it appeared on the wire — `None` when
    /// the body is being handed through untouched.
    ///
    /// This is how a caller tells "the server did not compress" from "this
    /// build cannot decode what it sent" without guessing from a header
    /// that is no longer there.
    pub fn coding(&self) -> Option<&'static str> {
        match &self.state {
            State::Decoding { decoder, .. } => Some(decoder.token()),
            State::Through | State::Ended => None,
        }
    }
}

/// Hand-written for the same reason [`Deadline`](crate::deadline::Deadline)'s is: the derive
/// would demand `Debug` of things that do not need it, and the decoder's
/// window is not worth printing.
impl<B: Debug> Debug for Decompressed<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Decompressed")
            .field("inner", &self.inner)
            .field(
                "coding",
                &match &self.state {
                    State::Through => "none",
                    State::Decoding { decoder, .. } => decoder.token(),
                    State::Ended => "ended",
                },
            )
            .finish()
    }
}

impl<B> http_body::Body for Decompressed<B>
where
    B: http_body::Body<Data = Bytes> + Unpin,
    // Same `send-bound-exception: amendment-C1` point `Deadline` and
    // `Response::chunk` already stand on: the error is re-classified into
    // `hclient_core::Error`, whose source is an `Arc<dyn Error + Send +
    // Sync>`.
    B::Error: StdError + Send + Sync + 'static, // send-bound-exception: amendment-C1
{
    type Data = Bytes;
    /// Not `B::Error`: a corrupt gzip stream has no `B::Error` to be, and
    /// one cannot be invented for a generic `B`. Re-classification goes
    /// through the same `classify_body_error` `Deadline` uses, so a body
    /// error that was already an `Error` — a fired deadline, most of all —
    /// keeps the category it was given.
    type Error = Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, Error>>> {
        // `B: Unpin`, so no projection and no `unsafe` — the crate forbids
        // it, exactly as in `Deadline::poll_frame`.
        let this = self.get_mut();
        loop {
            let (decoder, fed) = match &mut this.state {
                State::Through => {
                    return Pin::new(&mut this.inner)
                        .poll_frame(cx)
                        .map(|o| o.map(|r| r.map_err(classify_body_error)));
                }
                State::Ended => return Poll::Ready(None),
                State::Decoding { decoder, fed } => (decoder, fed),
            };

            // Polling the INNER body here, once per compressed frame, is
            // what makes the wrapper order load-bearing: the deadline sits
            // inside, so it is consulted for every frame off the wire even
            // though this loop may go round many times before it yields
            // anything to the caller. See the module doc comment.
            let frame = match Pin::new(&mut this.inner).poll_frame(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(v) => v,
            };

            match frame {
                Some(Ok(frame)) => match frame.into_data() {
                    Ok(data) => {
                        *fed = true;
                        match decoder.push(&data) {
                            Ok(out) if out.is_empty() => continue,
                            Ok(out) => return Poll::Ready(Some(Ok(http_body::Frame::data(out)))),
                            Err(e) => {
                                let token = decoder.token();
                                this.state = State::Ended;
                                return Poll::Ready(Some(Err(decode_error(token, e))));
                            }
                        }
                    }
                    // Trailers travel on untouched: they are not part of
                    // the coded stream, and `Response::chunk` skips them
                    // anyway.
                    Err(other) => return Poll::Ready(Some(Ok(other))),
                },
                Some(Err(e)) => {
                    this.state = State::Ended;
                    return Poll::Ready(Some(Err(classify_body_error(e))));
                }
                None => {
                    // A body with no bytes at all under a
                    // `Content-Encoding` is not a truncated stream: 204,
                    // 304 and the response to a HEAD all legitimately
                    // carry the header with nothing after it, and running
                    // gzip's trailer check over zero bytes would turn each
                    // of them into a spurious `Decode` error. Only a
                    // stream that actually started has an end to be
                    // missing.
                    let out = if *fed {
                        let token = decoder.token();
                        match decoder.finish() {
                            Ok(out) => out,
                            Err(e) => {
                                this.state = State::Ended;
                                return Poll::Ready(Some(Err(decode_error(token, e))));
                            }
                        }
                    } else {
                        Bytes::new()
                    };
                    this.state = State::Ended;
                    if out.is_empty() {
                        return Poll::Ready(None);
                    }
                    return Poll::Ready(Some(Ok(http_body::Frame::data(out))));
                }
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        match &self.state {
            State::Through => self.inner.is_end_stream(),
            // An inner body that has ended is NOT the end of this one:
            // the decoder may still be holding buffered plaintext, and its
            // integrity check has not run. Saying `true` here would let a
            // caller conclude a truncated response was complete.
            State::Decoding { .. } => false,
            State::Ended => true,
        }
    }

    fn size_hint(&self) -> http_body::SizeHint {
        match &self.state {
            State::Through => self.inner.size_hint(),
            // No promise at all, rather than a guess: the inner hint
            // counts compressed bytes, and the ratio is the server's
            // business. This is the same discipline `hclient-fetch`'s
            // `content_length_hint` applies to `Content-Length` under a
            // `Content-Encoding`, one layer up.
            State::Decoding { .. } => http_body::SizeHint::default(),
            State::Ended => http_body::SizeHint::with_exact(0),
        }
    }
}

fn decode_error(coding: &'static str, source: std::io::Error) -> Error {
    Error::new(ErrorKind::Decode, DecodeFailed { coding, source })
}

#[cfg(test)]
mod tests {
    use super::*;
    use hclient_core::{Capabilities, DecompressionSupport};

    fn caps(d: DecompressionSupport) -> Capabilities {
        let mut c = Capabilities::default();
        c.response_decompression = d;
        c
    }

    /// Everything this build registered.
    const ALL: Decoders = Decoders::compiled_in();

    /// **The property the registry makes structural, pinned anyway.**
    ///
    /// This was `every_advertised_coding_has_a_decoder_in_this_build`, and
    /// it existed because `Decoders::compiled_in` read the cargo features
    /// to decide what to ADVERTISE while `Coding::decoder` read them again
    /// to decide what could be BUILT — two readings that could disagree,
    /// and a client advertising `br` in a build with no brotli decoder
    /// receives bytes nothing can read.
    ///
    /// One [`Registration`] carries both now, so they cannot disagree: the
    /// token that goes into `Accept-Encoding` and the `new` that builds
    /// the decoder are fields of one value, behind one `#[cfg]`. What is
    /// left to check is that the registry is *reachable* — that every
    /// registration can be looked up by the token it registered under,
    /// which is what would break if two codings ever registered one name
    /// or if `lookup` stopped matching the map's own key.
    #[test]
    fn every_registration_is_reachable_by_its_own_token() {
        for r in REGISTRATIONS {
            let found = lookup(r.token)
                .unwrap_or_else(|| panic!("`{}` is registered and cannot be looked up", r.token));
            assert_eq!(found.token, r.token);
            for alias in r.aliases {
                let found =
                    lookup(alias).unwrap_or_else(|| panic!("alias `{alias}` does not resolve"));
                assert_eq!(
                    found.token, r.token,
                    "`{alias}` resolves to the wrong coding"
                );
            }
        }
        assert_eq!(
            registry().len(),
            REGISTRATIONS.len(),
            "two codings registered the same token, and one silently won"
        );
    }

    /// A token is matched ASCII-case-insensitively — RFC 9110 §8.4.1 —
    /// and the map's own key is only the fast path.
    #[test]
    fn a_registered_token_is_matched_whatever_its_case() {
        for r in REGISTRATIONS {
            let shouted = r.token.to_ascii_uppercase();
            let found = lookup(&shouted)
                .unwrap_or_else(|| panic!("`{shouted}` did not match `{}`", r.token));
            assert_eq!(found.token, r.token);
        }
    }

    /// `accept_encoding` names exactly what is registered, and nothing
    /// else — the property, not the string. A literal
    /// `assert_eq!(.., "zstd, br, gzip, deflate")` would pass just as
    /// happily for a build with three decoders switched off, which is why
    /// that assertion lives in the one test below that is about the
    /// *order*.
    #[test]
    fn accept_encoding_names_exactly_what_can_be_decoded() {
        let Some(v) = ALL.accept_encoding() else {
            assert!(
                ALL.is_empty(),
                "only an empty registry may advertise nothing"
            );
            return;
        };
        let mut count = 0;
        for token in v.to_str().unwrap().split(',') {
            count += 1;
            let one = http::HeaderValue::from_str(token.trim()).unwrap();
            assert!(
                ALL.decoder(&one).is_some(),
                "advertised `{token}`, which this build cannot decode"
            );
        }
        assert_eq!(
            count,
            REGISTRATIONS.len(),
            "advertised {count} of {} registered codings",
            REGISTRATIONS.len()
        );
    }

    /// `deflate` is offered last, and that is a decision rather than the
    /// order anything happens to be declared in: it is the one coding
    /// whose wire format this client has to guess at, so a server picking
    /// the first token it knows should reach for it only when it has
    /// nothing else.
    ///
    /// **This is also what says the registry did not reorder them.** A
    /// `BTreeMap` orders by key, so a walk of it advertises `br, deflate,
    /// gzip, zstd` — alphabetical, `deflate` second — which is why
    /// [`Registration::preference`] is a field. Written as a literal
    /// deliberately, and only here.
    #[cfg(all(
        feature = "gzip",
        feature = "brotli",
        feature = "deflate",
        feature = "zstd"
    ))]
    #[test]
    fn the_ambiguous_coding_is_offered_last() {
        let v = ALL.accept_encoding().expect("something is compiled in");
        assert_eq!(
            v.to_str().unwrap(),
            "zstd, br, gzip, deflate",
            "the order is preference, best first and the guess last"
        );
    }

    /// A coding this build did not register is not matched — which is
    /// what stops a client from handing a caller a body nothing here can
    /// read.
    ///
    /// **It is checked against the registry rather than against a
    /// hand-built subset**, because there is no subset to build any more:
    /// `Decoders` was four `bool`s that a test could set independently of
    /// the cargo features, and it is one bit now. So the case this test
    /// wants — a token that is *not* registered — is written as a token
    /// no build ever registers, and the feature-specific half is covered
    /// by `just features`, which compiles this file in all sixteen
    /// combinations and runs the two tests above in each.
    #[test]
    fn an_unregistered_coding_is_not_matched() {
        for token in ["compress", "x-compress", "identity", "", "lz4"] {
            assert_eq!(
                lookup(token).map(|r| r.token),
                None,
                "`{token}` is not a coding this crate registers"
            );
        }
        // The control: without it this passes for a registry that matches
        // nothing at all.
        for r in REGISTRATIONS {
            assert!(lookup(r.token).is_some());
        }
    }

    #[test]
    fn content_encoding_matching_is_case_insensitive_and_rejects_lists() {
        #[cfg(feature = "gzip")]
        {
            assert!(
                ALL.decoder(&http::HeaderValue::from_static("GZIP"))
                    .is_some()
            );
            assert!(
                ALL.decoder(&http::HeaderValue::from_static(" x-gzip "))
                    .is_some(),
                "RFC 9110 §8.4.1.3's deprecated alias, and the surrounding \
                 whitespace a header may carry"
            );
        }
        assert!(
            ALL.decoder(&http::HeaderValue::from_static("identity"))
                .is_none()
        );
        assert!(ALL.decoder(&http::HeaderValue::from_static("")).is_none());
        assert!(
            ALL.decoder(&http::HeaderValue::from_static("gzip, br"))
                .is_none(),
            "two codings applied in order: reversing one and calling the body \
             decoded would corrupt it"
        );
    }

    #[test]
    fn an_internal_transport_gets_no_header_and_no_decoding() {
        let mut h = http::HeaderMap::new();
        let d = negotiate(&mut h, &caps(DecompressionSupport::Internal), ALL);
        assert!(d.is_empty(), "decoding twice would corrupt every response");
        assert!(!h.contains_key(http::header::ACCEPT_ENCODING));
    }

    /// The half the `FORBIDDEN_HEADERS` shortcut would get wrong: the two
    /// claims come apart here, and the answers must differ.
    #[test]
    fn a_transport_that_forbids_the_header_but_decodes_nothing_still_gets_decoding() {
        let mut c = caps(DecompressionSupport::None);
        c.forbidden_request_headers = &[http::header::ACCEPT_ENCODING];
        let mut h = http::HeaderMap::new();
        let d = negotiate(&mut h, &c, ALL);
        assert!(
            !h.contains_key(http::header::ACCEPT_ENCODING),
            "the transport forbids this header; we must not add it"
        );
        // **The property, not the value.** In a build with no coding
        // features `ALL` is "this client may decode" while the registry
        // is empty, so `negotiate` answers `none()` and the two are
        // unequal — which is correct and which an `assert_eq!(d, ALL)`
        // reads as a failure. That equality held only because the old
        // `Decoders` was four `bool`s, so "may decode" and "has a
        // decoder" were one value; splitting them is what let the
        // registry be the single list of codings. `just test-no-default`
        // is what caught it.
        assert_eq!(
            d.is_empty(),
            ALL.is_empty(),
            "a `Content-Encoding` the server applied unbidden is still ours to reverse"
        );
    }

    #[test]
    fn a_caller_who_set_accept_encoding_keeps_it_and_gets_the_raw_body() {
        let mut h = http::HeaderMap::new();
        h.insert(
            http::header::ACCEPT_ENCODING,
            http::HeaderValue::from_static("zstd"),
        );
        let d = negotiate(&mut h, &caps(DecompressionSupport::None), ALL);
        assert_eq!(
            h[http::header::ACCEPT_ENCODING],
            "zstd",
            "the caller's own negotiation stands"
        );
        assert!(
            d.is_empty(),
            "decoding an answer to a question we did not ask is the same surprise \
             as overriding the header"
        );
    }

    #[test]
    fn decoding_strips_the_two_headers_that_stop_being_true() {
        let mut parts = http::Response::builder()
            .header(http::header::CONTENT_ENCODING, "gzip")
            .header(http::header::CONTENT_LENGTH, "42")
            .header(http::header::CONTENT_TYPE, "text/plain")
            .body(())
            .unwrap()
            .into_parts()
            .0;
        let got = decoder_for(&mut parts, ALL);
        assert_eq!(got.is_some(), cfg!(feature = "gzip"));
        if got.is_some() {
            assert!(!parts.headers.contains_key(http::header::CONTENT_ENCODING));
            assert!(!parts.headers.contains_key(http::header::CONTENT_LENGTH));
            assert_eq!(
                parts.headers[http::header::CONTENT_TYPE],
                "text/plain",
                "only the two headers about the encoding may be touched"
            );
        }
    }

    /// The example must be a coding this crate has no decoder for: the
    /// assertion is *a coding we cannot reverse is left alone*, so naming
    /// one that later gains a decoder turns the test red — correctly.
    /// `compress` is RFC 9110
    /// §8.4.1.1's LZW, named in the module doc as a coding with no decoder
    /// here, so the test will fail again on the day that stops being true.
    #[test]
    fn a_body_we_do_not_decode_keeps_its_headers() {
        let mut parts = http::Response::builder()
            .header(http::header::CONTENT_ENCODING, "compress")
            .header(http::header::CONTENT_LENGTH, "42")
            .body(())
            .unwrap()
            .into_parts()
            .0;
        assert!(decoder_for(&mut parts, ALL).is_none());
        assert_eq!(parts.headers[http::header::CONTENT_ENCODING], "compress");
        assert_eq!(
            parts.headers[http::header::CONTENT_LENGTH],
            "42",
            "the body really is 42 encoded bytes long — we changed nothing"
        );
    }
}
