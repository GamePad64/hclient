//! `multipart/form-data` request bodies — RFC 7578.
//!
//! The shape of this module is decided by one requirement: **a multipart
//! body must be able to stream.** Concatenating every part into a single
//! `Bytes` is four lines and is wrong for the case multipart exists for —
//! a file large enough that a second copy of it is the thing that fails.
//! So the encoder is an [`http_body::Body`] that walks a queue of
//! segments, and a part whose content is a stream is polled through
//! rather than drained into memory.
//!
//! # What a caller gets, in each case
//!
//! The replay contract is not a setting. It is read off the parts, and it
//! is knowable **before** sending, which is [`RetryKind`]'s whole promise:
//!
//! | every part resolved to bytes | the body is | `retry_kind()` | on the wire |
//! |---|---|---|---|
//! | yes | [`RequestBody::Rewindable`] | [`RetryKind::ViaFactory`] | `Content-Length` |
//! | no | [`RequestBody::Streaming`] | [`RetryKind::Impossible`] | `Content-Length` if every stream's own `size_hint()` is exact, otherwise chunked |
//!
//! "Resolved to bytes" is not the same as "was written as bytes": a part
//! built from a [`RequestBody::Rewindable`] is unwrapped through the same
//! bounded loop `hclient-fetch` and `hclient-wasi` use, so a rewindable
//! part whose factory hands back a `Full` counts as bytes. One whose
//! factory hands back a `Streaming` does not, and the form is single-pass.
//!
//! The consequence a caller feels is at the layer above: a single-pass
//! multipart body cannot be replayed for a `425 Too Early`, and cannot be
//! resent across a 307/308 redirect. **The way to opt into retries is to
//! give the parts bytes** — there is no flag, because a flag would be a
//! promise this module could not keep for a stream it has already handed
//! to a transport.
//!
//! Note the first row is `Rewindable` rather than `Full`. Buffered parts
//! are kept as separate `Bytes` and emitted as separate frames; joining
//! them into one buffer would be exactly the copy this module is shaped
//! to avoid, and `Bytes::clone` makes the factory free.
//!
//! # The boundary
//!
//! [`Boundary::random`] draws **128 bits** from the operating system
//! through `getrandom`, which is already in this crate's graph for SSE
//! reconnect jitter — no dependency is added by this module, and none is
//! needed by it.
//!
//! RFC 2046 §5.1 (cited by RFC 7578 §4.1) requires that the delimiter not
//! appear inside any encapsulated part, and this module **does not check
//! that it doesn't**. The check is not omitted for cost: it is omitted
//! because it cannot be made whole. A streaming part's content is not
//! readable before it is sent, so a scan could only ever cover the
//! buffered parts — a guarantee for some inputs, which reads as a
//! guarantee for all of them and is worse than an honest probability.
//!
//! The probability is the argument. 128 bits drawn per form, never
//! reused, and drawn **after** the caller supplied the content: an
//! adversary choosing a file cannot choose it to contain a value that
//! does not exist yet, and never sees the value of a previous request's
//! boundary in this one. That last clause is a real property and not a
//! remark — it is why [`Boundary::random`] is called once per
//! [`crate::RequestBuilder::multipart`] rather than once per `Client`.
//!
//! **An entropy failure is an error and never a fixed fallback**, which
//! is the opposite resolution from `sse.rs`'s `jitter()`, three files
//! over, where a failed draw becomes `0.0`. The two are not inconsistent:
//! jitter's degenerate value is *un-jittered backoff*, slower and safe,
//! where a fixed boundary is the single value most likely to appear in
//! someone's content — every copy of this library would emit it, so it is
//! the one string an attacker could plant. A degraded value is only
//! acceptable when the degradation has a direction.
//!
//! # Header encoding inside a part
//!
//! Field names and file names are written **as UTF-8, directly**, which
//! RFC 7578 §5.1.2 permits in as many words, and with three bytes
//! escaped: LF as `%0A`, CR as `%0D` and `"` as `%22`. That is the WHATWG
//! HTML rule and is what Chromium, WebKit and Firefox emit; all three
//! moved to it from backslash-escaping.
//!
//! There is no `filename*`. RFC 7578 §4.2 is unusually blunt about it —
//! *"The encoding method described in \[RFC5987\], which would add a
//! `filename*` parameter to the Content-Disposition header field, MUST
//! NOT be used"* — so the RFC 6266 / RFC 5987 tangle has one answer here
//! and it is written into the code rather than chosen per call.
//!
//! **The escape is a framing property before it is an interoperability
//! one.** A file name is caller data that lands inside a header field; a
//! raw CR LF in one would end the field and let the rest of the string be
//! read as further part headers, or as a delimiter. That is why the three
//! bytes are escaped rather than rejected, and why every other C0
//! control byte (and DEL) is **rejected** — those have no escape anyone
//! agrees on, and no legitimate file name carries one.
//!
//! The known wart, stated because a caller will meet it: `%` is not
//! escaped, so a file name that genuinely contains the text `%22` is
//! indistinguishable on the wire from one containing `"`. That ambiguity
//! is the browsers', it is [whatwg/html#7575], and inventing a fourth
//! escape here would produce bytes no server has ever been written
//! against.
//!
//! [whatwg/html#7575]: https://github.com/whatwg/html/issues/7575
//! [`RetryKind`]: hclient_core::RetryKind
//! [`RetryKind::ViaFactory`]: hclient_core::RetryKind::ViaFactory
//! [`RetryKind::Impossible`]: hclient_core::RetryKind::Impossible

