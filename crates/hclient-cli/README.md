# hclient-cli — `hc`

An HTTP command-line client, with httpie's request grammar and the TLS
backend chosen at runtime rather than at build time.

```
cargo install hclient-cli --version 0.1.0-alpha.2
```

The version is needed while the only releases are pre-releases: a bare
`cargo install hclient-cli` resolves `*`, which does not match one, and
reports "could not find `hclient-cli` in registry".

```
hc example.com                          # GET
hc example.com name=alice               # POST, because there is a body
hc -v https://example.com               # show the request and the handshake
hc --backend native-tls https://ex.com  # pick the TLS stack at runtime
```

## Choosing a backend

curl supports several TLS backends, but they are chosen when the binary is
built. Only a `MultiSSL` build honours `CURL_SSL_BACKEND`, most
distributions do not ship one, and curl's man page says an unknown name
"makes curl stay with the default" — silently.

`hc --version` prints what this binary carries, and naming a backend it
does not have is an error with its own exit code, listing the ones it has.

## Request items

httpie's grammar, copied rather than invented, because `xh` uses the same
one and a third spelling would make every example on the internet wrong.

| form | means |
|---|---|
| `name=value` | a data field: a JSON string, or a form field under `-f` |
| `name:=value` | a JSON value: `n:=42`, `xs:=[1,2]`, `ok:=true` |
| `name==value` | a query parameter, appended to the URL's own |
| `name:value` | a request header; `name:` alone removes one |
| `name@path` | a file, as a multipart part |

The method is optional. `hc example.com` is a GET; adding a data item
makes it a POST.

## Streaming

`--sse` reads a server-sent event stream and prints one event per line,
reconnecting with backoff when the server drops it. `--ws` opens a
WebSocket and pipes stdin to it and it to stdout.

Both refuse flags they cannot honour by name rather than ignoring them:
`--sse` has no request body, no redirect policy and no `--http`, because
the SSE builder does not carry them.

## Exit codes

Distinct on purpose, so a script can tell a refused backend from an
unreachable server.

| code | meaning |
|---|---|
| 0 | the request completed |
| 2 | the command line is wrong |
| 3 | the named backend, or `--ws`, is not in this build |
| 4 | the request failed |
| 5 / 6 | `--check-status` and a 4xx / 5xx |
| 7 | an I/O failure |

## Licence

MIT or Apache-2.0, at your option.
