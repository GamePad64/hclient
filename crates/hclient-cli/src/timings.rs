//! `-w/--write-out`: curl's timing report, over this workspace's hooks.
//!
//! # Where the numbers come from, and where they cannot come from
//!
//! Every value here is read off an [`Event`] — there is no second clock
//! and no instrumentation of our own inside the request. `ConnectTiming`
//! already carries `dns`, `tcp` and an optional `tls`, and `Head` carries
//! `elapsed`, so the phases curl reports are ones this client already
//! measures for anyone watching.
//!
//! **`time_total` is the exception and is measured out here**, around the
//! whole exchange including reading the body, because no event marks the
//! end of a body — the caller's last `poll_frame` does, and that is the
//! CLI's own code.
//!
//! # A phase that did not happen reads zero, which is curl's convention
//!
//! A connection taken from the pool emits `Reused` and no `Connected`, so
//! there is no DNS lookup, no TCP connect and no handshake **for this
//! request** — and `time_connect` is `0`, exactly as curl reports it.
//! `time_appconnect` is `0` over plain `http://` for the same reason.
//!
//! This is a report rather than a measurement failure, and the difference
//! is visible: `num_connects` says whether a connection was made at all,
//! so a reader who needs to tell *no handshake happened* from *the
//! handshake took no time* has the field that answers it. Inventing a
//! sentinel instead would have broken every script that parses curl's.
//!
//! # Zero cost when it is not asked for
//!
//! `Hooks::WATCHING` gates the backend's clock reads, so a build of `hc`
//! is only paying for this while `-w` is on the command line — the client
//! is constructed with [`NoHooks`](hclient_core::unversioned::NoHooks)
//! otherwise. That is why `backend::build` has two arms rather than
//! installing a recorder that discards.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use hclient_core::unversioned::{Event, Hooks};

/// What the hooks saw, plus what the CLI measured around them.
#[derive(Debug, Default, Clone)]
pub struct Timings {
    /// Name resolution, from the first `Connected` of the exchange.
    pub dns: Duration,
    /// The TCP connect that followed it.
    pub tcp: Duration,
    /// The TLS handshake, absent over plain `http://`.
    pub tls: Option<Duration>,
    /// Time to the response head — the first `Head` event's `elapsed`.
    pub ttfb: Option<Duration>,
    /// How many connections were made. `0` means every hop was pooled.
    pub connects: u32,
    /// How many response heads arrived. One per hop, so a redirect chain
    /// of two requests is `2`.
    pub heads: u32,
    /// The peer of the first connection, where there was one. `None` for
    /// a Unix socket as well as for a wholly pooled exchange.
    pub remote: Option<std::net::SocketAddr>,
    /// The handshake's own report — `"TLSv1.3"`, the IANA suite name, and
    /// the ALPN the peer selected. All three `None` over `http://`, and
    /// all three `None` on a backend that does not report them, which
    /// `tls` in [`Self::tls`] separates by being `Some` only when a
    /// handshake was timed.
    pub tls_version: Option<String>,
    pub tls_cipher: Option<String>,
    pub alpn: Option<Vec<u8>>,
}

/// The [`Hooks`] impl that fills a [`Timings`].
///
/// Cheap to clone — it is one `Arc` — because `Native::hooks` takes the
/// value and the caller keeps a handle to read afterwards.
#[derive(Debug, Clone, Default)]
pub struct Recorder(Arc<Mutex<Timings>>);

impl Recorder {
    pub fn new() -> Self {
        Self::default()
    }

    /// A snapshot. Takes the lock rather than holding one, so a poisoned
    /// mutex — only reachable if a hook panicked — degrades to zeros
    /// rather than taking the process down after the request succeeded.
    pub fn snapshot(&self) -> Timings {
        self.0.lock().map(|t| t.clone()).unwrap_or_default()
    }
}