use bytes::{BufMut, Bytes, BytesMut};
use hclient_core::{Error, ErrorKind, RequestBody};
use http_body::{Body, Frame, SizeHint};
use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll};

/// What can go wrong while turning a [`Form`] into bytes.
///
/// Every variant is a *build* failure: it is raised before a connection
/// is opened, and reaches the caller out of `send()` like any other
/// builder error.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MultipartError {
    /// The operating system would not supply randomness for a boundary.
    ///
    /// There is deliberately no fallback — see this module's
    /// documentation on why a fixed boundary is the one value that must
    /// never be emitted.
    #[error("no entropy available for a multipart boundary: {0}")]
    NoEntropy(#[from] getrandom::Error),

    /// A caller-supplied boundary that RFC 2046 §5.1.1 does not allow:
    /// empty, longer than 70 characters, ending in a space, or carrying a
    /// character outside `bcharsnospace` plus space.
    #[error("not an RFC 2046 boundary: {0:?}")]
    InvalidBoundary(String),

    /// A field name or file name carrying a C0 control byte other than
    /// CR, LF or HTAB, or DEL.
    ///
    /// CR and LF are escaped (`%0D`, `%0A`); these have no escape any
    /// receiver agrees on, and would corrupt the part's header block.
    #[error(
        "{field} contains control byte {byte:#04x}, which has no representation in a part header"
    )]
    ControlByte {
        /// `"a field name"` or `"a file name"`.
        field: &'static str,
        /// The offending byte.
        byte: u8,
    },

    /// A part's `Content-Type` is not a valid header value.
    #[error("a part's Content-Type is not a valid header value: {0}")]
    InvalidContentType(#[from] http::header::InvalidHeaderValue),

    /// A part's body was a [`RequestBody::Rewindable`] whose factory kept
    /// returning another one.
    ///
    /// The same bound, for the same reason, as `hclient-fetch`'s and
    /// `hclient-wasi`'s: a factory that always rewinds to a factory would
    /// otherwise unwrap for ever.
    #[error(
        "a part's RequestBody::Rewindable factory nested {MAX_REWIND_DEPTH} levels deep or more"
    )]
    RewindTooDeep,
}

/// A part's body yielded a trailers frame.
///
/// `multipart/form-data` has nowhere to put one: a part ends at the next
/// delimiter and carries no trailer section. Dropping the frame would
/// send a well-formed request missing data the caller supplied, so the
/// body fails instead — the same call this workspace makes for undeclared
/// HTTP/1 request trailers one crate over.
#[derive(Debug, thiserror::Error)]
#[error("a multipart part's body emitted trailers, which multipart/form-data cannot carry")]
pub struct TrailersInAPart;

/// The same constant, the same off-by-one and the same reason as
/// `hclient-fetch`'s and `hclient-wasi`'s: this names how many times the
/// loop inspects a body, so the deepest nest that resolves is
/// `MAX_REWIND_DEPTH - 1`.
const MAX_REWIND_DEPTH: u8 = 16;

/// A validated `multipart/form-data` boundary.
///
/// A type rather than a `String` because two questions have to be settled
/// before these bytes reach a header, and settling them at the point of
/// use would mean settling them twice: whether the value is an RFC 2046
/// boundary at all, and whether it needs quoting as a media-type
/// parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Boundary(String);

impl Boundary {
    /// 128 bits from the operating system, hex, behind a fixed prefix.
    ///
    /// The prefix is cosmetic — it is what shows up in a packet capture —
    /// and the 32 hex characters are the whole of the guarantee. Every
    /// character is an HTTP token character, so [`Self::content_type`]
    /// never has to quote one of these.
    pub fn random() -> Result<Self, MultipartError> {
        let mut raw = [0u8; 16];
        getrandom::fill(&mut raw)?;
        let mut s = String::with_capacity(12 + 32);
        s.push_str("----hclient-");
        for b in raw {
            // `write!` would need `fmt::Write` in scope and can fail on a
            // `String` only in ways that cannot happen; two table lookups
            // say the same thing and cannot.
            const HEX: &[u8; 16] = b"0123456789abcdef";
            s.push(HEX[usize::from(b >> 4)] as char);
            s.push(HEX[usize::from(b & 0x0F)] as char);
        }
        Ok(Self(s))
    }

