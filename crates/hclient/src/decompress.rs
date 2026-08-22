//! Response decompression: asking for a content coding, and reversing the
//! one the server chose.
//!
//! # Why this is in `Client` and not a `tower` layer
//!
//! `docs/v02-design.md` §W5 settles it, and it is no longer an argument
//! but a test: a layer wrapping the transport changes the CLIENT's type,
//! so `struct App { http: Client }` stops compiling, and
//! `tests/deadline_client_type.rs` plus
//! `tests/compression_client_type.rs` (this task) pin exactly that.
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
//! # The `deflate` wrapper question, and why it is answered by looking
//!
//! RFC 9110 §8.4.1.2 is unambiguous about what the token means and
//! equally unambiguous that the wire is not:
//!
//! > The "deflate" coding is a "zlib" data format [RFC1950] containing a
//! > "deflate" compressed data stream [RFC1951] […]
//! >
//! > | *Note:* Some non-conformant implementations send the "deflate"
//! > | compressed data without the zlib wrapper.
//!
//! So a client that advertises `deflate` is taking on a guess, and the
//! only question is where the guess is made. **It is made once, from the
//! first two bytes, before a single byte of output exists** —
//! [`DeflateStream::looks_like_zlib`]. A caller gets the same thing for
//! both encodings on the wire: the plaintext, `Content-Encoding` and
//! `Content-Length` stripped, and [`Decompressed::coding`] reading
//! `"deflate"` — which is what that accessor promises, the token *as it
//! appeared on the wire*, and the wire does not distinguish them either.
//! A stream that then fails to decode is [`DecodeFailed`] with
//! `coding == "deflate"`, never a short body and never the encoded bytes
//! handed on as if they were plain.
//!
//! The reason two bytes are enough is not probability, it is RFC 1951
//! §3.2.3. zlib's first byte carries `CM` in its low nibble and `deflate`
//! is `CM == 8`; a RAW stream's first byte carries `BFINAL` in bit 0 and
//! `BTYPE` in bits 1-2, so a low nibble of `8` means `BFINAL = 0`,
//! `BTYPE = 00` — a stored block — **with bit 3 set**, and §3.2.4 has a
//! decoder skip the remaining bits of that byte, which every encoder
//! therefore writes as zero. A conformant raw stream cannot present
//! `CM == 8`. The `(CMF << 8 | FLG) % 31 == 0` check is a second,
//! independent one.
//!
//! curl answers the same question the other way, and it is worth knowing
//! which is stronger. `lib/content_encoding.c` always tries zlib and, on
//! `Z_DATA_ERROR`, calls `inflateReset2(z, -MAX_WBITS)` and replays the
//! buffer — but only while `zlib_init == ZLIB_INIT`, and the comment
//! three lines below says why: *"If we are in a state that would wrongly
//! allow restart in raw mode at the next call, assume output has already
//! started."* A retry is available only before the first output byte, so
//! it is a rule with a window; looking at the header is a rule without
//! one, and it needs no replay buffer beyond those two bytes.
//!
//! # The `zstd` window, which is the one thing a server can make us
//! allocate
//!
//! RFC 8878 §3.1.1.1.2: *"To properly decode compressed data, a decoder
//! will need to allocate a buffer of at least Window_Size bytes"*, and
//! `Window_Size` is declared in the frame header by whoever compressed
//! it — up to 3.75 TB. The same section grants the defence and names the
//! number to use:
//!
//! > In order to protect decoders from unreasonable memory requirements,
//! > a decoder is allowed to reject a compressed frame that requests a
//! > memory size beyond the decoder's authorized range.
//! >
//! > For improved interoperability, it's recommended for decoders to
//! > support values of Window_Size up to 8 MB and for encoders not to
//! > generate frames requiring a Window_Size larger than 8 MB.
//!
//! [`ZSTD_MAX_WINDOW`] is that 8 MB, and it is the same answer Chrome
//! reached for `Content-Encoding: zstd` (`ZSTD_d_windowLogMax`, 8 MiB,
//! with its own net error code for the frames it turns away). `ruzstd`
//! defaults to **100 MB** — `DEFAULT_MAX_WINDOW_SIZE`, read rather than
//! assumed — so this is a narrowing of somebody else's default and not a
//! belt on a bare waist. The check is made in `ruzstd` *before* the
//! allocation (`FrameDecoderState::new` calls `check_window_size` and
//! only then `DecoderScratch::new(window_size)`), so a rejected frame
//! costs nothing.
//!
//! **[`crate::limit::Limited`] cannot stand in for this**, which is why the
//! number is here at all: it counts bytes yielded to the caller, and a
//! window is one allocation made before the first byte is yielded. Two
//! further facts, both measured by reading `ruzstd` 0.9:
//!
//! - The window is not always reserved up front. `DecoderScratch::new`
//!   builds an EMPTY ring buffer that grows with the data, so a first
//!   frame declaring 8 MB and carrying ten bytes allocates ten bytes'
//!   worth. `DecodeBuffer::reset` — the path a SECOND frame takes on the
//!   same decoder — does `buffer.reserve(window_size)` eagerly. So the
//!   up-front allocation exists and needs two frames to reach.
//! - While a frame is unfinished the decoder must RETAIN `Window_Size`
//!   decoded bytes to resolve back-references, so up to 8 MB of
//!   plaintext can be held before any of it is handed over, whatever
//!   [`crate::limit::Limited`] was set to. That is the cost of the coding, not
//!   of this wrapper.
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

