//! Which of the three things `hc` does, and the refusals that keep the
//! other two from silently ignoring a flag.
//!
//! # One rule, applied twice
//!
//! `hc` sends **one request** and prints **one response**, and almost
//! every flag it has is about that shape: a body to send, a status to
//! check, a redirect to follow, a report to print once the exchange has
//! finished. `--sse` is not that shape — it is a stream of
//! events and a session of frames — and each is opened through a seam that
//! is deliberately narrower than [`hclient::RequestBuilder`]:
//!
//! [`hclient::Client::sse`] hands back an `SseBuilder` carrying a **URL
//! and headers and nothing else**: no body, no query, no per-request
//! redirect policy, no auth helper, no `require_version`, and no way to
//! read the response head back out of the stream it opens.
//!
//! So a flag whose effect would have to travel on the request some other
//! way has exactly two honest fates, and this module picks the second:
//! **be dropped in silence, or be refused by name.** That is the same
//! decision `Capabilities` makes one library down, where a client setting
//! a transport cannot honour is an error at `build()` rather than a
//! setting nobody applied; and the same one `hclient-proxy`'s `system`
//! reader makes for a machine's proxy configuration it cannot express.
//!
//! Every refusal below is [`crate::run::Fail::Usage`], **exit 2** — the
//! code that already means "the command line is wrong". None of them is a
//! new exit code, because none of them is a new *kind* of failure: the
//! caller wrote two things that cannot both be true.
//!
//! # What is deliberately *not* refused
//!
//! `--backend`, `-k`, `--resolve` and `--no-color` all configure the
//! transport or the terminal rather than the request, so they mean in
//! both streaming modes exactly what they mean for a request, and are
//! passed through untouched. `--bearer` survives too, and it is the one
//! interesting case: it is a single `Authorization` header, which is the
//! one thing the seam *does* carry — where `--auth` is refused beside it,
//! because Basic needs an encoder this crate does not have and Digest
//! needs a `401` round trip neither seam can make.
//!
//! `--follow` survives too, and it is the other interesting case: the
//! builder has no redirect setter, but a `Client` does, so the effect has
//! somewhere else to travel and is honoured rather than refused.

use crate::args::{Cli, Item};

/// What this run is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// One request, one response — everything `hc` did before.
    Request,
    /// `--sse`. `reconnect` is `--sse-reconnect`; see [`select`] for why
    /// it is opt-in.
    Sse { reconnect: bool },
}

impl Mode {
    /// The flag that named this mode, for a refusal message.
    const fn flag(self) -> &'static str {
        match self {
            Self::Request => "",
            Self::Sse { .. } => "--sse",
        }
    }

    /// What a stream of these is called, so one message template can serve
    /// both modes without reading as if it were written for neither.
    const fn unit(self) -> &'static str {
        match self {
            Self::Request => "response",
            Self::Sse { .. } => "events",
        }
    }

    pub const fn is_streaming(self) -> bool {
        !matches!(self, Self::Request)
    }
}

/// Which mode the command line asks for.
///
/// **`--sse` does not reconnect, and `--sse-reconnect` is the opt-in.**
/// The library made this decision first and this follows it rather than
/// re-deciding: `SseBuilder::connect()` on its own is one attempt, and
/// reconnection is reached only through `with_timer(..)`, which is a
/// *type-level* gate precisely so that nothing can enable it by accident.
///
/// Three things say the same about a command-line tool. A reconnect after
/// a clean end of stream **sends a second request the caller asked for
/// once**, and the cost of that lands at the server rather than here. The
/// reconnecting stream treats everything but five `ErrorKind`s as
/// retryable — `Connect`, `Resolve`, `Tls`, `Timeout`, `Body` included —
/// so it converts most failures into silence, where a one-shot run turns
/// them into an exit code a script can read. And a stream that ends is
/// itself information: the server said goodbye, and a client that quietly
/// reopens has hidden that.
///
/// So the default is the honest one and the loop is asked for by name.
pub fn select(cli: &Cli) -> Result<Mode, String> {
    if cli.sse_reconnect && !cli.sse {
        return Err(
            "`--sse-reconnect` says how `--sse` behaves when the stream ends, and there is \
             no `--sse` here to behave that way."
                .into(),
        );
    }
    Ok(if cli.sse {
        Mode::Sse {
            reconnect: cli.sse_reconnect,
        }
    } else {
        Mode::Request
    })
}