    /// A boundary the caller chose, checked against RFC 2046 §5.1.1.
    ///
    /// Exists for the two cases a random value cannot serve: reproducing
    /// a recorded request byte for byte, and interoperating with a peer
    /// that was written against a fixed value. It is validated rather
    /// than trusted because an unchecked boundary does not fail loudly —
    /// it produces a request that parses as one empty part.
    pub fn new(value: impl Into<String>) -> Result<Self, MultipartError> {
        let value = value.into();
        // `bcharsnospace := DIGIT / ALPHA / "'" / "(" / ")" / "+" / "_"
        //   / "," / "-" / "." / "/" / ":" / "=" / "?"`, plus SPACE
        // anywhere but last, 1..=70 characters.
        let ok = (1..=70).contains(&value.len())
            && !value.ends_with(' ')
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b" '()+_,-./:=? ".contains(&b));
        if ok {
            Ok(Self(value))
        } else {
            Err(MultipartError::InvalidBoundary(value))
        }
    }

    /// The delimiter itself, without the leading `--`.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The `Content-Type` this boundary belongs in.
    ///
    /// Quoted only where it has to be. Every character RFC 2046 allows in
    /// a boundary is safe inside a quoted string — the grammar contains
    /// neither `"` nor `\` — but nine of them (` '()+,/:=?`) are not HTTP
    /// token characters, so a bare parameter carrying one is not the
    /// value the receiver would read back. [`Self::random`] never
    /// produces one; [`Self::new`] can.
    pub fn content_type(&self) -> http::HeaderValue {
        let token = self
            .0
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b));
        let v = if token {
            format!("multipart/form-data; boundary={}", self.0)
        } else {
            format!("multipart/form-data; boundary=\"{}\"", self.0)
        };
        http::HeaderValue::try_from(v).expect("a validated boundary is a valid header value")
    }
}

/// One part of a [`Form`].
///
/// The body is a [`RequestBody`], the same vocabulary
/// [`crate::RequestBuilder::body`] takes, rather than a second way of
/// saying the same thing. That is what lets a part be a stream, a
/// rewindable factory or a buffer without this module owning three
/// constructors for it — and it is what lets the form's own replay
/// contract be *read* off the parts instead of declared.
#[derive(Debug)]
pub struct Part {
    name: String,
    file_name: Option<String>,
    content_type: Option<String>,
    body: RequestBody,
}

impl Part {
    /// A part with an explicit body.
    pub fn new(name: impl Into<String>, body: RequestBody) -> Self {
        Self {
            name: name.into(),
            file_name: None,
            content_type: None,
            body,
        }
    }

    /// A text part.
    ///
    /// No `Content-Type` is written. RFC 7578 §4.4 makes the default for
    /// a part `text/plain`, so writing it would add bytes that say what
    /// their absence already says — and writing `text/plain; charset=utf-8`
    /// would be the *stronger* claim, which §4.5's charset machinery
    /// exists to let a form make deliberately rather than by default.
    pub fn text(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self::new(
            name,
            RequestBody::Full(Bytes::from(value.into().into_bytes())),
        )
    }

    /// A part with a buffered body.
    pub fn bytes(name: impl Into<String>, value: impl Into<Bytes>) -> Self {
        Self::new(name, RequestBody::Full(value.into()))
    }

    /// The `filename` parameter of this part's `Content-Disposition`.
    pub fn file_name(mut self, name: impl Into<String>) -> Self {
        self.file_name = Some(name.into());
        self
    }

    /// This part's `Content-Type`.
    pub fn mime(mut self, value: impl Into<String>) -> Self {
        self.content_type = Some(value.into());
        self
    }

    /// The part's header block, delimiter included, ready to send.
    fn head(&self, boundary: &Boundary) -> Result<Bytes, MultipartError> {
        let mut out = String::new();
        out.push_str("--");
        out.push_str(boundary.as_str());
        out.push_str("\r\nContent-Disposition: form-data; name=\"");
        escape_into(&self.name, "a field name", &mut out)?;
        out.push('"');
        if let Some(f) = &self.file_name {
            out.push_str("; filename=\"");
            escape_into(f, "a file name", &mut out)?;
            out.push('"');
        }
        out.push_str("\r\n");
        if let Some(ct) = &self.content_type {
            // Through `HeaderValue` rather than a hand-rolled scan: this
            // string ends up in a header field, and the type that decides
            // what may go in one is the one that should answer.
            let v = http::HeaderValue::try_from(ct.as_str())?;
            out.push_str("Content-Type: ");
            out.push_str(std::str::from_utf8(v.as_bytes()).map_err(|_| {
                MultipartError::ControlByte {
                    field: "a part's Content-Type",
                    byte: 0x80,
                }
            })?);
            out.push_str("\r\n");
        }
        out.push_str("\r\n");
        Ok(Bytes::from(out.into_bytes()))
    }
}

/// The WHATWG HTML escape: LF, CR and `"`, and nothing else.
///
/// Every other byte goes out as itself, UTF-8 included — RFC 7578 §5.1.2
/// permits exactly that — except the remaining C0 controls and DEL, which
/// are refused. See this module's documentation for why the refusal is
/// there and why `%` is not escaped.
fn escape_into(raw: &str, field: &'static str, out: &mut String) -> Result<(), MultipartError> {
    for c in raw.chars() {
        match c {
            '\n' => out.push_str("%0A"),
            '\r' => out.push_str("%0D"),
            '"' => out.push_str("%22"),
            '\t' => out.push('\t'),
            c if (c.is_control() && (c as u32) < 0x80) || c == '\u{7f}' => {
                return Err(MultipartError::ControlByte {
                    field,
                    byte: c as u8,
                });
            }
            c => out.push(c),
        }
    }
    Ok(())
}