use crate::response::classify_body_error;
use bytes::Bytes;
use hclient_core::{Capabilities, DecompressionSupport, Error, ErrorKind};
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
    /// has to GUESS at (see the module doc) is the one to be offered
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

/// One coding's incremental decoder.
///
/// Both are push-shaped (`std::io::Write`, with a `Vec<u8>` collecting the
/// plaintext), which is what lets this be driven a frame at a time from
/// `poll_frame` with no IO traits and no executor anywhere near it.
///
/// **Both payloads are boxed**, and not only to satisfy
/// `clippy::large_enum_variant` (which measures 224 bytes for the gzip
/// decoder against 2,656 for brotli's, whose ring buffer and Huffman
/// tables live inline). [`Decompressed`] wraps EVERY response body this
/// client hands back, decoded or not, and is moved around by value with
/// it; unboxed, a plain uncompressed response would carry 2.6 KB of
/// nothing on every `Response`. One allocation per body that actually
/// decodes is the cheaper side of that trade by a wide margin.
pub(crate) enum Decoder {
    #[cfg(feature = "gzip")]
    Gzip(Box<flate2::write::GzDecoder<Vec<u8>>>),
    /// `Option` so that the final `into_inner` — which is what checks the
    /// stream was not truncated — can consume the writer without consuming
    /// the enum.
    #[cfg(feature = "brotli")]
    Brotli(Option<Box<brotli_decompressor::writer::DecompressorWriter<Vec<u8>>>>),
    /// Two `flate2` types behind one token, chosen from the first two
    /// bytes — see the module doc and [`DeflateStream`].
    #[cfg(feature = "deflate")]
    Deflate(Box<DeflateStream>),
    /// The one decoder here that is not push-shaped upstream, so this
    /// crate owns the buffering — see [`ZstdStream`].
    #[cfg(feature = "zstd")]
    Zstd(Box<ZstdStream>),
}

/// Hand-written: `brotli_decompressor`'s writer has no `Debug`, and a
/// decoder's internal window is not something to print anyway.
impl std::fmt::Debug for Decoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Decoder({})", self.token())
    }
}

/// The size of the brotli decoder's internal output buffer, in bytes. Not
/// a limit on anything a caller can see — the writer loops over it until
/// the input is consumed — only how often it hands decoded bytes to the
/// `Vec` behind it.
#[cfg(feature = "brotli")]
const BROTLI_BUFFER: usize = 8 * 1024;

