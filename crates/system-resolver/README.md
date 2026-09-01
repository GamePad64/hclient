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
  `NXDOMAIN` cannot be told apart from an empty answer.
- There is no message header, so no `AD` bit and no `TC`. You get records.
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