/// Every combination this tool will not pretend to honour, named.
///
/// A pure function of the command line and the parsed items, so the whole
/// table is testable with no socket, no server and no feature set — which
/// is what `backend::choose` was extracted for one file over, after a
/// mutation replacing its refusal with a silent fallback survived all 30
/// tests because the refusing arm is unreachable in the build CI runs.
///
/// `items` is passed rather than read off `cli` because the grammar is
/// classified in `run.rs`, and *which kind* an argument turned out to be
/// is the whole of what this has to refuse.
pub fn refuse_unusable(mode: Mode, cli: &Cli, items: &[Item]) -> Result<(), String> {
    if !mode.is_streaming() {
        return Ok(());
    }
    let flag = mode.flag();
    let unit = mode.unit();

    // ── the two that are about output, and are the same argument twice ──
    if cli.print.is_some() {
        return Err(format!(
            "`--print` names parts of one request/response exchange — H, B, h, b — and \
             `{flag}` is a stream of {unit} rather than one exchange. `-v` (more detail) \
             and `-b` (payload only) still mean what they mean everywhere else."
        ));
    }
    if cli.headers {
        return Err(format!(
            "`--headers` prints one response head, and `{flag}` has none to print: {}",
            // Not merely "there are several": `SseStream` owns the
            // `Response` it was built from and exposes neither `status()`
            // nor `headers()`, so this is unreachable rather than
            // unimplemented.
            "the stream owns the response it was opened with and hands back only events."
        ));
    }

    // ── the request body, in all four spellings ─────────────────────────
    if let Some(bad) = items.iter().find(|i| !matches!(i, Item::Header { .. })) {
        let (form, why) = match bad {
            Item::Header { .. } => unreachable!("filtered out above"),
            Item::Query { .. } => (
                "name==value",
                "a query belongs in the URL here — write it into the URL you pass, \
                 because the seam takes the URL as a string and appends nothing to it",
            ),
            Item::Data { .. } | Item::JsonRaw { .. } | Item::File { .. } => (
                match bad {
                    Item::Data { .. } => "name=value",
                    Item::JsonRaw { .. } => "name:=value",
                    _ => "name@file",
                },
                "it sets a request body, and the request that opens this stream carries none",
            ),
        };
        return Err(format!(
            "`{form}` items cannot be used with `{flag}`: {why}. Header items \
             (`name:value`) are carried."
        ));
    }
    for (flagname, set) in [
        ("--raw-body", cli.raw_body.is_some()),
        ("--form", cli.form),
        ("--json", cli.json),
    ] {
        if set {
            return Err(format!(
                "`{flagname}` sets a request body, and the request `{flag}` opens the \
                 stream with carries none."
            ));
        }
    }

    // ── flags whose effect has nowhere to travel ────────────────────────
    if cli.auth.is_some() || cli.digest {
        return Err(format!(
            "`--auth` cannot be used with `{flag}`: Basic would need an encoder this \
             binary does not carry for one header, and Digest needs the `401` round trip \
             the streaming seam cannot make. `--bearer` is carried, because it is one \
             header."
        ));
    }
    if cli.http.is_some() {
        return Err(format!(
            "`--http` cannot be used with `{flag}`: the SSE builder carries a URL and \
             headers and has no place to put a version demand."
        ));
    }
    if cli.check_status {
        // A flag that could never fire is the silently-ignored setting
        // one layer up, so it is refused rather than accepted and left
        // inert.
        return Err(format!(
            "`--check-status` cannot be used with `{flag}`: any status but 200 already \
             fails the stream outright — that is WHATWG's rule, not this tool's — so the \
             flag could never fire."
        ));
    }
    if cli.write_out.is_some() {
        return Err(format!(
            "`-w`/`--write-out` reports one finished exchange and is printed after its \
             body; `{flag}` has no end to print it after, and no `time_total` to report."
        ));
    }

    // `--timeout` alone is a bound on the one connection, which is a
    // thing a caller can want. With `--sse-reconnect` it is not a bound
    // at all.
    if matches!(mode, Mode::Sse { reconnect: true }) && cli.timeout.is_some() {
        return Err(
            "`--timeout` and `--sse-reconnect` together are a loop rather than a bound: \
             the timeout is the client's whole-operation bound and applies to each \
             connection, so every connection would be cut at that point and immediately \
             reopened. Use one or the other."
                .into(),
        );
    }
    Ok(())
}

