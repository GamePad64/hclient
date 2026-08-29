//! `--sse`: an HTTP response read as a stream of Server-Sent Events.
//!
//! # What is printed, and why the annotated form is the wire format
//!
//! An [`SseEvent`] is one of three things, and the two output forms are
//! decided by [`crate::mode::Verbosity`] exactly as a response's are —
//! a pipe gets the payload alone so `hc --sse … | jq` needs no flag, a
//! terminal gets more.
//!
//! The annotated form is **the event re-serialised in SSE's own syntax**
//! rather than a shape invented here:
//!
//! ```text
//! event: tick
//! id: 7
//! data: {"n":1}
//!
//! : a comment
//! retry: 3000
//! ```
//!
//! That decides three things at once and needs no further argument. It
//! cannot omit a field the event carried, because every field has a
//! spelling. It cannot invent one, because the format has no room for it.
//! And its output can be diffed against the bytes the server sent, which
//! is the property a hand-designed `[tick #7] {"n":1}` would not have.
//! Multi-line data is one `data:` line per line, which is what the
//! decoder joined them from.
//!
//! The payload form prints **`data` and nothing else**, one message per
//! line, and drops comments and `retry:` entirely. That is not tidiness:
//! a comment carries no data — its whole purpose on the wire is to keep a
//! connection warm — and a pipeline reading one line per message would
//! get a line that is not a message. `retry:` is likewise an instruction
//! to the client rather than a datum, and under a plain `--sse` it is not
//! even acted on, which the annotated form says by printing it as what it
//! is.
//!
//! # `--follow` is honoured here, and on the client rather than the request
//!
//! [`hclient::sse::SseBuilder`] has no per-request redirect setter, so the
//! policy goes on the `Client` — see `backend::Config::redirect`. It is
//! the only flag in this mode whose effect has somewhere else to travel;
//! every other one that could not is refused by name in `crate::mode`.
//!
//! # No timing recorder is installed, even under `-v`
//!
//! `-v` installs one for a request because the handshake line is printed
//! above the **response head**, and this mode prints none — the stream
//! owns the response it was opened with and exposes neither its status
//! nor its headers. `-w` is refused outright. So there is nothing left to
//! read a clock for, and the transport carries `NoHooks`, whose
//! `WATCHING` is `false`.

use crate::mode::Verbosity;
use crate::run::Fail;
use anstyle::{AnsiColor, Style};
use hclient::sse::SseEvent;
use std::io::Write;

const DIM: Style = Style::new().dimmed();
const KEY: Style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Cyan)));

/// Print one event.
///
/// Separated from the loop below so the whole vocabulary is testable with
/// no server, no socket and no runtime — which is what the unit tests at
/// the bottom of this file are.
pub fn print_event(out: &mut impl Write, event: &SseEvent, how: Verbosity) -> std::io::Result<()> {
    match (event, how) {
        // The payload form: the data of a message, and nothing else at
        // all. A comment or a `retry:` would each be a line that is not a
        // message in a stream a caller is reading one line per message.
        (SseEvent::Message { data, .. }, Verbosity::Payload) => writeln!(out, "{data}"),
        (SseEvent::Comment(_) | SseEvent::Retry(_), Verbosity::Payload) => Ok(()),

        (SseEvent::Message { event, data, id }, _) => {
            if let Some(name) = event {
                writeln!(out, "{KEY}event{KEY:#}: {name}")?;
            }
            if let Some(id) = id {
                writeln!(out, "{KEY}id{KEY:#}: {id}")?;
            }
            // One `data:` line per line, which is what the decoder joined
            // them from — so the printed form re-serialises to what
            // arrived rather than to a longer single line.
            for line in data.split('\n') {
                writeln!(out, "{KEY}data{KEY:#}: {line}")?;
            }
            writeln!(out)
        }
        // SSE's own comment syntax, dimmed: it is not data, and a reader
        // scanning the output should be able to see that without reading
        // the words.
        //
        // **Colon-space, not colon.** The decoder strips one leading
        // space after the colon (`decode.rs`, and it is WHATWG's rule),
        // so `": {text}"` is the form that parses back to this same
        // event and a bare `":{text}"` is not — which is the whole claim
        // the annotated form makes. Found by a test asserting the
        // round trip rather than by reading the decoder.
        (SseEvent::Comment(text), _) => writeln!(out, "{DIM}: {text}{DIM:#}"),
        // Printed as what it is — an instruction to the client — and
        // under a plain `--sse` nothing acts on it, which is exactly why
        // showing it beats swallowing it.
        (SseEvent::Retry(d), _) => {
            writeln!(out, "{KEY}retry{KEY:#}: {}", d.as_millis())
        }
    }
}

