//! Turning a command line into one request, and one response into output.

use crate::args::{BackendName, Cli, HttpVersion, Item, Print};
use crate::{backend, output};
use std::io::Write;

#[derive(Debug)]
pub enum Fail {
    /// Something the caller wrote is wrong. Exit 2, like clap's own.
    Usage(String),
    /// A backend was named and this build has not got it. Exit 3, so a
    /// script can tell it from a network failure — which is the whole
    /// reason the refusal exists.
    Backend(backend::Refused),
    /// The request did not complete.
    Request(hclient::Error),
    /// `--check-status` and the server answered 4xx or 5xx.
    Status(http::StatusCode),
    /// A `--write-out` format naming a variable this build does not know.
    ///
    /// Its own code, and **exit 2** rather than a new one: it is a
    /// mistake in what the caller wrote, which is what 2 already means,
    /// and it is caught after the request rather than before only because
    /// that is when the values exist.
    WriteOut(crate::timings::Unknown),
    Io(std::io::Error),
}

impl Fail {
    pub fn code(&self) -> i32 {
        match self {
            Self::Usage(_) => 2,
            Self::Backend(_) => 3,
            Self::Request(_) => 4,
            Self::Status(s) if s.is_client_error() => 5,
            Self::Status(_) => 6,
            Self::WriteOut(_) => 2,
            Self::Io(_) => 7,
        }
    }
}

impl std::fmt::Display for Fail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Usage(m) => write!(f, "{m}"),
            Self::Backend(r) => write!(f, "{r}"),
            Self::WriteOut(u) => write!(f, "{u}"),
            Self::Request(e) => {
                write!(f, "{e}")?;
                // The chain, because this library's errors carry a typed
                // source and the top line alone often says only "connect".
                // A cause whose text the line above already contains is
                // dropped: this library's top-level `Display` often
                // includes its source, and printing it twice reads as two
                // failures rather than one.
                let top = e.to_string();
                let mut src = std::error::Error::source(e);
                while let Some(s) = src {
                    let text = s.to_string();
                    if !top.contains(&text) {
                        write!(f, "\n  caused by: {text}")?;
                    }
                    src = s.source();
                }
                Ok(())
            }
            Self::Status(s) => write!(f, "response status {s}"),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

/// A URL is anything that is not a method. Methods are recognised by the
/// registered set rather than by "is it uppercase", because `PATCH` and a
/// host named `PATCH` are distinguishable only by that list, and a bare
/// uppercase word is far more likely to be a typo the caller wants named.
fn is_method(s: &str) -> bool {
    matches!(
        s.to_ascii_uppercase().as_str(),
        "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD" | "OPTIONS" | "TRACE" | "CONNECT"
    )
}

/// `example.com/x` -> `http://example.com/x`, and `:8080/x` ->
/// `http://localhost:8080/x`.
///
/// The scheme default is **http**, which is httpie's and curl's, and it is
/// worth saying why it is not https: a tool that silently upgraded would
/// make `hc http://localhost:8080` and `hc localhost:8080` behave
/// differently from every other client on the machine, and the second is
/// how a developer reaches their own server.
fn normalise_url(raw: &str) -> String {
    if raw.starts_with("http://") || raw.starts_with("https://") {
        return raw.to_owned();
    }
    if let Some(rest) = raw.strip_prefix(':') {
        // `:8080/path` and `:/path`.
        let rest = rest
            .strip_prefix('/')
            .map_or_else(|| format!("localhost:{rest}"), |p| format!("localhost/{p}"));
        return format!("http://{rest}");
    }
    format!("http://{raw}")
}

struct Parsed {
    method: Option<http::Method>,
    url: String,
    items: Vec<Item>,
}