/// A `multipart/form-data` body under construction.
#[derive(Debug, Default)]
pub struct Form {
    parts: Vec<Part>,
}

impl Form {
    /// An empty form.
    ///
    /// A form with no parts is legal and encodes to the closing
    /// delimiter alone — it is not an error, because "the user submitted
    /// a form with nothing in it" is a thing that happens and a server
    /// that cares can say so.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a part. Order is preserved: RFC 7578 §5.3 makes the order
    /// of same-named parts significant, and a receiver reading them into
    /// a list would otherwise get someone else's order.
    pub fn part(mut self, part: Part) -> Self {
        self.parts.push(part);
        self
    }

    /// Encodes the form against `boundary`.
    ///
    /// The variant handed back is decided by the parts and not by an
    /// argument — see this module's table. Public because a caller
    /// driving a [`Transport`](hclient_core::unversioned::Transport)
    /// directly has no [`crate::RequestBuilder`] to do it for them; they
    /// must then also set the `Content-Type` themselves, from
    /// [`Boundary::content_type`].
    pub fn encode(self, boundary: &Boundary) -> Result<RequestBody, MultipartError> {
        let mut segments = Vec::with_capacity(self.parts.len() * 3 + 1);
        for part in self.parts {
            segments.push(Segment::Bytes(part.head(boundary)?));
            match resolve(part.body)? {
                Resolved::Bytes(b) => segments.push(Segment::Bytes(b)),
                Resolved::Stream(s) => segments.push(Segment::Stream(s)),
            }
            segments.push(Segment::Bytes(Bytes::from_static(b"\r\n")));
        }
        segments.push(Segment::Bytes(closing(boundary)));

        // `Option<Vec<_>>` collected from `Option<_>` — one stream
        // anywhere makes the whole thing `None`. Written as a collect
        // rather than a flag plus a second pass so that "every part is
        // bytes" and "here are those bytes" are one answer and cannot
        // disagree.
        let buffered: Option<Vec<Bytes>> = segments
            .iter()
            .map(|s| match s {
                Segment::Bytes(b) => Some(b.clone()),
                Segment::Stream(_) => None,
            })
            .collect();
        let Some(buffered) = buffered else {
            return Ok(RequestBody::Streaming(Box::new(MultipartBody::new(
                segments,
            ))));
        };
        // Every piece is a `Bytes`, so the whole form can be rebuilt from
        // clones — and `Bytes::clone` is a refcount bump, which is what
        // makes replay free rather than a second copy of the payload. The
        // closure owns only `Bytes`, which is also why it satisfies
        // `RequestBody::rewindable`'s `Send + Sync` bound without this
        // file declaring one: a `Vec<RequestBody>` would not, because
        // `RequestBody` is `!Sync`.
        Ok(RequestBody::rewindable(move || {
            RequestBody::Streaming(Box::new(MultipartBody::new(
                buffered.iter().cloned().map(Segment::Bytes).collect(),
            )))
        }))
    }
}

fn closing(boundary: &Boundary) -> Bytes {
    let mut b = BytesMut::with_capacity(boundary.as_str().len() + 6);
    b.put_slice(b"--");
    b.put_slice(boundary.as_str().as_bytes());
    b.put_slice(b"--\r\n");
    b.freeze()
}

enum Resolved {
    Bytes(Bytes),
    Stream(StreamingPart),
}

/// The body inside a [`RequestBody::Streaming`], named so that a part can
/// hold one.
///
/// The `Send` is not this module's choice: it is the bound
/// `RequestBody::Streaming` already carries (spec amendment-C2, and the
/// reasoning is on `RewindFactory` in `hclient-core`), and a part body has
/// to be storable back into that variant.
type StreamingPart = Box<dyn Body<Data = Bytes, Error = Error> + Unpin + Send>; // send-bound-exception: amendment-C2

/// Unwraps a part's body to either bytes or a stream, through the same
/// bounded loop `hclient-fetch` and `hclient-wasi` use for the whole
/// request body.
///
/// A `Rewindable` part is resolved **once**, here, and what it resolved
/// to is what a replay of the whole form sends. That is the factory
/// contract read the way `RequestBody::Rewindable`'s own doc comment
/// states it — every call produces an equivalent body — rather than a
/// second interpretation of it invented by this module.
fn resolve(body: RequestBody) -> Result<Resolved, MultipartError> {
    let mut body = body;
    for _ in 0..MAX_REWIND_DEPTH {
        match body {
            RequestBody::Empty => return Ok(Resolved::Bytes(Bytes::new())),
            RequestBody::Full(bytes) => return Ok(Resolved::Bytes(bytes)),
            RequestBody::Rewindable(f) => body = f(),
            RequestBody::Streaming(s) => return Ok(Resolved::Stream(s)),
        }
    }
    Err(MultipartError::RewindTooDeep)
}

/// One piece of the encoded form: a rendered header block, a buffered
/// part, a separator, or a part that has to be polled.
enum Segment {
    Bytes(Bytes),
    Stream(StreamingPart),
}

/// The encoder: a queue of segments, emitted in order.
struct MultipartBody {
    queue: VecDeque<Segment>,
    /// Bytes still to come, where that is knowable. `None` the moment any
    /// stream in the queue declines to say — which is what decides
    /// `Content-Length` against chunked one layer up, since hyper reads
    /// this and nothing else.
    remaining: Option<u64>,
}

