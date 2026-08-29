//! `--ws`: a WebSocket session, stdin in and messages out.
//!
//! # The structural obstacle, and what it forced
//!
//! `hclient_tungstenite::Tungstenite` implements `WebSocketConnect` and
//! **borrows** a `hclient_native::Native` — it does not own one, because
//! `hclient::Client::builder` takes its transport by value and `Native`
//! is not `Clone`. `backend::build` hands back an erased
//! [`hclient::Client`], which has thrown the concrete transport away.
//!
//! So this mode cannot go through that door, and the repair is in
//! `backend.rs`: construction is split into *make the transport* and
//! *wrap it in a client*. The arms below build the `Native` themselves,
//! keep it on their own stack frame for the length of the session, and
//! borrow it. `Client::transport_as::<Native<..>>()` was the alternative
//! and is worse in a way the connector's own doc already names: it hands
//! back an `Option`, because nothing checked at `build()` that the
//! backend is the one the caller is about to name — and here we know,
//! because we just built it. Splitting removes the question instead of
//! answering it at run time.
//!
//! What the split does **not** disturb is the refusal `--backend` exists
//! for: both paths go through `backend::choose`, so a name this build has
//! not got is refused identically whether the run is a request or a
//! socket.
//!
//! # Two halves of one value, driven by hand
//!
//! [`hclient_core::unversioned::WebSocket`] is a `Stream` and a `Sink` on
//! one value, deliberately, so that splitting stays the caller's choice.
//! This driver does not split: one `select!` loop reads stdin, reads the
//! socket and watches for an interrupt, and the send happens inside a
//! branch rather than as a branch — which is what keeps the two-step
//! `poll_ready`/`start_send`/`poll_flush` off a cancellation path it is
//! not safe on.
//!
//! `poll_fn` rather than `futures_util::{StreamExt, SinkExt}`: the two
//! traits are `futures-core`'s and `futures-sink`'s, both already in this
//! graph because `hclient-core` declares the seam in terms of them, so
//! four lines here cost no crate at all.
//!
//! # How it ends, and the one thing that had to be built
//!
//! Three ways, and each is a decision:
//!
//! - **The peer closes.** Its `Message::Close` is delivered, the stream
//!   ends, and `hc` exits `0` — a peer saying goodbye is an answer, not a
//!   failure, which is the same reading `error_for_status` gives a `3xx`.
//! - **stdin reaches EOF.** A `Close` goes out and the loop keeps reading
//!   until the peer's own close comes back. Without that, `printf 'hi\n'
//!   | hc --ws …` would tear the socket down before the answer to `hi`
//!   arrived, which is the shape of bug that makes a tool untestable and
//!   unusable in a pipeline at the same time.
//! - **Ctrl-C.** The first one closes politely; the second stops waiting.
//!   The second exists because the first is only polite if the peer
//!   answers, and a peer that has vanished must not be able to make a
//!   caller's interrupt do nothing.
//!
//! The stdin reader is a **`std::thread`**, not `tokio::io::stdin()`, and
//! that is not a preference: tokio's stdin is a blocking task, and
//! dropping a `Runtime` waits for blocking tasks to finish — so a session
//! ended by the peer while a read was outstanding would hang at process
//! exit, waiting for a line nobody was going to type. A plain thread is
//! not joined at exit.
//!
//! # No ping/pong bound is set
//!
//! `Tungstenite::keep_alive` is off unless asked for, and nothing here
//! asks. That is stated rather than defaulted for the reason the library
//! states: a default that pings sends traffic nobody asked for. The cost
//! is real and is worth knowing — a peer that vanishes without a `FIN`
//! leaves `hc --ws` waiting for ever, and Ctrl-C is what ends it.

use crate::mode::Verbosity;
use crate::run::Fail;
use anstyle::{AnsiColor, Style};
use futures_core::Stream;
use futures_sink::Sink;
use hclient_core::Error;
use hclient_core::unversioned::{CloseFrame, Message, WebSocketConnect};
use std::io::Write;
use std::pin::Pin;

const DIM: Style = Style::new().dimmed();
const IN: Style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Cyan)));
const OUT: Style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Green)));

/// RFC 6455 §7.4.1: a normal closure, which is what stdin ending and a
/// caller's interrupt both are.
const NORMAL: u16 = 1000;

