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
//! [`Decompressed`]`<`[`Deadline`](crate::body::Deadline)`<T::Body, Tm>>` — the
//! deadline INSIDE, wrapped directly around the transport's own body, and
//! the decoder outside it. Reversed, the bound would be walked around by
//! the very traffic it exists to bound.
//!
//! [`Deadline`](crate::body::Deadline) is checked on every poll of itself (it
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

use decoder::Decoder;
// The same `#[cfg]` its definition carries: `just features` builds each
// coding alone, and an unconditional import of a `brotli`-gated constant
// is unresolved in fifteen of the sixteen sets.
#[cfg(feature = "brotli")]
use decoder::BROTLI_BUFFER;
#[cfg(feature = "deflate")]
use deflate::DeflateStream;
#[cfg(feature = "zstd")]
use zstd::ZstdStream;

use crate::response::classify_body_error;
use bytes::Bytes;
use hclient_core::{Capabilities, DecompressionSupport, Error, ErrorKind};
use std::error::Error as StdError;
use std::fmt::Debug;
use std::pin::Pin;
use std::task::{Context, Poll};

/// A content coding this client can reverse.
///
/// Deliberately not `pub`: it names what the build can do, and the answer
/// belongs to [`Decoders`], which is the only thing allowed to produce one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Coding {
    Gzip,
    Brotli,
    Deflate,
    Zstd,
}

impl Coding {
    /// The token as it appears in `Content-Encoding` / `Accept-Encoding`.
    fn token(self) -> &'static str {
        match self {
            Coding::Gzip => "gzip",
            Coding::Brotli => "br",
            Coding::Deflate => "deflate",
            Coding::Zstd => "zstd",
        }
    }

    /// A fresh decoder for this coding, or `None` if this build did not
    /// compile one in.
    ///
    /// An `Option` rather than an `unreachable!()` guarded by "the caller
    /// checked": this and [`Decoders::has`] are two readings of the same
    /// cargo features, and `every_advertised_coding_has_a_decoder_in_this_
    /// build` below pins that they agree — in every feature combination,
    /// since that test compiles into all four. If they ever stop agreeing,
    /// the failure here is "the body is handed over untouched, headers and
    /// all", which is a correct response, rather than a panic.
    fn decoder(self) -> Option<Decoder> {
        match self {
            #[cfg(feature = "gzip")]
            Coding::Gzip => Some(Decoder::Gzip(Box::new(flate2::write::GzDecoder::new(
                Vec::new(),
            )))),
            #[cfg(feature = "brotli")]
            Coding::Brotli => Some(Decoder::Brotli(Some(Box::new(
                brotli_decompressor::writer::DecompressorWriter::new(Vec::new(), BROTLI_BUFFER),
            )))),
            #[cfg(feature = "deflate")]
            Coding::Deflate => Some(Decoder::Deflate(Box::new(DeflateStream::new()))),
            #[cfg(feature = "zstd")]
            Coding::Zstd => Some(Decoder::Zstd(Box::new(ZstdStream::new()))),
            // Whichever codings this build has no decoder for. Written as
            // a wildcard rather than named arms because which names are
            // left depends on the feature set, and `#[cfg]`-ing the arm
            // list four times over would say the same thing less clearly.
            #[cfg(not(all(
                feature = "gzip",
                feature = "brotli",
                feature = "deflate",
                feature = "zstd"
            )))]
            _ => None,
        }
    }
}

/// The content codings this build can actually reverse.
///
/// **One value, three readers** — what goes into `Accept-Encoding`, what a
/// `Content-Encoding` is matched against, and which decoder is
/// constructed all come from here. That is the point, and it is
/// `hclient-native`'s `reuse_of` recipe applied to a different capability:
/// a client that advertised `br` in a build without the `brotli` feature
/// would be asking for bytes it cannot read, which is the same defect as a
/// capability that lies, entered from the request side.
///
/// [`Self::compiled_in`] reads the cargo features with `cfg!` — an
/// expression, so there is no `#[cfg]` branch switching behaviour here,
/// only two booleans whose value differs per build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Decoders {
    gzip: bool,
    brotli: bool,
    deflate: bool,
    zstd: bool,
}

