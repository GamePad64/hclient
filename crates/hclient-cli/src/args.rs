//! The command line, and httpie's request-item grammar.
//!
//! The grammar is **copied rather than invented**: `k=v`, `k:=v`, `k==v`,
//! `h:v`, `f@path`. httpie chose it, `xh` re-implemented it compatibly,
//! and a third spelling would make every example on the internet wrong for
//! this tool. What is not copied is httpie's default of sending JSON for a
//! bare `k=v` with no method — that is decided in `run.rs` and stated
//! there.

use clap::{Parser, ValueEnum};

/// One `KEY<sep>VALUE` argument, already classified.
///
/// Separators are matched **longest first**, which is the only subtle part
/// of the grammar: `:=` and `==` both begin with a character that is
/// itself a separator, so a left-to-right scan for `:` would read `a:=1`
/// as the header `a` with value `=1`. Every implementation of this grammar
/// has that ordering, and getting it wrong is silent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// `name:value` — a request header. An empty value (`name:`) removes a
    /// header this tool would otherwise send, which is how httpie spells
    /// "do not send the default `User-Agent`".
    Header { name: String, value: String },
    /// `name==value` — a query parameter, appended to whatever the URL
    /// already carries.
    Query { name: String, value: String },
    /// `name:=value` — a JSON field whose value is parsed as JSON, so
    /// `n:=1` is a number and `xs:=[1,2]` is an array.
    JsonRaw { name: String, value: String },
    /// `name=value` — a data field. It is a JSON string in a JSON body and
    /// a form field in a form body; which one it becomes is `--form`'s
    /// decision, not this type's.
    Data { name: String, value: String },
    /// `name@path` — a file, as a multipart part.
    File { name: String, path: String },
}

/// What went wrong reading one item, with the text that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemError {
    pub arg: String,
    pub reason: ItemReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemReason {
    /// No separator at all — most often a URL typed after the first one.
    NoSeparator,
    /// A separator with nothing to its left.
    EmptyName,
    /// `:=` whose right-hand side is not JSON.
    BadJson(String),
    /// A second URL. The grammar reads `https://example.com` as a header
    /// named `https` with value `//example.com`, because a `:` is a
    /// separator — so the most likely mistake a caller can make produces a
    /// silently wrong request rather than an error.
    LooksLikeAUrl,
}

impl std::fmt::Display for ItemError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.reason {
            ItemReason::NoSeparator => write!(
                f,
                "`{}` is not a request item: expected one of `name=value` (data), \
                 `name:=value` (raw JSON), `name==value` (query), `name:value` (header) \
                 or `name@file` (upload)",
                self.arg
            ),
            ItemReason::EmptyName => {
                write!(f, "`{}` has no name before the separator", self.arg)
            }
            ItemReason::LooksLikeAUrl => write!(
                f,
                "`{}` looks like a URL, and hc sends one request to one URL. \
                 (The item grammar would otherwise read it as a header named `{}`, \
                 because `:` separates a header from its value.)",
                self.arg,
                self.arg.split(':').next().unwrap_or_default()
            ),
            ItemReason::BadJson(e) => write!(
                f,
                "`{}` uses `:=`, so its value must be JSON, and it is not: {e}",
                self.arg
            ),
        }
    }
}

