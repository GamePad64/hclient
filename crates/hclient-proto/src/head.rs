//! RFC 9112 §4's response head, parsed from bytes.
//!
//! Sans-io like everything else here: bytes in, a head and a byte count
//! out, no socket and no notion of where the bytes came from.
//!
//! # Why this exists when `hyper` parses heads perfectly well
//!
//! Because one caller cannot use hyper for it. `hclient-proxy`'s
//! `CONNECT` handshake speaks HTTP once, on a socket that stops being an
//! HTTP connection the moment the proxy answers `200` — and driving
//! hyper's h1 client to do that was what tied the whole proxy family to
//! it. A `CONNECT` response is the one HTTP message with **no body under
//! any framing rule** (RFC 9110 §9.3.6), so the hard half of HTTP/1 —
//! chunked decoding, `Content-Length`, and the interaction between the
//! two — has no subject here.
//!
//! # Incomplete is the parser's own answer, not a scan of ours
//!
//! The input is a [`winnow::Partial`], so a head that has not finished
//! arriving comes back as [`ErrMode::Incomplete`] from whichever
//! combinator ran off the end, and `parse_response` turns that into
//! `Ok(None)`. Nothing here looks for `\r\n\r\n` by hand, which is the
//! part a hand-written scan gets subtly wrong: the terminator can arrive
//! split across two reads, and a scan that has to be restarted from the
//! beginning each time is quadratic in the head's length.
//!
//! # What it refuses
//!
//! A **bare LF** line terminator, which RFC 9112 §2.2 permits a recipient
//! to accept. Refused because the sender is forbidden to produce one, and
//! because a parser with two line grammars is one that two implementations
//! can disagree about — the shape this workspace refuses for `HTTP-date`
//! one module over. An **obs-fold** continuation line, which §5.2 makes a
//! `MUST` reject or replace for a client, and rejecting is the direction
//! that cannot invent a value. And **whitespace before the colon**, which
//! §5 makes a `MUST` reject because it is the request-smuggling shape.

pub use crate::error::HeadError;

use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue, StatusCode, Version};
use winnow::combinator::{alt, opt, preceded, repeat, terminated};
use winnow::error::{ErrMode, Needed};
use winnow::stream::Partial;
use winnow::token::{literal, take_while};
use winnow::{ModalResult, Parser};

/// A response head that a caller can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseHead {
    pub version: Version,
    pub status: StatusCode,
    pub headers: HeaderMap,
}

/// Parse a response head out of `buf`.
///
/// `Ok(None)` means *not yet* — the head has not finished arriving — and
/// is the answer a caller loops on. It is deliberately not an error:
/// incomplete and malformed are different facts, and a caller that could
/// not tell them apart would either give up on a slow proxy or wait
/// forever for a broken one.
///
/// The `usize` is how many bytes the head occupied, **including** its
/// terminating CRLF. Everything after that in `buf` belongs to whoever
/// comes next, which for a `CONNECT` tunnel is the origin.
pub fn parse_response(buf: &[u8]) -> Result<Option<(ResponseHead, usize)>, HeadError> {
    let mut input = Partial::new(buf);
    match head.parse_next(&mut input) {
        Ok(h) => Ok(Some((h, buf.len() - input.len()))),
        Err(ErrMode::Incomplete(_)) => Ok(None),
        Err(e) => Err(e.into_inner().unwrap_or(HeadError::MalformedStatusLine)),
    }
}

/// [`parse_response`] over a [`Bytes`], handing back the remainder.
///
/// The split is the whole reason a caller wants this rather than the
/// slice form: what follows a `CONNECT` response head is the origin's
/// first bytes, and they must survive as a `Bytes` rather than be copied
/// out of a borrow that is about to end.
pub fn parse_response_bytes(buf: &mut Bytes) -> Result<Option<ResponseHead>, HeadError> {
    match parse_response(buf)? {
        None => Ok(None),
        Some((head, end)) => {
            let _ = buf.split_to(end);
            Ok(Some(head))
        }
    }
}