async fn recv<S>(s: &mut S) -> Option<S::Item>
where
    S: Stream + Unpin,
{
    std::future::poll_fn(|cx| Pin::new(&mut *s).poll_next(cx)).await
}

/// `poll_ready` -> `start_send` -> `poll_flush`, which is the whole of
/// `Sink`'s contract and the reason this is never a `select!` arm: it
/// keeps state between the three steps, so a cancellation between them
/// would drop a message the sink had already accepted.
async fn send<S>(s: &mut S, m: Message) -> Result<(), Error>
where
    S: Sink<Message, Error = Error> + Unpin,
{
    std::future::poll_fn(|cx| Pin::new(&mut *s).poll_ready(cx)).await?;
    Pin::new(&mut *s).start_send(m)?;
    std::future::poll_fn(|cx| Pin::new(&mut *s).poll_flush(cx)).await
}

/// One inbound message, printed.
///
/// Split out for the same reason `sse::print_event` is: the vocabulary is
/// then testable with no socket at all.
/// `payload` is the unfiltered descriptor, for the same reason
/// [`crate::output::body`] takes one: a binary message written through
/// `anstream`'s strip filter loses every byte the filter cannot read as
/// text, so `hc --ws … > out.bin` would write a file that is not what
/// arrived.
pub fn print_inbound(
    out: &mut impl Write,
    payload: &mut impl Write,
    message: &Message,
    how: Verbosity,
) -> std::io::Result<()> {
    match (message, how) {
        // The payload form writes the message and nothing else, and a
        // binary one is written as **bytes** — a tool that turned an
        // image into replacement characters is not one anybody can pipe,
        // which is `output::body`'s rule one module over.
        (Message::Text(t), Verbosity::Payload) => {
            out.flush()?;
            writeln!(payload, "{t}")?;
            payload.flush()
        }
        (Message::Binary(b), Verbosity::Payload) => {
            out.flush()?;
            payload.write_all(b)?;
            writeln!(payload)?;
            payload.flush()
        }
        (Message::Close(_), Verbosity::Payload) => Ok(()),

        (Message::Text(t), _) => writeln!(out, "{IN}<{IN:#} {t}"),
        // Not the bytes: in the annotated form the caller is reading a
        // transcript, and a transcript with a PNG in the middle of it is
        // not one. The payload form above is where the bytes go.
        (Message::Binary(b), _) => {
            writeln!(out, "{IN}<{IN:#} {DIM}<{} bytes, binary>{DIM:#}", b.len())
        }
        (Message::Close(frame), _) => match frame {
            Some(CloseFrame { code, reason }) if !reason.is_empty() => {
                writeln!(out, "{DIM}< close {code} {reason}{DIM:#}")
            }
            Some(CloseFrame { code, .. }) => writeln!(out, "{DIM}< close {code}{DIM:#}"),
            // RFC 6455 §5.5.1 allows a close with no body at all, and it
            // is not the same fact as a close carrying 1000: nothing was
            // said about why.
            None => writeln!(out, "{DIM}< close (no code){DIM:#}"),
        },
    }
}

