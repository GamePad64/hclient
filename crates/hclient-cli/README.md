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
