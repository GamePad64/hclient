# A system resolver for arbitrary record types, on five platforms

## 1. What it is, and the one thing it is not

A crate that asks **the machine's own resolver** for a DNS record, for
record types `getaddrinfo` cannot return. It is not a resolver: it sends
no queries of its own, keeps no cache, implements no retry and validates
no signatures. Everything it does, the platform does — this only reaches
the platform in a way that is the same shape on all five.

**Why that is worth a crate.** `hickory-resolver` (20,425,890 downloads a
quarter) is an excellent resolver and it is *its own*: it reads
`/etc/resolv.conf` and speaks to the servers named there. That is the
wrong answer wherever the interesting configuration is not in that file —
a VPN's split-DNS, a corporate zone, Android's Private DNS, or Windows'
per-interface servers, none of which the file describes. `dns-lookup`
(4,691,734) reaches the platform and offers only what `getaddrinfo` does:
A and AAAA. `resolv` (**319**) does what this proposes and only on glibc.

So the gap is real and narrow: **the platform's answers, for types the
platform's convenience API will not return.**

## 2. What was measured, and on what

Every claim below was executed rather than read, on a Linux host and on a
Windows 11 machine (build 26200). Nothing here rests on documentation
alone, and three of the findings contradict what the documentation
suggested.

| platform | call | what comes back |
|---|---|---|
| Linux (glibc, musl) | `res_query` | the wire message |
| macOS, iOS | `res_9_query` | the wire message |
| Android ≥ 29 | `android_res_nquery` + `android_res_nresult` | the wire message |
| Windows 11 / Server 2025 | `DnsQueryRaw` | the wire message |
| Windows 10 | `DnsQuery_UTF8` | **parsed structures**, per type |

Four of the five hand over a DNS message. Windows 10 does not, and the
whole shape of this crate follows from that one row.

### 2.1 Windows 10 is the constraint, and all three escapes are closed

- `DnsQueryRaw` is **Windows 11 / Server 2025 and later**. Absent on 10.
- `DNS_QUERY_RETURN_MESSAGE` through `DnsQuery_UTF8` **changes nothing**:
  the same `DNS_RECORD` list comes back with the flag as without, and
  reading it as the documented `DNS_MESSAGE_BUFFER` yields a "header"
  that is the record's own `pNext` and `pName` pointers.
- The same flag through **`DnsQueryEx`** likewise changes nothing —
  identical `wType` and `wDataLength` with and without — and request
  structure versions 2 and 3 are refused outright with
  `ERROR_INVALID_PARAMETER`.

So on Windows 10 the answer arrives as whatever `DNS_RECORD`'s union
holds, and what it holds depends on whether the OS knows the type:

```
CAA    (257)  00 05 "issue" "letsencrypt.org"        ← RDATA verbatim
TLSA   (52)   03 01 01 | 00 | 20 00 00 00 | <32 B>   ← DNS_TLSA_DATA
DS     (43)   09 43 0d 02 | 20 00 00 00 | <32 B>     ← DNS_DS_DATA
DNSKEY (48)   00 01 03 0d | 40 00 00 00 | <64 B>     ← DNS_DNSKEY_DATA
```

Nothing in the record says which of the two it is. On a supported Windows
that would be a trap — a type the OS learns in an update would silently
turn from bytes into a struct, and a consumer reading it as RDATA would
start returning rubbish after a patch Tuesday. **Windows 10 is out of
support**, so its table cannot grow, and an enumerated set written against
it stays correct. That is the argument for the design below, and it is
the owner's, not mine: I had this as a reason the crate could not exist.

### 2.2 What `DnsQueryRaw` costs, and what it does not

It is asynchronous — a completion routine on a thread the caller does not
own — and `protocol` is **required**, with 0 refused: only
`DNS_PROTOCOL_UDP` (1) and `DNS_PROTOCOL_TCP` (2). The choice is
observable. Asking for CAA over UDP came back truncated, `TC=1`, zero
answers; over TCP the same query returned all eleven records, in a
message prefixed with RFC 1035 §4.2.2's two-byte length.

**It does not cost the cache**, which was the open question and is the
half most worth having: the cache is much of why a system resolver is
worth asking at all. Two measurements, because the first was not sound:

- After `Clear-DnsClientCache`, a `DnsQueryRaw` for an A record **created
  two entries** in the Windows resolver's own store, exactly as
  `DnsQuery_UTF8` does.
- For type 65, which `Get-DnsClientCache` does not display at all, two
  identical queries put **one** source port on the wire — counted with
  `pktmon` — so the second was answered without a packet.

The first attempt to answer this used TTL: a second query reporting a
lower TTL looked like a cache. It is not evidence, because an upstream
resolver's cache produces the same falling TTL while every query still
leaves the machine. The store and the packet counter distinguish them;
the TTL does not.

## 3. The seam

