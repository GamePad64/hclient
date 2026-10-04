# DNS and its decoders

> Part of the [AGENTS.md](../../AGENTS.md) record, split 2026-10-05. The root
> file keeps the operational rules and the index of parts. Sections below
> keep their original headings, and a "this file" in the text named the
> whole record when it was written.

### The decoder was chosen for its granularity, and the wrong one cost ninety lines

`hclient-dns-system` decodes HTTPS records with **`domain`** rather than
`dns-message-parser`, and the reason is not quality — the crate it
replaces is maintained, released a year ago and has no `unsafe` in its
`src`. It is that `dns-message-parser` exposes decoding at the **message**
level and nothing smaller (`decode(Bytes)`, one entry point), where
`system-resolver` hands over **RDATA**, because that is the only shape all
five of its platforms have.

So this crate used to build a **synthetic DNS response around every record
it wanted read**: a twelve-byte header, a question section, a
length-prefixed owner name per record, the TTL written back out — ninety
lines whose entire purpose was to be taken apart again by the next call.
`domain::rdata::svcb::Https::parse` takes a parser over one record's
octets, so the envelope has no subject.

**`hickory-proto` was the first candidate and the measurement is what
ruled it out.** It decodes RDATA directly too — `RData::read(decoder,
RecordType::HTTPS, Restrict::new(len))` — and it depends on `url`
**unconditionally**, for `caa.rs` and one error variant, a record type
nothing here asks about. `url` brings `idna` and the zerovec family,
`cargo tree -e normal`, unique crates:

| build | `dns-message-parser` | **`domain`** | `hickory-proto` |
|---|---|---|---|
| `hclient-dns-system`, Linux | 28 | **31** | 68 |
| `hclient-dns-system`, Windows | 31 | **34** | 71 |
| `hclient-dns-system`, macOS | 28 | **31** | 67 |
| `hclient` + `default-transport`, Linux | 92 | **94** | 103 |
| `hclient` + `default-transport`, **Windows** | 69 | **72** | **105** |

The Windows row is the one that decides it: `url`, `idna`, `zerovec`,
`yoke`, `tinystr` and the rest are **zero** there today, because
`hclient-idn` takes UTS 46 from `icuuc.dll` on purpose. `hickory-proto`
puts every one of them back, on the two platforms that crate exists to
keep them off. `domain` costs three — itself, `domain-macros` and
`octseq` — because `jiff` and `jiff-core` are already in the graph from
the cookie and cache date parsers, and it carries no `url` and no ICU at
all. It also takes `default-features = false`: `std` buys `hashbrown` and
`foldhash` for a map this path never builds, and `rand` a query id it
never mints.

**Three things the change bought, each pinned by a test.**

*An AliasMode record carrying SvcParams is ignored per RFC 9460 §2.4.1
rather than rejecting the whole RRSet* — and the test that pinned the old
behaviour named this outcome in advance: *"if this test ever fails,
upstream started honouring §2.4.1 and `endpoint_from_binding`'s AliasMode
branch becomes reachable with real parameters for the first time."* It
failed on the first run. Upstream did nothing; the branch had implemented
the MUST correctly since it was written and had never been reached with a
record that exercised it, because the decoder in front of it refused
those records first. **A rule can be right, tested and dead at the same
time**, and only a change underneath it says which.

*An RRSet larger than a DNS message can frame is answered rather than
refused.* The 65535-octet bound was the envelope's, never the resolver's,
and `SvcbLookupError::AnswerTooLarge` went with it — an error nothing can
raise is worse than no error, because a caller writes a match arm for a
case that cannot happen. `NameNotUsable` went the same way: it refused an
owner name with no *wire* form, and nothing writes a wire name any more.

*A compression pointer inside RDATA can now only reach this record's own
octets.* §2.2 forbids one there and a non-conformant sender may still
write one; the envelope had to **argue** that a pointer resolved against
a message this crate had assembled out of several records reached nothing
an attacker chose. Parsing where the bytes arrive makes that structural
instead of argued.

**And one hazard closed itself.** Two tests here existed only to pin
`dns-message-parser`'s `ServiceParameter` comparing and hashing **by key
number alone**, so that `assert_eq!(param, ServiceParameter::ALPN { .. })`
passed without checking a value. `domain` derives no `PartialEq` on
`AllValues` at all, so the trap cannot be sprung; what replaces the two
tests asserts that two records differing only *inside* a parameter come
out different, which is the property those tests were protecting.

**What it cost.** `SvcbLookupError::Malformed` carries
`domain::base::wire::ParseError` where it carried
`dns_message_parser::DecodeError` — a public change, free in a
pre-release and the reason to make it now. `bytes` left this crate's
direct dependencies with the envelope's one `Bytes::copy_from_slice`. And
RFC 9460 §2.2's *reject the entire RRSet* is now a `?` in a loop rather
than a property of a message-level decoder: the rule had to be **stated**
where it used to be inherited, which is the honest direction, since a
rule nobody wrote is a rule nobody can test.

