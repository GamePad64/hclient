# hclient-cli — `hc`

An HTTP command-line client where **the backend is a flag, not a
packaging decision** — and where a backend this build has not got is
refused by name rather than silently replaced.

```
# While the only releases are pre-releases, the version is required:
# a bare `cargo install hclient-cli` resolves `*`, which does not match a
# pre-release, and reports **"could not find `hclient-cli` in registry"** —
# which reads as "no such crate" rather than "not a stable release yet".
cargo install hclient-cli --version 0.1.0-alpha.2

hc httpbin.org/get
hc POST api.example.com/users name=alice admin:=true
hc --backend native-tls https://internal.corp/    # the OS trust store
hc --sse https://example.com/events               # Server-Sent Events
```

## Why this exists

curl supports several TLS backends, **chosen when the binary was built**.
Only a `MultiSSL` build honours `CURL_SSL_BACKEND` at runtime, the stock
build on most distributions is not one, and curl's own man page says that
setting a name it does not have *"makes curl stay with the default"*.

So the honest framing is not that curl cannot do this:

- curl *can*, in a build almost nobody has;
- when it cannot, it says nothing;
- and the choice belongs to whoever packaged the binary rather than to
  whoever runs it.

`hc --version` prints what this binary carries. Naming one it does not
carry is an error with its own exit code, listing what it has.

That is possible because `hclient::Client` names no type parameters:
every backend arm returns the same `Client`, so `--backend` is an
ordinary `match` and the rest of the tool is written once.

## Request items

httpie's grammar, copied rather than invented — `xh` implements the same
one, and a third spelling would make every example on the internet wrong
for this tool.

| form | means |
|---|---|
| `name=value` | a data field — a JSON string, or a form field under `-f` |
| `name:=value` | a JSON value: `n:=42`, `xs:=[1,2]`, `ok:=true` |
| `name==value` | a query parameter, appended to the URL's own |
| `name:value` | a request header; `name:` alone **removes** one |
| `name@path` | a file, as a multipart part |

The method is optional: `hc example.com` is a GET, and
`hc example.com name=alice` is a POST, because there is a body.

## Flags worth knowing

- `--backend rustls|native-tls` — see above.
- `-k`, `--insecure` — do not verify the server's certificate. For a host
  whose identity you are establishing some other way. It offers nothing
  against an active attacker.
- `--resolve HOST[:PORT]:ADDRESS` — send a name to an address of your
  choosing, so a certificate and a `Host` stay the name's while only the
  address moves. curl's three-part form is accepted; the port is ignored,
  because the resolver seam underneath is asked for a name and never for a
  port. IPv6 is bracketed: `--resolve api.test:443:[::1]`.
- `--print HBhb` — request head, request body, response head, response
  body. Without it a terminal gets the head and body and a pipe gets the
  body alone, so `hc … | jq` needs no flag.
- `--check-status` — exit non-zero on a 4xx or 5xx. Off by default,
  because about half the requests ever made have a status the caller
  wants to read rather than raise.
- `-L`, `--follow` — follow redirects, at most `--max-redirects` of them.
  **Off by default**, as in curl and httpie: without it a `3xx` is the
  answer and is printed as one.

## `--sse`: Server-Sent Events

It reads a URL as something that goes on rather than as one exchange, so
it prints until the stream ends and takes a narrower set of flags than a
request does — see below.

```
hc --sse https://example.com/events        # data lines, one per message
hc --sse -v https://example.com/events     # the events, re-serialised
hc --sse --sse-reconnect https://…/events  # reopen when the stream ends
```

**One connection unless `--sse-reconnect` is given.** A reconnect after a
clean end of stream sends a second request the caller asked for once, and
the reconnecting stream treats almost every failure as retryable — so it
turns errors into silence where a one-shot run turns them into an exit
code. `--sse-reconnect` reopens with jittered exponential backoff,
carries `Last-Event-ID`, and honours a server's own `retry:`.

A pipe gets `data` alone, one line per message, so `hc --sse … | jq`
needs no flag. A terminal — and `-v` anywhere — gets the event written
back in **SSE's own syntax**, `event:`/`id:`/`data:`, `: ` for a comment
and `retry:` for a retry, so the output can be diffed against the bytes
that arrived. `-b` forces the pipe's form.

### What it refuses

It is opened through a seam narrower than an ordinary request —
`Client::sse` carries a URL and headers and nothing else — so a flag whose
effect has nowhere to travel is **refused by name** (exit 2) rather than
accepted and dropped.

| refused with `--sse` | because |
|---|---|
| `--print`, `--headers` | they name parts of one exchange; a stream is not one |
| any non-header request item, `--raw-body`, `-f`, `-j` | the opening request carries no body |
| `-a`/`--auth`, `--digest` | Basic needs an encoder this binary has not got and Digest a `401` round trip; `--bearer` is carried, being one header |
| `--http` | there is nowhere to put a version demand |
| `--check-status` | a non-200 already fails the stream, so the flag could never fire |
| `-w`/`--write-out` | it reports one finished exchange, after its body |
| `--timeout` with `--sse-reconnect` | the two together cut and reopen for ever, which is a loop rather than a bound |

`--backend`, `-k`, `--resolve` and `--bearer` mean the same in both modes,
and `--follow` is honoured — the builder has no redirect setter, but a
`Client` does, so the effect has somewhere else to travel.

## Exit codes

Distinct on purpose, so a script can tell a refused backend from an
unreachable server.

| code | meaning |
|---|---|
| 0 | the request completed |
| 2 | the command line is wrong |
| 3 | the named backend is not in this build |
| 4 | the request failed |
| 5 / 6 | `--check-status` and a 4xx / 5xx |
| 7 | an I/O failure |

## Build-time features

`default = ["rustls", "native-tls"]` — both, so `--backend` has more than
one legal value; that is the whole point. `http2` and `http3` add the
protocols. A build with `--no-default-features` and no backend feature
says so rather than failing at the first request.

There is no `sse` feature: `hclient::sse` is unconditional in `hclient`,
so `--sse` is in every build and a feature would only be a way to switch
off code that is already linked.