/// Classify one argument.
///
/// The scan finds the **earliest** position at which any separator starts,
/// and at that position prefers the longest separator. Earliest-then-longest
/// is what makes `Content-Type:application/json` a header (the `:` comes
/// before the `/`) while `a:=1` is raw JSON rather than a header named `a`.
pub fn parse_item(arg: &str) -> Result<Item, ItemError> {
    let bytes = arg.as_bytes();
    let mut found: Option<(usize, &'static str)> = None;
    for (i, w) in bytes.iter().enumerate() {
        // Two-character separators first at this position, then one.
        let two = bytes.get(i + 1).map(|n| (*w, *n));
        let sep = match two {
            Some((b':', b'=')) => Some(":="),
            Some((b'=', b'=')) => Some("=="),
            _ => None,
        }
        .or(match w {
            b'=' => Some("="),
            b':' => Some(":"),
            b'@' => Some("@"),
            _ => None,
        });
        if let Some(s) = sep {
            found = Some((i, s));
            break;
        }
    }
    let Some((at, sep)) = found else {
        return Err(ItemError {
            arg: arg.into(),
            reason: ItemReason::NoSeparator,
        });
    };
    let name = &arg[..at];
    if name.is_empty() {
        return Err(ItemError {
            arg: arg.into(),
            reason: ItemReason::EmptyName,
        });
    }
    let value = &arg[at + sep.len()..];
    Ok(match sep {
        ":=" => {
            if let Err(e) = serde_json::from_str::<serde_json::Value>(value) {
                return Err(ItemError {
                    arg: arg.into(),
                    reason: ItemReason::BadJson(e.to_string()),
                });
            }
            Item::JsonRaw {
                name: name.into(),
                value: value.into(),
            }
        }
        "==" => Item::Query {
            name: name.into(),
            value: value.into(),
        },
        "=" => Item::Data {
            name: name.into(),
            value: value.into(),
        },
        ":" => {
            // A scheme followed by an authority is a URL and not a header:
            // no header is named `http` or `https`, and the `//` is what
            // makes the distinction exact rather than a guess. `https:x`
            // stays an ordinary header, so the refusal is as narrow as the
            // mistake it names.
            if value.starts_with("//") && matches!(name, "http" | "https") {
                return Err(ItemError {
                    arg: arg.into(),
                    reason: ItemReason::LooksLikeAUrl,
                });
            }
            Item::Header {
                name: name.into(),
                value: value.into(),
            }
        }
        "@" => Item::File {
            name: name.into(),
            path: value.into(),
        },
        _ => unreachable!("the scan above produces exactly these five"),
    })
}

/// Which parts of the exchange to print.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Print {
    pub request_head: bool,
    pub request_body: bool,
    pub response_head: bool,
    pub response_body: bool,
}

impl Print {
    /// httpie's `--print` letters: `H` request head, `B` request body,
    /// `h` response head, `b` response body.
    pub fn parse(s: &str) -> Result<Self, char> {
        let mut p = Self::default();
        for c in s.chars() {
            match c {
                'H' => p.request_head = true,
                'B' => p.request_body = true,
                'h' => p.response_head = true,
                'b' => p.response_body = true,
                other => return Err(other),
            }
        }
        Ok(p)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BackendName {
    /// rustls, with this machine's trust store through the platform
    /// verifier.
    Rustls,
    /// The platform's own TLS stack — SChannel, Security.framework,
    /// OpenSSL — for a deployment whose trust decisions live in the OS.
    NativeTls,
}

impl std::fmt::Display for BackendName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Rustls => "rustls",
            Self::NativeTls => "native-tls",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum HttpVersion {
    #[value(name = "1.1")]
    Http11,
    #[value(name = "2")]
    Http2,
    #[value(name = "3")]
    Http3,
}

#[derive(Debug, Parser)]
#[command(
    name = "hc",
    about = "An HTTP client that lets you choose the backend at runtime",
    long_about = "hc sends one HTTP request and prints what came back.\n\n\
                  Unlike curl, the TLS and transport backend is chosen with a flag rather \
                  than by whoever packaged the binary — and a backend this build does not \
                  have is refused by name rather than silently ignored. `hc --version` \
                  lists what is compiled in.",
    version,
    disable_version_flag = true
)]
pub struct Cli {
    /// The method. Optional: omit it and `hc` sends GET, or POST if the
    /// request has a body — httpie's rule, and the one that makes
    /// `hc example.com name=alice` do what it reads like.
    #[arg(value_parser = method_or_url, hide = true)]
    pub first: String,

    /// Remaining positionals: possibly the URL, then request items.
    pub rest: Vec<String>,

    /// Which backend to use. A name this build does not carry is an error
    /// naming it, never a silent fallback.
    #[arg(long, value_enum)]
    pub backend: Option<BackendName>,

    /// Print this selection of the exchange: `H` request head, `B` request
    /// body, `h` response head, `b` response body.
    #[arg(long, value_name = "HBhb")]
    pub print: Option<String>,

    /// Print the response headers as well as the body.
    #[arg(short = 'v', long)]
    pub verbose: bool,

    /// Print only the response headers.
    #[arg(long)]
    pub headers: bool,

