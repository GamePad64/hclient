# system-resolver

Ask the operating system's own DNS resolver for any record type.

`getaddrinfo` only returns A and AAAA. This returns whatever you ask for:
HTTPS/SVCB, CAA, TLSA, anything. It calls the platform's own resolver API,
so the answer comes from the same place and under the same configuration
as every other lookup on the machine.

```toml
[dependencies]
system-resolver = "0.1"
```

```rust
// RR type 65 is HTTPS (RFC 9460).
for record in system_resolver::lookup("cloudflare.com", 65)? {
    println!("{} ttl {:?} rdata {} bytes", record.name, record.ttl, record.rdata.len());
}
```

## Why not a resolver crate

`hickory-resolver` and the c-ares bindings are resolvers: they read
`/etc/resolv.conf` and send their own queries to the servers listed there.
That misses anything configured elsewhere, such as a VPN's split DNS,
per-interface servers on Windows, macOS supplemental resolvers or Android's
Private DNS, and it does not use the system cache.

This crate sends nothing. No socket, no cache, no retries, no config
parsing. Whatever your machine already does still happens, because the
machine is what answers.

**On macOS the trap is one API call away, and it was measured rather than
feared.** `resolver(5)` describes macOS as running several DNS clients
with a "Super" meta-client routing between them by best domain match, and
libc's own `res_query` is documented against the *primary* client alone —
so it misses the per-domain files in `/etc/resolver/` and a VPN's
split-DNS zone. Asked for a `.local` name on a Mac, which every machine
has through mDNS: `res_query` fails with rcode 3 from a nameserver that
has never heard of `.local`, and `DNSServiceQueryRecord` answers. An
ordinary unicast name is the control and both answer it. This crate calls
the second.

## Platforms

| target | API used |
|---|---|
| Linux (glibc, musl) | `res_query` |
| macOS, iOS | `DNSServiceQueryRecord` |
| Android 29+ | `android_res_nquery` |
| FreeBSD | `res_query` |
| Windows 11 | `DnsQueryRaw` |
| Windows 10 | `DnsQuery_UTF8` |

Anything else compiles and returns `Error::Unsupported`.

## Limits

Not every platform can be asked for everything. `support().allows(rtype)`
answers before you spend a query, and `lookup` returns
`Error::UnsupportedType` naming the type rather than guessing.

- musl cannot pass a type number above 255, so `CAA` (257) and `URI` (256)
  are unavailable there.
- Windows 10 parses 43 record types into structs before the crate can see
  them. 26 are converted back to RDATA; 16 are refused by name.
- Apple reports "no such name" and "no such record" with one code, so
  `Error::NameDoesNotExist` is unreachable there: a name that does not
  exist and a name with no record of that type are the same answer.
- **Two platforms hand back records rather than a message, and lose the
  header with it.** Apple and Windows 10 answer one record's RDATA at a
  time, so there is no `TC` and no rcode. Linux, FreeBSD, Android and
  Windows 11 hand over the whole message, which this crate walks, so both
  reach you — as `Error::Truncated` and `Error::ResponseCode`.
- No platform exposes the `AD` bit.
- It blocks. Call it from a blocking thread pool.
- FreeBSD is compiled and type-checked on every push, but has never been
  run on FreeBSD.

## Record data

`Record::rdata` is the raw RDATA bytes. This crate does not decode them,
because that would mean a type per RFC and most callers want one of them.

Use a decoder that takes a record type and a byte slice: `hickory-proto`'s
`RData::read` or `domain`'s `AllRecordData::parse_rdata`. Names inside
RDATA are expanded before you get them, so a bare field can be decoded
outside the message it arrived in.

## Testing

`cargo test` runs everything that needs no network. The tests that need a
name server are `#[ignore]`d:

```
cargo test -- --ignored
```

## Minimum Rust version

1.96.

## License

MIT or Apache-2.0, at your option.