fn split_positionals(cli: &Cli) -> Result<Parsed, Fail> {
    let mut all = Vec::with_capacity(cli.rest.len() + 1);
    all.push(cli.first.clone());
    all.extend(cli.rest.iter().cloned());

    let mut it = all.into_iter();
    let first = it
        .next()
        .ok_or_else(|| Fail::Usage("no URL given".into()))?;
    let (method, url) = if is_method(&first) {
        let url = it.next().ok_or_else(|| {
            Fail::Usage(format!("`{first}` is a method, so a URL has to follow it"))
        })?;
        (
            Some(
                http::Method::try_from(first.to_ascii_uppercase().as_str())
                    .map_err(|_| Fail::Usage(format!("`{first}` is not a usable method")))?,
            ),
            url,
        )
    } else {
        (None, first)
    };

    let mut items = Vec::new();
    for a in it {
        items.push(crate::args::parse_item(&a).map_err(|e| Fail::Usage(e.to_string()))?);
    }
    Ok(Parsed {
        method,
        url: normalise_url(&url),
        items,
    })
}

/// `HOST:PORT:ADDR` or `HOST:ADDR`, curl's spelling.
///
/// The port is **accepted and dropped**, and that is stated in the flag's
/// help rather than left to be discovered: `Resolve` is asked for a name
/// and a family and carries no port, so a per-port override would be keyed
/// on something the seam cannot see. Accepting curl's three-part form
/// anyway is what lets a command line be copied across unchanged.
fn parse_resolve(spec: &str) -> Result<(String, std::net::IpAddr), Fail> {
    let bad = |why: String| Fail::Usage(format!("`--resolve {spec}`: {why}"));

    // The address comes last, and an IPv6 one is **bracketed** — curl
    // writes `example.com:443:[::1]`, which is RFC 3986's rule for an
    // address that has to sit beside a port. Reading the brackets is what
    // makes the rest of the split unambiguous: without them a bare `::1`
    // is indistinguishable from a host with empty labels.
    let (head, addr_text) = if let Some(open) = spec.rfind('[') {
        if !spec.ends_with(']') {
            return Err(bad(
                "an IPv6 address opens with `[` and must close with `]`".into(),
            ));
        }
        (
            spec[..open].trim_end_matches(':'),
            &spec[open + 1..spec.len() - 1],
        )
    } else {
        let (h, a) = spec
            .rsplit_once(':')
            .ok_or_else(|| bad("expected HOST:ADDRESS, or curl's HOST:PORT:ADDRESS".into()))?;
        (h, a)
    };

    let addr: std::net::IpAddr = addr_text
        .parse()
        .map_err(|_| bad(format!("`{addr_text}` is not an IP address")))?;

    // What is left is `HOST` or curl's `HOST:PORT`. The port is dropped
    // rather than refused, so a command line can be copied from curl
    // unchanged — and dropping it is honest because `Resolve` is asked for
    // a name and a family and carries no port at all, which the flag's own
    // help says.
    let host = head.rsplit_once(':').map_or(head, |(h, p)| {
        if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) {
            h
        } else {
            head
        }
    });
    if host.is_empty() {
        return Err(bad("no host before the address".into()));
    }
    Ok((host.to_owned(), addr))
}

/// What to print, from three flags that all mean it.
fn print_selection(cli: &Cli, is_tty: bool) -> Result<Print, Fail> {
    if let Some(s) = &cli.print {
        return Print::parse(s)
            .map_err(|c| Fail::Usage(format!("`--print {s}`: `{c}` is not one of H, B, h, b")));
    }
    if cli.headers {
        return Ok(Print {
            response_head: true,
            ..Print::default()
        });
    }
    if cli.body {
        return Ok(Print {
            response_body: true,
            ..Print::default()
        });
    }
    if cli.verbose {
        return Ok(Print {
            request_head: true,
            request_body: true,
            response_head: true,
            response_body: true,
        });
    }
    // The default depends on where the output goes, which is httpie's rule
    // and the one that makes `hc … | jq` work without a flag: a pipe gets
    // the body alone, a terminal gets the headers too.
    Ok(if is_tty {
        Print {
            response_head: true,
            response_body: true,
            ..Print::default()
        }
    } else {
        Print {
            response_body: true,
            ..Print::default()
        }
    })
}