/// How much to say about each event.
///
/// The same three-way choice `print_selection` already makes for a
/// response, and made the same way — a terminal gets the annotated form
/// and a pipe gets the payload alone, so `hc --sse … | jq` needs no flag —
/// with `-v` and `-b` as the two overrides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verbosity {
    /// `-b`, or any pipe: the payload and nothing else, one per line.
    Payload,
    /// A terminal: the payload with what it arrived as.
    Annotated,
    /// `-v`: that, plus what this tool sent.
    Verbose,
}

pub fn verbosity(cli: &Cli, is_tty: bool) -> Verbosity {
    if cli.verbose {
        Verbosity::Verbose
    } else if cli.body || !is_tty {
        Verbosity::Payload
    } else {
        Verbosity::Annotated
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;

    fn cli(args: &[&str]) -> Cli {
        let mut all = vec!["hc"];
        all.extend_from_slice(args);
        Cli::try_parse_from(all).expect("the fixture's own command lines parse")
    }

    /// The default is one-shot, and the loop has to be asked for — which
    /// is the library's own gate (`with_timer`) restated at the command
    /// line rather than a second decision.
    #[test]
    fn sse_does_not_reconnect_unless_asked() {
        assert_eq!(
            select(&cli(&["--sse", "http://x/"])).unwrap(),
            Mode::Sse { reconnect: false }
        );
        assert_eq!(
            select(&cli(&["--sse", "--sse-reconnect", "http://x/"])).unwrap(),
            Mode::Sse { reconnect: true }
        );
    }

    #[test]
    fn reconnect_without_the_mode_it_configures_is_refused() {
        let e = select(&cli(&["--sse-reconnect", "http://x/"])).unwrap_err();
        assert!(e.contains("no `--sse` here"), "{e}");
    }

    /// Every refusal names the flag the caller wrote **and** the mode it
    /// collided with. A refusal a caller cannot act on is barely better
    /// than a silent drop, which is the same standard `Refused`'s own test
    /// holds `--backend` to one file over.
    #[test]
    fn every_streaming_refusal_names_both_halves_of_the_collision() {
        let cases: &[(&str, &[&str])] = &[
            ("--print", &["--print", "hb"]),
            ("--headers", &["--headers"]),
            ("--raw-body", &["--raw-body", "/dev/null"]),
            ("--form", &["-f"]),
            ("--json", &["-j"]),
            ("--auth", &["-a", "u:p"]),
            ("--http", &["--http", "2"]),
            ("--check-status", &["--check-status"]),
            ("--write-out", &["-w", "%{http_code}"]),
        ];
        for (name, extra) in cases {
            let mut args = vec!["--sse", "http://x/"];
            args.extend_from_slice(extra);
            let c = cli(&args);
            let m = select(&c).unwrap();
            let e = refuse_unusable(m, &c, &[])
                .expect_err("an accepted flag here would be one that silently does nothing");
            assert!(e.contains(name), "{name}: {e}");
            assert!(e.contains("--sse"), "{name}: {e}");
        }
    }

    /// The control for the table above: the flags that genuinely mean the
    /// same thing in a stream are **not** refused, or the rule would read
    /// as "`--sse` takes no flags".
    #[test]
    fn the_flags_that_still_mean_something_are_carried() {
        for extra in [
            vec!["-k"],
            vec!["--resolve", "a:127.0.0.1"],
            vec!["--bearer", "t"],
            vec!["-v"],
            vec!["-b"],
        ] {
            let mut args = vec!["--sse", "http://x/"];
            args.extend_from_slice(&extra);
            let c = cli(&args);
            let m = select(&c).unwrap();
            assert!(
                refuse_unusable(m, &c, &[]).is_ok(),
                "{extra:?} is refused under `--sse` and should not be"
            );
        }
    }

    #[test]
    fn a_header_item_is_carried_and_every_other_item_kind_is_named() {
        let c = cli(&["--sse", "http://x/"]);
        let m = select(&c).unwrap();
        let header = [Item::Header {
            name: "x".into(),
            value: "1".into(),
        }];
        assert!(refuse_unusable(m, &c, &header).is_ok());

        for (item, form) in [
            (
                Item::Query {
                    name: "a".into(),
                    value: "1".into(),
                },
                "name==value",
            ),
            (
                Item::Data {
                    name: "a".into(),
                    value: "1".into(),
                },
                "name=value",
            ),
            (
                Item::JsonRaw {
                    name: "a".into(),
                    value: "1".into(),
                },
                "name:=value",
            ),
            (
                Item::File {
                    name: "a".into(),
                    path: "/x".into(),
                },
                "name@file",
            ),
        ] {
            let e = refuse_unusable(m, &c, &[item]).unwrap_err();
            assert!(e.contains(form), "{form}: {e}");
        }
    }

    /// The two flags the table does **not** refuse, and the one shape of
    /// `--timeout` it does — checked in both directions, because a table
    /// that refused everything would pass the test above and be wrong
    /// here.
    #[test]
    fn follow_and_timeout_are_refused_only_where_they_cannot_be_honoured() {
        // `--follow` is honoured, on the client rather than on the
        // request — see `sse::run`.
        let sse = cli(&["--sse", "http://x/", "-L"]);
        assert!(refuse_unusable(select(&sse).unwrap(), &sse, &[]).is_ok());

        // One connection can be bounded; a reconnect loop of bounded
        // connections is a loop rather than a bound.
        let sse_t = cli(&["--sse", "http://x/", "--timeout", "1"]);
        assert!(refuse_unusable(select(&sse_t).unwrap(), &sse_t, &[]).is_ok());
        let both = cli(&["--sse", "--sse-reconnect", "http://x/", "--timeout", "1"]);
        assert!(
            refuse_unusable(select(&both).unwrap(), &both, &[])
                .unwrap_err()
                .contains("loop rather than a bound")
        );
    }

    /// And the control for the whole module: an ordinary request refuses
    /// none of it, or every one of these tests would be passing for a
    /// function that says no to everything.
    #[test]
    fn a_plain_request_refuses_nothing_here() {
        let c = cli(&[
            "http://x/",
            "-w",
            "%{http_code}",
            "--check-status",
            "-L",
            "--http",
            "2",
        ]);
        assert_eq!(select(&c).unwrap(), Mode::Request);
        assert!(
            refuse_unusable(
                Mode::Request,
                &c,
                &[Item::Data {
                    name: "a".into(),
                    value: "1".into()
                }]
            )
            .is_ok()
        );
    }

    #[test]
    fn a_pipe_gets_the_payload_alone_and_a_terminal_gets_more() {
        let plain = cli(&["--sse", "http://x/"]);
        assert_eq!(verbosity(&plain, false), Verbosity::Payload);
        assert_eq!(verbosity(&plain, true), Verbosity::Annotated);
        // `-b` forces the pipe's form even on a terminal, and `-v` the
        // fullest one even in a pipe.
        assert_eq!(
            verbosity(&cli(&["-b", "--sse", "http://x/"]), true),
            Verbosity::Payload
        );
        assert_eq!(
            verbosity(&cli(&["-v", "--sse", "http://x/"]), false),
            Verbosity::Verbose
        );
    }
}