impl Hooks for Recorder {
    fn on(&self, event: Event<'_>) {
        let Ok(mut t) = self.0.lock() else { return };
        match event {
            Event::Connected(c) => {
                // The **first** connection of the exchange is the one curl
                // reports: its `time_connect` is about reaching the origin
                // the caller named, and a redirect's second connection
                // would overwrite that with an unrelated number.
                if t.connects == 0 {
                    t.dns = c.timing.dns;
                    t.tcp = c.timing.tcp;
                    t.tls = c.timing.tls;
                    t.remote = c.remote;
                    t.tls_version = c.tls_version.map(ToOwned::to_owned);
                    t.tls_cipher = c.tls_cipher.map(ToOwned::to_owned);
                    t.alpn = c.alpn.map(<[u8]>::to_vec);
                }
                t.connects += 1;
            }
            Event::Head(h) => {
                if t.heads == 0 {
                    t.ttfb = Some(h.elapsed);
                }
                t.heads += 1;
            }
            _ => {}
        }
    }
}

/// Everything the format string can name that is not a duration.
#[derive(Debug, Default, Clone)]
pub struct Facts {
    pub status: u16,
    pub version: String,
    pub url: String,
    pub size_download: u64,
    pub total: Duration,
}

/// A `%{...}` this build does not know.
#[derive(Debug)]
pub struct Unknown(pub String);

impl std::fmt::Display for Unknown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "unknown --write-out variable `%{{{}}}`.\n\nThis build knows: {}",
            self.0,
            KNOWN.join(", ")
        )
    }
}

impl std::error::Error for Unknown {}

/// Every name [`render`] answers, in the order a reader would want them
/// listed. Also the message an unknown one is refused with, so the two
/// cannot drift.
pub const KNOWN: &[&str] = &[
    "time_namelookup",
    "time_connect",
    "time_appconnect",
    "time_starttransfer",
    "time_total",
    "http_code",
    "http_version",
    "num_connects",
    "num_redirects",
    "remote_ip",
    "remote_port",
    "size_download",
    "url_effective",
];

/// Six decimal places, which is curl's format for every `time_*`.
fn secs(d: Duration) -> String {
    format!("{:.6}", d.as_secs_f64())
}

/// Expands `fmt`, or names the first variable it does not know.
///
/// A **pure function of its inputs**, which is what lets the whole
/// vocabulary be tested with no socket — the split this workspace uses
/// between `sys` and its parsers, applied to a formatter.
///
/// Unknown names are a **refusal** rather than passed through as
/// literal text: a script asking for `%{time_pretransfer}` and silently
/// getting the characters back would report a timing that never existed.
pub fn render(fmt: &str, t: &Timings, f: &Facts) -> Result<String, Unknown> {
    let mut out = String::with_capacity(fmt.len());
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            // `\n`, `\t` and `\r` are curl's escapes. Anything else after
            // a backslash is that character, so a Windows path in a
            // format string does not have to be doubled.
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            },
            '%' => match chars.peek() {
                // `%%` is a literal percent, so a format string can print
                // one without opening a variable.
                Some('%') => {
                    chars.next();
                    out.push('%');
                }
                Some('{') => {
                    chars.next();
                    let mut name = String::new();
                    let mut closed = false;
                    for c in chars.by_ref() {
                        if c == '}' {
                            closed = true;
                            break;
                        }
                        name.push(c);
                    }
                    if !closed {
                        return Err(Unknown(format!("{name} (unterminated: no `}}`)")));
                    }
                    out.push_str(&value(&name, t, f).ok_or_else(|| Unknown(name.clone()))?);
                }
                // A bare `%` is itself. curl warns here; refusing would
                // make a literal percent sign an error, which is worse
                // for a format string people write by hand.
                _ => out.push('%'),
            },
            other => out.push(other),
        }
    }
    Ok(out)
}