/// Open the socket and run the session.
///
/// Generic over the connector rather than over the transport, so the two
/// backend arms in `run.rs` are each one line and this function is
/// written once. The bound is the seam itself, which is the whole of what
/// this needs — see `WebSocketConnect`'s own doc: something that cannot
/// do WebSocket does not write the impl, and asking it does not compile.
pub async fn run<C>(
    connector: C,
    req: http::Request<()>,
    how: Verbosity,
    out: &mut impl Write,
) -> Result<(), Fail>
where
    C: WebSocketConnect,
{
    if how == Verbosity::Verbose {
        writeln!(
            out,
            "{OUT}GET{OUT:#} {} {DIM}HTTP/1.1 (upgrade){DIM:#}",
            req.uri()
        )
        .map_err(Fail::Io)?;
        for (name, value) in req.headers() {
            let shown = if name == http::header::AUTHORIZATION {
                "<redacted>".to_owned()
            } else {
                String::from_utf8_lossy(value.as_bytes()).into_owned()
            };
            writeln!(out, "{OUT}{name}{OUT:#}: {shown}").map_err(Fail::Io)?;
        }
        writeln!(out).map_err(Fail::Io)?;
    }

    // `Box::pin` rather than `pin!`: the result is `Unpin`, which is what
    // lets both halves be reached through `Pin::new(&mut ..)` in a loop
    // that touches them in different branches.
    let mut ws = Box::pin(connector.websocket(req).await.map_err(Fail::Request)?);

    let mut lines = stdin_lines();
    let mut interrupts = interrupts();
    // Once a close has been **sent or seen** there is nothing left to
    // say, so stdin stops being read: a line typed after the close would
    // either be dropped in silence or be a frame after a close, and RFC
    // 6455 §5.5.1 forbids the second. A line already typed and not yet
    // sent is dropped, which is the honest outcome — the connection it
    // was for is over.
    let mut closing = false;

    loop {
        tokio::select! {
            // Biased so that what has already arrived from the peer is
            // printed before a new line is sent. Without it the ordering
            // of a transcript is the scheduler's rather than the wire's.
            biased;

            message = recv(&mut ws) => {
                let Some(message) = message else { break };
                let message = message.map_err(Fail::Request)?;
                // **The peer's close ends the sending half too**, and
                // this line is the fix for a race the mutation runs
                // surfaced rather than a rule read off the RFC. Without
                // it, a peer that closes first left `closing` false, so
                // stdin reaching EOF a moment later still sent a `Close`
                // — onto a socket the peer had already torn down, which
                // came back as a transport error and exited 4 on a
                // session that had ended perfectly well. It is also what
                // §5.5.1 requires: nothing is sent after a close, in
                // either direction, and `tungstenite` has already
                // answered this one for us.
                if matches!(message, Message::Close(_)) {
                    closing = true;
                }
                print_inbound(out, &mut std::io::stdout(), &message, how).map_err(Fail::Io)?;
                out.flush().map_err(Fail::Io)?;
            }

            line = lines.recv(), if !closing => {
                match line {
                    Some(line) => {
                        if how == Verbosity::Verbose {
                            writeln!(out, "{OUT}>{OUT:#} {line}").map_err(Fail::Io)?;
                            out.flush().map_err(Fail::Io)?;
                        }
                        send(&mut ws, Message::Text(line)).await.map_err(Fail::Request)?;
                    }
                    // EOF. Say goodbye and keep reading: the answer to
                    // the last line sent has not necessarily arrived.
                    None => {
                        closing = true;
                        goodbye(&mut ws).await;
                    }
                }
            }

            _ = interrupts.recv() => {
                if closing {
                    // The second one. A polite close is only polite if
                    // the peer answers, and a caller's interrupt must not
                    // be able to do nothing.
                    break;
                }
                closing = true;
                goodbye(&mut ws).await;
            }
        }
    }
    out.flush().map_err(Fail::Io)?;
    Ok(())
}

/// Send the closing handshake, and **do not fail the run if it is
/// refused.**
///
/// This is the one send whose error is swallowed, and the reason is that
/// its failure is not a failure of anything. A `Close` we write at stdin
/// EOF or at a Ctrl-C is a courtesy; a socket that will not take it is a
/// socket on which a close is already under way, which is the state the
/// courtesy exists to reach.
///
/// The case is real and was found by running the suite oversubscribed
/// rather than by reading. `tungstenite`'s `read` answers a peer's
/// `Close` itself, and one read can lift **both** a data frame and the
/// `Close` behind it off the socket — so a caller that has been handed
/// only the data frame can already be in `ClosedByPeer`, and the next
/// `start_send` is `Sending after closing is not allowed`. It surfaced
/// as exit 4 on a session that had delivered every byte correctly.
///
/// **This and the `closing` flag are two covers for one window, and the
/// suite pins only their conjunction** — which is worth knowing before
/// deleting either as redundant. Measured at `-j96`, twelve runs each:
/// removing the tolerance alone, 0 failures; removing the flag alone, 0
/// failures; removing **both**, 7. So no test discriminates them, and
/// they are both kept for reasons that are not the same reason. The flag
/// is RFC 6455 §5.5.1 — nothing is sent after a close, in either
/// direction — and it is what makes the common case not arise. The
/// tolerance covers the rest, because the flag reads *our* view of the
/// stream and the state that refuses the send is `tungstenite`'s.
///
/// A user's line is **not** treated this way: a `Text` that cannot be
/// sent is data the caller believes went out, and that is an error they
/// have to be told about.
async fn goodbye<S>(ws: &mut S)
where
    S: Sink<Message, Error = Error> + Unpin,
{
    let _ = send(
        ws,
        Message::Close(Some(CloseFrame {
            code: NORMAL,
            reason: String::new(),
        })),
    )
    .await;
}

