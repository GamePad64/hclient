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
//! [`zstd`], the seam they implement is in [`decoder`], and the seam a
//! *caller* implements — plus why the set of codings is a list on the
//! client rather than a table in this file — is in [`coding`].
//!
//! **The set is open**, which is the one thing about this module a
//! reader should know before anything else here: `Accept-Encoding` is
//! assembled from `Config::decompression`, a `Vec` of
//! [`ContentCoding`] the caller may replace, narrow to nothing, or add
//! their own to. The four this crate ships are values of that trait in
//! [`compression`] and have no standing the seam does not give them.
//!
//! # What is NOT here
//!
//! - **Request-body compression.** Response only; out of scope for W5.
//! - **`compress`/`x-compress`.** RFC 9110 §8.4.1.1's LZW coding. This
//!   crate ships no decoder for it, so the default list never advertises
//!   it and nothing matches it — and since the set is open, a caller who
//!   has one is no longer waiting on this crate to add it.
//! - **A `q`-value on `Accept-Encoding`.** The header is a plain list in
//!   the order the caller's [`ContentCoding`] list is written; RFC 9110
//!   §12.5.3 allows weights and nothing here needs one, because no answer
//!   in the set is worse than no answer at all. A caller who wants a
//!   coding preferred writes it earlier, which is the same expressiveness
//!   at none of the parsing.
//! - **Telling a caller which `deflate` arrived.** See above: the wire
//!   does not distinguish them, so neither does the accessor.
//! - **Tidying the headers of a transport that decoded for us.** Under
//!   [`true`] the response may still carry a
//!   `Content-Encoding` and a `Content-Length` describing the wire rather
//!   than the body handed over — `fetch` does exactly that, and
//!   `hclient-fetch`'s `Body::size_hint` is built around it. This module
//!   strips those two headers only where it decoded the body ITSELF, and
//!   leaves them alone otherwise: a `Client` that rewrote headers over
//!   bytes it never saw would be making a claim on the transport's behalf.
//!   The trigger to revisit is a portable consumer that reads
//!   `Content-Encoding` off a response and gets a different answer per
//!   target for the same server; nothing in this workspace does yet.

// One module per coding, plus the seam they all implement. The gate is on
// the `mod` declaration rather than on every item inside it, which is what
// keeps a coding's file free of `#[cfg]` entirely.
#[cfg(feature = "brotli")]
mod brotli;
mod coding;
mod decoder;
#[cfg(feature = "deflate")]
mod deflate;
#[cfg(feature = "gzip")]
mod gzip;
#[cfg(feature = "zstd")]
mod zstd;

pub use coding::{ContentCoding, SharedContentCoding};
pub(crate) use coding::{builtin, validate};
pub use decoder::{Decode, Decoder};

use crate::error::DecodeFailed;

use crate::response::classify_body_error;
use bytes::Bytes;
use hclient_core::caps::Capabilities;
use hclient_core::error::{Error, ErrorKind};
use std::error::Error as StdError;
use std::fmt::Debug;
use std::pin::Pin;
use std::task::{Context, Poll};