impl Decoder {
    /// Feeds `input` in and takes whatever plaintext came out — possibly
    /// nothing, when the coding needs more input before it can produce a
    /// byte.
    fn push(&mut self, input: &[u8]) -> Result<Bytes, std::io::Error> {
        #[allow(unused_imports)]
        use std::io::Write as _;
        match self {
            #[cfg(feature = "gzip")]
            Decoder::Gzip(d) => {
                d.write_all(input)?;
                Ok(take(d.get_mut()))
            }
            #[cfg(feature = "brotli")]
            Decoder::Brotli(d) => {
                let w = d.as_mut().ok_or_else(brotli_after_end)?;
                w.write_all(input)?;
                Ok(take(w.get_mut()))
            }
            #[cfg(feature = "deflate")]
            Decoder::Deflate(d) => d.push(input),
            #[cfg(feature = "zstd")]
            Decoder::Zstd(d) => d.push(input),
            #[cfg(not(any(
                feature = "gzip",
                feature = "brotli",
                feature = "deflate",
                feature = "zstd"
            )))]
            _ => {
                let _ = input;
                match *self {}
            }
        }
    }

    /// The end of the compressed stream: whatever is still buffered, plus
    /// the integrity check each coding carries.
    ///
    /// This is where a TRUNCATED body becomes an error rather than a short
    /// read — gzip's trailing CRC and length, brotli's own end-of-stream
    /// marker, zlib's Adler-32, zstd's last-block flag and its optional
    /// XXH64. A body cut off mid-transfer that was merely flushed would
    /// reach the caller as a complete, shorter document.
    ///
    /// Raw DEFLATE is the one coding here with no trailer of its own, and
    /// it is not an exception: RFC 1951 §3.2.3's `BFINAL` bit is the end
    /// marker, and `flate2` reports a stream that ended without one.
    fn finish(&mut self) -> Result<Bytes, std::io::Error> {
        match self {
            #[cfg(feature = "gzip")]
            Decoder::Gzip(d) => {
                d.try_finish()?;
                Ok(take(d.get_mut()))
            }
            #[cfg(feature = "brotli")]
            Decoder::Brotli(d) => {
                // `into_inner` closes the stream, and its `Err` is exactly
                // "the input ended before the brotli stream did". It
                // consumes the writer, hence the `Option`: a second
                // `finish` cannot happen (`Decompressed` moves to `Ended`
                // first), and if it ever did it would be an error, not a
                // silent second end.
                let w = d.take().ok_or_else(brotli_after_end)?;
                match w.into_inner() {
                    Ok(mut out) => Ok(take(&mut out)),
                    Err(_) => Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "the brotli stream ended before its end-of-stream marker",
                    )),
                }
            }
            #[cfg(feature = "deflate")]
            Decoder::Deflate(d) => d.finish(),
            #[cfg(feature = "zstd")]
            Decoder::Zstd(d) => d.finish(),
            #[cfg(not(any(
                feature = "gzip",
                feature = "brotli",
                feature = "deflate",
                feature = "zstd"
            )))]
            _ => match *self {},
        }
    }

    fn token(&self) -> &'static str {
        match self {
            #[cfg(feature = "gzip")]
            Decoder::Gzip(_) => Coding::Gzip.token(),
            #[cfg(feature = "brotli")]
            Decoder::Brotli(_) => Coding::Brotli.token(),
            // `"deflate"` whichever wrapper the sniff chose: the token is
            // what appeared on the wire, and the wire has exactly one
            // spelling for both — see the module doc.
            #[cfg(feature = "deflate")]
            Decoder::Deflate(_) => Coding::Deflate.token(),
            #[cfg(feature = "zstd")]
            Decoder::Zstd(_) => Coding::Zstd.token(),
            #[cfg(not(any(
                feature = "gzip",
                feature = "brotli",
                feature = "deflate",
                feature = "zstd"
            )))]
            _ => match *self {},
        }
    }
}

#[cfg(feature = "brotli")]
fn brotli_after_end() -> std::io::Error {
    std::io::Error::other("the brotli decoder was used after its stream ended")
}

/// Takes the accumulated plaintext out of a decoder's output buffer,
/// leaving it empty for the next frame.
#[cfg(any(feature = "gzip", feature = "brotli", feature = "deflate"))]
fn take(out: &mut Vec<u8>) -> Bytes {
    Bytes::from(std::mem::take(out))
}

