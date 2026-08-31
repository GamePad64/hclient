# `system-resolver`: what was measured, and what it decided

The crate exists — `crates/system-resolver`, with its own README for what
it is and its module docs for how each platform is reached. **This file is
the measurement log**, kept separately because the code can only carry the
decisions it made and not the readings that ruled the alternatives out.

Everything below was executed rather than read, on a Linux host and on a
Windows 11 machine (build 26200). Three of the findings contradict what
the documentation suggested, and one contradicts what this project
believed and had shipped.

## 1. What each platform hands over

| platform | call | what comes back |
|---|---|---|
| Linux (glibc, musl) | `res_query` | the wire message |
| macOS, iOS | `res_9_query` | the wire message |
| Android ≥ 29 | `android_res_nquery` + `android_res_nresult` | the wire message |
| Windows 11 / Server 2025 | `DnsQueryRaw` | the wire message |
| Windows 10 | `DnsQuery_UTF8` | **records the OS has already taken apart** |

Four and a half of the five hand over a message. Windows 10 does not, and
the shape of the crate follows from that one row: the common type is
**records**, because synthesising a message on Windows 10 would mean
inventing an rcode and flags nobody reported.

## 2. The Windows finding, which was a live defect

`hclient-dns-system` read an HTTPS record as `DNS_SVCB_DATA` — a struct
with a `PSTR` at offset 8 — on a claim its own header recorded as *taken
on the project owner's word and never executed*. The same header named the
exact consequence of the claim being wrong: the payload would be raw
response bytes and reading it as that struct would build a pointer out of
them.

**Measured, `cloudflare.com`, RR type 65, Windows 11:** `wDataLength` is
**61** and the union holds `0001 00 0001 0006 02 68 33 02 68 32 …` — SVCB
wire format. Offset 8, where `pszTargetName` would have been read from,
holds `h3`. Those 61 octets are byte-for-byte the RDATA inside the
`res_query` answer this repository had captured on Linux years earlier.

### 2.1 The rule behind it, confirmed in both directions

`DnsQuery_UTF8` fills in a `DNS_RECORDA` whose data union has **no
discriminator in the record**. Which member is live is decided by the
type:

- a type the union **names** arrives parsed into that member;
- a type the union **does not name** arrives as the record's own RDATA,
  with `wDataLength` its length.

| type | measured | union member |
|---|---|---|
| `A` (1) | 4 octets, the address | `DNS_A_DATA` |
| `MX` (15) | 16 octets: a pointer and a preference | `DNS_MX_DATAA` |
| `HTTPS` (65) | 61 octets of RDATA | **none** |
| `CAA` (257) | 23 octets of RDATA, `00 09 "issuewild" "comodoca.com"` | **none** |

And the metadata says it from a second direction: `DNS_TYPE_HTTPS`,
`DNS_TYPE_CERT` and `DNS_TYPE_LOC` all exist as constants with no union
member, so it is the **union** that decides and not the constant list.

That is why `Support::AnyExcept(&[..])` and not a `bool`: the exception is
forty-three types, and everything else — most of the registry — works on
Windows exactly as it does anywhere else.

## 3. `DnsQueryRaw`, and the three readings that decide its code

It is Windows 11 / Server 2025 and later, and it hands over the wire
message — so a machine that has it behaves like the other four.

- **The call returns `DNS_REQUEST_PENDING` (9506) and the completion
  routine then always runs.** Given a `protocol` of 0 it returns `87`
  (`ERROR_INVALID_PARAMETER`) and the routine **never runs**. So the rule
  that cannot hang is: wait for the routine exactly when the return is
  `DNS_REQUEST_PENDING`.
- **The two-octet length prefix is TCP's, not the API's.** Over
  `DNS_PROTOCOL_TCP` the first two octets equalled the rest of the
  buffer's length every time (195/195, 594/594, 154/154, 256/256); over
  UDP they are the message's own ID. The crate checks the prefix rather
  than assuming it.
- **UDP truncates and TCP does not.** `CAA` for `cloudflare.com` is 32
  octets over UDP — a header with `TC` set — and 596 over TCP. `protocol`
  is required, so there is no *let the OS decide*.
- `NXDOMAIN` arrives as a whole message, `queryStatus` 9003 beside 258
  octets, so the rcode is read by the shared walker rather than translated
  here.

**The detection has to be dynamic**, and that is not a style choice:
`windows-sys` emits `DnsQueryRaw` *and* `DnsQueryRawResultFree` as
`raw-dylib` imports, and the loader resolves imports at process start — so
a binary that so much as names one of them fails to start on Windows 10.

### 3.1 It does not cost the cache

Which was the open question, and the half most worth having. Two
measurements, because the first was not sound:

- After `Clear-DnsClientCache`, a `DnsQueryRaw` for an A record **created
  two entries** in the Windows resolver's own store, exactly as
  `DnsQuery_UTF8` does.