/// Open the stream and print it until it ends.
///
/// `reconnect` is `--sse-reconnect`. The two branches are written out
/// rather than unified behind a trait because the library deliberately
/// gives them different **types** — `SseStream` and
/// `ReconnectingSseStream`, gated by `with_timer` — and flattening that
/// back into one value here would be re-introducing the runtime flag
/// `SseOptions`'s own doc records removing.
pub async fn run(
    client: &hclient::Client,
    url: &str,
    headers: &[(String, String)],
    bearer: Option<&str>,
    reconnect: bool,
    how: Verbosity,
    out: &mut impl Write,
) -> Result<(), Fail> {
    let mut b = client.sse(url);
    for (name, value) in headers {
        b = b.header(name, value);
    }
    if let Some(token) = bearer {
        b = b.header("authorization", &format!("Bearer {token}"));
    }

    // `-v` echoes the request this tool is about to make, which is the
    // same thing `--print H` does for a request: the caller never typed
    // the `Accept: text/event-stream` the builder adds, and a diagnostic
    // that omitted it would be describing a request nobody sent.
    if how == Verbosity::Verbose {
        writeln!(out, "{KEY}GET{KEY:#} {url} {DIM}HTTP/1.1{DIM:#}").map_err(Fail::Io)?;
        writeln!(out, "{KEY}accept{KEY:#}: text/event-stream").map_err(Fail::Io)?;
        for (name, value) in headers {
            writeln!(out, "{KEY}{name}{KEY:#}: {value}").map_err(Fail::Io)?;
        }
        if bearer.is_some() {
            writeln!(out, "{KEY}authorization{KEY:#}: Bearer <redacted>").map_err(Fail::Io)?;
        }
        writeln!(out).map_err(Fail::Io)?;
    }

    if reconnect {
        let mut stream = b
            .with_timer(hclient_rt_tokio::Tokio)
            .connect()
            .await
            .map_err(Fail::Request)?;
        while let Some(event) = stream.next().await {
            print_event(out, &event.map_err(Fail::Request)?, how).map_err(Fail::Io)?;
            out.flush().map_err(Fail::Io)?;
        }
    } else {
        let mut stream = b.connect().await.map_err(Fail::Request)?;
        while let Some(event) = stream.next().await {
            print_event(out, &event.map_err(Fail::Request)?, how).map_err(Fail::Io)?;
            // Flushed per event, not per run: a stream is read as it
            // arrives, and a buffered `hc --sse | while read` would sit
            // silent until the buffer filled.
            out.flush().map_err(Fail::Io)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn render(event: &SseEvent, how: Verbosity) -> String {
        // Through the same `AutoStream` the binary writes to, with
        // colour off, so these assertions are about the text rather than
        // about `anstyle`'s escapes — which are emitted unconditionally
        // when a `Style` is written to a bare `Vec`.
        let mut out = anstream::AutoStream::new(Vec::new(), anstream::ColorChoice::Never);
        print_event(&mut out, event, how).expect("a Vec never fails to write");
        let buf = out.into_inner();
        String::from_utf8(buf).expect("everything printed here is UTF-8")
    }

    fn message(event: Option<&str>, data: &str, id: Option<&str>) -> SseEvent {
        SseEvent::Message {
            event: event.map(str::to_owned),
            data: data.to_owned(),
            id: id.map(str::to_owned),
        }
    }

    /// The payload form is one line per message and **nothing else**, or
    /// `hc --sse … | jq` reads a comment as a document.
    #[test]
    fn the_payload_form_prints_data_and_drops_everything_that_is_not_data() {
        assert_eq!(
            render(
                &message(Some("tick"), "{\"n\":1}", Some("7")),
                Verbosity::Payload
            ),
            "{\"n\":1}\n"
        );
        assert_eq!(
            render(&SseEvent::Comment("keep-alive".into()), Verbosity::Payload),
            ""
        );
        assert_eq!(
            render(
                &SseEvent::Retry(Duration::from_millis(3000)),
                Verbosity::Payload
            ),
            ""
        );
    }

    /// The annotated form re-serialises the event in SSE's own syntax, so
    /// it can be diffed against the bytes that arrived. Asserted as the
    /// exact text rather than as "contains", because "cannot invent a
    /// field" is half of what the choice buys.
    #[test]
    fn the_annotated_form_is_the_event_written_back_in_the_wire_format() {
        assert_eq!(
            render(
                &message(Some("tick"), "hello", Some("7")),
                Verbosity::Annotated
            ),
            "event: tick\nid: 7\ndata: hello\n\n"
        );
        // The two optional fields are absent rather than printed empty:
        // `event:` with nothing after it is a different event.
        assert_eq!(
            render(&message(None, "hello", None), Verbosity::Annotated),
            "data: hello\n\n"
        );
        assert_eq!(
            render(&SseEvent::Comment("hi".into()), Verbosity::Annotated),
            ": hi\n"
        );
        assert_eq!(
            render(
                &SseEvent::Retry(Duration::from_millis(3000)),
                Verbosity::Annotated
            ),
            "retry: 3000\n"
        );
    }

    /// Multi-line data goes back out as one `data:` line per line, which
    /// is what the decoder joined it from. A single line carrying an
    /// embedded newline would not re-parse to the same event.
    #[test]
    fn multi_line_data_is_re_split_into_one_data_line_each() {
        assert_eq!(
            render(&message(None, "one\ntwo", None), Verbosity::Annotated),
            "data: one\ndata: two\n\n"
        );
        // And the payload form leaves it as the one value it is, because
        // there the caller wants the datum rather than the framing.
        assert_eq!(
            render(&message(None, "one\ntwo", None), Verbosity::Payload),
            "one\ntwo\n"
        );
    }
}