/// The `deflate` coding, both of it.
///
/// RFC 9110 §8.4.1.2 says zlib and its own Note says a long tail of
/// servers disagrees, so this picks between them from the first two bytes
/// off the wire. The module doc has the argument for why two bytes are
/// enough and why the decision is made here rather than after a failure,
/// as curl's is.
///
/// **`flate2::Decompress` rather than the two `flate2::write` decoders**,
/// and that is not a preference: `write::ZlibDecoder::try_finish` calls
/// `zio::finish`, which runs the decompressor until it stops producing
/// and then returns `Ok(())` **without ever asking whether the stream
/// ended** — read in flate2 1.1's `src/zio.rs:173`. So a truncated body
/// reached the caller as a complete, shorter document, which is the exact
/// defect the trailer checks on the other three codings exist against. It
/// was found by a wire test, one of the two the first draft of this file
/// wrote in the shape `tests/compression.rs` warns against.
/// `Decompress::decompress_vec` answers `Status::StreamEnd`, which is the
/// question, and `Decompress::new(zlib_header)` is the same switch in one
/// type instead of two.
///
/// The states are one-way: `Sniffing` -> `Running`, never back.
#[cfg(feature = "deflate")]
pub(crate) enum DeflateStream {
    /// Fewer than two bytes have arrived, so the question cannot be
    /// answered yet. The buffer holds at most one byte, which is why it
    /// is not a bound anybody has to think about.
    Sniffing(Vec<u8>),
    Running {
        dec: flate2::Decompress,
        /// `StreamEnd` has been seen — RFC 1951 §3.2.3's `BFINAL` block
        /// for the raw form, and that plus RFC 1950's Adler-32 for the
        /// wrapped one. What [`Self::finish`] asks about.
        done: bool,
    },
}

/// How many decoded bytes are asked for per `decompress_vec` call. Not a
/// bound on anything a caller sees — the loop runs until the decoder stops
/// making progress — only how often it grows a `Vec`.
#[cfg(feature = "deflate")]
const DEFLATE_CHUNK: usize = 16 * 1024;

#[cfg(feature = "deflate")]
impl DeflateStream {
    fn new() -> Self {
        DeflateStream::Sniffing(Vec::new())
    }

    /// Does `head` open an RFC 1950 stream?
    ///
    /// Three conditions, and the first is the one that carries the
    /// argument (module doc): `CM == 8` cannot be the first nibble of a
    /// conformant raw stream, because RFC 1951 §3.2.3 packs `BFINAL` into
    /// bit 0 and `BTYPE` into bits 1-2, so a low nibble of `8` is a
    /// stored block with the padding bit §3.2.4 tells encoders to zero.
    ///
    /// `FDICT` is deliberately NOT tested. A zlib stream needing a preset
    /// dictionary is still a zlib stream; classifying it as raw would
    /// swap flate2's honest "a dictionary is required" for a confusing
    /// failure inside the wrong decoder.
    fn looks_like_zlib(head: [u8; 2]) -> bool {
        let cmf = head[0];
        // CM, the low nibble: 8 is "deflate", and RFC 1950 §2.2 defines
        // no other value a client would meet.
        let method = cmf & 0x0f == 8;
        // CINFO, the high nibble: log2(window) - 8, and §2.2 forbids
        // values above 7 outright.
        let window = cmf >> 4 <= 7;
        // FCHECK: the two header bytes, read big-endian, are a multiple
        // of 31 by construction.
        let check = u16::from_be_bytes(head).is_multiple_of(31);
        method && window && check
    }

