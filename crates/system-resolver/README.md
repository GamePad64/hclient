# system-resolver

**Ask the machine's own resolver for any DNS record type — Linux, macOS,
iOS, Android, FreeBSD and Windows.**

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
| Linux, glibc | `res_query` | any type |
| Linux, musl | `res_query` | any type **up to 255** — `CAA` and `URI` are above it |
| macOS, iOS | `DNSServiceQueryRecord` | any type |
| Android ≥ 29 | `android_res_nquery` | any type |
| FreeBSD | `res_query` | any type — compiled, never run |
| Windows 11 / Server 2025 | `DnsQueryRaw` | any type |
| older Windows | `DnsQuery_UTF8` | any type **except** 16 |

`support()` answers it, and `support().allows(rtype)` answers it for one
type. A type this build cannot answer is refused **by name and before a
query**, never guessed at.

It is a function rather than a constant for one reason: on Windows the
answer genuinely differs between two machines running the same binary.

### The Windows exception, which is the only interesting row

`DnsQueryRaw` hands over the wire message, so a Windows 11 behaves exactly
like the four platforms above. It is resolved with `GetProcAddress`,
because naming it as a linked symbol stops the binary from starting on a
Windows that lacks it.

`DnsQuery_UTF8`, the fallback, fills in a `DNS_RECORD` whose data union
carries no discriminator. Which member is live is decided by the type: a
type the union does **not** name arrives as the record's own **RDATA**,
and one it names *may* arrive as that structure. So `HTTPS`, `CAA`,
`CERT`, `LOC` and most of the registry work there exactly as everywhere
else.

The forty-two this crate knows the OS parses are `A`, `AAAA`, `MX`, `TXT`,
`SRV`, `NS`, `SOA`, `CNAME`, `PTR`, `DS`, `DNSKEY`, `TLSA` and thirty
more — essentially every record in everyday use — so refusing them would
refuse the reason anyone asks a system resolver at all. Twenty-six are
read out of the structure and **written back into the RDATA the wire
would have carried**; sixteen are refused by name. `Record::rdata` for one
of those twenty-six is therefore what Windows *understood*, not the octets
that arrived — names come back uncompressed, and case is Windows'.

**The union names one type it does not parse**, which is worth knowing
before trusting the metadata: `DNS_SVCB_DATA` exists and RR type 64
arrives as RDATA anyway. `SVCB` therefore works exactly as `HTTPS` does,
and the crate checks `wDataLength` against the structure it would be
rather than trusting the table — anything that does not fit is handed over
as RDATA.

Measured rather than read, on Windows 11, where both calls exist: type 65
comes back with `wDataLength = 61` and bytes that are SVCB wire format,
byte-for-byte the RDATA that Linux's `res_query` reports for the same
name; `MX` came back as a structure and `CAA` as RDATA; and the crate's
own test compares the two Windows paths across a record of every shape,
`SVCB` among them, and they agree.

### Apple does not use `res_9_query`, and the reason is measured

The obvious Apple backend is the same `res_query` the other Unixes use.
It was, and it is not, because it fails twice on a real Mac.

**It cannot be used concurrently.** The same query, sixty-four times:
64/64 answered one after another, **12/64 from eight threads**, with 46 of
the failures leaving the answer buffer untouched. A caller that runs
lookups on a blocking pool — which is what this crate's seam is for — was
most of the way to failing.

**And it answers from the wrong resolver.** macOS routes queries between
several DNS clients; `/etc/resolv.conf` configures only the primary one.
Asking the machine for its own `.local` name, which a supplemental mDNS
client owns: `res_9_query` fails with rcode 3, `DNSServiceQueryRecord`
returns the address. A VPN's split-DNS zone is the same shape.

So this platform uses `DNSServiceQueryRecord`, whose callback carries
`rrtype`, `rrclass`, `rdlen`, the raw rdata, a TTL and the interface the
query was resolved on. One consequence is visible from outside:
**`NXDOMAIN` is not distinguishable there** — the daemon reports a missing
name and a missing record type with one code — so a name that does not
exist is an empty answer rather than `Error::NameDoesNotExist`.

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