impl MultipartBody {
    fn new(segments: Vec<Segment>) -> Self {
        let mut remaining = Some(0u64);
        for s in &segments {
            let len = match s {
                Segment::Bytes(b) => Some(b.len() as u64),
                // **Read from the component that knows.** A part never
                // declares its own length here; the body does, and a body
                // that will not say makes the whole form chunked.
                Segment::Stream(s) => s.size_hint().exact(),
            };
            remaining = match (remaining, len) {
                (Some(a), Some(b)) => Some(a + b),
                _ => None,
            };
        }
        Self {
            queue: segments.into(),
            remaining,
        }
    }
}

impl Body for MultipartBody {
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Error>>> {
        // Every field is `Unpin` (`Segment::Stream` is already
        // `Box<dyn .. + Unpin>`), so the projection is a plain `get_mut`.
        let this = self.get_mut();
        loop {
            let Some(front) = this.queue.front_mut() else {
                return Poll::Ready(None);
            };
            let data = match front {
                Segment::Bytes(b) => {
                    let b = std::mem::take(b);
                    this.queue.pop_front();
                    // An empty segment is not a frame. A part with an
                    // empty body is legal and must still produce its
                    // header block and its delimiter, so the empty piece
                    // is skipped rather than sent as a zero-length frame.
                    if b.is_empty() {
                        continue;
                    }
                    b
                }
                Segment::Stream(s) => match Pin::new(&mut **s).poll_frame(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(None) => {
                        this.queue.pop_front();
                        continue;
                    }
                    Poll::Ready(Some(Err(e))) => return Poll::Ready(Some(Err(e))),
                    Poll::Ready(Some(Ok(f))) => match f.into_data() {
                        Ok(d) => d,
                        // A trailers frame. See `TrailersInAPart`: there
                        // is nowhere in the format to put it, and
                        // dropping it would complete a request missing
                        // data the caller supplied.
                        Err(_) => {
                            return Poll::Ready(Some(Err(Error::new(
                                ErrorKind::Body,
                                TrailersInAPart,
                            ))));
                        }
                    },
                },
            };
            // Saturating rather than checked: a stream whose `size_hint`
            // over-promised is hyper's error to raise against the
            // `Content-Length` it already wrote, not a panic here.
            this.remaining = this.remaining.map(|r| r.saturating_sub(data.len() as u64));
            return Poll::Ready(Some(Ok(Frame::data(data))));
        }
    }

    fn is_end_stream(&self) -> bool {
        self.queue.is_empty()
    }

