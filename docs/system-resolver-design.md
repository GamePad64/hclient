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
| macOS, iOS | `DNSServiceQueryRecord` | **records, one per callback** |
| Android ≥ 29 | `android_res_nquery` + `android_res_nresult` | the wire message |
| Windows 11 / Server 2025 | `DnsQueryRaw` | the wire message |
| Windows 10 | `DnsQuery_UTF8` | **records the OS has already taken apart** |

Three and a half of the five hand over a message. Windows 10 does not,
and neither does Apple once the backend is the right one — so the shape of
the crate follows from those rows: the common type is **records**, because
synthesising a message where the platform never had one would mean
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

**But the second half is necessary and not sufficient, and that cost a
crash.** `DNS_SVCB_DATA` exists, and RR type 64 arrives as RDATA anyway —
measured on `_dns.resolver.arpa`: `wDataLength` 22 and 44 against a
structure of 32 octets, with SVCB wire format in the union. It was found
by implementing a re-encoder for `SVCB` on the strength of the union
alone, without measuring the one type it was being written for, and
watching the cross-path test die with an access violation. That is the
defect §2 describes, reintroduced by trusting a table.

So the table is **checked rather than trusted**: `wDataLength` is compared
against the structure it would be, and anything that does not fit is
handed over as RDATA. For the eight fixed-size shapes the check is exact
and would have caught `SVCB` on its own.

`Support::AnyExcept(&[..])` and not a `bool`, then. The forty-two types
the OS really parses are **not** the obscure ones — `A`, `AAAA`, `MX`,
`TXT`, `SRV`, `NS`, `SOA`, `CNAME`, `PTR`, `DS`, `DNSKEY`, `TLSA` — so
refusing them would have refused the reason anyone asks a system resolver
at all. Twenty-six are read out of their structure and **written back into
the RDATA the wire would have carried**; §4.2 has the sixteen that are
not. `SVCB` is on neither list: it arrives raw, exactly as `HTTPS` does.

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

### 4.1 The RDATA is synthesised, and what that costs

For the twenty-six types the structure is read and written back out. What
comes back is therefore **what Windows understood**, not the octets that
arrived, and the differences are worth knowing before comparing one byte
for byte:

- **Names are written out in full.** On the wire a name inside RDATA may
  be a compression pointer; there is nothing here to point into. This
  crate does the same on the platforms that *do* hand over a message —
  see §4.3, which is a finding of its own — so the two agree.
- **Case is Windows'.** DNS names are case-insensitive and a resolver may
  return either; nothing restores what the origin sent.
- **A character-string cannot carry a NUL.** `TXT`, `HINFO`, `X25`,
  `ISDN` and `NAPTR` strings come back as NUL-terminated C strings, so an
  octet the RFCs permit is not representable — and a record containing one
  is truncated by **Windows**, before this crate sees it.

### 4.2 What is not supported there, precisely

- **Sixteen record types**, refused **by name** and before a query
  through `Support::AnyExcept` — never guessed at, because handing a
  caller a structure's bytes as though they were RDATA is the defect §2
  describes. They are `SIG` and `RRSIG`, which carry a signature over a
  canonical form this crate would have to reproduce exactly or hand back a
  record that fails validation; `NSEC`, `NXT`, `NSEC3` and `NSEC3PARAM`,
  whose type bitmaps their structures do not expose as one; `OPT`, `TKEY`
  and `TSIG`, which are protocol machinery rather than answers; and `WKS`,
  `ATMA`, `NULL`, `DHCID`, `WINS` and `WINSR`, which have no consumer
  here.

  `SVCB` is **not** among them, and neither is `HTTPS`: both arrive as
  RDATA and need nothing. That `SVCB` does, despite its union member, is
  §2.1's correction.

  The list is **derived** in the crate rather than written down — it is
  the union's membership minus the types with a re-encoder — because a
  third copy is a third thing to forget.
