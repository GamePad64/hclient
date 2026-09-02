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
| FreeBSD | `res_query` | the wire message — **compiled, never run**, §4.7 |
| Windows 11 / Server 2025 | `DnsQueryRaw` | the wire message |
| Windows 10 | `DnsQuery_UTF8` | **records the OS has already taken apart** |

Four of the six hand over a message. Windows 10 does not, and neither
does Apple once the backend is the right one — so the shape of
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
| `hclient-dns-system`'s `live_lookup_of_a_name_that_publishes_https_records` | passes — an HTTPS lookup for `cloudflare.com` yields endpoints advertising `h3` |

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

## 4.6 The requirement nothing asserted, and why the runner hid it

`sys/mod.rs` has said since the crate existed that a `res_query` backend
needs two things — a symbol to link against, and **a libc whose resolver
state is per-thread** — and its own comment claimed the second had been
established "by running the crate's own concurrency case". There was no
such case. Measured: `thread::spawn` appeared nowhere in the crate.

So the property Apple's arm failed, and the only property that
distinguishes a usable `res_query` platform from an unusable one, was
asserted by nothing at all. It was found by a hand-written probe that
lives in no repository, and the sentence describing that probe as a test
of this crate's had been in the source ever since.

**The runner is what made it invisible.** This workspace runs `cargo
nextest`, which gives each test its own process — so the four live tests
"running in parallel" are parallel *processes*, and share no resolver
state by construction. A suite that looked concurrent exercised nothing
concurrent. That is the third distinct thing nextest's process isolation
has done in this workspace, after hiding a file-descriptor limit on macOS
and separating global tracer providers, and the first where it cost
coverage rather than saved it.

`concurrent_lookups_all_answer_where_a_serial_burst_does` is the repair:
eight threads, eight queries each, the same 64 as the probe that found the
Apple defect. It spawns its own threads rather than relying on the runner,
for the reason above.

**The serial burst is a control rather than a warm-up.** 64 one at a time,
then 64 from eight threads, and the two assertions say different things: a
serial failure means the machine cannot resolve, and only a serial pass
beside a concurrent failure is evidence about the backend. Both were
checked in the failing direction by mutation — dropping the concurrent
half's answers gives *64/64 one at a time and 0/64 from 8 threads*, and
dropping the serial half's gives *this machine cannot resolve rather than
cannot resolve concurrently*.

Run on both targets the arm claims: `x86_64-unknown-linux-gnu` and
`x86_64-unknown-linux-musl`, passing on each. It is `#[ignore]`d with the
other live tests, and it is the instrument any further platform is
established with — the symbol a backend links against fails loudly, and
this property fails quietly, which is what it did.

## 4.7 FreeBSD: added on decision, with the unrun half named

The arm exists — `sys/mod.rs` routes `target_os = "freebsd"` to
`res_query.rs`, the same module Linux uses. It was added at the project
owner's request after the case against adding it was put and heard, and
what follows is the record of which half is established and which is not,
because that distinction is the whole of what this file is for.

### 4.7.1 The symbol: established, and it fails loudly

`lib/libc/resolv/Symbol.map` in the FreeBSD source exports **both**
`res_query` and `__res_query`, each under `FBSD_1.0`. Two things follow.
The plain name links, so no `link_name` is needed. And the library is
`libc` — the map lives under `lib/libc`, and `resolver(3)` says the same
from the other side, *"Standard C Library (libc, -lc)"* — so no
`-lresolv`, which FreeBSD does not ship as a separate library anyway.

**The glibc trap has no counterpart here.** On glibc, `__res_query` is a
non-default compat symbol at the same address as `res_query` and does not
link by that name (§ the note in `res_query.rs`); on FreeBSD each name
appears in exactly one version node, so neither is a compat alias of the
other.

**This is the same class of evidence Linux and musl were settled with** —
exported symbols, read rather than assumed — and it is strictly better
than `libc`'s own `link_name` table, which covers `res_init` alone and
would have suggested the `__res_` prefix for a reason that does not apply.

It is also the cheap half: a wrong symbol is a **link error**, so the
first person to build for the target finds it in seconds.

### 4.7.2 The per-thread state: read, not run

`resolver(3)` states it in the platform's own words — *"This
implementation of the resolver is thread-safe, but it will not function
properly if the programmer attempts to declare his or her own `_res`
structure in an attempt to replace the per-thread version referred to by
that macro"* — and adds that `res_init()` returns -1 *"in a threaded
program if per-thread storage could not be allocated"*.

That is better than anything Apple's arm ever had, whose entire evidence
was `libresolv.9.tbd` showing a symbol exists (§4.5.2). It is still a
manual page, and §4.5.1 is this file's record of a manual page being
right about what it said and silent about what mattered.

**What would settle it is one command on a FreeBSD machine**, and it is
the command that established the Linux arm:

```
cargo test -p system-resolver --test live -- --ignored
```

`concurrent_lookups_all_answer_where_a_serial_burst_does` (§4.6) is
written for this question and no other.