```rust
/// One record, as the wire carries it.
pub struct Record {
    pub name: String,
    pub rtype: u16,
    pub class: u16,
    pub ttl: Duration,
    /// The RDATA, exactly as it appeared — no interpretation.
    pub rdata: Vec<u8>,
}

/// What this build can ask for. **Not a constant**, unlike everything
/// else of this shape in `hclient`: on Windows the answer is decided by
/// `GetProcAddress` at run time, so a `const` would be a lie on one of
/// the two Windows.
pub enum Support {
    /// Any type at all — the four platforms that hand over a message,
    /// and Windows 11.
    Any,
    /// These types and no others, because each needs a translation
    /// written for it. Windows 10.
    Only(&'static [u16]),
}

pub fn support() -> Support;

/// Blocking. The platform calls are blocking or are made so; a caller
/// that needs otherwise runs this where blocking is allowed, which is
/// what `hclient`'s own `Blocking` seam exists for.
pub fn lookup(name: &str, rtype: u16) -> Result<Vec<Record>, Error>;
```

**`rdata` is bytes and stays bytes.** Interpreting them is the caller's
or another crate's; a resolver that also decoded records would have to
grow a type per RFC and would make every consumer wait for the one it
needs. `hclient-dns`'s SVCB decoder is the model: parsing lives beside
the consumer, not beside the syscall.

**`Support::Only` is a list, not a `bool`.** A caller that wants TLSA
needs to know whether *this* build can answer, and a single `false` on
Windows 10 would hide that CAA works there while SSHFP does not.

## 4. Why the answer is records rather than the message

Four platforms could hand over the whole message: header, question,
answer, authority, additional. Windows 10 cannot — it has already parsed
it. Making the message the common type would mean **synthesising** one on
Windows 10 from records that no longer carry a header, which is inventing
an rcode and flags nobody reported.

Records are what all five genuinely have. What is lost is the header —
`AA`, `TC`, and the rcode as such — and the loss is stated rather than
worked around: a caller that needs to tell `NXDOMAIN` from an empty answer
gets that through `Error`, and a caller that needs `TC` is doing something
this crate is not for.

## 5. The split, which is the only reason this is maintainable

`hclient-dns-system` already keeps it and it carries over unchanged:
**the half that touches the OS holds no decisions.** Every platform module
is a handful of lines that fetch bytes; every rule — how a message is
walked, what a truncated answer means, how a Windows structure becomes
RDATA — is a pure function tested on any host.

The Android module in `hclient-dns-system` is the sharpest example and the
one to copy: its untestable half is four JNI-free FFI calls, and its
testable half takes the lookup as a closure, so `platform()` hands it the
real call and a test hands it a table.

For Windows 10 this is what makes the enumerated set affordable: each
type's translation from `DNS_*_DATA` back to RDATA is a pure function over
a struct laid out in bytes, and the bytes above came from a real machine,
so the tests can be written from them.

## 6. What it does not do, each with the reason

- **No resolver of its own.** Then it would be `hickory`, which exists.
- **No caching.** The platform's is the point; a second one would answer
  from a different clock.
- **No decoding of RDATA.** §3.
- **No async API.** Every platform call is blocking or is made so, and an
  async wrapper over a blocking call is a thread pool with an opinion.
  A caller has one already.
- **No DNSSEC validation.** The platform validates or does not; reporting
  `AD` would mean carrying the header, which §4 rules out.
- **No Windows 10 support for types the OS parses into a struct this
  crate has not been taught.** Refused by name through `Support::Only`,
  which is the *silently ignored setting* defect avoided one level down.

## 7. Cost

| platform | crates added |
|---|---|
| Linux, macOS, iOS | **0** — `libc` is not even needed; the three symbols are declared directly |
| Windows | **0** — `windows-sys` with one feature |
| Android | **0** — the NDK symbols are in `libandroid`, linked by name |

No JNI: `android_res_nquery` is a C entry point, unlike Android's proxy
settings, which live behind a JVM and cost `jni` + `ndk-context`. That
asymmetry is worth knowing before assuming the two Android integrations
are alike.

## 8. Open questions

- **Does Windows 10 cache the types it returns as raw RDATA?** On Windows
  11 the raw path preserves the cache, proven by packet count. Windows 10
  takes a different call and this has not been measured — it needs a
  Windows 10 machine.
- **Which types does Windows 10 parse?** The four above are known. The
  rest of the enumerated set has to be established the same way, one
  probe per type, and the set is what `Support::Only` returns.
- **iOS.** `res_9_query` is in `libresolv` on both Apple platforms and the
  same code compiles for both, but nothing here has been run on iOS.

## 9. Name

`system-resolver`, `sysresolve`, `native-resolver`, `platform-dns` and
`resolv-sys` are all free on crates.io as of this writing. The argument
for a plain descriptive name is `hclient`'s own: for a crate somebody
finds by searching for what it does, legibility beats distinctiveness.