- **Records of another type beside the answer.** A CNAME chain is visible
  on every platform that hands over a message and is not visible here:
  a `DNS_RECORD` of another type is a parsed structure of that type, and
  this path would have to know that type's shape too. A caller that wants
  it asks for `CNAME`.
- **The header, and therefore `TC`.** A truncated answer is refused on the
  message platforms and is invisible here — whatever records the OS
  obtained come back with nothing saying the set is short. `NXDOMAIN` is
  the one header fact that survives, because the API reports it as a
  status of its own.
- **`AA`, and the rcode as anything but those two outcomes.** Neither is
  reachable through `DNS_RECORD`, which is the same loss the crate takes
  deliberately everywhere (§1) rather than a Windows 10 one.

### 4.3 The synthesis found a defect on the other four platforms

The test comparing the two Windows calls failed the first time it ran, on
`NS`: the wire said `03 6e 73 33 c0 0c` and the synthesis said
`ns3.cloudflare.com` written out. The synthesis was right.

**A compression pointer is meaningless the moment the message is gone**,
and this crate hands back records rather than messages — so RDATA that
still carried one was bytes a caller could neither read nor resolve. That
was true on Linux, macOS, Android and Windows 11, for every type whose
RDATA may hold a name: `NS`, `CNAME`, `PTR`, `SOA`, `MX`, `MINFO`, `RP`,
`AFSDB`, `RT`, `SRV` and the rest of the RFC 1035 set — which RFC 3597 §4
closes, so the table cannot grow with the registry.

The walker expands them now, and the two Windows paths agree because both
produce the same self-contained bytes. It was the platform this project
has no machine for that made the defect visible on the four it does.

### 4.4 What is not known about it

- **Whether Windows 10 caches the types it returns as raw RDATA.** On
  Windows 11 the raw path preserves the cache, proven by packet count
  (§3.1). Windows 10 takes a different call and this has not been
  measured; it needs the hardware.
- **Whether its union names fewer types than Windows 11's.** A later OS
  knows more record types, not fewer, so the refusal list in §4.2 is an
  **upper bound** on what Windows 10 parses — which is the safe direction: a type
  Windows 10 hands over raw and this crate refuses costs a caller a
  fallback, where the reverse would cost it a wrong answer.
- **Whether it is worth keeping at all.** Windows 10 is out of support, so
  its table cannot grow — which is what makes an enumerated set stay
  correct rather than rot, and is the argument that made this design
  possible. It was the project owner's, and it reversed a conclusion this
  file's first draft had reached the other way.

## 4.5 Apple: `res_query` was the wrong call twice, and both are measured

Read out of Apple's manual pages first, then executed on a macOS 27
machine — and the running found a worse defect than the reading had.

### 4.5.1 What the documentation said

`resolver(5)` describes macOS as running several DNS *clients* with a
"Super" meta-client routing between them by best domain match;
`/etc/resolv.conf` is "configuration for the default (or `primary`) DNS
resolver client", and client configurations may live in the System
Configuration Database, about which "users of the DNS system should make
no assumptions". `resolver(3)` — the page Apple ships for `res_query` —
mentions none of it: `res_init()` "reads the configuration file",
`res_query()` "sends it to the local server", `FILES` lists
`/etc/resolv.conf` alone. So a VPN's split-DNS resolvers and the
per-domain ones in `/etc/resolver/` belong to the Super client, and that
call does not consult them.

That conclusion is composed from two pages rather than stated in either,
which is why it was worth executing.

### 4.5.2 What running it found, which is worse

**`res_9_query` cannot be used concurrently.** The same query, sixty-four
times: **64/64** answered one after another and **12/64** from eight
threads, 46 of the failures leaving the answer buffer untouched — the call
returning before anything went out. `sys/mod.rs` names exactly this as one
of the two things a `res_query` backend needs, "a libc whose resolver
state is per-thread", and for Apple that half had been taken from
`libresolv.9.tbd`, which shows a symbol exists and says nothing about its
state.