### 4.7.3 What was actually run here, and what those runs do not prove

| what | result |
|---|---|
| `cargo check -p system-resolver --all-features --all-targets --target x86_64-unknown-freebsd` | clean |
| the same for `hclient-dns-system` | clean |
| both wired into `just check-targets` | 20 invocations over 6 targets, clean |
| that gate broken on purpose — a `#[cfg(target_os = "freebsd")]` compile error | fails, naming the invocation |
| `cargo build -p system-resolver --tests --target x86_64-unknown-freebsd` | **fails at `-lexecinfo`** |

The last row is the honest boundary. There is no FreeBSD sysroot on this
host, so the link dies fetching a system library and never reaches symbol
resolution — it therefore says **nothing** about `res_query`, in either
direction. A `cargo check` does not link at all, and an rlib does not
either, which is why `cargo build -p system-resolver` succeeding proves
less than it looks like it does.

So the arm's position is exactly `hclient-winhttp`'s: compiled on every
push for a platform nothing here can run, with one difference in its
favour — the live suite that would establish it already exists and names
the question.

## 4.7 The two Linux libcs turned out to be two platforms

Asked whether `res_query` is safe to call the way this crate calls it, and
the answer split glibc from musl twice over — once on thread safety, once
on what can be asked at all.

### 4.7.1 glibc documents it unsafe, and the implementation disagrees

`resolver(3)`: *"The traditional resolver interfaces such as `res_init()`
and `res_query()` use some static (global) state stored in the `_res`
structure, rendering these functions non-thread-safe"*, and its
`ATTRIBUTES` table lists **only** the `res_n*` family as `MT-Safe`.

The implementation says otherwise, measured three ways on this host:

- `__res_state()` returns a **distinct pointer per thread** — nine threads,
  nine addresses;
- disassembled out of the shipped `libc.so.6`, `res_query` is
  `__resolv_context_get` → `__res_context_query` → `__resolv_context_put`,
  and `__resolv_context_get` reads through **`%fs:`**, the thread-local
  segment;
- the crate's own concurrency burst answers 64/64 from eight threads.

So on glibc 2.43 the "global state" of the sentence is thread-local, and
the wording is inherited from the BIND-era API rather than describing this
libc.

**A `res_nquery` backend was built for glibc and then withdrawn**, and
the withdrawal is the finding rather than the building. It works: it is
glibc's documented `MT-Safe` entry, it needs no struct layout — `_res` is
`(*__res_state())`, that symbol links, and the pointer is passed through
and never dereferenced — and it costs one initialisation per thread, since
a fresh thread's state answers `-1` until `__res_ninit` while
re-initialising an initialised one **leaks**, 1660 KiB over 200 000 calls.
All measured, and all beside the point.

What settled it is that `res_nquery(__res_state(), ..)` is handed **the
very object** `res_query` fetches itself. Both stand on the same
per-thread fact, so the documented entry point documents the other half:
`res_nquery` is `MT-Safe` *given a state per thread*, and where that state
comes from is exactly what is not documented. Taking it would have bought
the appearance of following the manual while resting on the same
undocumented property, behind more machinery — a second module, three
symbols instead of one, a thread-local, and a leak to avoid. The only
route that would rest on the contract alone is a state of this crate's
own, and that needs the layout `libc` declines to declare on any platform.

**What is kept is the part with teeth.**
`glibc_hands_each_thread_its_own_resolver_state` asserts the fact directly
rather than through the outcome of a burst, which is the difference
between catching Apple's defect and being lucky about it — and it guards
whichever entry point the crate calls, because they share the state it
checks. Where a platform documents the property (FreeBSD) the test is a
confirmation; on glibc, where the manual says the opposite, it is the
whole of the evidence.

### 4.7.2 musl cannot be asked for a type above 255

Found by running the live suite on `x86_64-unknown-linux-musl`, where two
of five tests failed — both on `CAA`. Probed against glibc as the control,
one host, one name, one moment:

| type | musl | glibc |
|---|---|---|
| 1 `A`, 28 `AAAA`, 65 `HTTPS`, 255 `ANY` | answers | answers |
| **257 `CAA`** | **−1** | **515 octets** |

So the boundary is the call's, not the network's and not any one type's:
musl's `res_query` will not carry a type number above 255.

**Claiming every type was therefore a capability that lies**, which is
the defect this crate was written to remove one platform over — and it had
shipped, invisible because nothing in CI builds for musl and the live
tests are `#[ignore]`d.

### 4.7.3 And the enum became a struct, which is where the answer belongs

The first repair added a fourth variant, `UpTo(255)`. It worked, and it
made the shape of the type the question: an enum of four cases had grown a
case the first time a platform answered something new, and there was no
reason to think that was the last one.