**`hclient-dns` keeps its `codec` feature and `hclient-dns-system` no
longer asks for it.** That feature carries `binding_from_decoded`, the
decoder-shaped half of the conversion; the half that is shared —
`RawBinding`, `RawParam` and §2.4/§2.5/§8's client rules over them — is
behind no feature, because it names no decoder. `hclient-dns-doh` still
asks for `codec` and should: it decodes whole messages, which is what
that decoder is for. **Two decoders in one workspace is the cost of
having two granularities**, and it is the right cost — the alternative
measured above is one decoder and ICU on every platform.

**It keeps neither, and the sentence above had the split right and drew
the wrong line with it.** `binding_from_decoded` was `pub`, and its
signature named `domain` four times — the parameter, the error type and
both bounds — while `hclient-dns` re-exported no `domain` at all. So an
outside caller could not name the types the function demanded without
adding the crate to their own manifest at a matching version, which made
`domain`'s major version part of the `Resolve` seam's promise. The
function is `hclient-dns-doh`'s now, private, beside the
`Message::from_slice` that produces its input; the feature went with it
for want of a subject, and `hclient-dns` has no optional dependency left.

**Not a regression from the decoder swap, which is worth stating because
the timing invites the charge.** The parent commit's signature was
`pub fn binding_from_decoded(binding: &dns_message_parser::rr::ServiceBinding)`
— the same leak through a different foreign crate. Moving to `domain`
replaced one leaked type with another; what the swap did was make the
leak worth looking at, not create it. **The leak outlived the decoder it
leaked**, which is the durable form of this workspace's rule about a
claim being as perishable as its subject: here the *defect* was the thing
that failed to perish.

`ca5ab5e4` is the precedent and the argument is its: this is cheap now
and a major version later. Measured from outside rather than read — one
source file naming the old signature, with `domain` in the probe crate's
own manifest, compiles against the parent commit and is `E0425` against
this one, and rustdoc's rendered surface for `hclient-dns` has **zero**
item declarations naming `domain` where it had four.

What it costs is that `raw_param` — one decoded `SvcParam` to one
`RawParam`, twelve arms — is now duplicated between `hclient-dns-doh` and
`hclient-dns-system` rather than between `hclient-dns` and the latter.
Unifying it is the owner's call and deliberately not done: the only home
that serves both is `hclient-dns`, which would put the system resolver
back on a feature it stopped asking for and re-leak `domain` through the
seam this move cleared. Recorded at both sites, so a change to either is
a reason to read the other.

**That cost has since been refunded, and the sentence above is kept
because the reasoning is what expired rather than what was wrong.** It
was right that the two granularities are real and that `hickory-proto`
is the wrong way to unify them — re-measured, still about 60 crates with
`idna` and five ICU crates. What it did not check is whether the decoder
already chosen does **both**: `domain` exposes `base::message` beside
`Https::parse`, so `hclient-dns-doh` moved onto it and
`dns-message-parser` is gone from the workspace — no manifest, no code,
no lockfile entry. The acceptance of a second decoder was conditional on
one crate not covering both, and nothing forced a re-check of the
condition when the first half of the move landed.

**It is not a saving and the commit says so**: `hclient-dns-doh` goes
26 → 31 crates, three out (`base64`, `dns-message-parser`, `hex`) and
eight in (`domain`, `domain-macros`, `octseq`, `jiff`, `jiff-core`,
`hashbrown`, `foldhash`, `syn 2.0`). `jiff` is already in `hclient`'s
graph for the cookie and cache date parsers, and `hashbrown`/`foldhash`
are the price of `domain`'s `alloc` feature, which building a query at
all requires — measured by dropping it and watching three `E0599`s. One
duplicate was traded for another: `base64` 0.22 left with the old
decoder, which is the duplicate this file records `cargo deny` warning
about, and `syn 2.0` arrived under `domain-macros` beside `thiserror`'s
`syn 3.0`.

**The feature keeps its name with one caller rather than two**, because
what it gates is unchanged — whether this crate links a DNS codec at
all — and `codec` names that where `doh` would name the consumer.

**And then it kept neither name nor subject**: the one item it gated
leaked `domain` through a `pub fn` and went to that single caller, so
there is no flag and no optional dependency. See the section above; the
naming argument was sound and had about a day left to be right in.

Three things the swap changed rather than preserved, each pinned.
`RawParam::Ech` stopped putting RFC 9460 §7.3's length prefix back on,
because `domain` never strips it — the round-trip test that caught the
original stripping is what says the bytes `00 03 ab cd ef` still reach
`SvcbEndpoint::ech_config_list`. The question check compares **names**
rather than strings, so DNS 0x20 case-insensitivity is
`ToName::name_eq`'s `eq_ignore_ascii_case` rather than a fold of ours,
and the trailing-dot case is structural rather than trimmed. And
`decode_answer` borrows the body where it used to consume it, since
`Bytes` implements `domain`'s `Octets` only under a feature and a slice
needs none.

**And a comment claiming a check was load-bearing proved nothing until a
fixture grew a field.** `limit_to_in` filters answer records by class
where `limit_to` does not, and swapping them passed all 73 tests: the
DoH fixture wrote `CLASS IN` into every answer record it had ever built,
so no test could tell whether the field was read. `Rr::in_class` made the
distinction expressible and two tests now pin it — the wrong class is an
empty stream, the right one is an address. That is this file's own rule
about a line that reads as load-bearing and proves nothing, met from the
direction where the line was a comment the change itself had just
written.