- For type 65, which `Get-DnsClientCache` does not display at all, two
  identical queries put **one** source port on the wire — counted with
  `pktmon` — so the second was answered without a packet.

The first attempt used TTL: a second query reporting a lower TTL looked
like a cache. It is not evidence, because an upstream resolver's cache
produces the same falling TTL while every query still leaves the machine.

## 4. Windows 10: supported on a best-effort basis

**It is supported, and it is the one platform in the table nothing has
ever been run on.** There is no Windows 10 machine in this project or in
CI. What stands in for one is that its code path — `DnsQuery_UTF8` — is
present on Windows 11 too and *was* executed there, against `DnsQueryRaw`
on the same machine and the same name: `the_parsed_path_answers_the_same_rdata_as_the_raw_one`
compares the two and they agree. That is the strongest evidence available
without the hardware, and it is not the same as having run it.

### 4.1 What is not supported there, precisely

- **The forty-three record types Windows parses into a structure of its
  own.** They are refused **by name**, before a query, through
  `Support::AnyExcept` — never guessed at, because handing a caller a
  structure's bytes as though they were RDATA is the defect §2 describes.
  The list is `A`, `NS`, `MD`, `MF`, `CNAME`, `SOA`, `MB`, `MG`, `MR`,
  `NULL`, `WKS`, `PTR`, `HINFO`, `MINFO`, `MX`, `TXT`, `RP`, `AFSDB`,
  `X25`, `ISDN`, `RT`, `SIG`, `KEY`, `AAAA`, `NXT`, `SRV`, `ATMA`,
  `NAPTR`, `DNAME`, `OPT`, `DS`, `RRSIG`, `NSEC`, `DNSKEY`, `DHCID`,
  `NSEC3`, `NSEC3PARAM`, `TLSA`, `SVCB`, `TKEY`, `TSIG`, `WINS`, `WINSR`
  — written in the crate as the metadata's own constants rather than as
  numbers, so a reader can check it against the union by name.
- **Records of another type beside the answer.** A CNAME chain is visible
  on every platform that hands over a message and is not visible here:
  a `DNS_RECORD` of another type is a parsed structure of that type, and
  this path has no RDATA for it to hand back.
- **The header, and therefore `TC`.** A truncated answer is refused on the
  message platforms and is invisible here — whatever records the OS
  obtained come back with nothing saying the set is short. `NXDOMAIN` is
  the one header fact that survives, because the API reports it as a
  status of its own.
- **`AA`, and the rcode as anything but those two outcomes.** Neither is
  reachable through `DNS_RECORD`, which is the same loss the crate takes
  deliberately everywhere (§1) rather than a Windows 10 one.

### 4.2 What is not known about it

- **Whether Windows 10 caches the types it returns as raw RDATA.** On
  Windows 11 the raw path preserves the cache, proven by packet count
  (§3.1). Windows 10 takes a different call and this has not been
  measured; it needs the hardware.
- **Whether its union names fewer types than Windows 11's.** A later OS
  knows more record types, not fewer, so the list in §4.1 is an **upper
  bound** on what Windows 10 parses — which is the safe direction: a type
  Windows 10 hands over raw and this crate refuses costs a caller a
  fallback, where the reverse would cost it a wrong answer.
- **Whether it is worth keeping at all.** Windows 10 is out of support, so
  its table cannot grow — which is what makes an enumerated set stay
  correct rather than rot, and is the argument that made this design
  possible. It was the project owner's, and it reversed a conclusion this
  file's first draft had reached the other way.

## 5. What was deliberately not done, each with the reason

- **No resolver of its own.** Then it would be `hickory`, which exists.
- **No caching.** The platform's is the point; a second one would answer
  from a different clock.
- **No decoding of RDATA.** `Record::rdata` is bytes. A crate that decoded
  records would need a type per RFC and would make every consumer wait for
  the one it needs — `hclient-dns-system` is the model, applying RFC
  9460's rules beside the consumer rather than beside the syscall.
- **No async API.** Every platform call blocks or is made to; an async
  wrapper over a blocking call is a thread pool with an opinion, and a
  caller has one.
- **No DNSSEC validation**, and no `AD`: reporting it would mean carrying
  the header, which §1 rules out.

## 6. Cost

| platform | crates added beyond `thiserror` and `cfg-if` |
|---|---|
| Linux, macOS, iOS | **0** — `libc` is not needed; the symbols are declared directly |
| Windows | **0** — `windows-sys` with three features |
| Android | **0** — the NDK symbols are in `libandroid`, linked by name |

No JNI: `android_res_nquery` is a C entry point, unlike Android's *proxy*
settings, which live behind a JVM and cost `jni` + `ndk-context`. That
asymmetry is worth knowing before assuming the two Android integrations
are alike.

## 7. Still open

- **iOS.** `res_9_query` is in `libresolv` on both Apple platforms and the
  same code compiles for both, but nothing here has been run on iOS.
- **A Windows 10 machine**, for §4.2.
