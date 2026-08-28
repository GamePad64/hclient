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
pub fn body(out: &mut impl Write, content_type: Option<&str>, bytes: &[u8]) -> std::io::Result<()> {
    let is_json = content_type.is_some_and(|ct| {
        let base = ct.split(';').next().unwrap_or("").trim();
        base.eq_ignore_ascii_case("application/json")
            || base.to_ascii_lowercase().ends_with("+json")
    });
    if is_json && let Ok(v) = serde_json::from_slice::<serde_json::Value>(bytes) {
        json_value(out, &v, 0)?;
        return writeln!(out);
    }
    out.write_all(bytes)?;
    // A trailing newline only where the payload does not already end in
    // one: adding a second would corrupt a diff of two runs.
    if !bytes.is_empty() && !bytes.ends_with(b"\n") {
        writeln!(out)?;
    }
    Ok(())
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