fn value(name: &str, t: &Timings, f: &Facts) -> Option<String> {
    let tls = t.tls.unwrap_or_default();
    Some(match name {
        "time_namelookup" => secs(t.dns),
        // curl's `time_connect` is *cumulative from the start*, not the
        // duration of the connect alone — so DNS is included. The same
        // holds for `time_appconnect`, which adds the handshake.
        "time_connect" => secs(t.dns + t.tcp),
        "time_appconnect" => {
            if t.tls.is_some() {
                secs(t.dns + t.tcp + tls)
            } else {
                secs(Duration::ZERO)
            }
        }
        "time_starttransfer" => secs(t.ttfb.unwrap_or_default()),
        "time_total" => secs(f.total),
        "http_code" => f.status.to_string(),
        "http_version" => f.version.clone(),
        "num_connects" => t.connects.to_string(),
        // One head per hop, so the redirects followed are one fewer. A
        // hop that failed before a head is not counted, which is right:
        // it was not a redirect, it was a failure.
        "num_redirects" => t.heads.saturating_sub(1).to_string(),
        "remote_ip" => t.remote.map(|a| a.ip().to_string()).unwrap_or_default(),
        "remote_port" => t.remote.map(|a| a.port().to_string()).unwrap_or_default(),
        "size_download" => f.size_download.to_string(),
        "url_effective" => f.url.clone(),
        _ => return None,
    })
}