    fn push(&mut self, input: &[u8]) -> Result<Bytes, std::io::Error> {
        // The bytes to feed: normally just `input`, but on the frame that
        // completes the two-byte header it is that header plus whatever
        // came with it, because nothing buffered has been fed yet.
        let mut carried = None;
        if let DeflateStream::Sniffing(buf) = self {
            buf.extend_from_slice(input);
            if buf.len() < 2 {
                return Ok(Bytes::new());
            }
            let all = std::mem::take(buf);
            *self = DeflateStream::Running {
                dec: flate2::Decompress::new(Self::looks_like_zlib([all[0], all[1]])),
                done: false,
            };
            carried = Some(all);
        }
        let bytes: &[u8] = carried.as_deref().unwrap_or(input);
        let DeflateStream::Running { dec, done } = self else {
            // Unreachable: the block above leaves `Sniffing` only by
            // returning. An empty answer rather than a panic, for the
            // reason `Coding::decoder`'s `Option` is an `Option`.
            return Ok(Bytes::new());
        };
        if *done && !bytes.is_empty() {
            // Mirrors brotli's arm one type over. Discarding them would be
            // this crate deciding that bytes a server sent are not part of
            // the document.
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "bytes arrived after the end of the `deflate` stream",
            ));
        }
        Self::drive(dec, done, bytes, flate2::FlushDecompress::None)
    }

    /// Runs the decoder until it stops making progress, or until the
    /// stream ends.
    ///
    /// The loop deliberately makes one **more** call than there is input
    /// for, and that is the defect this shape was written to fix: zlib
    /// reports `StreamEnd` from the call that reads RFC 1950's four-byte
    /// Adler-32 trailer, and with a `consumed < bytes.len()` guard that
    /// call never happens on the frame that carried it — so a complete
    /// body finished with `done == false` and was reported as truncated.
    /// With an empty input slice and no output, the next iteration makes
    /// no progress and breaks, so the extra call costs one iteration and
    /// cannot spin.
    fn drive(
        dec: &mut flate2::Decompress,
        done: &mut bool,
        bytes: &[u8],
        flush: flate2::FlushDecompress,
    ) -> Result<Bytes, std::io::Error> {
        let mut out = Vec::new();
        let mut consumed = 0usize;
        while !*done {
            out.reserve(DEFLATE_CHUNK);
            let (before_in, before_out) = (dec.total_in(), dec.total_out());
            let status = dec
                .decompress_vec(&bytes[consumed..], &mut out, flush)
                .map_err(std::io::Error::other)?;
            consumed += (dec.total_in() - before_in) as usize;
            if status == flate2::Status::StreamEnd {
                *done = true;
                break;
            }
            if dec.total_in() == before_in && dec.total_out() == before_out {
                // Neither read nor wrote: the decoder wants bytes that
                // have not arrived.
                break;
            }
        }
        if *done && consumed < bytes.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "bytes arrived after the end of the `deflate` stream",
            ));
        }
        Ok(Bytes::from(out))
    }

    fn finish(&mut self) -> Result<Bytes, std::io::Error> {
        match self {
            // The whole body was one byte or none. The shortest possible
            // raw stream is two bytes and the shortest zlib one is eight,
            // so this is a truncation whatever the wrapper would have
            // been — there is no guess to get wrong.
            DeflateStream::Sniffing(_) => Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the `deflate` body ended before even a two-byte header arrived",
            )),
            DeflateStream::Running { dec, done } => {
                // One last call, with nothing left to give it: the
                // decoder may still be holding output, and `StreamEnd`
                // may still be one call away — see `drive`.
                let last = Self::drive(dec, done, &[], flate2::FlushDecompress::Finish)?;
                if !*done {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "the `deflate` body ended in the middle of the compressed stream",
                    ));
                }
                Ok(last)
            }
        }
    }
}

/// The largest `Window_Size` a `zstd` frame may declare before this client
/// refuses it — RFC 8878 §3.1.1.1.2's recommended interoperability floor,
/// and the same number Chrome settled on for `Content-Encoding: zstd`.
///
/// It is the only bound that can be placed on this coding's memory, since
/// the window is allocated before a byte is yielded and
/// [`crate::limit::Limited`] counts bytes yielded. See the module doc.
#[cfg(feature = "zstd")]
const ZSTD_MAX_WINDOW: u64 = 8 * 1024 * 1024;

/// How many decoded bytes are taken out of the frame decoder per call.
///
/// Not a limit on anything a caller can see — [`ZstdStream::drive`] loops
/// until the decoder stops producing — only how often it copies. Same
/// role as [`BROTLI_BUFFER`], and heap-resident rather than a stack array
/// because everything here is reached from a `poll_frame`, where this
/// crate already has two future-size guards.
#[cfg(feature = "zstd")]
const ZSTD_CHUNK: usize = 16 * 1024;