    /// Print only the response body. The default when stdout is not a
    /// terminal.
    #[arg(short = 'b', long)]
    pub body: bool,

    /// Send the data items as a form body rather than as JSON.
    #[arg(short = 'f', long)]
    pub form: bool,

    /// Send the data items as JSON. On by default when there are any.
    #[arg(short = 'j', long)]
    pub json: bool,

    /// Follow redirects.
    #[arg(short = 'L', long)]
    pub follow: bool,

    /// How many redirects to follow.
    #[arg(long, value_name = "N", default_value_t = 10)]
    pub max_redirects: u8,

    /// **Do not verify the server's certificate.** For a host whose
    /// identity you are establishing some other way — a development server,
    /// a device on a local network. It offers nothing against an active
    /// attacker.
    #[arg(short = 'k', long)]
    pub insecure: bool,

    /// Send this host to this address instead of resolving it:
    /// `--resolve example.com:127.0.0.1`. Repeatable.
    ///
    /// curl spells it `HOST:PORT:ADDRESS`; the port is accepted and
    /// ignored, because the resolver seam this is built on is asked for a
    /// name and never for a port.
    #[arg(long, value_name = "HOST[:PORT]:ADDRESS")]
    pub resolve: Vec<String>,

    /// `user:password` for HTTP basic authentication.
    #[arg(short = 'a', long, value_name = "USER:PASS")]
    pub auth: Option<String>,

    /// Use digest authentication with `--auth`'s credentials.
    #[arg(long)]
    pub digest: bool,

    /// A bearer token.
    #[arg(long, value_name = "TOKEN")]
    pub bearer: Option<String>,

    /// Force a protocol version.
    #[arg(long, value_enum, value_name = "VERSION")]
    pub http: Option<HttpVersion>,

    /// Give up after this many seconds.
    #[arg(long, value_name = "SECONDS")]
    pub timeout: Option<f64>,

    /// Read the request body from this file, or `-` for stdin.
    #[arg(long, value_name = "PATH")]
    pub raw_body: Option<String>,

    /// Exit non-zero on a 4xx or 5xx response.
    #[arg(long)]
    pub check_status: bool,

    /// Never colour the output.
    #[arg(long)]
    pub no_color: bool,

    /// Read the URL as a stream of Server-Sent Events and print them
    /// until the stream ends or the caller interrupts.
    ///
    /// **One connection.** When the stream ends, so does `hc`. Add
    /// `--sse-reconnect` for the browser's `EventSource` behaviour; see
    /// `mode::select` for why that is the opt-in rather than the default.
    #[arg(long)]
    pub sse: bool,

    /// Reopen an `--sse` stream after it ends, with jittered exponential
    /// backoff and `Last-Event-ID`.
    #[arg(long)]
    pub sse_reconnect: bool,

    /// Open a WebSocket, send each line of stdin as a Text message and
    /// print the messages that come back.
    ///
    /// Ends when stdin reaches EOF or the peer closes; the first Ctrl-C
    /// closes politely and the second gives up waiting.
    #[arg(long)]
    pub ws: bool,

    /// Print a timing report after the response, curl's `--write-out`.
    ///
    /// `%{time_total}`, `%{http_code}` and eleven more; `\n` and `%%`
    /// work as curl's do. An unknown variable is refused by name rather
    /// than printed back as text, because a script that asked for a
    /// timing and silently got the characters would report one that never
    /// happened.
    #[arg(short = 'w', long, value_name = "FORMAT")]
    pub write_out: Option<String>,

    /// Print the backends this build carries, and exit.
    #[arg(short = 'V', long)]
    pub version: bool,
}

