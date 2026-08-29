//! Printing an exchange.
//!
//! Two rules decide everything here. **Colour is for a terminal**, so it
//! is off when stdout is not one and off under `NO_COLOR`, which
//! `anstream` answers for us. And **the body is written as bytes**, never
//! through a `String`: a response can be an image, and a tool that
//! mangles one into replacement characters is not a tool anybody can pipe.

use anstyle::{AnsiColor, Style};
use std::io::Write;

const DIM: Style = Style::new().dimmed();
const KEY: Style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Cyan)));
const OK: Style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Green)));
const WARN: Style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Yellow)));
const BAD: Style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Red)));
const STR: Style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Green)));
const NUM: Style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Magenta)));

fn version_str(v: http::Version) -> &'static str {
    match v {
        http::Version::HTTP_09 => "HTTP/0.9",
        http::Version::HTTP_10 => "HTTP/1.0",
        http::Version::HTTP_11 => "HTTP/1.1",
        http::Version::HTTP_2 => "HTTP/2",
        http::Version::HTTP_3 => "HTTP/3",
        _ => "HTTP",
    }
}

/// The request line and headers, as they were asked for.
///
/// This is what *this program* built, which is not necessarily byte for
/// byte what went on the wire — a transport adds its own framing, and
/// HTTP/2 sends pseudo-headers rather than a request line. Said here
/// rather than implied, because `curl -v` prints the real thing and a
/// reader will assume the same.
pub fn request_head(
    out: &mut impl Write,
    method: &http::Method,
    uri: &http::Uri,
    headers: &http::HeaderMap,
) -> std::io::Result<()> {
    let target = uri
        .path_and_query()
        .map_or_else(|| "/".into(), ToString::to_string);
    writeln!(out, "{KEY}{method}{KEY:#} {target} {DIM}HTTP/1.1{DIM:#}")?;
    header_map(out, headers)?;
    writeln!(out)
}

pub fn response_head(
    out: &mut impl Write,
    version: http::Version,
    status: http::StatusCode,
    headers: &http::HeaderMap,
) -> std::io::Result<()> {
    let style = if status.is_success() {
        OK
    } else if status.is_client_error() || status.is_server_error() {
        BAD
    } else {
        WARN
    };
    let reason = status.canonical_reason().unwrap_or("");
    writeln!(
        out,
        "{DIM}{}{DIM:#} {style}{}{style:#} {style}{reason}{style:#}",
        version_str(version),
        status.as_u16()
    )?;
    header_map(out, headers)?;
    writeln!(out)
}

fn header_map(out: &mut impl Write, headers: &http::HeaderMap) -> std::io::Result<()> {
    for (name, value) in headers {
        // Not `to_str().unwrap_or`: a header value is bytes and may not be
        // UTF-8. Showing the lossy form is right for a human and wrong to
        // hide, so it is escaped rather than replaced.
        match value.to_str() {
            Ok(v) => writeln!(out, "{KEY}{name}{KEY:#}: {v}")?,
            Err(_) => writeln!(
                out,
                "{KEY}{name}{KEY:#}: {DIM}<{} non-UTF-8 bytes>{DIM:#}",
                value.as_bytes().len()
            )?,
        }
    }
    Ok(())
}

/// The body.
///
/// JSON is re-printed indented and coloured **only when it parses**; a
/// body that claims `application/json` and is not gets written through
/// untouched, because a tool that swallows a malformed payload is hiding
/// the one thing the caller needs to see.
///
/// # Two writers, and the second one is a correctness requirement
///
/// This module's own opening rule is that *the body is written as bytes,
/// never through a `String`: a response can be an image, and a tool that
/// mangles one into replacement characters is not a tool anybody can
/// pipe.* It was not being kept, and the cause was the wrapper rather
/// than this function: everything went through one
/// `anstream::AutoStream`, and with colour off that is a `StripStream` —
/// an ANSI parser, which deletes bytes it cannot interpret.
///
/// Measured on `anstream` 0.6 before anything was changed, and then
/// through the built binary: a PNG's magic `89 50 4e 47 0d 0a 1a 0a`
/// comes back out of a piped `hc` as `50 4e 47 0d 0a 0a` — the `0x89`
/// and the `0x1a` are gone, so `hc … > out.png` wrote a file no decoder
/// will open. Colour off is not a corner: it is every pipe, every
/// `--no-color` and every `NO_COLOR`.
///
/// So `payload` is the same file descriptor **without the filter**, and
/// `text` is flushed before anything reaches it, because two sinks onto
/// one descriptor otherwise interleave by buffer rather than by order.
/// The styled branch keeps `text`: what it writes is this program's own
/// output, which is what the filter is for.
pub fn body(
    text: &mut impl Write,
    payload: &mut impl Write,
    content_type: Option<&str>,
    bytes: &[u8],
) -> std::io::Result<()> {
    let is_json = content_type.is_some_and(|ct| {
        let base = ct.split(';').next().unwrap_or("").trim();
        base.eq_ignore_ascii_case("application/json")
            || base.to_ascii_lowercase().ends_with("+json")
    });
    if is_json && let Ok(v) = serde_json::from_slice::<serde_json::Value>(bytes) {
        json_value(text, &v, 0)?;
        return writeln!(text);
    }
    text.flush()?;
    payload.write_all(bytes)?;
    // A trailing newline only where the payload does not already end in
    // one: adding a second would corrupt a diff of two runs.
    if !bytes.is_empty() && !bytes.ends_with(b"\n") {
        writeln!(payload)?;
    }
    payload.flush()
}