`Support` is now a range and a hole-punch —
`{ range: RangeInclusive<u16>, except: &'static [u16] }`, both fields
crate-private — and the four answers are three shapes of one thing: every
type is the full range with nothing excepted, every type up to 255 is a
shorter range, every type but sixteen is the full range with a list, and
nothing at all is an empty range. A fifth platform with a stranger answer
is a different pair of values rather than a different type. The two halves
compose, which the enum could not express, and a test asserts it although
no platform answers that way today — that is what keeps the fields
independent rather than a tagged union in a struct's clothes.

**What is lost is exhaustiveness, and it is worth naming because this
crate had leaned on it.** A new variant used to be a compile error at
every reader, which is exactly how `UpTo` was caught — the compiler listed
the four readers that had to decide. What replaces it is that there is now
**one** reader path: the fields are crate-private, so a caller has one
question, `allows(rtype)`, and cannot come to depend on how the answer is
stored. Outside the crate there is nothing to be exhaustive about.

The live suite followed the type. `support_and_lookup_agree_about_every_type_that_separates_a_platform`
is a **biconditional over three types** — `A`, which Windows 10 parses and
refuses; `CAA`, above musl's ceiling; `HTTPS`, answerable wherever there
is a backend — where it used to be one arm per variant, edited on the day
the ceiling arrived and due for editing again.

**Its first draft was too weak and a mutation walked past it.** Asserting
*an allowed type is not refused by name* passes when the ceiling is
dropped, because musl then answers `-1`, which is `NoResponse` — neither
`UnsupportedType` nor `Unsupported`. It asserts `Ok` now, which excludes
nothing a resolver could legitimately say, since *no records of this type*
is `Ok(vec![])` everywhere here. Verified: with the ceiling removed the
weak form left this test green and three neighbours red; the strong form
takes all four.

Adding a variant to a deliberately exhaustive enum was a breaking change
and so is replacing the enum, and both were free exactly once — before
`0.1.0`.

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

## 6.1 The compatibility gate, which is live now

`system-resolver` 0.1.0 reached crates.io on 2026-09-01, and that is what
gave `just semver` a baseline: cargo-semver-checks fetches the newest
release, and until there was one the recipe could only report
`system-resolver not found in registry`. It was recorded here as a
deliberate instance of *a recipe nothing calls*, accepted for as long as
it had no subject. It has one, and it is a step in the `lint` job.

**What makes it worth running on every push rather than at release time
is a classification that had to be measured**, because the earlier
reading of it was about a different case. A working tree sitting at the
**published** number — `0.1.0 -> 0.1.0` — is *"no change; assume minor"*
and runs **196** checks. The pre-release pair this document records two
sections up, `0.1.0-alpha.2 -> 0.1.0-alpha.2`, is *"assume major"* and
runs **0**: a major step permits breaking, and every step inside a
pre-release is a major step. So the vacuum belongs to the pre-release
rather than to equal versions, and a stable crate is checked from the
moment it is published without a version bump or a `--release-type` flag.

Verified against the published crate rather than a git baseline: marking
`Error` as `#[non_exhaustive]` takes the recipe to exit 100, naming
`enum_marked_non_exhaustive` and the item. The fail-closed half — a run
that executes nothing prints `0 checks` beside `no semver update
required` and exits zero — is unchanged and is what the recipe reads the
count for.

## 7. Still open

- **iOS.** `dns_sd.h` is available on both Apple platforms and the same
  code compiles for both, but nothing here has been run on iOS. The
  concurrency and split-DNS measurements in §4.5 are macOS 27's.
- **A Mac actually on a VPN.** §4.5's split-DNS case was made with the
  `.local` client every Mac has, which is the same shape and not the same
  thing. Nothing is expected to differ; it has not been seen.
- **A Windows 10 machine**, for §4.4.
- **A FreeBSD machine.** The arm is in (§4.7); its symbol is established
  and its per-thread claim is read rather than run, which is the same
  shape of gap that let Apple's arm ship unusable, and it is the only
  unrun half of any arm this crate ships. One live run settles it:
  `cargo test -p system-resolver --test live -- --ignored` on the
  machine.

- **The reentrant family is not a shortcut, measured.** `res_nquery` with
  a caller-owned state would make the per-thread question moot — and
  `libc` 0.2.189 declares **one** resolver function, `res_init`, and **no**
  `res_state` struct on any platform. So that route means declaring a C
  structure whose layout differs between libcs, where a mismatch is silent
  memory corruption rather than a compile error. The loud unknown is
  preferable to the quiet one.

- **illumos, OpenBSD, NetBSD, Haiku, Redox, AIX, QNX.** The targets exist
  and answer `Support::None`. None carries deployment weight for an HTTP
  client, and each carries the same unestablished-arm risk for it. An
  honest `Support::None` costs a caller one fallback; a wrong guess costs
  a wrong answer. OpenHarmony (`target_env = "ohos"`, a musl-based Linux)
  is the cheapest of them and is equally unestablished.
- **WASI is not open and never will be.** `wasi:sockets/ip-name-lookup`
  offers `resolve-addresses` and nothing else — names to addresses, no
  record type at all — so the absence there is structural rather than
  unfinished.