pub async fn run(cli: Cli, is_tty: bool, colour: anstream::ColorChoice) -> Result<(), Fail> {
    let parsed = split_positionals(&cli)?;
    let print = print_selection(&cli, is_tty)?;

    let mut resolve = Vec::new();
    for spec in &cli.resolve {
        resolve.push(parse_resolve(spec)?);
    }
    // The clock starts before the client is built, because `time_total`
    // is the caller's wall time for the whole operation and a trust-store
    // read is part of it.
    let started = std::time::Instant::now();
    let recorder = cli
        .write_out
        .as_ref()
        .map(|_| crate::timings::Recorder::new());
    let client = backend::build(
        cli.backend,
        &backend::Config {
            insecure: cli.insecure,
            resolve,
            total: cli.timeout.map(std::time::Duration::from_secs_f64),
            timings: recorder.clone(),
        },
    )
    .map_err(Fail::Backend)?;

    // Split the items by kind before choosing a method: whether there is a
    // body is what decides GET against POST.
    let mut headers: Vec<(String, String)> = Vec::new();
    let mut query: Vec<(String, String)> = Vec::new();
    let mut data: Vec<(String, String)> = Vec::new();
    let mut raw_json: Vec<(String, String)> = Vec::new();
    let mut files: Vec<(String, String)> = Vec::new();
    for item in parsed.items {
        match item {
            Item::Header { name, value } => headers.push((name, value)),
            Item::Query { name, value } => query.push((name, value)),
            Item::Data { name, value } => data.push((name, value)),
            Item::JsonRaw { name, value } => raw_json.push((name, value)),
            Item::File { name, path } => files.push((name, path)),
        }
    }

    let has_body =
        !data.is_empty() || !raw_json.is_empty() || !files.is_empty() || cli.raw_body.is_some();
    let method = parsed.method.unwrap_or(if has_body {
        http::Method::POST
    } else {
        http::Method::GET
    });

    let mut req = client.request(method.clone(), &parsed.url);
    for (k, v) in &query {
        req = req.query([(k, v)]);
    }
    // The effective header set is decided BEFORE anything is applied,
    // rather than by adding a default and taking it away again — the
    // builder has no remover, and a set computed first is also the only
    // way `User-Agent:` (empty) can suppress a default that would
    // otherwise already be in the request.
    for (name, value) in effective_headers(&headers) {
        req = req.header(&name, &value);
    }

    // The body. Four shapes and they are mutually exclusive, so a caller
    // who asked for two gets told rather than getting whichever the code
    // happened to check first.
    let mut request_body_preview: Option<Vec<u8>> = None;
    if files.is_empty() {
        if let Some(path) = &cli.raw_body {
            if has_data(&data, &raw_json) {
                return Err(Fail::Usage(
                    "`--raw-body` and data items both set the request body; use one".into(),
                ));
            }
            let bytes = read_body(path).map_err(Fail::Io)?;
            request_body_preview = Some(bytes.clone());
            req = req.body(hclient_core::RequestBody::Full(bytes::Bytes::from(bytes)));
        } else if cli.form {
            let pairs: Vec<(&str, &str)> =
                data.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
            if !raw_json.is_empty() {
                return Err(Fail::Usage(
                    "`:=` items are JSON and cannot go into a `--form` body".into(),
                ));
            }
            req = req.form(pairs);
        } else if has_data(&data, &raw_json) {
            let mut obj = serde_json::Map::new();
            for (k, v) in &data {
                obj.insert(k.clone(), serde_json::Value::String(v.clone()));
            }
            for (k, v) in &raw_json {
                // Already validated when the item was parsed, so a failure
                // here is impossible rather than unhandled.
                let parsed: serde_json::Value = serde_json::from_str(v)
                    .expect("`:=` values are validated by `parse_item` before reaching here");
                obj.insert(k.clone(), parsed);
            }
            let value = serde_json::Value::Object(obj);
            request_body_preview = serde_json::to_vec(&value).ok();
            req = req.json(&value);
        }
    } else {
        let mut form = hclient::multipart::Form::new();
        for (k, v) in &data {
            form = form.part(hclient::multipart::Part::text(k.clone(), v.clone()));
        }
        for (name, path) in &files {
            let bytes = std::fs::read(path).map_err(Fail::Io)?;
            let file_name = std::path::Path::new(path)
                .file_name()
                .map_or_else(|| path.clone(), |f| f.to_string_lossy().into_owned());
            form = form
                .part(hclient::multipart::Part::bytes(name.clone(), bytes).file_name(file_name));
        }
        req = req.multipart(form);
    }

    if let Some(spec) = &cli.auth {
        let (user, pass) = spec.split_once(':').unwrap_or((spec.as_str(), ""));
        req = if cli.digest {
            req.digest_auth(user, pass)
        } else {
            req.basic_auth(user, pass)
        };
    }
    if let Some(token) = &cli.bearer {
        req = req.bearer_auth(token);
    }
    if let Some(v) = cli.http {
        // A demand rather than a preference, which is the honest shape:
        // `Capabilities` report the **floor**, so there is no capability a
        // caller can ask whether HTTP/2 will be used. A transport that
        // cannot select refuses this instead of ignoring it.
        req = req.require_version(match v {
            HttpVersion::Http11 => http::Version::HTTP_11,
            HttpVersion::Http2 => http::Version::HTTP_2,
            HttpVersion::Http3 => http::Version::HTTP_3,
        });
    }
    if cli.follow {
        req = req.redirect(hclient::redirect::RedirectPolicy::Limited(
            cli.max_redirects,
        ));
    }

    let mut out = anstream::AutoStream::new(std::io::stdout().lock(), colour);
    if print.request_head {
        let uri: http::Uri = parsed
            .url
            .parse()
            .map_err(|e| Fail::Usage(format!("`{}` is not a URL: {e}", parsed.url)))?;
        // The SAME set the request was built from, not the caller's raw
        // items: a printed head that omits the `User-Agent` actually sent
        // is a diagnostic that lies, which is worse than no `--print` at
        // all.
        let mut hm = http::HeaderMap::new();
        // The `Content-Type` this program is about to cause is added to
        // the printed set, because the builder sets it and the caller
        // never typed it — a `--print H` that showed a JSON body under no
        // content type would be misleading in the one place it matters.
        let implied_ct = if !files.is_empty() {
            None // the boundary is drawn inside the builder; see below
        } else if cli.form {
            Some("application/x-www-form-urlencoded")
        } else if request_body_preview.is_some() && cli.raw_body.is_none() {
            Some("application/json")
        } else {
            None
        };
        if let Some(ct) = implied_ct
            && !headers
                .iter()
                .any(|(n, _)| n.eq_ignore_ascii_case("content-type"))
        {
            hm.insert(
                http::header::CONTENT_TYPE,
                http::HeaderValue::from_static(ct),
            );
        }
        for (k, v) in &effective_headers(&headers) {
            if let (Ok(n), Ok(val)) = (
                http::HeaderName::try_from(k.as_str()),
                http::HeaderValue::try_from(v.as_str()),
            ) {
                hm.insert(n, val);
            }
        }
        output::request_head(&mut out, &method, &uri, &hm).map_err(Fail::Io)?;
    }
    if print.request_body
        && let Some(b) = &request_body_preview
    {
        output::body(&mut out, Some("application/json"), b).map_err(Fail::Io)?;
        writeln!(out).map_err(Fail::Io)?;
    }

    let response = req.send().await.map_err(Fail::Request)?;
    let version = response.version();
    let status = response.status();
    // The URL that **answered**, which differs from the one asked for
    // exactly when a redirect was followed — read here because `collect`
    // takes the response by value.
    let url_effective = response.url().clone();
    let collected = response.collect().await.map_err(Fail::Request)?;

    if print.response_head {
        output::response_head(&mut out, version, status, collected.headers()).map_err(Fail::Io)?;
    }
    if print.response_body {
        let ct = collected
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(ToOwned::to_owned);
        output::body(&mut out, ct.as_deref(), collected.bytes()).map_err(Fail::Io)?;
    }
    // `--write-out` prints **after** the body and to stdout, which is
    // curl's placement. It is a surprise worth knowing about — a piped
    // body gets the report appended — and matching curl is worth more
    // than fixing it here, since every script that already handles it
    // handles it that way.
    if let Some(fmt) = &cli.write_out {
        let t = recorder
            .as_ref()
            .map(crate::timings::Recorder::snapshot)
            .unwrap_or_default();
        let facts = crate::timings::Facts {
            status: status.as_u16(),
            version: crate::timings::version_name(version).to_owned(),
            url: url_effective.to_string(),
            size_download: collected.bytes().len() as u64,
            total: started.elapsed(),
        };
        let rendered = crate::timings::render(fmt, &t, &facts).map_err(Fail::WriteOut)?;
        write!(out, "{rendered}").map_err(Fail::Io)?;
    }
    out.flush().map_err(Fail::Io)?;

    if cli.check_status && (status.is_client_error() || status.is_server_error()) {
        return Err(Fail::Status(status));
    }
    Ok(())
}