fn json_value(out: &mut impl Write, v: &serde_json::Value, depth: usize) -> std::io::Result<()> {
    let pad = "    ".repeat(depth);
    let inner = "    ".repeat(depth + 1);
    match v {
        serde_json::Value::Null => write!(out, "{DIM}null{DIM:#}"),
        serde_json::Value::Bool(b) => write!(out, "{NUM}{b}{NUM:#}"),
        serde_json::Value::Number(n) => write!(out, "{NUM}{n}{NUM:#}"),
        serde_json::Value::String(s) => {
            // Through serde_json so the escaping is the format's rather
            // than ours — a string containing a quote or a newline is one
            // place a hand-rolled printer produces invalid JSON.
            let quoted = serde_json::to_string(s).unwrap_or_else(|_| "\"?\"".into());
            write!(out, "{STR}{quoted}{STR:#}")
        }
        serde_json::Value::Array(xs) if xs.is_empty() => write!(out, "[]"),
        serde_json::Value::Array(xs) => {
            writeln!(out, "[")?;
            for (i, x) in xs.iter().enumerate() {
                write!(out, "{inner}")?;
                json_value(out, x, depth + 1)?;
                if i + 1 < xs.len() {
                    write!(out, ",")?;
                }
                writeln!(out)?;
            }
            write!(out, "{pad}]")
        }
        serde_json::Value::Object(m) if m.is_empty() => write!(out, "{{}}"),
        serde_json::Value::Object(m) => {
            writeln!(out, "{{")?;
            for (i, (k, val)) in m.iter().enumerate() {
                let quoted = serde_json::to_string(k).unwrap_or_else(|_| "\"?\"".into());
                write!(out, "{inner}{KEY}{quoted}{KEY:#}: ")?;
                json_value(out, val, depth + 1)?;
                if i + 1 < m.len() {
                    write!(out, ",")?;
                }
                writeln!(out)?;
            }
            write!(out, "{pad}}}")
        }
    }
}