    fn size_hint(&self) -> SizeHint {
        match self.remaining {
            Some(n) => SizeHint::with_exact(n),
            None => SizeHint::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hclient_core::RetryKind;
    use http_body_util::BodyExt;

    fn fixed() -> Boundary {
        Boundary::new("XbXb").expect("a token boundary")
    }

    /// Drains a `RequestBody` to bytes, resolving `Rewindable` the way a
    /// transport does.
    fn drain(body: RequestBody) -> Bytes {
        match body {
            RequestBody::Rewindable(f) => drain(f()),
            RequestBody::Streaming(s) => futures_executor::block_on(s.collect())
                .expect("collect")
                .to_bytes(),
            RequestBody::Full(b) => b,
            RequestBody::Empty => Bytes::new(),
        }
    }

    /// The whole encoding, byte for byte, on the smallest form that has
    /// each of the three optional pieces.
    ///
    /// A golden test rather than a set of `contains` assertions: the CRLF
    /// that closes a part belongs to the next delimiter (RFC 2046
    /// §5.1.1), and a `contains` check stays green for an encoder that
    /// has lost one.
    #[test]
    fn the_encoding_is_exactly_rfc_7578() {
        let f = Form::new().part(Part::text("a", "1")).part(
            Part::bytes("f", &b"\x00\x01"[..])
                .file_name("x.bin")
                .mime("application/octet-stream"),
        );
        let got = drain(f.encode(&fixed()).expect("encode"));
        assert_eq!(
            got,
            Bytes::from_static(
                b"--XbXb\r\n\
                  Content-Disposition: form-data; name=\"a\"\r\n\
                  \r\n\
                  1\r\n\
                  --XbXb\r\n\
                  Content-Disposition: form-data; name=\"f\"; filename=\"x.bin\"\r\n\
                  Content-Type: application/octet-stream\r\n\
                  \r\n\
                  \x00\x01\r\n\
                  --XbXb--\r\n"
            )
        );
    }

    /// **A form of buffered parts is replayable, and its length is known.**
    /// Both halves in one test because they are one decision: the parts
    /// resolved to bytes, so the body is a factory AND its size is exact.
    #[test]
    fn buffered_parts_give_a_replayable_body_with_an_exact_length() {
        let body = Form::new()
            .part(Part::text("a", "hello"))
            .encode(&fixed())
            .expect("encode");
        assert_eq!(body.retry_kind(), RetryKind::ViaFactory);
        let first = drain(body.rewind().expect("rewinds"));
        let RequestBody::Rewindable(f) = &body else {
            panic!("expected Rewindable, got {body:?}");
        };
        let RequestBody::Streaming(s) = f() else {
            panic!("the factory must hand back a stream");
        };
        assert_eq!(
            s.size_hint().exact(),
            Some(first.len() as u64),
            "the size hint is what hyper turns into Content-Length"
        );
        assert_eq!(
            drain(body.rewind().expect("rewinds")),
            first,
            "and replays byte for byte"
        );
    }

    /// **A stream anywhere makes the whole form single-pass**, which is
    /// the caller-visible half of the module's table.
    #[test]
    fn one_streaming_part_makes_the_form_single_pass() {
        let body = Form::new()
            .part(Part::text("a", "1"))
            .part(Part::new(
                "f",
                RequestBody::Streaming(Box::new(Chunks::new(vec![b"xy".to_vec()], None))),
            ))
            .encode(&fixed())
            .expect("encode");
        assert_eq!(body.retry_kind(), RetryKind::Impossible);
        assert!(body.rewind().is_none());
    }

    /// **The length is read off the stream, never declared by the part.**
    /// A stream that knows its size gives the whole form an exact hint; an
    /// otherwise identical one that does not makes it unknown, and that is
    /// the difference between `Content-Length` and chunked on the wire.
    #[test]
    fn a_streams_own_size_hint_decides_the_forms() {
        for (declared, want) in [(Some(2u64), true), (None, false)] {
            let body = Form::new()
                .part(Part::new(
                    "f",
                    RequestBody::Streaming(Box::new(Chunks::new(vec![b"xy".to_vec()], declared))),
                ))
                .encode(&fixed())
                .expect("encode");
            let RequestBody::Streaming(s) = &body else {
                panic!("expected Streaming");
            };
            let exact = s.size_hint().exact();
            assert_eq!(exact.is_some(), want, "declared = {declared:?}");
            if want {
                assert_eq!(
                    exact,
                    Some(drain(body).len() as u64),
                    "and it is the right number"
                );
            }
        }
    }

    /// **A `Rewindable` part is unwrapped**, so it counts as bytes and the
    /// form stays replayable — the row the module's table calls "resolved
    /// to bytes" rather than "written as bytes".
    #[test]
    fn a_rewindable_part_resolves_to_its_bytes() {
        let body = Form::new()
            .part(Part::new(
                "a",
                RequestBody::rewindable(|| RequestBody::Full(Bytes::from_static(b"deep"))),
            ))
            .encode(&fixed())
            .expect("encode");
        assert_eq!(body.retry_kind(), RetryKind::ViaFactory);
        assert!(drain(body).ends_with(b"\r\ndeep\r\n--XbXb--\r\n"));
    }

    /// A `Rewindable` whose factory hands back a `Streaming` is a stream,
    /// so the form is single-pass — the other half of the same row, and
    /// the shape that sent nothing at all on the h3 path before v0.3.
    #[test]
    fn a_rewindable_part_that_resolves_to_a_stream_is_a_stream() {
        let body = Form::new()
            .part(Part::new(
                "a",
                RequestBody::rewindable(|| {
                    RequestBody::Streaming(Box::new(Chunks::new(vec![b"s".to_vec()], None)))
                }),
            ))
            .encode(&fixed())
            .expect("encode");
        assert_eq!(body.retry_kind(), RetryKind::Impossible);
    }

    /// The bound `hclient-fetch` and `hclient-wasi` already carry, at the
    /// same off-by-one: 15 layers resolve, 16 do not.
    #[test]
    fn a_rewindable_part_nested_too_deep_is_refused() {
        for (depth, ok) in [(15u8, true), (16u8, false)] {
            let mut b = RequestBody::Full(Bytes::from_static(b"x"));
            for _ in 0..depth {
                let inner = std::sync::Arc::new(std::sync::Mutex::new(Some(b)));
                b = RequestBody::rewindable(move || {
                    inner.lock().expect("lock").take().expect("one call")
                });
            }
            let got = Form::new().part(Part::new("a", b)).encode(&fixed());
            assert_eq!(got.is_ok(), ok, "depth {depth}");
        }
    }

    /// **CR, LF and `"` are escaped and nothing else is**, which is what
    /// keeps a caller-supplied file name from becoming a header of its
    /// own — the injection this escape exists for.
    #[test]
    fn the_three_whatwg_bytes_are_escaped_in_a_name_and_a_file_name() {
        let f = Form::new()
            .part(Part::text("na\"me", "v").file_name("a\r\nX-Injected: 1\r\n\r\nb.txt"));
        let got = drain(f.encode(&fixed()).expect("encode"));
        let text = String::from_utf8(got.to_vec()).expect("utf-8");
        assert!(
            text.contains(
                "Content-Disposition: form-data; name=\"na%22me\"; \
                 filename=\"a%0D%0AX-Injected: 1%0D%0A%0D%0Ab.txt\"\r\n"
            ),
            "{text}"
        );
        assert_eq!(
            text.matches("\r\n").count(),
            5,
            "and no CRLF the encoder did not write: {text:?}"
        );
    }

    /// Non-ASCII goes out as UTF-8, and there is no `filename*` — RFC 7578
    /// §4.2 makes that a MUST NOT, so its absence is a claim rather than
    /// an omission.
    #[test]
    fn a_non_ascii_file_name_is_utf8_and_never_grows_a_filename_star() {
        let f = Form::new().part(Part::text("f", "v").file_name("naïve 日.txt"));
        let text =
            String::from_utf8(drain(f.encode(&fixed()).expect("encode")).to_vec()).expect("utf-8");
        assert!(text.contains("filename=\"naïve 日.txt\""), "{text}");
        assert!(
            !text.contains("filename*"),
            "RFC 7578 §4.2 MUST NOT: {text}"
        );
    }

    /// **`%` is not escaped, so the escape is not reversible** — a name
    /// that genuinely contains `%22` and one that contains `"` produce the
    /// same bytes.
    ///
    /// Asserted as an equality rather than described, because it is the
    /// documented wart and a reader meeting it needs to know it is
    /// deliberate: escaping `%` as well would make the encoding
    /// unambiguous and would put bytes on the wire that no browser sends
    /// and no server has been written against. Written after the mutation
    /// that adds `'%' => "%25"` survived every other test here — none of
    /// them feeds a name containing a literal `%`.
    #[test]
    fn a_literal_percent_is_left_alone_and_the_escape_is_therefore_ambiguous() {
        let one = drain(
            Form::new()
                .part(Part::text("a%22b", "v").file_name("100%.txt"))
                .encode(&fixed())
                .expect("encode"),
        );
        assert!(
            String::from_utf8_lossy(&one).contains("name=\"a%22b\"; filename=\"100%.txt\""),
            "{:?}",
            String::from_utf8_lossy(&one)
        );
        let two = drain(
            Form::new()
                .part(Part::text("a\"b", "v").file_name("100%.txt"))
                .encode(&fixed())
                .expect("encode"),
        );
        assert_eq!(
            one, two,
            "the two names are indistinguishable on the wire — whatwg/html#7575"
        );
    }

    /// **`size_hint` is what is LEFT, not what there was.** `http_body`'s
    /// contract says so, and a consumer asking part-way through has every
    /// right to; nothing in this workspace does, which is why the mutation
    /// that stops the counter survived until this test was written.
    #[test]
    fn the_size_hint_counts_down_as_frames_leave() {
        let body = Form::new()
            .part(Part::text("a", "hello"))
            .encode(&fixed())
            .expect("encode");
        let RequestBody::Rewindable(f) = &body else {
            panic!("expected Rewindable");
        };
        let RequestBody::Streaming(mut s) = f() else {
            panic!("expected Streaming");
        };
        let total = s.size_hint().exact().expect("an exact total");
        let frame =
            futures_executor::block_on(std::future::poll_fn(|cx| Pin::new(&mut *s).poll_frame(cx)))
                .expect("a frame")
                .expect("not an error");
        let n = frame.into_data().expect("data").len() as u64;
        assert!(n > 0 && n < total, "a partial frame: {n} of {total}");
        assert_eq!(
            s.size_hint().exact(),
            Some(total - n),
            "the hint must be the remainder, not the total"
        );
    }

    /// Any other C0 control, and DEL, is a refusal rather than a guess.
    #[test]
    fn other_control_bytes_are_refused_and_a_tab_is_not() {
        for (name, ok) in [
            ("a\tb", true),
            ("a\u{0}b", false),
            ("a\u{7f}b", false),
            ("a\u{1}b", false),
        ] {
            let got = Form::new()
                .part(Part::text("n", "v").file_name(name))
                .encode(&fixed());
            assert_eq!(got.is_ok(), ok, "{name:?}");
        }
    }

    /// **Two forms never share a boundary.** The security argument in the
    /// module documentation rests on this and on nothing else.
    #[test]
    fn every_draw_is_a_different_boundary() {
        let n = 64;
        let seen: std::collections::HashSet<String> = (0..n)
            .map(|_| Boundary::random().expect("entropy").as_str().to_owned())
            .collect();
        assert_eq!(seen.len(), n, "a repeat in {n} draws is not a coincidence");
        let one = Boundary::random().expect("entropy");
        assert_eq!(
            one.as_str().len(),
            12 + 32,
            "128 bits of hex behind the prefix"
        );
        assert!(
            one.as_str()
                .strip_prefix("----hclient-")
                .expect("prefix")
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
            "{}",
            one.as_str()
        );
    }

    /// RFC 2046 §5.1.1: 1..=70 characters from `bcharsnospace` plus space,
    /// and not ending in one.
    #[test]
    fn a_caller_supplied_boundary_is_checked_against_rfc_2046() {
        for (raw, ok) in [
            ("simple", true),
            ("a b", true),
            ("a b ", false),
            ("", false),
            (&"x".repeat(70)[..], true),
            (&"x".repeat(71)[..], false),
            ("has\"quote", false),
            ("has\\backslash", false),
            ("日", false),
        ] {
            assert_eq!(Boundary::new(raw).is_ok(), ok, "{raw:?}");
        }
    }

    /// **Quoted only where it has to be.** A bare parameter carrying a
    /// non-token character is not the value the receiver reads back, and
    /// `Boundary::new` can produce one where `random` cannot.
    #[test]
    fn the_content_type_quotes_a_boundary_that_needs_it() {
        assert_eq!(
            Boundary::new("plain").expect("ok").content_type(),
            "multipart/form-data; boundary=plain"
        );
        assert_eq!(
            Boundary::new("a b").expect("ok").content_type(),
            "multipart/form-data; boundary=\"a b\""
        );
        let r = Boundary::random().expect("entropy");
        assert!(
            !r.content_type().to_str().expect("ascii").contains('"'),
            "a drawn boundary is all token characters"
        );
    }

    /// An empty form is the closing delimiter and nothing else.
    #[test]
    fn an_empty_form_is_legal() {
        assert_eq!(
            drain(Form::new().encode(&fixed()).expect("encode")),
            Bytes::from_static(b"--XbXb--\r\n")
        );
    }

    /// A part with an empty body still gets its header block and its
    /// delimiters — the empty piece is skipped as a *frame*, not as a
    /// part.
    #[test]
    fn an_empty_part_still_has_a_head_and_a_delimiter() {
        let got = drain(
            Form::new()
                .part(Part::new("a", RequestBody::Empty))
                .encode(&fixed())
                .expect("encode"),
        );
        assert_eq!(
            got,
            Bytes::from_static(
                b"--XbXb\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\n\r\n--XbXb--\r\n"
            )
        );
    }

    /// **An empty piece is skipped as a frame**, so an empty part costs a
    /// head and a delimiter and no data frame at all.
    ///
    /// Counted here rather than only checked on the wire, and that is the
    /// whole reason this test exists: hyper's h1 dispatcher discards empty
    /// chunks itself (1.11, `proto/h1/dispatch.rs:405` and `:412`), so the
    /// wire test above stays green with this skip removed — verified by
    /// mutation. Borrowing the property from hyper would leave every other
    /// consumer of this body — the h2 path, `hclient-fetch`, anyone
    /// counting frames rather than bytes — holding a frame nobody meant to
    /// send, and would make a zero-length chunk one `is_end_stream` away
    /// from being RFC 9112 §7.1's terminator.
    #[test]
    fn an_empty_piece_is_never_a_frame() {
        let body = Form::new()
            .part(Part::new("empty", RequestBody::Empty))
            .part(Part::text("full", "x"))
            .encode(&fixed())
            .expect("encode");
        let RequestBody::Rewindable(f) = &body else {
            panic!("expected Rewindable");
        };
        let RequestBody::Streaming(mut s) = f() else {
            panic!("expected Streaming");
        };
        let mut frames = Vec::new();
        while let Some(r) =
            futures_executor::block_on(std::future::poll_fn(|cx| Pin::new(&mut *s).poll_frame(cx)))
        {
            frames.push(r.expect("not an error").into_data().expect("data"));
        }
        assert!(
            frames.iter().all(|f| !f.is_empty()),
            "no zero-length frame: {frames:?}"
        );
        // The empty part is still a part: head, blank line, delimiter.
        let joined: Vec<u8> = frames.concat();
        assert!(
            String::from_utf8_lossy(&joined).contains("name=\"empty\"\r\n\r\n\r\n--XbXb"),
            "{:?}",
            String::from_utf8_lossy(&joined)
        );
    }

    /// A part body that emits trailers fails the body rather than
    /// completing a request without them.
    #[test]
    fn trailers_from_a_part_are_a_body_error() {
        let body = Form::new()
            .part(Part::new(
                "a",
                RequestBody::Streaming(Box::new(Chunks::with_trailers())),
            ))
            .encode(&fixed())
            .expect("encode");
        let RequestBody::Streaming(s) = body else {
            panic!("expected Streaming");
        };
        let err = futures_executor::block_on(s.collect()).expect_err("trailers must fail");
        assert_eq!(*err.kind(), ErrorKind::Body, "{err:?}");
        assert!(
            std::error::Error::source(&err)
                .and_then(|s| s.downcast_ref::<TrailersInAPart>())
                .is_some(),
            "{err:?}"
        );
    }

    /// A test body: a queue of chunks, and a `size_hint` it is *told*
    /// rather than one it works out — so a test can hand the encoder a
    /// stream that does and does not know its own length.
    struct Chunks {
        chunks: VecDeque<Vec<u8>>,
        declared: Option<u64>,
        trailers: bool,
    }

    impl Chunks {
        fn new(chunks: Vec<Vec<u8>>, declared: Option<u64>) -> Self {
            Self {
                chunks: chunks.into(),
                declared,
                trailers: false,
            }
        }
        fn with_trailers() -> Self {
            Self {
                chunks: VecDeque::new(),
                declared: None,
                trailers: true,
            }
        }
    }

    impl Body for Chunks {
        type Data = Bytes;
        type Error = Error;
        fn poll_frame(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Error>>> {
            if let Some(c) = self.chunks.pop_front() {
                return Poll::Ready(Some(Ok(Frame::data(Bytes::from(c)))));
            }
            if self.trailers {
                self.trailers = false;
                return Poll::Ready(Some(Ok(Frame::trailers(http::HeaderMap::new()))));
            }
            Poll::Ready(None)
        }
        fn size_hint(&self) -> SizeHint {
            match self.declared {
                Some(n) => SizeHint::with_exact(n),
                None => SizeHint::default(),
            }
        }
    }
}