fn has_data(data: &[(String, String)], raw: &[(String, String)]) -> bool {
    !data.is_empty() || !raw.is_empty()
}

fn read_body(path: &str) -> std::io::Result<Vec<u8>> {
    if path == "-" {
        use std::io::Read as _;
        let mut buf = Vec::new();
        std::io::stdin().read_to_end(&mut buf)?;
        Ok(buf)
    } else {
        std::fs::read(path)
    }
}

/// `hc --version`: the build's backends, which is the thing this tool is
/// for and the thing curl's `--version` does not let you change.
pub fn print_version(out: &mut impl Write) -> std::io::Result<()> {
    writeln!(out, "hc {}", env!("CARGO_PKG_VERSION"))?;
    writeln!(out, "backends: {}", backend::available_list())?;
    writeln!(
        out,
        "default:  {}",
        backend::default_backend().map_or_else(|| "none".into(), |b: BackendName| b.to_string())
    )?;
    let mut protocols = vec!["http/1.1"];
    if cfg!(feature = "http2") {
        protocols.push("h2");
    }
    if cfg!(feature = "http3") {
        protocols.push("h3");
    }
    writeln!(out, "protocols: {}", protocols.join(", "))
}

/// The caller's header items, with a default `User-Agent` where they did
/// not name one.
///
/// Two rules, both httpie's. A later item **replaces** an earlier one of
/// the same name rather than adding a second — for `User-Agent` and
/// `Authorization` a duplicate is a request most servers reject, and a
/// caller writing the same name twice means the second. And an **empty
/// value drops** the header, which is how the default above is
/// suppressed; a header with a genuinely empty value is therefore not
/// expressible, which is httpie's behaviour too and is said in
/// `Item::Header`'s doc rather than left to be discovered.
fn effective_headers(items: &[(String, String)]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::with_capacity(items.len() + 1);
    // A client that sends no `User-Agent` is unusual enough that servers
    // and WAFs reject it, and both incumbents send one.
    out.push((
        "user-agent".into(),
        concat!("hc/", env!("CARGO_PKG_VERSION")).into(),
    ));
    for (name, value) in items {
        let at = out.iter().position(|(n, _)| n.eq_ignore_ascii_case(name));
        match (at, value.is_empty()) {
            (Some(i), true) => {
                out.remove(i);
            }
            (Some(i), false) => out[i] = (name.clone(), value.clone()),
            (None, true) => {}
            (None, false) => out.push((name.clone(), value.clone())),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_host_gets_http_and_a_leading_colon_gets_localhost() {
        assert_eq!(normalise_url("example.com/x"), "http://example.com/x");
        assert_eq!(normalise_url(":8080/x"), "http://localhost:8080/x");
        assert_eq!(normalise_url(":/x"), "http://localhost/x");
        // An explicit scheme is left exactly as written — including
        // `https`, which is why nothing here upgrades.
        assert_eq!(normalise_url("https://example.com"), "https://example.com");
        assert_eq!(
            normalise_url("http://localhost:1/x"),
            "http://localhost:1/x"
        );
    }

    #[test]
    fn a_method_is_recognised_from_the_registered_set_and_not_from_its_case() {
        assert!(is_method("get"));
        assert!(is_method("PATCH"));
        // A bare uppercase word is far more likely to be a typo than a
        // method, so it is not one.
        assert!(!is_method("FOO"));
        assert!(!is_method("example.com"));
    }

    #[test]
    fn resolve_accepts_curls_three_part_form_and_the_two_part_one() {
        assert_eq!(
            parse_resolve("example.com:8080:127.0.0.1").unwrap(),
            (
                "example.com".into(),
                "127.0.0.1".parse::<std::net::IpAddr>().unwrap()
            )
        );
        assert_eq!(
            parse_resolve("example.com:127.0.0.1").unwrap(),
            (
                "example.com".into(),
                "127.0.0.1".parse::<std::net::IpAddr>().unwrap()
            )
        );
    }

    #[test]
    fn an_ipv6_address_is_bracketed_the_way_curl_writes_it() {
        let v6 = "::1".parse::<std::net::IpAddr>().unwrap();
        assert_eq!(
            parse_resolve("example.com:[::1]").unwrap(),
            ("example.com".into(), v6)
        );
        assert_eq!(
            parse_resolve("example.com:443:[::1]").unwrap(),
            ("example.com".into(), v6)
        );
        // Unbracketed is refused rather than guessed at: a bare `::1` next
        // to a port cannot be told from a host with empty labels, which is
        // why RFC 3986 has the brackets in the first place.
        assert!(parse_resolve("example.com:::1").is_err());
        assert!(parse_resolve("example.com:[::1").is_err());
    }

    #[test]
    fn a_resolve_without_an_address_is_named_rather_than_ignored() {
        let e = parse_resolve("example.com:not-an-ip").unwrap_err();
        let Fail::Usage(m) = e else {
            panic!("expected a usage error")
        };
        assert!(m.contains("not an IP address"), "{m}");
    }

    #[test]
    fn a_default_user_agent_is_added_replaced_and_removable() {
        let none = effective_headers(&[]);
        assert_eq!(none.len(), 1);
        assert!(none[0].0.eq_ignore_ascii_case("user-agent"));

        let replaced = effective_headers(&[("User-Agent".into(), "mine/1".into())]);
        assert_eq!(
            replaced,
            vec![("User-Agent".to_string(), "mine/1".to_string())]
        );

        // The empty form is the only way to send no `User-Agent` at all,
        // which is what `Item::Header`'s doc promises.
        assert!(effective_headers(&[("User-Agent".into(), String::new())]).is_empty());
    }

    #[test]
    fn a_repeated_header_replaces_rather_than_duplicating() {
        // A duplicate `Authorization` is a request most servers reject,
        // and a caller who wrote the name twice meant the second.
        let out = effective_headers(&[("X-A".into(), "1".into()), ("x-a".into(), "2".into())]);
        let xs: Vec<_> = out
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case("x-a"))
            .collect();
        assert_eq!(xs.len(), 1);
        assert_eq!(xs[0].1, "2");
    }

    #[test]
    fn each_failure_has_its_own_exit_code() {
        // A script has to be able to tell "this build has no such backend"
        // from "the server was unreachable", which is the whole reason the
        // refusal exists rather than a fallback.
        let codes = [
            Fail::Usage(String::new()).code(),
            Fail::Backend(crate::backend::Refused::NoneAtAll).code(),
            Fail::Status(http::StatusCode::NOT_FOUND).code(),
            Fail::Status(http::StatusCode::INTERNAL_SERVER_ERROR).code(),
        ];
        let mut sorted = codes.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            codes.len(),
            "exit codes must not collide: {codes:?}"
        );
        assert!(codes.iter().all(|c| *c != 0), "no failure may exit zero");
    }
}