/// `HTTP/1.1`, `HTTP/2`, `HTTP/3` — curl's `%{http_version}` spelling.
///
/// `http::Version`'s own `Debug` gives `HTTP/1.1` and `HTTP/2.0`, and the
/// second is not what curl prints or what anyone writes: the protocol is
/// `HTTP/2`, with no minor number since RFC 7540.
pub fn version_name(v: http::Version) -> &'static str {
    match v {
        http::Version::HTTP_09 => "0.9",
        http::Version::HTTP_10 => "1.0",
        http::Version::HTTP_11 => "1.1",
        http::Version::HTTP_2 => "2",
        http::Version::HTTP_3 => "3",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timings() -> Timings {
        Timings {
            dns: Duration::from_millis(10),
            tcp: Duration::from_millis(20),
            tls: Some(Duration::from_millis(40)),
            ttfb: Some(Duration::from_millis(95)),
            connects: 1,
            heads: 1,
            remote: Some("93.184.216.34:443".parse().unwrap()),
            tls_version: Some("TLSv1.3".into()),
            tls_cipher: Some("TLS_AES_256_GCM_SHA384".into()),
            alpn: Some(b"h2".to_vec()),
        }
    }

    fn facts() -> Facts {
        Facts {
            status: 200,
            version: "2".into(),
            url: "https://example.com/after".into(),
            size_download: 1234,
            total: Duration::from_millis(150),
        }
    }

    /// The phases are **cumulative from the start**, which is curl's
    /// definition and the thing a reader is most likely to get wrong: the
    /// numbers are milestones on one timeline, not durations of phases.
    #[test]
    fn the_time_variables_are_cumulative_milestones_not_phase_durations() {
        let out = render(
            "%{time_namelookup} %{time_connect} %{time_appconnect} %{time_starttransfer} %{time_total}",
            &timings(),
            &facts(),
        )
        .unwrap();
        assert_eq!(
            out, "0.010000 0.030000 0.070000 0.095000 0.150000",
            "10, 10+20, 10+20+40, ttfb, total"
        );
    }

    /// Over plain `http://` there is no handshake, so `time_appconnect` is
    /// zero — curl's convention, and the reason `num_connects` exists
    /// beside it: a reader who must tell *no handshake* from *an instant
    /// handshake* has a field that says which.
    #[test]
    fn without_tls_the_appconnect_milestone_is_zero_and_not_the_connect_one() {
        let mut t = timings();
        t.tls = None;
        let out = render("%{time_connect} %{time_appconnect}", &t, &facts()).unwrap();
        assert_eq!(out, "0.030000 0.000000");
    }

    /// A wholly pooled exchange emits no `Connected` at all, so every
    /// connect milestone is zero and `num_connects` is what says why.
    #[test]
    fn a_pooled_exchange_reports_zero_connects_and_zero_connect_time() {
        let t = Timings {
            ttfb: Some(Duration::from_millis(5)),
            heads: 1,
            ..Default::default()
        };
        let out = render(
            "%{time_connect}|%{num_connects}|%{time_starttransfer}",
            &t,
            &facts(),
        )
        .unwrap();
        assert_eq!(out, "0.000000|0|0.005000");
    }

    /// One head per hop, so the redirects followed are one fewer — and a
    /// single-hop request reports zero rather than underflowing.
    #[test]
    fn redirects_are_heads_minus_one_and_never_underflow() {
        let mut t = timings();
        t.heads = 3;
        assert_eq!(render("%{num_redirects}", &t, &facts()).unwrap(), "2");
        t.heads = 1;
        assert_eq!(render("%{num_redirects}", &t, &facts()).unwrap(), "0");
        t.heads = 0;
        assert_eq!(render("%{num_redirects}", &t, &facts()).unwrap(), "0");
    }

    #[test]
    fn the_non_timing_variables_read_off_the_response() {
        let out = render(
            "%{http_code} %{http_version} %{size_download} %{url_effective} %{remote_ip} %{remote_port}",
            &timings(),
            &facts(),
        )
        .unwrap();
        assert_eq!(
            out,
            "200 2 1234 https://example.com/after 93.184.216.34 443"
        );
    }

    /// **The refusal, and the reason it is one.** A script asking for a
    /// variable this build has not got would otherwise be handed the
    /// characters back and report a timing that never happened.
    #[test]
    fn an_unknown_variable_is_refused_by_name_and_lists_what_exists() {
        let e = render("%{time_pretransfer}", &timings(), &facts()).unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("time_pretransfer"), "{msg}");
        assert!(msg.contains("time_total"), "it lists what it knows: {msg}");
    }

    /// An unterminated `%{` is refused too — silently treating it as
    /// literal text is the same defect one character earlier.
    #[test]
    fn an_unterminated_variable_is_refused() {
        let e = render("%{time_total", &timings(), &facts()).unwrap_err();
        assert!(e.to_string().contains("unterminated"), "{e}");
    }

    #[test]
    fn escapes_and_literals_behave_as_curls_do() {
        let f = facts();
        let t = timings();
        assert_eq!(render(r"a\nb", &t, &f).unwrap(), "a\nb");
        assert_eq!(render(r"a\tb\rc", &t, &f).unwrap(), "a\tb\rc");
        // `%%` is a literal percent...
        assert_eq!(render("100%%", &t, &f).unwrap(), "100%");
        // ...and a bare `%` is itself, so a hand-written format string
        // containing a percent sign is not an error.
        assert_eq!(render("50% done", &t, &f).unwrap(), "50% done");
        // A backslash before anything else is that character, so a path
        // does not have to be doubled.
        assert_eq!(render(r"C:\path", &t, &f).unwrap(), "C:path");
    }

    /// Every name in [`KNOWN`] renders, and the list is not aspirational.
    /// Checked by driving it rather than by reading it — a name added to
    /// the list and not to `value` would otherwise be advertised in the
    /// refusal message and then refused.
    #[test]
    fn every_advertised_variable_actually_renders() {
        for name in KNOWN {
            let fmt = format!("%{{{name}}}");
            assert!(
                render(&fmt, &timings(), &facts()).is_ok(),
                "`{name}` is listed as known and does not render"
            );
        }
    }

    /// The recorder keeps the **first** connection's numbers, because
    /// `time_connect` is about reaching the origin the caller named — a
    /// redirect's second connection would overwrite it with an unrelated
    /// one.
    #[test]
    fn a_second_connection_does_not_overwrite_the_firsts_timings() {
        use hclient_core::unversioned::{ConnectTiming, Connected, Event};
        let rec = Recorder::new();
        let uri: http::Uri = "https://example.com/".parse().unwrap();
        for tcp in [7u64, 999] {
            rec.on(Event::Connected(
                Connected::new(
                    hclient_core::unversioned::ConnectionId::UNWATCHED,
                    &uri,
                    http::Version::HTTP_11,
                )
                .timing(
                    ConnectTiming::new()
                        .dns(Duration::from_millis(1))
                        .tcp(Duration::from_millis(tcp))
                        .total(Duration::from_millis(tcp + 1)),
                )
                .remote(Some("1.2.3.4:443".parse().unwrap())),
            ));
        }
        let t = rec.snapshot();
        assert_eq!(t.tcp, Duration::from_millis(7), "the first, not the last");
        assert_eq!(t.connects, 2, "and both are counted");
    }
}