/// The `zstd` coding, and the buffering `ruzstd` does not do.
///
/// The other three decoders are push-shaped upstream: hand them bytes and
/// they hand back whatever came out. `ruzstd` is pull-shaped — its
/// `StreamingDecoder` reads from a source and its `FrameDecoder` wants a
/// whole block at a time — so the mismatch is absorbed here, in the one
/// place that knows a frame off the wire is not a frame of the coding.
///
/// `FrameDecoder::decode_from_to` is the seam used, and its contract is
/// exactly what is needed: *"The source slice may contain only parts of a
/// frame but must contain at least one full block to make progress […]
/// if read == 0 then the source did not contain a full block"*. So
/// [`Self::pending`] holds whatever has not been consumed and never
/// exceeds one zstd block plus a header, because everything else is
/// consumed on the spot.
///
/// **Frames are plural, and that is not a nicety.** RFC 8878 §3.1: *"the
/// decompressed content of multiple concatenated frames is the
/// concatenation of each frame's decompressed content"*, and neither
/// `ruzstd` entry point streams across a frame boundary — `StreamingDecoder`
/// stops at `is_finished()`, `decode_from_to` never re-initialises. So
/// [`Self::start_frame`] does, and the alternative — stopping at the
/// first frame's end — would hand a caller a body silently missing
/// everything after it.
#[cfg(feature = "zstd")]
pub(crate) struct ZstdStream {
    dec: ruzstd::decoding::FrameDecoder,
    /// Compressed bytes that have arrived and are not yet consumed.
    pending: Vec<u8>,
    /// The decoded-byte scratch `decode_from_to` writes into.
    out: Vec<u8>,
}

#[cfg(feature = "zstd")]
impl ZstdStream {
    fn new() -> Self {
        let mut dec = ruzstd::decoding::FrameDecoder::new();
        // The narrowing this crate makes over `ruzstd`'s own 100 MB
        // `DEFAULT_MAX_WINDOW_SIZE`. `ruzstd` checks it before it
        // allocates, so a refused frame costs nothing.
        dec.set_max_window_size(ZSTD_MAX_WINDOW);
        Self {
            dec,
            pending: Vec::new(),
            out: vec![0; ZSTD_CHUNK],
        }
    }

    fn push(&mut self, input: &[u8]) -> Result<Bytes, std::io::Error> {
        self.pending.extend_from_slice(input);
        self.drive(false)
    }