type In<'a> = Partial<&'a [u8]>;

/// `status-line *( field-line CRLF ) CRLF`, RFC 9112 §2.1.
fn head(t: &mut In<'_>) -> ModalResult<ResponseHead, HeadError> {
    let (version, status) = terminated(status_line, crlf).parse_next(t)?;
    let fields: Vec<(HeaderName, HeaderValue)> =
        repeat(0.., terminated(field_line, crlf)).parse_next(t)?;
    crlf.parse_next(t)?;

    let mut headers = HeaderMap::with_capacity(fields.len());
    for (name, value) in fields {
        headers.append(name, value);
    }
    Ok(ResponseHead {
        version,
        status,
        headers,
    })
}

/// `HTTP-version SP status-code [ SP reason-phrase ]`.
///
/// The reason phrase is optional and is never read — RFC 9112 §4 makes it
/// meaningless — but it has to be *consumed*, or the CRLF after it would
/// not be where the next parser looks.
fn status_line(t: &mut In<'_>) -> ModalResult<(Version, StatusCode), HeadError> {
    let version = alt((
        literal("HTTP/1.1").value(Version::HTTP_11),
        literal("HTTP/1.0").value(Version::HTTP_10),
    ))
    .parse_next(t)?;
    let status = preceded(' ', status_code).parse_next(t)?;
    // `take_while` over everything that is not a line terminator: a
    // reason phrase may contain spaces, so it cannot be one token.
    let _reason =
        opt(preceded(' ', take_while(0.., |b| b != b'\r' && b != b'\n'))).parse_next(t)?;
    Ok((version, status))
}

/// Exactly three digits, and `http` decides whether they name a status.
fn status_code(t: &mut In<'_>) -> ModalResult<StatusCode, HeadError> {
    let digits = take_while(3, |b: u8| b.is_ascii_digit()).parse_next(t)?;
    StatusCode::from_bytes(digits)
        .map_err(|_| ErrMode::Cut(HeadError::BadStatus(String::from_utf8_lossy(digits).into())))
}

/// `field-name ":" OWS field-value OWS`.
fn field_line(t: &mut In<'_>) -> ModalResult<(HeaderName, HeaderValue), HeadError> {
    // A leading space or tab is obs-fold — a continuation of the previous
    // line. `Cut` rather than `Backtrack`, so `repeat` does not read it as
    // "no more fields" and hand a half-parsed head back.
    if let Some(b' ' | b'\t') = t.first() {
        return Err(ErrMode::Cut(HeadError::ObsFold));
    }
    let name = take_while(1.., is_tchar).parse_next(t)?;
    // No whitespace is allowed between the name and the colon, and
    // `is_tchar` already excludes it — so a `A : b` fails here, at the
    // colon, which is the refusal RFC 9112 §5 requires.
    literal(':')
        .parse_next(t)
        .map_err(|_: ErrMode<HeadError>| ErrMode::Cut(HeadError::MalformedHeader))?;
    let value = take_while(0.., |b| b != b'\r' && b != b'\n').parse_next(t)?;

    let name = HeaderName::from_bytes(name).map_err(|_| {
        ErrMode::Cut(HeadError::BadHeaderName(
            String::from_utf8_lossy(name).into(),
        ))
    })?;
    let value = HeaderValue::from_bytes(trim_ows(value))
        .map_err(|_| ErrMode::Cut(HeadError::BadHeaderValue(name.as_str().into())))?;
    Ok((name, value))
}