/// The content codings this crate ships, as values of
/// [`ContentCoding`].
///
/// Each is behind the cargo feature of its own name and each is a
/// zero-sized struct, so `compression::Gzip` names a coding the way
/// `redirect::SameOriginOnly` names a policy — a value to hand to
/// [`ClientBuilder::decompression`](crate::ClientBuilder::decompression),
/// with no standing the seam does not give it. A caller who wants the
/// default list writes nothing at all; one who wants a subset writes the
/// subset; one who wants their own writes their own beside these.
///
/// **A module rather than four items at the crate root**, which is the
/// front page's own rule: these are names a caller reaches for once, at
/// `build()`, and the root list is already 16 names and 12 doors.
pub mod compression {
    // Imported under any coding feature and unused under none, which is
    // the empty build this module is an empty module in. The `#[cfg]` is
    // on the `use` rather than on each `impl` for the reason the `mod`
    // declarations one file up carry theirs: one gate, and the items below
    // stay free of them.
    #[cfg(any(
        feature = "gzip",
        feature = "brotli",
        feature = "zstd",
        feature = "deflate"
    ))]
    use super::{coding::ContentCoding, decoder::Decoder};

    /// `gzip` — RFC 1952. Also answers to RFC 9110 §8.4.1.3's deprecated
    /// `x-gzip`, which is accepted and never advertised.
    ///
    /// The lowest amplification of the four, measured at 1:1,028 on one
    /// GiB of zeros, and the one coding Go's `net/http` asks for on its
    /// own (`transport.go:2858` in Go 1.26.4, where a search for `zstd`,
    /// `brotli` or `"br"` finds nothing).
    #[cfg(feature = "gzip")]
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct Gzip;

    #[cfg(feature = "gzip")]
    impl ContentCoding for Gzip {
        fn token(&self) -> &'static str {
            "gzip"
        }
        fn aliases(&self) -> &[&str] {
            // Two aliases exist in RFC 9110 and one is reachable here.
            // §8.4.1.1's `x-compress` names a coding this crate does not
            // reverse, and inventing a third — an `x-deflate`, say —
            // would be this client deciding what a token nobody specified
            // means, on the one coding whose wire format it already has
            // to guess at.
            &["x-gzip"]
        }
        fn decoder(&self) -> Decoder {
            Box::new(super::gzip::Gzip::new())
        }
    }

    /// `br` — RFC 7932.
    ///
    /// **The highest amplification of the four by two orders of
    /// magnitude**: 1,681 bytes on the wire for a decoded GiB, 1:638,751,
    /// against gzip's 1:1,028. Worth knowing before putting it in a list,
    /// and the reason
    /// [`ClientBuilder::response_limit`](crate::ClientBuilder::response_limit)
    /// is what stands between a client and a bomb.
    #[cfg(feature = "brotli")]
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct Brotli;

    #[cfg(feature = "brotli")]
    impl ContentCoding for Brotli {
        fn token(&self) -> &'static str {
            "br"
        }
        fn decoder(&self) -> Decoder {
            Box::new(super::brotli::Brotli::new())
        }
    }

    /// `zstd` — RFC 8878, with the decoder's window capped at 8 MB.
    ///
    /// The cap is RFC 8878 §3.1.1.1.2's recommended interoperability
    /// ceiling and Chrome's answer for this coding, against `ruzstd`'s own
    /// 100 MB default; `zstd`'s module doc has the argument and what it
    /// does *not* bound, which is the total a bomb yields one window at a
    /// time.
    #[cfg(feature = "zstd")]
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct Zstd;

    #[cfg(feature = "zstd")]
    impl ContentCoding for Zstd {
        fn token(&self) -> &'static str {
            "zstd"
        }
        fn decoder(&self) -> Decoder {
            Box::new(super::zstd::ZstdStream::new())
        }
    }

    /// `deflate` — and **the wire has one spelling for two formats**,
    /// which is why the default list offers this one last.
    ///
    /// RFC 9110 §8.4.1.2 specifies zlib (RFC 1950) and a long tail of
    /// servers sends the raw RFC 1951 stream instead, so this coding
    /// sniffs the first two bytes to tell them apart — see `deflate`'s
    /// module doc for why that is a decision rather than a probability.
    /// A caller who knows their server is one that offers `deflate`
    /// first is free to write it first.
    #[cfg(feature = "deflate")]
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct Deflate;

    #[cfg(feature = "deflate")]
    impl ContentCoding for Deflate {
        fn token(&self) -> &'static str {
            "deflate"
        }
        fn decoder(&self) -> Decoder {
            Box::new(super::deflate::DeflateStream::new())
        }
    }
}

/// The codings one request may ask for and reverse, as `negotiate`
/// answers it.
///
/// **A borrow of the client's own list, or an empty slice**, which is what
/// replaced the `Decoders(bool)` newtype. That type existed to say *may
/// this client decode at all* beside a registry that said *what codings
/// exist*, because the second was fixed at compile time and only the first
/// varied per request. Both vary now and both are the same list, so a
/// refusal is the empty slice and an ordinary request is the whole of it —
/// one value where there were two, and no way for them to disagree.
///
/// The lifetime is the client's `Config`, which outlives the request:
/// `negotiate` is called from `execute_with` with `&self.config`, and the
/// answer is handed to [`decoder_for`] a few lines later in the same
/// function.
pub(crate) type Allowed<'a> = &'a [SharedContentCoding];