    fn finish(&mut self) -> Result<Bytes, std::io::Error> {
        let out = self.drive(true)?;
        // `drive` stops when it needs input it does not have. At the end
        // of the body there is none coming, so anything left over — a
        // half-written block, a frame whose last block never arrived, a
        // missing four-byte checksum — is a truncation rather than a
        // pause. Without this the bytes that DID arrive would decode
        // perfectly well and reach the caller as a shorter document,
        // which is the same defect gzip's trailer check exists against.
        if !self.dec.is_finished() || !self.pending.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the zstd body ended in the middle of a frame",
            ));
        }
        Ok(out)
    }

    /// Decodes everything the bytes in hand allow.
    ///
    /// `eof` says whether more compressed bytes may still arrive, and it
    /// is read in exactly one place: deciding whether too few bytes for a
    /// frame header mean *wait* or *this frame is truncated*.
    fn drive(&mut self, eof: bool) -> Result<Bytes, std::io::Error> {
        let mut decoded = Vec::new();
        loop {
            // `is_finished()` is true both before the first frame and
            // after each completed one, which is the same question here:
            // is there a frame in progress to feed?
            if self.dec.is_finished() {
                // Everything the completed frame is still holding, before
                // `start_frame` resets the buffer under it.
                loop {
                    let n = std::io::Read::read(&mut self.dec, &mut self.out)?;
                    if n == 0 {
                        break;
                    }
                    decoded.extend_from_slice(&self.out[..n]);
                }
                self.verify_checksum()?;
                if !self.start_frame(eof)? {
                    break;
                }
                continue;
            }
            let (read, written) = self
                .dec
                .decode_from_to(&self.pending, &mut self.out)
                .map_err(std::io::Error::other)?;
            decoded.extend_from_slice(&self.out[..written]);
            self.pending.drain(..read);
            if read == 0 && written == 0 {
                // Neither consumed nor produced: the rest of a block has
                // not arrived. `decode_from_to` says so in as many words.
                break;
            }
        }
        Ok(Bytes::from(decoded))
    }

    /// Starts the next frame, answering whether there was one to start.
    ///
    /// A skippable frame (RFC 8878 §3.1.2) is stepped over rather than
    /// refused: it is user metadata a decoder is required to ignore, and
    /// `ruzstd` reports it as an error from `init` because `init` has no
    /// other channel.
    fn start_frame(&mut self, eof: bool) -> Result<bool, std::io::Error> {
        use ruzstd::decoding::errors::{FrameDecoderError, ReadFrameHeaderError};
        loop {
            if self.pending.is_empty() {
                return Ok(false);
            }
            // A zstd frame header is at most 18 bytes — magic 4, frame
            // header descriptor 1, window descriptor 1, dictionary id 4,
            // frame content size 8. Below that `init` cannot tell "not
            // yet" from "malformed", and it reports both as the same
            // error, so waiting is the only way to keep the two apart.
            // Once the body has ended there is nothing to wait for and a
            // short header IS malformed, which is what `eof` decides.
            if !eof && self.pending.len() < ZSTD_MAX_FRAME_HEADER {
                return Ok(false);
            }
            let mut src: &[u8] = &self.pending;
            match self.dec.init(&mut src) {
                Ok(()) => {
                    let consumed = self.pending.len() - src.len();
                    self.pending.drain(..consumed);
                    return Ok(true);
                }
                Err(FrameDecoderError::ReadFrameHeaderError(ReadFrameHeaderError::SkipFrame {
                    length,
                    ..
                })) => {
                    let consumed = self.pending.len() - src.len();
                    let skip = consumed.saturating_add(length as usize);
                    if self.pending.len() < skip {
                        if eof {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::UnexpectedEof,
                                "the zstd body ended inside a skippable frame",
                            ));
                        }
                        return Ok(false);
                    }
                    self.pending.drain(..skip);
                }
                Err(e) => return Err(std::io::Error::other(e)),
            }
        }
    }

    /// Compares the frame's XXH64 content checksum with the one computed
    /// while decoding — **which `ruzstd` does not do**.
    ///
    /// Read rather than assumed: `FrameDecoder` exposes
    /// `get_checksum_from_data()` and `get_calculated_checksum()` and
    /// compares them nowhere, so a body corrupted in a way that leaves
    /// the block structure intact decodes without complaint. The check is
    /// optional in the format — `Content_Checksum_flag` — and both
    /// accessors answer `None` when it was not sent, which is why this is
    /// a comparison of two `Option`s and not an assertion that one
    /// exists.
    fn verify_checksum(&self) -> Result<(), std::io::Error> {
        let (Some(want), Some(got)) = (
            self.dec.get_checksum_from_data(),
            self.dec.get_calculated_checksum(),
        ) else {
            return Ok(());
        };
        if want == got {
            return Ok(());
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the zstd frame's content checksum does not match the decoded bytes",
        ))
    }
}

/// The largest a zstd frame header can be: magic number 4, frame header
/// descriptor 1, window descriptor 1, dictionary id 4, frame content size
/// 8 (RFC 8878 §3.1.1.1).
#[cfg(feature = "zstd")]
const ZSTD_MAX_FRAME_HEADER: usize = 18;

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
impl<B: std::fmt::Debug> std::fmt::Debug for Decompressed<B> {
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
    B::Error: std::error::Error + Send + Sync + 'static, // send-bound-exception: amendment-C1
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
        let mut c = Capabilities::none();
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

    /// The example used to be `zstd`, and the day this crate grew a zstd
    /// decoder the test failed — which is the right failure and worth a
    /// line, because the assertion is *a coding we cannot reverse is left
    /// alone* and `zstd` had stopped being one. `compress` is RFC 9110
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
