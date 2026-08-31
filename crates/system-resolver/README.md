# system-resolver

**Ask the machine's own resolver for any DNS record type — Linux, macOS,
iOS, Android and Windows.**

```rust
// RR type 65, HTTPS (RFC 9460 §14.1).
for record in system_resolver::lookup("cloudflare.com", 65)? {
    println!("{} ttl {:?} rdata {} bytes", record.name, record.ttl, record.rdata.len());
}
```

## Why this and not a resolver

It is **not** a resolver: it sends no queries of its own, keeps no cache,
implements no retry and validates no signatures. Everything it does, the
platform does.

That is the whole point. `hickory-resolver` is an excellent resolver and it
is *its own* — it reads `/etc/resolv.conf` and talks to the servers named
there, which is the wrong answer wherever the interesting configuration is
not in that file: a VPN's split DNS, a corporate zone, Android's Private
DNS, Windows' per-interface servers. `dns-lookup` reaches the platform and
offers what `getaddrinfo` does, which is A and AAAA.

The gap is narrow and real: **the platform's answers, for types the
platform's convenience API will not return** — `HTTPS`, `SVCB`, `TLSA`,
`CAA`, `SSHFP`, `OPENPGPKEY`, and whatever the registry adds next.

## What each platform can be asked

| platform | call | can be asked for |
|---|---|---|
| Linux (glibc, musl) | `res_query` | any type |
| macOS, iOS | `res_9_query` | any type |
| Android ≥ 29 | `android_res_nquery` | any type |
| Windows | `DnsQuery_UTF8` | any type **except** the 43 the OS parses |

`SUPPORT` answers it, and `SUPPORT.allows(rtype)` answers it for one type.
A type this build cannot answer is refused **by name and before a query**,
never guessed at.

### The Windows exception, which is the only interesting row

`DnsQuery_UTF8` fills in a `DNS_RECORD` whose data union carries no
discriminator. Which member is live is decided by the type, and the rule is
readable from the Win32 metadata: a type the union **names** arrives as
that structure, and a type it does not name arrives as the record's own
**RDATA**. `DNS_TYPE_SVCB` is 64; `HTTPS` is 65 and the union names no
member for it — so `HTTPS` works here exactly as it does everywhere else,
and so do `CAA`, `CERT`, `LOC` and most of the registry.

Measured rather than read, on Windows 11: type 65 for `cloudflare.com`
comes back with `wDataLength = 61` and bytes that are SVCB wire format,
byte-for-byte the RDATA that Linux's `res_query` reports for the same name.
`MX` came back as a structure and `CAA` as RDATA, which is the rule
separating in both directions.

`DnsQueryRaw` would hand over the whole wire message and widen Windows to
*any type*. It exists only on Windows 11 / Server 2025, so taking it means
resolving the symbol at run time; that is the next step for this crate and
it is not taken yet.

## What it deliberately does not do

- **No resolver of its own** — then it would be `hickory`, which exists.
- **No caching** — the platform's is the point, and a second one would
  answer from a different clock.
- **No decoding of RDATA.** `Record::rdata` is bytes and stays bytes.
  Interpreting them belongs beside the consumer; a crate that decoded
  records would need a type per RFC and would make every consumer wait for
  the one it needs.
- **No async API.** Every platform call blocks. An async wrapper over a
  blocking call is a thread pool with an opinion, and a caller has one.
- **No DNSSEC validation**, and no `AD` bit: reporting it would mean
  carrying the header, and the header is what Windows cannot hand over.

## Records, not the message

Four of the five platforms could hand over the whole message. Windows
cannot — it has already taken it apart — so making the message the common
type would mean synthesising one there out of records that carry no header,
inventing an rcode and flags nobody reported.

What is lost is the header: `AA`, `TC` and the rcode as such. A caller that
needs to tell `NXDOMAIN` from an empty answer gets that from `Error`, where
`Error::NameDoesNotExist` and `Ok(vec![])` are different values.

## Testing

```
cargo nextest run -p system-resolver                     # rules only, no network
cargo nextest run -p system-resolver --run-ignored all   # and four live lookups
```

Everything except the four live tests is a pure function over bytes and
runs on any host, including the wire-message walker — which is the only
code here that reads bytes it did not write.