/// curl's two handshake lines, from the `Connected` event.
///
/// ```text
/// * SSL connection using TLSv1.3 / TLS_AES_256_GCM_SHA384
/// * ALPN: server accepted http/1.1
/// ```
///
/// **Nothing is printed for a plaintext connection**, which is curl's
/// behaviour and is also the honest one: there was no handshake to
/// describe. A backend that handshook and reports nothing prints the
/// third form — the distinction the `Connected` event carries in
/// `timing.tls` and which a line reading `TLS: unknown` would flatten.
pub fn tls_line(out: &mut impl Write, t: &crate::timings::Timings) -> std::io::Result<()> {
    if t.tls.is_none() {
        return Ok(());
    }
    match (&t.tls_version, &t.tls_cipher) {
        (Some(v), Some(c)) => writeln!(out, "{DIM}* SSL connection using {v} / {c}{DIM:#}")?,
        (Some(v), None) => writeln!(out, "{DIM}* SSL connection using {v}{DIM:#}")?,
        // Handshook, and the backend describes none of it — which is
        // `hclient-tls-native-tls`, whose own module doc says the platform
        // stacks expose no getter. Saying so beats printing nothing, since
        // the connection *was* encrypted.
        _ => writeln!(
            out,
            "{DIM}* SSL connection (this TLS backend reports no version or suite){DIM:#}"
        )?,
    }
    if let Some(alpn) = &t.alpn {
        writeln!(
            out,
            "{DIM}* ALPN: server accepted {}{DIM:#}",
            String::from_utf8_lossy(alpn)
        )?;
    }
    // **Only when the server asked**, because the silent case is nearly
    // every connection and a line saying so on each of them is noise
    // rather than information. When it did ask, the interesting half is
    // whether anything answered: a `403` over a connection that offered
    // no certificate is a different diagnosis from a `403` over one that
    // did, and this is the only place a reader can see which they have.
    if let Some(asked) = &t.client_cert_request {
        let named = match asked.authority_names.len() {
            0 => "naming no authority".to_string(),
            1 => "naming 1 authority".to_string(),
            n => format!("naming {n} authorities"),
        };
        let sent = if asked.answered {
            "one was sent"
        } else {
            "none was sent"
        };
        writeln!(
            out,
            "{DIM}* Server requested a client certificate, {named}; {sent}{DIM:#}"
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timings::Timings;
    use std::time::Duration;

    fn render(t: &Timings) -> String {
        let mut buf = Vec::new();
        tls_line(&mut buf, t).unwrap();
        String::from_utf8(buf).unwrap()
    }

    fn handshook() -> Timings {
        Timings {
            tls: Some(Duration::from_millis(1)),
            tls_version: Some("TLSv1.3".into()),
            tls_cipher: Some("TLS_AES_256_GCM_SHA384".into()),
            alpn: Some(b"h2".to_vec()),
            ..Default::default()
        }
    }

    #[test]
    fn a_full_report_reads_like_curls() {
        let out = render(&handshook());
        assert!(
            out.contains("SSL connection using TLSv1.3 / TLS_AES_256_GCM_SHA384"),
            "{out:?}"
        );
        assert!(out.contains("ALPN: server accepted h2"), "{out:?}");
    }

    /// **The line that must not appear.** A plaintext connection has no
    /// handshake to describe, and printing one would be this tool's own
    /// version of a capability that lies.
    #[test]
    fn a_plaintext_connection_prints_nothing_at_all() {
        let t = Timings {
            tls: None,
            ..handshook()
        };
        assert_eq!(render(&t), "", "no handshake, no line");
    }

    /// A backend that handshook and describes none of it says so, rather
    /// than printing nothing — the connection *was* encrypted, and
    /// silence would read as plaintext.
    #[test]
    fn a_silent_backend_is_reported_as_silent_and_not_as_plaintext() {
        let t = Timings {
            tls_version: None,
            tls_cipher: None,
            alpn: None,
            ..handshook()
        };
        let out = render(&t);
        assert!(out.contains("SSL connection"), "{out:?}");
        assert!(out.contains("reports no version"), "{out:?}");
        assert!(!out.contains("ALPN"), "there was none to report: {out:?}");
    }

    /// A server that asked and got nothing is the case this line exists
    /// for: it is what separates *not authorised* from *nothing was
    /// offered*, and nothing else the tool prints carries it.
    #[test]
    fn a_client_certificate_request_that_went_unanswered_is_reported() {
        let t = Timings {
            client_cert_request: Some(
                hclient::hooks::ClientCertRequest::new()
                    .authority_names(vec![b"a".to_vec(), b"b".to_vec()]),
            ),
            ..handshook()
        };
        let out = render(&t);
        assert!(out.contains("requested a client certificate"), "{out:?}");
        assert!(out.contains("naming 2 authorities"), "{out:?}");
        assert!(out.contains("none was sent"), "{out:?}");
    }

    #[test]
    fn a_client_certificate_that_was_sent_says_so() {
        let t = Timings {
            client_cert_request: Some(
                hclient::hooks::ClientCertRequest::new()
                    .authority_names(vec![b"a".to_vec()])
                    .answered(true),
            ),
            ..handshook()
        };
        let out = render(&t);
        assert!(out.contains("naming 1 authority;"), "{out:?}");
        assert!(out.contains("one was sent"), "{out:?}");
    }

    /// The control, and the reason the line is conditional: a server that
    /// did not ask must produce no line at all, or every ordinary
    /// `https://` request grows one.
    #[test]
    fn a_server_that_did_not_ask_produces_no_line() {
        let out = render(&handshook());
        assert!(!out.contains("client certificate"), "{out:?}");
    }

    /// A version with no suite still prints the version. Two backends
    /// could differ here and the reader wants what there is.
    #[test]
    fn a_version_without_a_suite_still_names_the_version() {
        let t = Timings {
            tls_cipher: None,
            ..handshook()
        };
        assert!(
            render(&t).contains("using TLSv1.3\n") || render(&t).contains("using TLSv1.3\u{1b}")
        );
    }
}