**It had never been executed.** This project has claimed Apple support
since v0.3; the first run of its own live suite on a Mac failed all four
tests, and passed under `--test-threads=1`. A caller runs these on a
blocking pool, so two concurrent HTTPS lookups on a Mac were most of the
way to failing while the capability said they would work.

**And the split-DNS reading was right.** Asking the machine for its own
`.local` name, type A — `.local` being a supplemental mDNS client every
Mac has, per `scutil --dns`:

| call | answer |
|---|---|
| `res_9_query` | failed, **rcode 3** from the primary nameserver |
| `DNSServiceQueryRecord` | `172.21.0.151` |

An ordinary unicast name is the control and both answer it.

### 4.5.3 So the backend is `DNSServiceQueryRecord`

Its callback is this crate's `Record` almost field for field — `rrtype`,
`rrclass`, `rdlen`, "the raw rdata of the resource record", a TTL, the
name, and `interfaceIndex`, "the interface on which the query was
resolved". It hands over RDATA rather than a message, which is why Apple
joins Windows in §1's table rather than the `res_query` platforms.

**Two things about it are not in the header**, and both cost a wrong
implementation before they were measured:

- **`kDNSServiceFlagsReturnIntermediates` is what makes a negative answer
  arrive at all.** Without it the daemon never reports that a name has no
  record of the type asked for — nothing in four seconds. With
  `kDNSServiceFlagsTimeout` instead, nothing until `kDNSServiceErr_Timeout`
  at 30.0 s. With `ReturnIntermediates`, `kDNSServiceErr_NoSuchRecord` at
  **1.2 ms**. The header describes that flag as being about intermediate
  results in a CNAME chain.
- **`kDNSServiceFlagsTimeout` suppresses the negative it is documented
  only to bound**, so it is not passed and the wait is bounded by polling
  the query's own socket instead.

**One capability is lost and it is stated rather than papered over**:
`NXDOMAIN` is not distinguishable on Apple. The daemon reports a missing
name and a missing record type with one code, and there is no header to
read an rcode out of, so a name that does not exist is an empty answer.
`Error::NameDoesNotExist` says so on the variant, and the live test
branches on the platform rather than accepting either answer — so a
platform that *can* distinguish and quietly stopped still fails.

### 4.5.4 The acceptance run, and the third reason this workspace needs nextest

Measured on macOS 27, arm64, rustc 1.98, against the workspace as it
stands rather than against a standalone copy of this crate — because the
crate passing on its own says nothing about the adapter above it. Every
number below is one run of the tree.

| what ran | result |
|---|---|
| `cargo nextest run --workspace --all-features` | **2304 passed, 0 failed**, 12.4 s |
| `system-resolver`'s four live tests | pass, run in parallel |
| `hclient-dns-system`'s `live_lookup_of_a_name_that_publishes_https_records` | passes — `lookup_svcb("cloudflare.com")` yields endpoints advertising `h3` |

The last row is the one worth having: it is the whole path, from
`DNSServiceQueryRecord`'s callback through this crate's `Record` and the
adapter's SVCB envelope to an `SvcbEndpoint` a transport would act on.
The two rows above it would both be green for a backend nothing consumed.

**The first attempt at that run failed, and the cause is a fact about
macOS rather than about this code.** Under `cargo test` — which runs the
tests of one binary as threads of one process — the suite died with
`Os { code: 24, kind: TooManyOpenFiles }` in `hclient-native`'s fixtures,
and a neighbouring Alt-Svc test timed out behind it. macOS's default
`ulimit -n` is **256**, where the Linux hosts this workspace is developed
on give 1024 or more, so sockets belonging to tests that had already
finished were still counted against the limit.