/// clap needs *something* for the first positional; every check that
/// matters happens in `run.rs`, where the URL and the method are separated
/// with the whole argument list in hand.
fn method_or_url(s: &str) -> Result<String, std::convert::Infallible> {
    Ok(s.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ordering rule, which is the only subtle part of the grammar and
    /// the one every implementation gets wrong once: a left-to-right scan
    /// for `:` reads `a:=1` as a header.
    #[test]
    fn two_character_separators_win_over_the_one_character_ones_they_start_with() {
        assert_eq!(
            parse_item("a:=1"),
            Ok(Item::JsonRaw {
                name: "a".into(),
                value: "1".into()
            })
        );
        assert_eq!(
            parse_item("a==1"),
            Ok(Item::Query {
                name: "a".into(),
                value: "1".into()
            })
        );
        // And the one-character forms still work where no pair follows.
        assert_eq!(
            parse_item("a:1"),
            Ok(Item::Header {
                name: "a".into(),
                value: "1".into()
            })
        );
        assert_eq!(
            parse_item("a=1"),
            Ok(Item::Data {
                name: "a".into(),
                value: "1".into()
            })
        );
    }

    /// Earliest-then-longest, not longest-anywhere: the `:` in a header
    /// comes before the `/` in its value, and a `=` later in the value must
    /// not steal the classification.
    #[test]
    fn the_earliest_separator_decides_even_when_a_later_one_is_longer() {
        assert_eq!(
            parse_item("Content-Type:application/json"),
            Ok(Item::Header {
                name: "Content-Type".into(),
                value: "application/json".into()
            })
        );
        assert_eq!(
            parse_item("q=a==b"),
            Ok(Item::Data {
                name: "q".into(),
                value: "a==b".into()
            })
        );
    }

    #[test]
    fn a_raw_json_value_is_validated_where_it_is_written() {
        // The point of validating here rather than at send time: the error
        // names the argument the caller typed.
        let e = parse_item("a:={bad").unwrap_err();
        assert!(matches!(e.reason, ItemReason::BadJson(_)));
        assert!(
            e.to_string().contains("a:={bad"),
            "the message must quote the argument"
        );
    }

    #[test]
    fn an_argument_with_no_separator_is_named_rather_than_guessed() {
        let e = parse_item("nonsense").unwrap_err();
        assert_eq!(e.reason, ItemReason::NoSeparator);
        // The message lists the five forms rather than saying "invalid",
        // because the caller has to know which one they meant.
        assert!(e.to_string().contains("name=value"));
    }

    /// The trap this grammar sets for its own users: `:` is a separator,
    /// so a second URL parses cleanly as a header named `https` and the
    /// request goes out wrong with nothing said.
    #[test]
    fn a_second_url_is_refused_rather_than_read_as_a_header_named_https() {
        let e = parse_item("https://example.com").unwrap_err();
        assert_eq!(e.reason, ItemReason::LooksLikeAUrl);
        assert!(e.to_string().contains("looks like a URL"));
        assert_eq!(
            parse_item("http://x/y").unwrap_err().reason,
            ItemReason::LooksLikeAUrl
        );

        // And the refusal is exactly as wide as the mistake: a header
        // whose value does not open an authority is untouched, as is any
        // other name.
        assert_eq!(
            parse_item("https:x"),
            Ok(Item::Header {
                name: "https".into(),
                value: "x".into()
            })
        );
        assert_eq!(
            parse_item("Link://weird"),
            Ok(Item::Header {
                name: "Link".into(),
                value: "//weird".into()
            })
        );
    }

    #[test]
    fn an_empty_name_is_refused() {
        assert_eq!(parse_item("=v").unwrap_err().reason, ItemReason::EmptyName);
        assert_eq!(parse_item(":v").unwrap_err().reason, ItemReason::EmptyName);
    }

    #[test]
    fn an_empty_header_value_is_a_header_item_and_not_an_error() {
        // It is how a caller suppresses a default header; `run.rs` gives
        // it that meaning, and this pins that it survives parsing.
        assert_eq!(
            parse_item("User-Agent:"),
            Ok(Item::Header {
                name: "User-Agent".into(),
                value: String::new()
            })
        );
    }

    #[test]
    fn a_file_item_keeps_its_path_verbatim() {
        assert_eq!(
            parse_item("photo@/tmp/a b.png"),
            Ok(Item::File {
                name: "photo".into(),
                path: "/tmp/a b.png".into()
            })
        );
    }

    #[test]
    fn print_letters_are_the_four_httpie_uses_and_nothing_else() {
        assert_eq!(
            Print::parse("Hb"),
            Ok(Print {
                request_head: true,
                response_body: true,
                ..Print::default()
            })
        );
        assert_eq!(Print::parse("x"), Err('x'));
    }
}