impl Decoders {
    /// Every coding this crate knows how to name, **in the order they go
    /// into `Accept-Encoding`**, which is a decision rather than the order
    /// they were written in.
    ///
    /// Best first, and `deflate` deliberately LAST. RFC 9110 §12.5.3 puts
    /// no meaning on the order of an unweighted list, so nothing here is
    /// entitled to a particular answer — but a server that walks the list
    /// and takes the first token it supports is the common
    /// implementation, and the one coding whose wire format this client
    /// has to GUESS at (see `deflate`'s module doc) is the one to be offered
    /// least often. Browsers send `gzip, deflate, br, zstd`, which is
    /// chronological rather than preferential; correctness here does not
    /// depend on either order, only exposure does.
    ///
    /// One array, three readers: [`Self::accept_encoding`] walks it,
    /// [`Self::has`] and [`Coding::token`] are what it walks with, and
    /// the tests iterate it so that adding a fifth coding without
    /// teaching `coding()` to match it fails a line.
    pub(crate) const PREFERENCE: [Coding; 4] =
        [Coding::Zstd, Coding::Brotli, Coding::Gzip, Coding::Deflate];

    /// What this build can reverse, from the features that pulled the
    /// decoders in.
    pub(crate) const fn compiled_in() -> Self {
        Self {
            gzip: cfg!(feature = "gzip"),
            brotli: cfg!(feature = "brotli"),
            deflate: cfg!(feature = "deflate"),
            zstd: cfg!(feature = "zstd"),
        }
    }

    /// Nothing may be reversed — what the capability gate returns for a
    /// transport that decodes for us, and what a build with none of the
    /// features has anyway.
    pub(crate) const fn none() -> Self {
        Self {
            gzip: false,
            brotli: false,
            deflate: false,
            zstd: false,
        }
    }

    pub(crate) const fn is_empty(self) -> bool {
        !self.gzip && !self.brotli && !self.deflate && !self.zstd
    }

    /// The `Accept-Encoding` value to send, or `None` when there is
    /// nothing to ask for.
    ///
    /// Assembled from [`Self::has`] and [`Coding::token`] — the same two
    /// functions [`Self::coding`] matches an incoming `Content-Encoding`
    /// with — rather than from a table of literal strings, so the set
    /// asked for and the set understood cannot drift, and neither can
    /// their spelling.
    pub(crate) fn accept_encoding(self) -> Option<http::HeaderValue> {
        let mut value = String::new();
        for coding in Self::PREFERENCE {
            if self.has(coding) {
                if !value.is_empty() {
                    value.push_str(", ");
                }
                value.push_str(coding.token());
            }
        }
        if value.is_empty() {
            return None;
        }
        // Infallible: every token is a compile-time ASCII constant from
        // `Coding::token`, and the separator is `", "`. Nothing here comes
        // from the network or the caller.
        Some(http::HeaderValue::from_str(&value).expect("content coding tokens are ASCII"))
    }

    /// The coding named by a response's `Content-Encoding`, if it is one
    /// this build can reverse.
    ///
    /// `None` for anything else, and "anything else" deliberately includes
    /// a LIST (`gzip, br` — two codings applied in order): reversing one
    /// layer of two and then declaring the body decoded would corrupt it,
    /// and no server sends a list to a client that asked for a single
    /// coding. `identity` and an empty value are the ordinary "not
    /// encoded" answers. Matching is ASCII-case-insensitive, as RFC 9110
    /// §8.4.1 requires; `x-gzip` is accepted as the deprecated alias for
    /// `gzip` that RFC 9110 §8.4.1.3 still names.
    ///
    /// There is deliberately **no `x-deflate`**: RFC 9110 names exactly
    /// two `x-` aliases, §8.4.1.1's `x-compress` and §8.4.1.3's `x-gzip`,
    /// and inventing a third would be this client deciding what a token
    /// nobody specified means — on the one coding whose wire format it is
    /// already having to guess at.
    pub(crate) fn coding(self, value: &http::HeaderValue) -> Option<Coding> {
        let token = value.to_str().ok()?.trim();
        let coding = if token.eq_ignore_ascii_case("gzip") || token.eq_ignore_ascii_case("x-gzip") {
            Coding::Gzip
        } else if token.eq_ignore_ascii_case("br") {
            Coding::Brotli
        } else if token.eq_ignore_ascii_case("deflate") {
            Coding::Deflate
        } else if token.eq_ignore_ascii_case("zstd") {
            Coding::Zstd
        } else {
            return None;
        };
        self.has(coding).then_some(coding)
    }