Under `cargo nextest run` the same tree passes **at the default 256**, in
the same 12 s: each test is its own process, so no test's descriptors
outlive it. That is a third reason for the rule this repository already
states twice — after *`cargo test` abandons the remaining binaries on the
first failure* and *its per-binary result lines have to be summed by
hand* — and it is the only one of the three that is invisible on Linux.

**Raising the limit is not the repair, and it was checked in both
directions**: at `ulimit -n 8192` under `cargo test` the file-descriptor
failures go away and nine `hclient-otel` tests fail instead, because they
each install a global tracer provider and, sharing one process, overwrite
each other's. Both failures are the same defect in the runner rather than
two defects in the tree, and both disappear under the runner this
workspace mandates.

**What this does not settle is the CI question**, and the difference is
worth stating rather than eliding. `CLAUDE.md` records that
`test (macos-latest)` never finished a run for twelve days and that the
hang is still undiagnosed. This is the first evidence that the suite
*completes* on macOS at all — but on macOS 27 on arm64 hardware, against
this tree, where CI runs a GitHub-hosted runner of another version on
another architecture. It narrows the question to that environment; it
does not answer it, and the descriptor limit above is the nearest thing
to a candidate this measurement produced.

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
| Windows | **3** — `windows-sys`, `windows-strings` and the `windows-link` they share |
| Android | **0** — the NDK symbols are in `libandroid`, linked by name |

`windows-strings` is there for a defect class rather than for tidiness.
The wide name `DnsQueryRaw` takes was a `Vec<u16>` with a zero chained on
the end, and losing that zero is not a compile error — it is a call that
reads past the end of an allocation. An `HSTRING` keeps the terminator
inside the allocation by construction, and an empty one still points at a
null, so the root name needs no special case. On the other side, its
`PCSTR::as_bytes` and `to_string` replace a hand-written `CStr` walk and a
hand-written UTF-8 check. Eleven crates for a Windows build in total.

No JNI: `android_res_nquery` is a C entry point, unlike Android's *proxy*
settings, which live behind a JVM and cost `jni` + `ndk-context`. That
asymmetry is worth knowing before assuming the two Android integrations
are alike.

## 7. Still open

- **`just semver` is not called by CI, and it cannot be until `0.1.0` is
  on crates.io.** cargo-semver-checks takes its baseline from the registry
  and there is nothing there yet — measured, `system-resolver not found in
  registry`. A job would be red for the absence of a release rather than
  for a defect, and a red job nobody can fix is how a check gets ignored.
  This is deliberately the recurring defect of *a recipe nothing calls*,
  accepted for as long as it has no subject, and it is written here rather
  than only in the justfile because this list is what gets read. The day
  the crate is published it is one line in the `lint` job.

- **iOS.** `dns_sd.h` is available on both Apple platforms and the same
  code compiles for both, but nothing here has been run on iOS. The
  concurrency and split-DNS measurements in §4.5 are macOS 27's.
- **A Mac actually on a VPN.** §4.5's split-DNS case was made with the
  `.local` client every Mac has, which is the same shape and not the same
  thing. Nothing is expected to differ; it has not been seen.
- **A Windows 10 machine**, for §4.4.
- **The BSDs and illumos.** The targets exist, this crate compiles for
  them today and answers `Support::None`, and adding one is a single arm
  in `sys/mod.rs`'s `cfg_if!`. What is missing is the establishing:
  `libc`'s own `link_name` table points at `__res_init` for FreeBSD,
  DragonFly, Cygwin and Haiku, which suggests the `__res_` prefix — but
  §2's whole lesson is that a table is not a measurement, and the glibc
  finding below shows the two names in that family need not behave alike.
  An honest `Support::None` costs a caller one fallback; a wrong guess
  costs a wrong answer.
- **WASI is not open and never will be.** `wasi:sockets/ip-name-lookup`
  offers `resolve-addresses` and nothing else — names to addresses, no
  record type at all — so the absence there is structural rather than
  unfinished.