/// Lines of stdin, on a thread of its own.
///
/// The channel closing **is** the EOF signal, which is why the sender is
/// moved into the thread and dropped there rather than being kept beside
/// the receiver.
fn stdin_lines() -> tokio::sync::mpsc::Receiver<String> {
    let (tx, rx) = tokio::sync::mpsc::channel(16);
    std::thread::spawn(move || {
        use std::io::BufRead as _;
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if tx.blocking_send(line).is_err() {
                break;
            }
        }
    });
    rx
}

/// Ctrl-C, as a stream rather than a fresh future per iteration.
///
/// `tokio::signal::ctrl_c()` builds a new listener each call, and a signal
/// that arrives while no listener exists is not delivered — so awaiting it
/// as a `select!` arm would drop the interrupt that landed between two
/// turns of the loop. One task holds the listener for the whole session
/// and forwards what it sees.
fn interrupts() -> tokio::sync::mpsc::Receiver<()> {
    let (tx, rx) = tokio::sync::mpsc::channel(2);
    tokio::spawn(async move {
        while tokio::signal::ctrl_c().await.is_ok() {
            if tx.send(()).await.is_err() {
                break;
            }
        }
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both sinks, concatenated in the order they are written.
    ///
    /// Every case below writes to exactly one of the two — the annotated
    /// form is text and the payload form is bytes — so the concatenation
    /// is the whole output rather than an interleaving that could be
    /// wrong.
    fn render(message: &Message, how: Verbosity) -> String {
        // Through the same `AutoStream` the binary writes to, with
        // colour off, so these assertions are about the text rather than
        // about `anstyle`'s escapes — which are emitted unconditionally
        // when a `Style` is written to a bare `Vec`.
        let mut out = anstream::AutoStream::new(Vec::new(), anstream::ColorChoice::Never);
        let mut payload = Vec::new();
        print_inbound(&mut out, &mut payload, message, how).expect("a Vec never fails to write");
        let mut buf = out.into_inner();
        buf.extend_from_slice(&payload);
        String::from_utf8_lossy(&buf).into_owned()
    }

    /// The payload form is the message and nothing else, so
    /// `hc --ws … | jq` works — and a binary message is bytes, not a
    /// description of bytes.
    #[test]
    fn the_payload_form_writes_the_message_and_nothing_around_it() {
        assert_eq!(
            render(&Message::Text("hi".into()), Verbosity::Payload),
            "hi\n"
        );
        assert_eq!(
            render(
                &Message::Binary(bytes::Bytes::from_static(&[0xff, 0x00])),
                Verbosity::Payload
            ),
            "\u{fffd}\u{0}\n"
        );
        // A close is framing rather than payload, and a pipeline reading
        // one line per message must not get a line for it.
        assert_eq!(
            render(
                &Message::Close(Some(CloseFrame {
                    code: 1000,
                    reason: String::new()
                })),
                Verbosity::Payload
            ),
            ""
        );
    }

    /// The annotated form is a transcript, so every line says which
    /// direction it came from and a binary message is described rather
    /// than dumped into the middle of it.
    #[test]
    fn the_annotated_form_marks_the_direction_and_describes_a_binary_message() {
        assert_eq!(
            render(&Message::Text("hi".into()), Verbosity::Annotated),
            "< hi\n"
        );
        assert_eq!(
            render(
                &Message::Binary(bytes::Bytes::from_static(b"abc")),
                Verbosity::Annotated
            ),
            "< <3 bytes, binary>\n"
        );
    }

    /// The three shapes a close can have are three different facts, and
    /// flattening them would lose the one that matters: a close with no
    /// body at all said nothing about why.
    #[test]
    fn a_close_with_a_reason_a_bare_code_and_no_body_read_differently() {
        assert_eq!(
            render(
                &Message::Close(Some(CloseFrame {
                    code: 1001,
                    reason: "bye".into()
                })),
                Verbosity::Annotated
            ),
            "< close 1001 bye\n"
        );
        assert_eq!(
            render(
                &Message::Close(Some(CloseFrame {
                    code: 1000,
                    reason: String::new()
                })),
                Verbosity::Annotated
            ),
            "< close 1000\n"
        );
        assert_eq!(
            render(&Message::Close(None), Verbosity::Annotated),
            "< close (no code)\n"
        );
    }
}