    pub(crate) const fn has(self, coding: Coding) -> bool {
        match coding {
            Coding::Gzip => self.gzip,
            Coding::Brotli => self.brotli,
            Coding::Deflate => self.deflate,
            Coding::Zstd => self.zstd,
        }
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
    let coding = allowed.coding(parts.headers.get(http::header::CONTENT_ENCODING)?)?;
    // The decoder is built BEFORE the headers are touched, so that the
    // only way to lose those two headers is to have something that will
    // actually reverse the coding. Failing the other way round would leave
    // a compressed body labelled as plaintext.
    let decoder = coding.decoder()?;
    parts.headers.remove(http::header::CONTENT_ENCODING);
    parts.headers.remove(http::header::CONTENT_LENGTH);
    Some(decoder)
}

/// The response body did not decode as its `Content-Encoding` promised.
///
/// `pub` and re-exported for the same reason [`crate::error::TotalTimeoutElapsed`]
/// and [`crate::error::InvalidBaseUrl`] are: a caller has to be able to tell this
/// apart from every other `ErrorKind::Decode` — a body that is not valid
/// gzip is a different problem from a body that is not valid UTF-8 — and
/// `Error::source().downcast_ref::<DecodeFailed>()` is the way.
#[derive(Debug, thiserror::Error)]
#[error("the response body is not valid `{coding}` data")]
pub struct DecodeFailed {
    /// The coding that was attempted, as it appeared on the wire.
    pub coding: &'static str,
    #[source]
    source: std::io::Error,
}

/// The response body with its `Content-Encoding` reversed.
///
/// Always in the type, whether or not anything is being decoded — the same
/// decision [`crate::body::Deadline`] documents, and for the same reason: a type
/// cannot appear and disappear with a runtime value. When there is nothing
/// to decode the cost is one enum test per frame and every call is
/// forwarded unchanged.
pub struct Decompressed<B> {
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
    /// is the [`crate::body::Deadline`] wrapper, whose own accessors report on
    /// the whole-operation bound — which is how a caller reaches
    /// `total_timeout()`/`is_expired()` through this one.
    pub fn get_ref(&self) -> &B {
        &self.inner
    }