/// The line terminator, and the one refusal that has to be written as a
/// rule rather than as a missing alternative: a bare `LF` is *accepted*
/// by RFC 9112 §2.2's leniency and refused here, so it must produce its
/// own error rather than fall through as "not a CRLF".
fn crlf(t: &mut In<'_>) -> ModalResult<(), HeadError> {
    match t.first() {
        None => Err(ErrMode::Incomplete(Needed::new(1))),
        Some(b'\n') => Err(ErrMode::Cut(HeadError::BareLf)),
        Some(b'\r') => {
            literal("\r\n").parse_next(t)?;
            Ok(())
        }
        Some(_) => Err(ErrMode::Backtrack(HeadError::MalformedStatusLine)),
    }
}

/// RFC 9110 §5.6.2's `tchar`.
fn is_tchar(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

fn trim_ows(v: &[u8]) -> &[u8] {
    let Some(start) = v.iter().position(|b| *b != b' ' && *b != b'\t') else {
        return &[];
    };
    let end = v.iter().rposition(|b| *b != b' ' && *b != b'\t').unwrap();
    &v[start..=end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(bytes: &[u8]) -> Result<Option<(ResponseHead, usize)>, HeadError> {
        parse_response(bytes)
    }

    #[test]
    fn an_ordinary_head_parses_and_reports_its_length() {
        let raw = b"HTTP/1.1 200 Connection established\r\nProxy-Agent: p/1\r\n\r\nleftover";
        let (h, n) = head(raw).unwrap().expect("complete");
        assert_eq!(h.status, StatusCode::OK);
        assert_eq!(h.version, Version::HTTP_11);
        assert_eq!(h.headers["proxy-agent"], "p/1");
        // The count is what separates the head from the tunnel's first
        // bytes, so it is the value a caller actually acts on.
        assert_eq!(&raw[n..], b"leftover");
    }

    #[test]
    fn an_incomplete_head_is_not_an_error() {
        // The distinction a caller loops on: nothing is wrong yet. The
        // last two are the case a hand-written scan gets wrong — the
        // terminator arriving split across reads.
        assert_eq!(head(b"").unwrap(), None);
        assert_eq!(head(b"HTTP/1.1 200 OK\r\n").unwrap(), None);
        assert_eq!(head(b"HTTP/1.1 200 OK\r\nA: b\r\n").unwrap(), None);
        assert_eq!(head(b"HTTP/1.1 200 OK\r\nA: b\r\n\r").unwrap(), None);
        assert_eq!(head(b"HTTP/1.1 2").unwrap(), None);
    }

    #[test]
    fn a_head_with_no_headers_at_all_is_complete() {
        let (h, n) = head(b"HTTP/1.1 407 \r\n\r\n").unwrap().expect("complete");
        assert_eq!(h.status, StatusCode::PROXY_AUTHENTICATION_REQUIRED);
        assert!(h.headers.is_empty());
        assert_eq!(n, 17);
    }

    #[test]
    fn an_absent_reason_phrase_is_allowed() {
        // RFC 9112 §4 makes it optional, and this parser never reads it.
        let (h, _) = head(b"HTTP/1.1 200\r\n\r\n").unwrap().expect("complete");
        assert_eq!(h.status, StatusCode::OK);
    }

    #[test]
    fn a_reason_phrase_with_spaces_is_consumed_whole() {
        let (h, _) = head(b"HTTP/1.1 502 Bad Gateway (upstream)\r\n\r\n")
            .unwrap()
            .expect("complete");
        assert_eq!(h.status, StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn a_repeated_header_keeps_both_values() {
        let (h, _) = head(b"HTTP/1.1 200 OK\r\nVia: a\r\nVia: b\r\n\r\n")
            .unwrap()
            .expect("complete");
        let via: Vec<_> = h.headers.get_all("via").iter().collect();
        assert_eq!(via.len(), 2);
    }

    #[test]
    fn the_value_is_trimmed_of_optional_whitespace_and_nothing_else() {
        let (h, _) = head(b"HTTP/1.1 200 OK\r\nA:   b c \t \r\n\r\n")
            .unwrap()
            .expect("complete");
        assert_eq!(h.headers["a"], "b c");
    }

    #[test]
    fn an_empty_value_is_a_value() {
        let (h, _) = head(b"HTTP/1.1 200 OK\r\nA:\r\n\r\n")
            .unwrap()
            .expect("complete");
        assert_eq!(h.headers["a"], "");
    }

    #[test]
    fn a_bare_lf_is_refused() {
        assert_eq!(head(b"HTTP/1.1 200 OK\n\n"), Err(HeadError::BareLf));
        assert_eq!(
            head(b"HTTP/1.1 200 OK\r\nA: b\n\r\n"),
            Err(HeadError::BareLf)
        );
    }

    #[test]
    fn an_obs_fold_continuation_is_refused() {
        assert_eq!(
            head(b"HTTP/1.1 200 OK\r\nA: b\r\n  continued\r\n\r\n"),
            Err(HeadError::ObsFold)
        );
    }

    #[test]
    fn whitespace_before_the_colon_is_refused() {
        // The request-smuggling shape, and RFC 9112 §5 makes rejecting it
        // a MUST.
        assert_eq!(
            head(b"HTTP/1.1 200 OK\r\nA : b\r\n\r\n"),
            Err(HeadError::MalformedHeader)
        );
    }

    #[test]
    fn a_status_that_is_not_three_digits_is_refused() {
        // Two digits then a space: `take_while(3, ..)` cannot match, and
        // the head is complete, so this is a refusal rather than a wait.
        assert!(head(b"HTTP/1.1 20 OK\r\n\r\n").is_err());
        assert!(head(b"HTTP/1.1 2xx OK\r\n\r\n").is_err());
        // Four digits: the first three parse, and the fourth is then not
        // the space or CRLF the grammar requires.
        assert!(head(b"HTTP/1.1 2000 OK\r\n\r\n").is_err());
    }

    #[test]
    fn a_three_digit_number_that_is_not_a_status_is_named() {
        // `http` refuses below 100; the parser's own shape accepts any
        // three digits, so this is the one place the two disagree and it
        // must be an error rather than a panic.
        assert_eq!(
            head(b"HTTP/1.1 099 What\r\n\r\n"),
            Err(HeadError::BadStatus("099".into()))
        );
    }

    #[test]
    fn another_protocol_on_the_status_line_is_refused() {
        assert!(head(b"HTTP/2.0 200 OK\r\n\r\n").is_err());
        assert!(head(b"ICY 200 OK\r\n\r\n").is_err());
        assert!(head(b"\0\0\0\0\r\n\r\n").is_err());
    }

    #[test]
    fn the_bytes_form_leaves_the_remainder_behind() {
        let mut buf = Bytes::from_static(b"HTTP/1.1 200 OK\r\n\r\nthe origin's first bytes");
        let h = parse_response_bytes(&mut buf).unwrap().expect("complete");
        assert_eq!(h.status, StatusCode::OK);
        assert_eq!(&buf[..], b"the origin's first bytes");
    }

    #[test]
    fn the_bytes_form_consumes_nothing_when_the_head_is_incomplete() {
        // A caller reads more and calls again, so the buffer must be
        // exactly as it was.
        let mut buf = Bytes::from_static(b"HTTP/1.1 200 OK\r\n");
        assert_eq!(parse_response_bytes(&mut buf).unwrap(), None);
        assert_eq!(&buf[..], b"HTTP/1.1 200 OK\r\n");
    }

    #[test]
    fn a_head_arriving_one_byte_at_a_time_is_never_wrong_before_it_is_complete() {
        // The property the `Partial` input exists for, asserted rather
        // than assumed: every prefix answers *not yet*, and only the whole
        // thing answers.
        let raw = b"HTTP/1.1 200 OK\r\nA: b\r\n\r\n";
        for n in 0..raw.len() {
            assert_eq!(head(&raw[..n]).unwrap(), None, "prefix of {n} bytes");
        }
        assert!(head(raw).unwrap().is_some());
    }
}