/// The decoder for what a `Content-Encoding` names, if `allowed` covers
/// it.
///
/// A single token only, never a LIST (`gzip, br` — two codings applied in
/// order): reversing one layer of two and then declaring the body decoded
/// would corrupt it, and no server sends a list to a client that asked for
/// a single coding. `identity` and an empty value are the ordinary "not
/// encoded" answers, and no coding claims either — which is now a fact
/// about the list a caller wrote rather than about a table in this file,
/// and is the one place that distinction could bite: a caller *may*
/// register a coding calling itself `identity`, and would then get it.
/// That is their list, and the alternative is this crate holding a
/// reserved-word list nobody asked for.
fn decoder_named(allowed: Allowed<'_>, value: &http::HeaderValue) -> Option<Decoder> {
    let token = value.to_str().ok()?.trim();
    Some(coding::lookup(allowed, token)?.decoder())
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
/// accident (`response_decompression`'s doc comment says so at the seam).
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
///
/// # Declining to ask is also declining to decode
///
/// Every refusal below returns the empty slice, and that one value is what
/// `Client::execute_with` later hands to [`decoder_for`] — so there is no
/// configuration in which this client asks for nothing and decodes anyway.
/// That is a property of the plumbing rather than a rule anybody has to
/// remember: the two halves read the same answer.
///
/// **A caller's empty list is the same refusal**, which is the whole of
/// how [`ClientBuilder::decompression`](crate::ClientBuilder::decompression)
/// turns decompression off: `available` is empty, the second branch
/// returns, no `Accept-Encoding` goes out and nothing is decoded. That is
/// the CPU lever the measured argument asked for — at 1900 MiB/s gzip
/// decode, 5000 RPS of 1.7 MiB responses is 4.5 cores — and it needed no
/// branch of its own, because *nothing to ask for* was already a case
/// this function had to answer for a build with no coding features.
///
/// It matters most for the `Range` case below, where a server may
/// compress a `206` regardless of what was asked. Decoding it would feed
/// a decoder the middle of a stream and produce an `ErrorKind::Decode`
/// where the caller asked for bytes; standing aside hands over exactly
/// what the server sent, which is the only thing a client can be right
/// about here. See that branch for why.
pub(crate) fn negotiate<'a>(
    method: &http::Method,
    headers: &mut http::HeaderMap,
    caps: &Capabilities,
    available: Allowed<'a>,
) -> Allowed<'a> {
    // The transport already decodes, and chose what to ask for. Decoding
    // again would corrupt every compressed response, and an
    // `Accept-Encoding` of ours could only contradict the one it sent.
    if caps.response_decompression {
        return &[];
    }
    // **A short-circuit and not a guard, which was measured rather than
    // assumed.** An empty list falls through this branch to exactly the
    // same answer: `coding::accept_encoding` returns `None` for an empty
    // slice, so no header is set, and `available` *is* the empty slice
    // this would have returned. Deleting it changes no behaviour — a
    // mutation that replaced the condition with `false` killed nothing,
    // and neither did one making `accept_encoding` fall back to the
    // built-in list; only **both together** failed a test, which is the
    // two-guards-covering-each-other shape this workspace records about
    // `hc --ws`.
    //
    // It is kept because it is the line that says *an empty list is a
    // configuration*, and the three refusals below are written the same
    // way — a reader looking for where "decode nothing" is honoured finds
    // it here rather than deducing it from a `None` two functions away.
    // What it must not be mistaken for is the thing that makes the empty
    // list work; that is `accept_encoding`'s own emptiness check, and the
    // test which fails when both go is
    // `an_empty_list_sends_no_accept_encoding`.
    if available.is_empty() {
        return &[];
    }
    // **A ranged request asks for a slice, and a slice of a coded stream
    // has no beginning.** gzip, brotli and zstd all carry a header and a
    // window at the front, so bytes taken from the middle are not a
    // stream this client — or any client — can start decoding: the
    // decoder would answer `ErrorKind::Decode` where the caller asked for
    // bytes. Asking for a coding on a `Range` request therefore buys
    // nothing and costs the server the compression, which is why
    // `net/http` declines too (`transport.go:2840-2858` in Go 1.26.4,
    // citing golang.org/issue/8923 — *auto-decoding a portion of a
    // gzipped document will just fail anyway*).
    //
    // **And the refusal to decode is the load-bearing half**, because a
    // server may compress a `206` whether or not it was asked. Returning
    // `none()` here is what makes those bytes reach the caller intact
    // rather than through a decoder that cannot read them; see this
    // function's doc comment for why one value settles both halves.
    //
    // Read off the request headers, which is the same gesture
    // `cache::policy::bypasses` already makes one module over for the
    // same header and a neighbouring reason — a range is a request about
    // part of a representation, and a layer that works on whole ones
    // stands aside.
    //
    // Decided once, before the first hop, and that stays right for the
    // whole chain: `Range` is not in `hclient_proto::redirect::
    // SENSITIVE_HEADERS`, so `next_hop`'s clone carries it to every
    // subsequent hop — a request that is ranged at hop 0 is ranged at
    // hop 3.
    if headers.contains_key(http::header::RANGE) {
        return &[];
    }
    // **A HEAD response has no body, so the coding has nothing to apply
    // to** — and nginx has answered a compressed HEAD wrongly for long
    // enough that Go names the ticket: `transport.go:2840-2858` again,
    // citing trac.nginx.org/nginx/ticket/358 and golang.org/issue/5522.
    // So this is two reasons agreeing rather than deference to one
    // implementation's bug: even against a server that gets it right,
    // what is being negotiated is the encoding of bytes that will not be
    // sent.
    //
    // Also decided once and also stable across the chain, by a different
    // mechanism from `Range`'s: RFC 9110 §15.4's method table never
    // rewrites HEAD, and nothing rewrites another method *to* HEAD, so
    // the method a hop carries is HEAD for all of them or none.
    //
    // The empty slice rather than "ask anyway and decode nothing": a
    // header nothing will act on is a header that has to be explained,
    // and `Decompressed::poll_frame`'s empty-body arm already covers the
    // case of a `Content-Encoding` on a bodiless response for the
    // transports that produce one.
    if method == http::Method::HEAD {
        return &[];
    }
    // The caller did their own negotiating. Their header stands untouched
    // and their body is handed over as it arrives: a caller asking for
    // `zstd`, or for `identity`, means it, and silently decoding on top of
    // an answer to a question we did not ask is the same class of surprise
    // as overriding the header itself.
    //
    // **This line read "reqwest makes the same call" and that was wrong**,
    // measured in reqwest 0.13.5 rather than recalled: it delegates to
    // `tower_http::decompression`, whose `Service::call` inserts its
    // `Accept-Encoding` only into a `header::Entry::Vacant` — so a
    // caller's header does stand — and then hands `self.accept` to the
    // `ResponseFuture` **unconditionally**
    // (`decompression/service.rs:114-124`). The response path matches on
    // `Content-Encoding` against that config alone
    // (`decompression/future.rs:43-58`), so reqwest sends the caller's
    // `Accept-Encoding: gzip` and still decodes the gzip that comes back.
    //
    // So the two clients differ here, and this one is the stricter: we
    // treat a caller-set header as taking the whole negotiation, where
    // reqwest treats it as taking only the request half. Ours is the
    // conservative direction — a caller who asked for something we do not
    // decode gets their bytes rather than a surprise — and it is also
    // what makes "ask for a subset" impossible to express, which is the
    // gap a per-coding runtime selector would close.
    //
    // **Go agrees with us, and that is worth naming beside the crate that
    // does not**, because this rule was wrong here for months and one
    // contrasting data point reads as an outlier where two opposed ones
    // read as a decision. `net/http` sets `requestedGzip` only in an
    // expression that requires `Accept-Encoding` to be empty
    // (`transport.go:2840-2858`, Go 1.26.4), carries it onto the response
    // as `addedGzip` (`:2888`), and decodes only under it —
    // `if rc.addedGzip && ...Content-Encoding == "gzip"` at `:2433`. Its
    // own comment at `:2836-2838` is the rule in one sentence: *"We only
    // attempt to uncompress the gzip stream if we were the layer that
    // requested it."* Same rule, same direction, arrived at
    // independently: whoever asked owns the answer.
    //
    // Both claims about third parties are checked rather than recalled,
    // and each is exactly as perishable as its reading: a `tower-http`
    // that starts consulting the request header, or a `net/http` that
    // stops, fails this paragraph rather than silently making it true
    // again.
    if headers.contains_key(http::header::ACCEPT_ENCODING) {
        return &[];
    }
    if !caps
        .forbidden_request_headers
        .contains(&http::header::ACCEPT_ENCODING)
        && let Some(v) = coding::accept_encoding(available)
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
pub(crate) fn decoder_for(
    parts: &mut http::response::Parts,
    allowed: Allowed<'_>,
) -> Option<Decoder> {
    // The decoder is built BEFORE the headers are touched, so that the
    // only way to lose those two headers is to have something that will
    // actually reverse the coding. Failing the other way round would leave
    // a compressed body labelled as plaintext.
    let decoder = decoder_named(allowed, parts.headers.get(http::header::CONTENT_ENCODING)?)?;
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
    /// client was not asked to decode what it sent" without guessing from
    /// a header that is no longer there.
    ///
    /// # Why a [`Cow`] and not a `&'static str`
    ///
    /// It was `Option<&'static str>` while the codings were a compiled-in
    /// table, where every token was a literal in this module. With
    /// [`ContentCoding`] open, **there is no `'static` string to hand
    /// back**: a body holds its [`Decoder`] and not the coding that built
    /// it, so a third-party coding naming itself out of its own state has
    /// nothing here for a `&'static str` to point at — and a
    /// `&str` borrowed from `&self` would tie the answer to the body,
    /// which is what a caller stops holding first.
    ///
    /// So it is the honest type, and it costs the built-ins nothing:
    /// [`Cow::Borrowed`] for all four, an [`Cow::Owned`] allocation only
    /// where a coding really does compute its name, and only when this is
    /// called. `Cow<'static, str>` compares with `&str` and derefs to one,
    /// so `body.coding().as_deref() == Some("gzip")` reads as it did.
    ///
    /// [`Cow`]: std::borrow::Cow
    /// [`Cow::Borrowed`]: std::borrow::Cow::Borrowed
    /// [`Cow::Owned`]: std::borrow::Cow::Owned
    pub fn coding(&self) -> Option<std::borrow::Cow<'static, str>> {
        match &self.state {
            State::Decoding { decoder, .. } => Some(owned(decoder.token())),
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
                    // `&str` from three arms of different lifetimes, which
                    // `debug_struct` takes by reference anyway.
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
    // `hclient_core::error::Error`, whose source is an `Arc<dyn Error + Send +
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
                            Ok(out) if out.is_empty() => {}
                            Ok(out) => return Poll::Ready(Some(Ok(http_body::Frame::data(out)))),
                            Err(e) => {
                                let token = owned(decoder.token());
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
                        // Taken as an owned value BEFORE `finish`, which
                        // needs `&mut`: `Decode::token` lends from the
                        // decoder now, where it used to be `&'static`, so
                        // holding the borrow across the call is `E0502`.
                        // One allocation on the error path of a body that
                        // has already failed.
                        let token = owned(decoder.token());
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

fn decode_error(coding: std::borrow::Cow<'static, str>, source: std::io::Error) -> Error {
    Error::new(ErrorKind::Decode, DecodeFailed { coding, source })
}

/// A decoder's token as something that outlives the decoder.
///
/// **The four built-in codings answer a literal**, so this is a pointer
/// comparison away from free for every response this crate decodes
/// itself: `Cow::Borrowed` where the token is one of the four, `Owned`
/// only for a coding whose name this crate has never seen. The
/// alternative — `Cow::Owned` unconditionally — would allocate on every
/// `ClientBody::coding()` call and on every decode error, for a string
/// that is a `&'static str` in all four shipped cases.
///
/// It is a match rather than a lookup because there is nothing to look in:
/// the body holds the decoder, and the coding list that could have
/// answered lives on the `Client`.
///
/// **This function is a mutation control, deliberately.** Replacing its
/// whole body with `Cow::Owned(token.to_owned())` leaves all 663 tests
/// green, and that is correct rather than a gap: the two answers are the
/// same string and compare equal, so nothing a caller can observe
/// distinguishes them. What the match buys is an allocation, and an
/// allocation is not a behaviour — pinning it would need a counting
/// allocator, which `#![forbid(unsafe_code)]` puts out of reach in this
/// crate's own test targets, exactly as `decoder.rs`'s `Out` measurement
/// records one file over. The claim above is therefore the *reason* and
/// the survivor is the honest consequence of it.
fn owned(token: &str) -> std::borrow::Cow<'static, str> {
    // The tokens this crate's own codings answer, so that the common case
    // borrows. A coding a caller wrote falls through and is copied.
    const BUILTIN: [&str; 4] = ["gzip", "br", "zstd", "deflate"];
    match BUILTIN.iter().find(|b| **b == token) {
        Some(b) => std::borrow::Cow::Borrowed(b),
        None => std::borrow::Cow::Owned(token.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hclient_core::caps::Capabilities;
    use std::sync::Arc;

    fn caps(d: bool) -> Capabilities {
        let mut c = Capabilities::default();
        c.response_decompression = d;
        c
    }

    /// The default list — whatever the cargo features compiled in, which
    /// is what a caller who never calls
    /// [`ClientBuilder::decompression`](crate::ClientBuilder::decompression)
    /// gets. Built per test rather than held in a `const`, because it is a
    /// `Vec` of `Arc`s now where it was a `Copy` bit.
    fn all() -> Vec<SharedContentCoding> {
        builtin()
    }

    /// The ordinary request every test below varies ONE thing away from:
    /// a plain `GET` with no headers of its own, which is the only shape
    /// that gets an `Accept-Encoding`. Spelled out as a helper so that a
    /// test naming `Method::HEAD` or a `Range` is visibly the control's
    /// single mutation rather than an unrelated setup.
    fn plain_get() -> (http::Method, http::HeaderMap) {
        (http::Method::GET, http::HeaderMap::new())
    }

    /// A coding written the way a caller outside this crate would write
    /// one, so that the tests below exercise the seam rather than the four
    /// implementations that happen to live here.
    ///
    /// **Its token is not one of the four**, deliberately: a third-party
    /// coding calling itself `gzip` would pass an assertion about ordering
    /// or advertising for the wrong reason.
    #[derive(Debug)]
    struct Caller(&'static str);

    impl ContentCoding for Caller {
        fn token(&self) -> &'static str {
            self.0
        }
        fn decoder(&self) -> Decoder {
            Box::new(Noop(self.0))
        }
    }

    /// The decoder `Caller` builds: hands its input back unchanged, which
    /// is enough for every question asked here — whether a decoder was
    /// *reached* — and nothing more.
    #[derive(Debug)]
    struct Noop(&'static str);

    impl Decode for Noop {
        fn push(&mut self, input: &[u8]) -> Result<Bytes, std::io::Error> {
            Ok(Bytes::copy_from_slice(input))
        }
        fn finish(&mut self) -> Result<Bytes, std::io::Error> {
            Ok(Bytes::new())
        }
        fn token(&self) -> &'static str {
            self.0
        }
    }

    fn one(token: &'static str) -> Vec<SharedContentCoding> {
        vec![Arc::new(Caller(token))]
    }

    /// **The property the registry used to make structural, pinned on the
    /// list that replaced it.**
    ///
    /// It was `every_registration_is_reachable_by_its_own_token`, and it
    /// existed because a `BTreeMap` built from a `const` table could hold
    /// two codings under one key, or could stop agreeing with `lookup`'s
    /// alias walk. There is no map now, so what it guards is the same
    /// question one shape over: **a coding that is in the list must be
    /// reachable by the token it advertises**, which is what would break
    /// if `lookup` stopped comparing case-insensitively or started at the
    /// aliases.
    ///
    /// It is the check the brief asked to keep: a coding compiled in and
    /// unreachable by its own token fails this line.
    #[test]
    fn every_coding_is_reachable_by_its_own_token() {
        let all = all();
        for c in &all {
            let found = coding::lookup(&all, c.token()).unwrap_or_else(|| {
                panic!("`{}` is in the list and cannot be looked up", c.token())
            });
            assert_eq!(found.token(), c.token());
            for alias in c.aliases() {
                let found = coding::lookup(&all, alias)
                    .unwrap_or_else(|| panic!("alias `{alias}` does not resolve"));
                assert_eq!(
                    found.token(),
                    c.token(),
                    "`{alias}` resolves to the wrong coding"
                );
            }
        }
    }

    /// A token is matched ASCII-case-insensitively — RFC 9110 §8.4.1.
    #[test]
    fn a_registered_token_is_matched_whatever_its_case() {
        let all = all();
        for c in &all {
            let shouted = c.token().to_ascii_uppercase();
            let found = coding::lookup(&all, &shouted)
                .unwrap_or_else(|| panic!("`{shouted}` did not match `{}`", c.token()));
            assert_eq!(found.token(), c.token());
        }
    }

    /// `accept_encoding` names exactly what is in the list, and nothing
    /// else — the property, not the string. A literal
    /// `assert_eq!(.., "zstd, br, gzip, deflate")` would pass just as
    /// happily for a build with three decoders switched off, which is why
    /// that assertion lives in the one test below that is about the
    /// *order*.
    #[test]
    fn accept_encoding_names_exactly_what_can_be_decoded() {
        let all = all();
        let Some(v) = coding::accept_encoding(&all) else {
            assert!(all.is_empty(), "only an empty list may advertise nothing");
            return;
        };
        let mut count = 0;
        for token in v.to_str().unwrap().split(',') {
            count += 1;
            let one = http::HeaderValue::from_str(token.trim()).unwrap();
            assert!(
                decoder_named(&all, &one).is_some(),
                "advertised `{token}`, which this build cannot decode"
            );
        }
        assert_eq!(
            count,
            all.len(),
            "advertised {count} of {} codings",
            all.len()
        );
    }

    /// `deflate` is offered last, and that is a decision rather than the
    /// order anything happens to be declared in: it is the one coding
    /// whose wire format this client has to guess at, so a server picking
    /// the first token it knows should reach for it only when it has
    /// nothing else.
    ///
    /// **This used to also say the registry did not reorder them** — a
    /// `BTreeMap` orders by key, so walking it advertised `br, deflate,
    /// gzip, zstd`, which is why `Registration::preference` was a field.
    /// A slice carries its own order, so what is left to pin is the
    /// *choice*, and it is written as a literal deliberately and only
    /// here.
    #[cfg(all(
        feature = "gzip",
        feature = "brotli",
        feature = "deflate",
        feature = "zstd"
    ))]
    #[test]
    fn the_ambiguous_coding_is_offered_last() {
        let v = coding::accept_encoding(&all()).expect("something is compiled in");
        assert_eq!(
            v.to_str().unwrap(),
            "zstd, br, gzip, deflate",
            "the order is the default list's, best first and the guess last"
        );
    }

    /// **The slice order reaches the wire**, which is the whole of what
    /// replaced `Registration::preference`.
    ///
    /// Written with codings of the test's own so that it says something in
    /// every feature build — and reversed against itself, because one
    /// ordering alone passes for an implementation that sorts.
    #[test]
    fn the_list_order_is_the_header_order() {
        let forward: Vec<SharedContentCoding> =
            vec![Arc::new(Caller("aaa")), Arc::new(Caller("zzz"))];
        let backward: Vec<SharedContentCoding> =
            vec![Arc::new(Caller("zzz")), Arc::new(Caller("aaa"))];
        assert_eq!(
            coding::accept_encoding(&forward).unwrap().to_str().unwrap(),
            "aaa, zzz"
        );
        assert_eq!(
            coding::accept_encoding(&backward)
                .unwrap()
                .to_str()
                .unwrap(),
            "zzz, aaa",
            "a sort would have answered `aaa, zzz` here too"
        );
    }

    /// A coding that is not in the list is not matched — which is what
    /// stops a client from handing a caller a body nothing here can read.
    #[test]
    fn a_coding_not_in_the_list_is_not_matched() {
        let all = all();
        for token in ["compress", "x-compress", "identity", "", "lz4"] {
            assert!(
                coding::lookup(&all, token).is_none(),
                "`{token}` is not a coding this crate's default list carries"
            );
        }
        // The control: without it this passes for a `lookup` that matches
        // nothing at all.
        for c in &all {
            assert!(coding::lookup(&all, c.token()).is_some());
        }
    }

    /// **An alias never shadows another coding's canonical token**, which
    /// is what the two passes in `lookup` buy and what one pass would get
    /// wrong.
    ///
    /// The case is reachable: this crate's own `Gzip` claims `x-gzip` as
    /// an alias, so a caller registering a coding *called* `x-gzip` must
    /// get theirs. Written with two codings of the test's own so it holds
    /// in every feature build.
    #[test]
    fn a_canonical_token_wins_over_another_codings_alias() {
        #[derive(Debug)]
        struct Aliasing;
        impl ContentCoding for Aliasing {
            fn token(&self) -> &'static str {
                "primary"
            }
            fn aliases(&self) -> &[&str] {
                &["shared"]
            }
            fn decoder(&self) -> Decoder {
                // send-bound-exception: amendment-C14
                Box::new(Noop("primary"))
            }
        }
        // The aliasing coding is FIRST, so a single pass that took
        // whichever matched first would answer `primary`.
        let list: Vec<SharedContentCoding> = vec![Arc::new(Aliasing), Arc::new(Caller("shared"))];
        assert_eq!(
            coding::lookup(&list, "shared").map(|c| c.token().to_owned()),
            Some("shared".to_owned()),
            "a coding's own token must beat another coding's alias for the same spelling"
        );
        // The control: the alias still resolves where nothing claims it as
        // a token.
        let alone: Vec<SharedContentCoding> = vec![Arc::new(Aliasing)];
        assert_eq!(
            coding::lookup(&alone, "shared").map(|c| c.token().to_owned()),
            Some("primary".to_owned())
        );
    }

    #[test]
    fn content_encoding_matching_is_case_insensitive_and_rejects_lists() {
        let all = all();
        #[cfg(feature = "gzip")]
        {
            assert!(decoder_named(&all, &http::HeaderValue::from_static("GZIP")).is_some());
            assert!(
                decoder_named(&all, &http::HeaderValue::from_static(" x-gzip ")).is_some(),
                "RFC 9110 §8.4.1.3's deprecated alias, and the surrounding \
                 whitespace a header may carry"
            );
        }
        assert!(decoder_named(&all, &http::HeaderValue::from_static("identity")).is_none());
        assert!(decoder_named(&all, &http::HeaderValue::from_static("")).is_none());
        assert!(
            decoder_named(&all, &http::HeaderValue::from_static("gzip, br")).is_none(),
            "two codings applied in order: reversing one and calling the body \
             decoded would corrupt it"
        );
    }

    #[test]
    fn an_internal_transport_gets_no_header_and_no_decoding() {
        let (m, mut h) = plain_get();
        let all = all();
        let d = negotiate(&m, &mut h, &caps(true), &all);
        assert!(d.is_empty(), "decoding twice would corrupt every response");
        assert!(!h.contains_key(http::header::ACCEPT_ENCODING));
    }

    /// **The control for both refusals below.** Without it each of them
    /// passes for a `negotiate` that never asks for anything at all, which
    /// is the shape this file's own `a_coding_not_in_the_list_is_not_matched`
    /// already guards against one assertion over.
    ///
    /// Written with a coding of the test's own rather than the default
    /// list, so that it says something in a build with no coding features
    /// too — which is where the old version of this test returned early
    /// and asserted nothing.
    #[test]
    fn a_plain_get_asks_for_every_coding_the_client_carries() {
        let (m, mut h) = plain_get();
        let list = one("testing");
        let d = negotiate(&m, &mut h, &caps(false), &list);
        assert_eq!(
            h.get(http::header::ACCEPT_ENCODING).unwrap(),
            "testing",
            "the ordinary request is the one that asks, and it asks for the list"
        );
        assert!(
            !d.is_empty(),
            "and it may reverse what it asked for — the other half of one value"
        );
    }

    /// **An empty list is the off switch**, and it is one call rather than
    /// a mode: no `Accept-Encoding` goes out and nothing may be decoded.
    ///
    /// The control is the test above, which differs in exactly the list.
    #[test]
    fn an_empty_list_asks_for_nothing_and_decodes_nothing() {
        let (m, mut h) = plain_get();
        let d = negotiate(&m, &mut h, &caps(false), &[]);
        assert!(
            !h.contains_key(http::header::ACCEPT_ENCODING),
            "a client configured to decode nothing must not ask for a coding"
        );
        assert!(d.is_empty());
        // And the other half, which is the one a header check alone would
        // miss: a server that compressed anyway is not decoded.
        assert!(decoder_named(&[], &http::HeaderValue::from_static("gzip")).is_none());
    }

    /// **A caller's own coding is asked for and its decoder is used**,
    /// which is the seam working end to end at this level — the list is
    /// advertised, the `Content-Encoding` matches it, and what comes back
    /// is the decoder that coding built rather than one of this crate's.
    #[test]
    fn a_coding_from_outside_this_crate_is_asked_for_and_reached() {
        let (m, mut h) = plain_get();
        let list = one("frob");
        let d = negotiate(&m, &mut h, &caps(false), &list);
        assert_eq!(h[http::header::ACCEPT_ENCODING], "frob");
        let decoder = decoder_named(d, &http::HeaderValue::from_static("frob"))
            .expect("the coding this client asked for must be the one it can reverse");
        assert_eq!(
            decoder.token(),
            "frob",
            "the decoder came from the caller's coding, not from this crate's list"
        );
    }

    /// **A slice of a coded stream has no beginning**, so asking for a
    /// coding on a ranged request buys a body that cannot be decoded. Go's
    /// `net/http` declines for this reason too — `transport.go:2840-2858`,
    /// golang.org/issue/8923 — and `cache::policy::bypasses` reads the
    /// same header one module over.
    ///
    /// The value is empty rather than merely "no header", which is the
    /// half that matters: a server may compress a `206` unasked, and this
    /// is what hands those bytes over intact instead of through a decoder
    /// that would answer `ErrorKind::Decode`.
    #[test]
    fn a_ranged_request_neither_asks_for_a_coding_nor_decodes_one() {
        let (m, mut h) = plain_get();
        h.insert(
            http::header::RANGE,
            http::HeaderValue::from_static("bytes=0-1023"),
        );
        let list = one("testing");
        let d = negotiate(&m, &mut h, &caps(false), &list);
        assert!(
            !h.contains_key(http::header::ACCEPT_ENCODING),
            "a partial representation cannot be decoded, so asking wastes the server's CPU"
        );
        assert!(
            d.is_empty(),
            "and a `206` the server compressed anyway must reach the caller as it arrived"
        );
    }

    /// **A HEAD response has no body**, so there is nothing for a coding
    /// to apply to — and nginx has answered a compressed HEAD wrongly long
    /// enough for Go to name the ticket beside its own refusal
    /// (`transport.go:2840-2858`, trac.nginx.org/nginx/ticket/358).
    ///
    /// The method is the only thing that differs from the control above.
    #[test]
    fn a_head_request_neither_asks_for_a_coding_nor_decodes_one() {
        let (_, mut h) = plain_get();
        let list = one("testing");
        let d = negotiate(&http::Method::HEAD, &mut h, &caps(false), &list);
        assert!(
            !h.contains_key(http::header::ACCEPT_ENCODING),
            "negotiating the encoding of bytes that will not be sent"
        );
        assert!(d.is_empty());
    }

    /// The two refusals above are about THIS request's method and headers,
    /// not about the verb being unusual: `POST` and `PUT` have bodies and
    /// are negotiated exactly as `GET` is.
    ///
    /// Without this, both tests above pass for a `negotiate` that asked
    /// only on `GET` — a narrowing nobody chose and one that would silently
    /// stop compressing every API call this client makes.
    #[test]
    fn a_method_with_a_response_body_is_negotiated_like_any_other() {
        for m in [http::Method::POST, http::Method::PUT, http::Method::DELETE] {
            let (_, mut h) = plain_get();
            let list = one("testing");
            let d = negotiate(&m, &mut h, &caps(false), &list);
            assert!(
                h.contains_key(http::header::ACCEPT_ENCODING),
                "`{m}` returns a body like any other, and nothing about it is partial"
            );
            assert!(!d.is_empty(), "`{m}`'s response is ours to decode");
        }
    }

    /// The half the `FORBIDDEN_HEADERS` shortcut would get wrong: the two
    /// claims come apart here, and the answers must differ.
    #[test]
    fn a_transport_that_forbids_the_header_but_decodes_nothing_still_gets_decoding() {
        let mut c = caps(false);
        c.forbidden_request_headers = &[http::header::ACCEPT_ENCODING];
        let (m, mut h) = plain_get();
        let list = one("testing");
        let d = negotiate(&m, &mut h, &c, &list);
        assert!(
            !h.contains_key(http::header::ACCEPT_ENCODING),
            "the transport forbids this header; we must not add it"
        );
        assert!(
            !d.is_empty(),
            "a `Content-Encoding` the server applied unbidden is still ours to reverse"
        );
    }

    #[test]
    fn a_caller_who_set_accept_encoding_keeps_it_and_gets_the_raw_body() {
        let (m, mut h) = plain_get();
        h.insert(
            http::header::ACCEPT_ENCODING,
            http::HeaderValue::from_static("zstd"),
        );
        let list = one("testing");
        let d = negotiate(&m, &mut h, &caps(false), &list);
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
        let all = all();
        let got = decoder_for(&mut parts, &all);
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
        assert!(decoder_for(&mut parts, &all()).is_none());
        assert_eq!(parts.headers[http::header::CONTENT_ENCODING], "compress");
        assert_eq!(
            parts.headers[http::header::CONTENT_LENGTH],
            "42",
            "the body really is 42 encoded bytes long — we changed nothing"
        );
    }

    /// **Every token this crate ships is a token**, which is the check
    /// `build()` runs and the one that replaced an `expect` whose
    /// justification died with the closed set.
    #[test]
    fn the_builtin_list_passes_its_own_validation() {
        validate(&all()).expect("this crate's own codings must name themselves legally");
    }

    /// A malformed token is refused, and **the space is the character that
    /// matters**: it is what would let one coding's token be read as two
    /// in a header whose separator is `", "`.
    #[test]
    fn a_token_that_is_not_a_token_is_refused_by_name() {
        for bad in ["my coding", "a,b", "", "brötli", "a\"b"] {
            let list: Vec<SharedContentCoding> = vec![Arc::new(Caller(bad))];
            let err = validate(&list)
                .expect_err("`{bad}` is not an RFC 9110 §5.6.2 token and must be refused");
            assert_eq!(err.token, bad, "the refusal must name the offending token");
        }
    }

    /// The control for the test above: the tokens a caller is likely to
    /// write really are accepted, including the punctuation §5.6.2 allows
    /// and which a stricter check would wrongly refuse.
    #[test]
    fn an_ordinary_token_is_accepted() {
        for good in ["gzip", "br", "x-my-coding", "zstd1.5", "a+b", "A_B"] {
            let list: Vec<SharedContentCoding> = vec![Arc::new(Caller(good))];
            validate(&list).unwrap_or_else(|e| panic!("`{good}` is a token: {e}"));
        }
    }

    /// **An alias is checked too**, although it never reaches a header
    /// this client writes — one rule refusing both is simpler to state
    /// than two, and a malformed alias is dead weight a caller should be
    /// told about.
    ///
    /// The error names the alias as the token and the coding separately,
    /// which is the case those two fields exist to tell apart.
    #[test]
    fn a_malformed_alias_is_refused_and_names_its_coding() {
        #[derive(Debug)]
        struct BadAlias;
        impl ContentCoding for BadAlias {
            fn token(&self) -> &'static str {
                "fine"
            }
            fn aliases(&self) -> &[&str] {
                &["not a token"]
            }
            fn decoder(&self) -> Decoder {
                // send-bound-exception: amendment-C14
                Box::new(Noop("fine"))
            }
        }
        let list: Vec<SharedContentCoding> = vec![Arc::new(BadAlias)];
        let err = validate(&list).expect_err("an alias that is not a token is refused");
        assert_eq!(err.token, "not a token");
        assert_eq!(
            err.coding, "fine",
            "the coding is named separately so a caller with several knows which"
        );
    }
}