    /// Unwraps to the body underneath, **abandoning any partially decoded
    /// stream**: whatever the decoder is holding is dropped with it, and
    /// what is left is the encoded remainder. Fine before reading starts,
    /// which is what it is for; a way to lose bytes in the middle.
    pub fn into_inner(self) -> B {
        self.inner
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

/// Hand-written for the same reason [`crate::body::Deadline`]'s is: the derive
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

    /// Every coding this crate knows, on. Named `ALL` rather than `BOTH`
    /// since W5's two became four.
    const ALL: Decoders = Decoders {
        gzip: true,
        brotli: true,
        deflate: true,
        zstd: true,
    };

    /// One member of `Decoders` per bit of `mask`, in
    /// [`Decoders::PREFERENCE`] order — so `subsets()` below enumerates
    /// all sixteen without naming any of them, which is the point: a
    /// fifth coding makes these tests cover thirty-two by arithmetic
    /// rather than by somebody remembering to add eight literals.
    fn from_mask(mask: u32) -> Decoders {
        let mut d = Decoders::none();
        for (i, coding) in Decoders::PREFERENCE.into_iter().enumerate() {
            if mask & (1 << i) == 0 {
                continue;
            }
            match coding {
                Coding::Gzip => d.gzip = true,
                Coding::Brotli => d.brotli = true,
                Coding::Deflate => d.deflate = true,
                Coding::Zstd => d.zstd = true,
            }
        }
        d
    }

    fn subsets() -> impl Iterator<Item = Decoders> {
        (0..(1u32 << Decoders::PREFERENCE.len())).map(from_mask)
    }

    /// The one-fact property, checked in whichever build is running — and
    /// this test compiles into all sixteen feature combinations, so
    /// between CI's `--all-features` run and `idn-feature-is-real`'s
    /// `--no-default-features` one it is checked in more than the maximal
    /// build.
    ///
    /// What it stops: `Decoders::compiled_in` reads the cargo features to
    /// decide what to ADVERTISE, and `Coding::decoder` reads them again to
    /// decide what can be BUILT. A client that asked for `br` in a build
    /// without the brotli decoder would receive bytes nothing here can
    /// read — the request-side form of a capability that lies.
    #[test]
    fn every_advertised_coding_has_a_decoder_in_this_build() {
        for coding in Decoders::PREFERENCE {
            assert_eq!(
                Decoders::compiled_in().has(coding),
                coding.decoder().is_some(),
                "`{}` is advertised by one reading of the features and not the other",
                coding.token()
            );
        }
    }

    /// `PREFERENCE` is the only list of codings in this module, and
    /// everything else reads it. A coding left out of it would be
    /// unreachable from `accept_encoding` while still matching in
    /// `coding()` — a client that decodes something it never asked for,
    /// which is the shape the caller-set-header rule exists against.
    #[test]
    fn the_preference_list_names_every_coding_exactly_once() {
        let mut tokens: Vec<&str> = Decoders::PREFERENCE.iter().map(|c| c.token()).collect();
        tokens.sort_unstable();
        tokens.dedup();
        assert_eq!(
            tokens.len(),
            Decoders::PREFERENCE.len(),
            "a duplicate would be advertised twice in one header"
        );
        for coding in Decoders::PREFERENCE {
            let v = http::HeaderValue::from_static(match coding {
                Coding::Gzip => "gzip",
                Coding::Brotli => "br",
                Coding::Deflate => "deflate",
                Coding::Zstd => "zstd",
            });
            assert_eq!(
                ALL.coding(&v),
                Some(coding),
                "`{}` is in the preference list and is not matched back",
                coding.token()
            );
        }
    }

    #[test]
    fn accept_encoding_names_exactly_what_can_be_decoded() {
        // The property, not the string: every token advertised must be one
        // `coding` recognises, for each of the sixteen possible builds. A
        // literal `assert_eq!(.., "zstd, br, gzip, deflate")` would pass
        // just as happily for a build with three of the decoders switched
        // off.
        let mut seen = 0;
        for d in subsets() {
            seen += 1;
            let Some(v) = d.accept_encoding() else {
                assert!(d.is_empty(), "only an empty set may advertise nothing");
                continue;
            };
            let mut count = 0;
            for token in v.to_str().unwrap().split(',') {
                count += 1;
                let one = http::HeaderValue::from_str(token.trim()).unwrap();
                assert!(
                    d.coding(&one).is_some(),
                    "advertised `{token}` that {d:?} cannot decode"
                );
            }
            // And the other direction, which the loop above cannot see: a
            // set of three that advertised two would satisfy every
            // assertion so far.
            let want = Decoders::PREFERENCE.iter().filter(|c| d.has(**c)).count();
            assert_eq!(
                count, want,
                "{d:?} advertised {count} of its {want} codings"
            );
        }
        assert_eq!(seen, 16, "the point of this test is that it is exhaustive");
    }

    /// `deflate` is offered last, and that is a decision (see
    /// `Decoders::PREFERENCE`) rather than the order the fields happen to
    /// be declared in: it is the one coding whose wire format this client
    /// has to guess at, so a server picking the first token it knows
    /// should reach for it only when it has nothing else.
    #[test]
    fn the_ambiguous_coding_is_offered_last() {
        let v = ALL.accept_encoding().expect("something is compiled in");
        assert_eq!(
            v.to_str().unwrap(),
            "zstd, br, gzip, deflate",
            "the order is preference, best first and the guess last"
        );
    }

    #[test]
    fn a_coding_that_is_not_compiled_in_is_not_matched() {
        let gzip_only = Decoders {
            gzip: true,
            brotli: false,
            deflate: false,
            zstd: false,
        };
        assert_eq!(
            gzip_only.coding(&http::HeaderValue::from_static("br")),
            None,
            "matching `br` in a build without the decoder would hand a caller \
             a body nothing can read"
        );
        assert_eq!(
            gzip_only.coding(&http::HeaderValue::from_static("gzip")),
            Some(Coding::Gzip)
        );
    }

    #[test]
    fn content_encoding_matching_is_case_insensitive_and_rejects_lists() {
        assert_eq!(
            ALL.coding(&http::HeaderValue::from_static("GZIP")),
            Some(Coding::Gzip)
        );
        assert_eq!(
            ALL.coding(&http::HeaderValue::from_static(" x-gzip ")),
            Some(Coding::Gzip)
        );
        assert_eq!(
            ALL.coding(&http::HeaderValue::from_static("identity")),
            None
        );
        assert_eq!(ALL.coding(&http::HeaderValue::from_static("")), None);
        assert_eq!(
            ALL.coding(&http::HeaderValue::from_static("gzip, br")),
            None,
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
        assert_eq!(
            d, ALL,
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
