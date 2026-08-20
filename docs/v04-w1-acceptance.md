# v0.4 W1 — a transport that chooses: what it does, what it costs, and what it does not do

`docs/v04-design.md` §W1, deliverables 2, 3 and 4. Deliverable 1
(`RedirectSupport`) landed independently and is written up in the design
document itself. **Deliverable 4 (Alt-Svc — the slow tier) is now built**,
and §9 is its own section; §7 is kept as it was written, because what it
predicted the work would need is worth reading against what the work turned
out to need. Deliverable 5 (the race) is **not** built, and §7 says what it
would need rather than leaving the reader to infer it from its absence.

Written the way the other acceptance documents are: every claim carries how
it is known, and a claim that is not pinned by a test says so.

---

## 1. Where it lives, and why a crate

**`crates/hclient-select`**, one public type: `Selecting<R, T, D>`, owning a
`Native<R, T, D>` and an `H3<R, T, D>`.

A crate rather than a feature of either member, for the reason `hclient-h3`
is not a feature of `hclient-native`: **Cargo's features are additive**. A
`hclient-native/select` feature would put the whole QUIC stack — and the
`UdpBind + Spawn` bounds it needs from the runtime — into every build in
any graph in which any crate switched it on, including the builds that will
only ever speak HTTP/1.1. A crate is opt-in by being named.

Measured, `cargo tree -p hclient-select -e normal --prefix none`, unique
crates: **66**, against `hclient-h3`'s 57. The 9 are what
`hclient-native` adds that `hclient-h3` did not already have.

P2 is the condition, and it is inherited rather than declared here:
`hclient-rt-tokio` needs its `udp` feature, or `TokioHandle` does not
implement `UdpAdoptStd` and `Selecting<TokioHandle, …>` cannot be named at
all. That is not this crate's dependency — it is `H3`'s — and the failure is
a compile error at the call site rather than a weaker transport.

**The members are concrete rather than `A: Transport, B: Transport`.** A
generic pair would have to be *told* which member speaks HTTP/3 — nothing in
`Transport` or `Capabilities` says so — and a caller could hand it two
HTTP/1.1 stacks and get a transport that "chooses" between them on the
strength of a record neither honours. With the two named, that is
unrepresentable. The generality is not free to add later either, but it is
also not free of a decision: a third member (`hclient-urlsession`, §W3)
brings `RedirectSupport::Internal` and `owns_cookie_jar: true`, both of
which §2's rule **refuses** against either member here, so admitting it is a
capability question and not a type-parameter question.

---

## 2. The capability disagreements, measured

`Transport::capabilities` returns a `&Capabilities` (P3), so the answer is
stored at construction. What is stored is decided field by field by **one
rule**:

> The stored value must be a statement that is true whichever member serves
> the request.

Under a selecting transport the caller does not know which stack answered,
so a promise that holds for one and not the other is not a promise. The rule
never asks for an order over variants — P4's objection — it asks which value
is *true*, and where the answer is "neither", `Selecting::new` refuses,
naming the field.

### What actually disagrees today

Measured in this tree, `Native::new(rt, Rustls, dns)` against
`H3::new(rt, Rustls, dns)`, and pinned by
`the_two_stacks_disagree_on_exactly_six_fields_today` — **six fields, not
the two the design document's examples name.** The count has moved twice
and in both directions. It was six when this section was written;
`request_trailers` joined in v0.4 Appendix C, when `hclient-native`
stopped under-declaring what it sends, and that one runs the *other* way
from the rest — there the TCP member is the one that can. Then
`client_certs` **left**, and that is the more interesting of the two: the
row below records it as "same shape" as `full_duplex`, and it was not a
disagreement between two protocol stacks at all but between two
constants, one in each member, over a question neither was asking its
TLS backend. The stored answer is still the weaker claim. Two of its examples were
fixed under it while it was being written (`RedirectSupport::Configurable`
deleted in `b2289c4`; `version_select` turned on for both in `4e9805f`), and
four of the six were never in it.

| field | `hclient-native` | `hclient-h3` | stored | why |
|---|---|---|---|---|
| `full_duplex` | `false` | `true` | **`false`** | `false` asks the caller to assume less and forbids nothing. This is the answer `hclient-native` already gives one level down for the same question — HTTP/1.1 cannot do duplex, HTTP/2 can, one transport reports one value — beside the cost that decided it: over-claiming deadlocks a caller structured for bidirectional streaming, under-claiming costs a buffered copy. The rule is imported from the crate that already had to make it |
| `response_trailers` | `false` | `true` | **`false`** | same shape: `false` costs a caller trailers it would not have looked for, `true` would have it look for trailers on a connection that cannot carry them |
| ~~`client_certs`~~ | ~~`false`~~ | ~~`true`~~ | — | **Not a disagreement, and the row is struck rather than deleted because "same shape" was the wrong reading and is worth not repeating.** Neither member asked the component that knows: `hclient-h3` set `true` unconditionally — two lines under its own comment reading *"Read from the TLS backend, never from a constant"* — and `hclient-native` had no line, so it took `Capabilities::none()`'s `false`. Both were wrong, in opposite directions: `hclient-tls-native-tls` has an `identity()` setter and `Rustls::from_config` accepts a config built `with_client_auth_cert`, so the TCP path can present one; and the QUIC path claimed it for every `T`, including one that cannot. `TlsIdentity::presents_client_certs` is where it is answered now, and `a_client_certificate_is_reported_by_both_stacks_and_by_the_pair` is the test that could not have existed while this row did |
| `timeouts.first_byte` | `true` | `false` | **`false`** | and this is the field where the two disagree in the *other* direction. Declaring a bound one stack silently ignores is the exact no-op v0.2 W4 created `TimeoutSupport` to prevent |
| `timeouts.between_bytes` | `true` | `false` | **`false`** | same |
| `early_data` | `None` | `Supported` | **`Supported`** | the one field whose *stronger* value is the true one — see below |

#### The struck row's own mutations

Five applied against an anchor of **1500**, `--no-fail-fast`, restored
with `git checkout` and a fresh mtime. The point of the table is the last
row: without a control, four kills are indistinguishable from a harness
that reports killed unconditionally.

| # | mutation | verdict | killed |
|---|---|---|---|
| M1 | `Rustls::presents_client_certs` returns `true` always | killed | 2 |
| M2 | …returns `false` always | killed | 1 |
| M3 | `hclient-h3` back to `c.client_certs = true` | killed | 3 |
| M4 | `hclient-native` drops its line, taking `Capabilities::none()`'s `false` | killed | 2 — `client_certs_is_read_from_the_tls_backend_not_from_a_constant` and `a_client_certificate_is_reported_by_both_stacks_and_by_the_pair` |
| M5 | **control** — `NoTls` overrides the method with the value it already defaults to | **survived, as intended** | 0 |

M2 kills one where M1 kills two, and that asymmetry is the shape of the
defect rather than a gap: `false` is what three of the four fixtures
already expect, so only the fixture built to differ can see it.

`timeouts.connect` is `true` on both and stays `true` on the pair; a
conjunction that returned `false` for everything would satisfy every row
above, so `the_stored_answer_holds_whichever_stack_serves_the_request`
asserts it, along with `streaming_request_body`, `version_select`,
`version_reported`, `redirects`, `cancel_on_drop`, `connection_reuse` and
`tls_config`.

### Why `early_data` is the stronger value and not an exception

`EarlyDataSupport::Supported` says the transport **can** place a request the
caller marked with `AllowEarlyData` into early data. It promises nothing
about any particular request — `hclient-h3` alone already does not place the
*first* request to an origin there, because there is no session ticket yet.
So "this transport can offer early data for a marked request" is true of the
pair, and `None` — "this transport never offers early data" — is false of
it.

False in the direction that matters, too: **nothing in `hclient` reads this
field** (grep `crates/hclient/src` for `early_data`: no matches), so
reporting `None` would not stop a marked request reaching the QUIC stack and
going out in 0-RTT. The weaker-looking value is the lie.

The contrast that keeps this from being ad hoc is `CancelSupport`, three
rows down in the same function: `Supported` there is a **duty owed on every
dropped future**, so a member that does not owe it makes the claim false and
the pair is refused. An ability that need not be exercised and a duty that
must be are different kinds of claim, and the rule reads them differently
because they say different things.

The safety decision is untouched, and it is why this can be said at all:
early data is entered only for a request the *caller* marked, per request,
and this transport marks nothing on their behalf.

### What the constructor refuses

Every remaining enum (`redirects`, `cancel_on_drop`, `connection_reuse`,
`response_decompression`, `tls_config`), the two *the transport already does
this itself* flags (`owns_cookie_jar`, `owns_cache`), and
`forbidden_request_headers`.

The two flags are `bool`s and are still refusals, which is the case that
shows the rule is about what a value **says** rather than about its type:
`owns_cookie_jar: false` makes `Client` run a jar of its own (doubling up
against the member that keeps one) and `true` makes it run none (dropping
cookies for the member that does not). Neither is weaker; both are wrong.

`forbidden_request_headers` refuses because the type leaves nothing else.
The honest combination is the **union** — a header one member will not send
is one this transport may not promise to send — and `&'static [HeaderName]`
has nowhere to put a slice computed at construction. That is P3's wall from
the other side.

**Exactly one refusal is reachable from the two members this workspace
ships**, and it is an ordinary mistake rather than a contrived one:
`Native::without_pool()` (a documented setting — it restores v0.1's
one-connection-per-request behaviour) reports `ReuseSupport::None` against
`hclient-h3`'s `Supported`, and `ReuseSupport::None` is not a weaker
`Supported`: it says every request gets a fresh connection, which is false
the moment the QUIC stack answers one.
`a_pooling_disagreement_is_refused_at_construction_naming_the_field` reads
the field name and both values off the error, and
`the_same_two_stacks_with_the_pool_on_are_one_transport` is its control.

The other arms are pinned through `hclient_select::combine`, which is public
for exactly this reason: a rule whose arms can only be exercised by a member
that does not exist yet would otherwise ship unpinned.

### A field added to `Capabilities` later

There is no compile-time guard. `Capabilities` is `#[non_exhaustive]`, so a
destructuring `let` outside `hclient-core` needs `..` and cannot be made
exhaustive — a new field would simply arrive in `combine`'s output as
`Capabilities::none()`'s value, decided by nobody.
`every_capability_field_is_accounted_for_and_a_new_one_fails_this_test`
reads the field names off the derived `Debug` and fails when the set moves.
It is a tripwire, not a mechanism, and it is named after what it is.

---

## 3. The choice, and what it costs

### The rule, in order

1. **A `RequireVersion` demand outranks the record, and asks nobody.** Both
   members report `version_select: true`, so the pair does, and a transport
   reporting `true` owes an answer. `HTTP_3` goes to the QUIC stack;
   anything else goes to the TCP stack, *including* `HTTP_09` and `HTTP_10`,
   which it refuses — that refusal is the member's and reads the same as it
   does without this transport.
2. **`http://` is never QUIC**, and costs no lookup. HTTP/3 has no cleartext
   form, and RFC 9460 §9.5 makes an HTTPS record against a cleartext origin
   an instruction to *upgrade the scheme*, which is a redirect-shaped
   decision belonging to whoever owns the request.
3. **An IP literal is never QUIC**, and costs no lookup: it has no name to
   ask about, and `_443._https.127.0.0.1` is a query with no answer that
   every literal-addressed request would pay for.
4. **`Resolve::supports_svcb()` is asked**, not inferred from an empty
   stream — the distinction that method exists to carry.
5. **The record is fetched under RFC 9460 §2.3's name**: the origin's own at
   the default port, `_<port>._https.<host>` anywhere else.
6. **The lowest-priority ServiceMode record decides**, AliasMode records
   (`priority: 0`, every parameter empty) skipped. A lookup error is not
   fatal.

Two of those need a note because `hclient-native` decided them differently.

**The prefixed name.** `hclient-native`'s connector refuses to build it, and
says why: *"it would then have to decide what `lookup_ipv4`/`lookup_ipv6`
are asked for (the prefixed name has no addresses), and that is a
resolver-facing question the `Resolve` seam does not answer today"* — so it
applies discovery at the default port only. That reason does not reach here:
this transport reads **one bit** off the record and resolves no addresses
from it at all, so the question that stopped the connector never comes up,
and the alternative — using the default-port record's ALPN for a service on
another port — is what that rule exists to prevent.

**The first-ranked record, and only it.** RFC 9460 §2.4.2 makes priority the
operator's preference order, so an origin whose best endpoint is HTTP/2-only
has asked for HTTP/2 even where it also publishes an `h3` alternative.
Reading `h3` off *any* record would override that, and would let one
endpoint in an attacker-influenced answer decide the protocol for a whole
origin.

### What it costs in DNS

Counted, not reasoned about — `crates/hclient-select/tests/dns_cost.rs`
counts the calls a resolver received.

| request | type-65 queries | |
|---|---|---|
| `https://origin/` chosen onto TCP | **1** | **was 2**, see §3.1 — one for the choice and one for the connection, and they are the same one now |
| `https://origin/` chosen onto QUIC | **1** | unchanged — `hclient-h3` does no SVCB lookup at all |
| `https://origin:port/` | **1** | unchanged — the connector does no discovery away from the default port, so this transport asks for the prefixed record itself |
| any request carrying `RequireVersion` | **0** | unchanged |
| `http://` or an IP literal | **0** | unchanged |

The first row is the only one that moved, and that is the whole of the
claim: **one type-65 query per request that has a name to ask about,
whichever stack answers.** What decides the count is no longer which member
serves the request.

§W1's *"costs no new discovery at all — only the acting"* was true of the
**mechanism** and not of the count; it is true of both now.

**There is still no cache here, deliberately.** One remembered answer per
origin would turn one query per request into one per origin, and the reason
not to is `hclient-native`'s own, about the other half of the same problem:
*"this origin has no HTTPS record" is a DNS answer with a TTL of its own,
which `SvcbEndpoint` does not carry, and inventing a lifetime for someone
else's answer is how a resolver's cache and ours drift apart.* A resolver
that caches (`hclient-dns-hickory`, with the real TTLs) already removes the
cost; one that does not (`hclient-dns-doh`, by an explicit decision of its
own) would not have it removed by a second cache here that no caller can
turn off. **Nothing below needed one:** not asking twice *within one
request* needs no lifetime at all, which is what makes it a different
problem from the one that has no honest answer.

### 3.1 The duplicate, and the shape that closed it

Written up as a finding when this section was first written — *"closing it
is a change to `hclient-native`"* — with three candidate shapes. It is
closed now, and this is which one and why the other two lost.

**What was built: `hclient_native::Prefetch`**, a trait with two methods,
implemented by `Native` alone.

```rust
let prepared = native.prepare(req).await;   // the connector's own lookup, now
match prepared.discovered() {               // three states, see below
    Discovered::Record { alpn } => …,       // and then either
    …
}
native.execute_prepared(prepared).await     // …the same request, with the answer
```

`Selecting::route` calls it, reads the one bit it needs (`h3` in the ALPN
list), and hands the same value back on the TCP arm. The connector then
does not look again, because it is holding what it would have found.

**The record never crosses the seam as data.** `prepare` *fetches* it —
with `Native`'s own resolver, under `Native`'s own rule about where
discovery applies, gated by `Native`'s own negative cache, for the
authority of the request handed in. `Prepared` then owns that request, and
there is no method that replaces it and no constructor that pairs a record
with a request it was not fetched for (the fields are `pub(crate)`;
`Prepared::new` exists and sets *nothing looked up*). So **the wrong-origin
question is not answered by a check — it cannot be asked**, which is the
first of the four things this work was to be judged on.

That distinction is the reason the request-extension shape lost, and it is
not a stylistic one:

- **A request extension is the caller's channel.** `Timeouts`,
  `AllowEarlyData` and `RequireVersion` are all statements a *caller* makes
  about their own request, which a transport reads and may refuse. A record
  is evidence a transport would otherwise have fetched, and evidence in the
  caller's channel is evidence anyone who can build a request can forge.
- **And an HTTPS record is not only a protocol list.** `SvcbEndpoint`
  carries `port`, `ipv4hint`, `ipv6hint` and `ech_config_list`. An
  extension carrying one would let any code that can build a request send
  the connection to another port and another address — a thing nothing in
  this workspace can do today except DNS. The check that would be needed to
  stop it (carry the origin inside the extension, compare it with the URI)
  is exactly the shape the brief asked to avoid, and it would still be a
  check against a value the caller chose.
- **It would also have to live in `hclient-core`**, beside the other three
  extension types, where every transport that never resolves a name would
  meet it.

**"`Native` skipping its own discovery when told" is the same change seen
from the other end — but only if "told" carries the record.** A bare skip
is a *worse* thing wearing the same number: the record contributes a port,
address hints, an ALPN restriction and an ECH slot to the connection, so a
connector that skipped discovery because someone else had done it would
connect to the origin's own endpoint and silently lose all four. The count
would improve by dropping a capability. That failure has a test of its own
(`the_record_this_transport_fetched_is_the_one_the_connection_is_made_under`
in `crates/hclient-select/tests/record_handover.rs`) precisely because
`dns_cost.rs` cannot see it: a connector that took the answer and threw it
away asks exactly as few questions as one that used it. M49 in §3.3 is that
mutation.

**The memoising `Resolve` adapter loses on all three counts.** It changes a
*user's graph* rather than the library's behaviour, so the duplicate stays
in the library for everyone who does not know to wrap their resolver; it
cannot tell one request from the next, because `Resolve` has no notion of a
request, so a memo is either unbounded or needs a lifetime this workspace
has written down twice that it will not invent (the third judged point:
*do not build a cache*); and it is the wrong unit — the problem was one
request asking the same question twice, and a per-origin memory is an
answer to a different question.

### 3.2 "There is no record" is an answer, and it travels as one

The second judged point, and the half a plain `Option` gets wrong.
`Prefetched` (internal) and `Discovered` (what a caller reads) both have
**three** states, not two:

| state | what it means | what the connector does |
|---|---|---|
| `NotConsulted` | nobody looked, and nothing is ruled out: `http://`, a non-default port, an origin the negative cache is holding off, **or a resolver that says it cannot ask** | looks, exactly as it did before this existed |
| `NoRecord` | looked, and there is none to act on — none published, the lookup failed, every answer was AliasMode | **does not look** |
| `Record { alpn }` | the first-ranked ServiceMode record's ALPN list | uses it, and does not look |

**The capability belongs in the first row, and putting it there was a
change to `hclient-native`.** `Resolve::supports_svcb() == false` used to
be `discovery::lookup`'s own early `None`, indistinguishable from "asked
and found nothing" — which is right for the connector, where both mean
*no record to act on*, and wrong the moment the answer leaves the crate:
"my resolver cannot ask" is a fact about a resolver, not about an origin.
The gate moved one level out, into `discovered_endpoint`, and now answers
`NotConsulted`. It is reachable rather than theoretical, because
`Selecting::new` takes a resolver of its own: a TCP member on a resolver
that cannot do SVCB, beside a selecting transport on one that can, sent
every origin to TCP with a capable resolver sitting unused.

Collapsed into one `None`, the connector would re-query exactly the origins
whose answer cost the most to get: the ones that publish nothing, where a
resolver has to reach an authoritative answer to say so. That is M46 in
§3.3, and it is killed by one test and only one, which is the point of
having written it.

`NotConsulted` is load-bearing in the other direction too, and it is what
keeps `hclient-select` from owning a copy of `hclient-native`'s rule about
where discovery applies. The rule in the caller is now: **ask the
connector, because it was going to ask anyway; where it did not look, look
for yourself.** At a non-default port the connector answers `NotConsulted`
— the record there lives under `_<port>._https.<host>`, a name only
`hclient-select` constructs — and this transport then makes exactly the
lookup it always made. A copy of the gate would have been a second place
for it to live, and the two would have drifted into asking twice again or
into never asking at all (M51).

### 3.3 Mutation testing

Anchor **313 tests** — `cargo nextest run --no-fail-fast -p hclient-select
-p hclient-native --all-features`, 100 + 209 as they stood, plus the four
new arms in `record_handover.rs` — verified before the run **and again
after every restore**. Each patch had to match exactly once or the mutation
was not run. The harness reads the **names** of the failing tests and
refuses to score a run whose name count disagrees with nextest's own
`Summary`; restores are `git checkout` followed by `os.utime`, because a
restore that preserves mtime leaves cargo holding the mutant.

One more rule earned the hard way, and it is the one this session's brief
already gave: **commit before every run.** `git checkout --` restores a
file to `HEAD`, so an uncommitted edit in a mutated file is destroyed by
the restore rather than by the mutation — which is exactly what happened
once here, and showed up as a broken anchor two runs later. A second
episode is recorded under M54 below.

**Eleven applied: ten killed, one control survived as intended, none
survived unintentionally.**

| # | mutation | verdict | killed by |
|---|---|---|---|
| M45 | the handed-over record is ignored, so the connector queries again | **killed** (3) | `a_request_chosen_onto_tcp_at_the_default_port_asks_for_the_record_once`, `an_origin_that_publishes_no_record_is_not_asked_about_twice`, `the_record_this_transport_fetched_is_the_one_the_connection_is_made_under` |
| M46 | `Looked(None)` conflated with `NotConsulted` — "no record" read as "nobody looked" | **killed** (1) | `an_origin_that_publishes_no_record_is_not_asked_about_twice` |
| M47 | the record is fetched for the wrong origin: the port gate goes, so the default-port record is used at a URI that named its own port | **killed** (8) | `a_record_is_not_applied_to_a_uri_that_named_its_own_port` (`hclient-native`), `a_record_this_transport_fetched_under_a_prefixed_name_does_not_move_the_connection`, `away_from_the_default_port_only_this_transport_asks`, `an_ip_literal_has_no_record_to_look_up_and_is_served_over_tcp`, and 4 more |
| M48 | discovery is skipped when nothing was handed over — a plain `Native::execute` never looks | **killed** (14) | `the_port_from_the_record_is_where_the_connection_goes`, `the_address_hints_reach_happy_eyeballs`, `the_record_narrows_the_alpn_offer`, `a_failed_discovery_is_not_repeated_by_the_next_request`, `the_record_and_the_addresses_are_asked_at_once`, and 9 more |
| M49 | the record is dropped on the way over: `prepare` reports what it found and hands over nothing | **killed** (1) | `the_record_this_transport_fetched_is_the_one_the_connection_is_made_under` |
| M50 | `hclient-select` asks its own resolver instead of the connector, so the duplicate comes back | **killed** (3) | the three M45 killed |
| M51 | `NotConsulted` is read as an answer, so this transport never asks where the connector does not | **killed** (12) | `away_from_the_default_port_only_this_transport_asks`, `a_record_offering_h3_puts_the_request_on_the_quic_server`, `an_origin_with_no_record_is_served_over_tcp`, `the_first_request_is_tcp_and_the_second_is_quic`, and 8 more |
| M52 | an inert record counts as one in play (the `is_inert` filter moved and could have been lost with it) | **killed** (1) | `a_record_that_sets_nothing_does_not_buy_a_second_race` |
| M54 | a resolver that says it cannot ask is read as an origin that publishes no record | **killed** (1) | `a_member_that_cannot_ask_has_not_answered_and_this_transport_still_asks` — see below, because it survived first |
| M55 | the capability is not asked at all, so a resolver that says it cannot answer SVCB is asked anyway | **killed** (2) | `a_resolver_that_says_it_cannot_ask_is_not_asked` (`hclient-native`), `a_member_that_cannot_ask_has_not_answered_and_this_transport_still_asks` |
| **M53** | **CONTROL** — `Prepared`'s `Debug` reports a constant instead of what was discovered | **survived, as intended** (0) | nothing, and nothing should: no test formats a `Prepared` and no code path reads that `Debug`. Without a control, eight kills would be indistinguishable from a harness that reports "killed" unconditionally |

**M54 survived its first run, and the test was wrong rather than the
mutation harmless.** The arm written for it named a port and connected to a
real server; discovery's gates are checked in order — the port, the
negative cache, then the capability — so it never reached the third one at
all. It asserted the right thing and passed for the reason next door. The
fixture was not touched: the arm moved to the origin's *default* port,
which is the only place the capability gate is reached, and where nothing
can listen, so what it observes is the **question** on two resolver logs
rather than the connection. M54 then died to it, and M55 — the capability
not asked at all — died to it and to `hclient-native`'s own arm, which is
the pair one wants: the gate is in one place and both sides of it are
watched.

Three others are worth reading twice. **M46 and M49 are each killed by
exactly one test**, and neither test existed before this work: the suite
that counted queries could not see either, because both leave the count at
one. And **M47 is the wrong-origin mutation in the only form it can take**
— there is no way to construct a `Prepared` whose record belongs to another
request, so the nearest reachable defect is a connector that *fetches* the
wrong origin's record, which is what dropping the port gate does. The
structural claim itself is not a test and is not written as one: it is that
`Prepared`'s fields are `pub(crate)`, its only record-bearing constructor is
`Prefetch::prepare`, and it has no `request_mut`. That is checkable by
reading three declarations, and it is stated here rather than claimed to be
pinned.

---

## 4. How it is checked

Two real servers behind **one authority**: an HTTP/3 `quinn` endpoint on UDP
and a `tokio-rustls` HTTP/1.1 listener on TCP, on the **same port number**
(the two are separate port spaces; the port is found by binding TCP and
retrying until the same number is free on UDP). Both present the same
certificate, both are alive in every test, and the only thing that differs
between arms is the record the resolver hands back — so a request reaching
one of them is a *choice*: the other was reachable and was not chosen.

Every assertion is causal. Nothing waits for a duration and then concludes;
the observations are all "this server answered and that one did not", which
is settled by the time the response is in hand. Each request carries a
10-second wall-clock bound, which is never an assertion — it is there so a
mutation that turns a choice into a hang is red rather than eternal.

`crates/hclient-select/tests/`: `capabilities.rs` (10), `choice.rs` (12),
`dns_cost.rs` (3), `body.rs` (3), plus the two fixtures. **28 tests.**

> As of deliverable 4 the crate has **100**, and `dns_cost.rs` has 5 —
> §9.7 for the census and §9.6 for the arms added there. As of §3.1's
> handover it has **104**: `record_handover.rs` (4), which watches the
> connection and the resolver's log rather than the query count alone. The counts in this section and
> the anchor below are the ones the run in §5 was made against and are left
> as they were, because a mutation table is only readable beside the suite
> it was run over.

---

## 5. Mutation testing

Anchor **28 tests**, `cargo nextest run -p hclient-select --all-features`,
all passing, verified before each run. Each mutation was applied at one
site — the patch had to match exactly once or it was not run — the whole
crate suite was run, and the **names** of the tests that turned red were
read off the output.

**One methodological note, because it nearly produced a false table.** The
first run scored all nineteen as *survived with zero failures*. They had
not survived: the scraper's regular expression did not match nextest's
`FAIL` lines, and the summary line's own failure count was being ignored.
The run was repeated with the count cross-checked against nextest's
`Summary`, which is what the rows below are. Reading killers rather than
counting them is the rule that caught it.

| # | mutation | verdict | killed by |
|---|---|---|---|
| M1 | the record's ALPN is ignored, so `h3` is never chosen | **killed** (5) | `a_record_offering_h3_puts_the_request_on_the_quic_server`, `the_first_ranked_record_decides_when_it_is_the_h3_one`, `an_alias_record_does_not_out_rank_the_service_record_behind_it`, `a_request_chosen_onto_quic_at_the_default_port_asks_once`, `one_client_reaches_both_servers_depending_only_on_the_record` |
| M2 | `h3` is chosen whenever a record exists, offered or not | **killed** (5) | `a_record_that_does_not_offer_h3_puts_the_request_on_the_tcp_server`, `the_first_ranked_record_decides_and_a_lower_one_does_not_override_it`, `a_request_chosen_onto_tcp_at_the_default_port_asks_for_the_record_twice`, `away_from_the_default_port_only_this_transport_asks`, `one_client_reaches_both_servers_depending_only_on_the_record` |
| M3 | the stored answer is the TCP member's capabilities, not the pair's | **killed** (7) | `the_stored_answer_holds_whichever_stack_serves_the_request`, `a_pooling_disagreement_is_refused_at_construction_naming_the_field`, `a_capability_only_one_member_has_is_not_promised_by_the_pair`, `a_disagreement_on_any_unordered_enum_is_refused_and_names_its_field`, `owning_a_jar_or_a_cache_is_a_refusal_rather_than_a_conjunction`, `either_member_offering_early_data_is_enough_for_the_pair_to_offer_it`, `two_different_forbidden_header_lists_have_no_honest_union_to_store` |
| M4 | …the QUIC member's, the other direction of M3 | **killed** (7) | the same seven |
| M5 | the constructor never refuses (`same` always returns `Ok`) | **killed** (3) | `a_pooling_disagreement_is_refused_at_construction_naming_the_field`, `a_disagreement_on_any_unordered_enum_is_refused_and_names_its_field`, `owning_a_jar_or_a_cache_is_a_refusal_rather_than_a_conjunction` |
| M6 | an AliasMode record is not skipped | **killed** (1) | `an_alias_record_does_not_out_rank_the_service_record_behind_it` |
| M7 | any record's ALPN decides, not the first-ranked one | **killed** (1) | `the_first_ranked_record_decides_and_a_lower_one_does_not_override_it` |
| M8 | `Resolve::supports_svcb` is not asked | **killed** (1) | `a_resolver_that_cannot_do_svcb_never_chooses_quic` |
| M9 | a `RequireVersion` demand is ignored | **killed** (2) | `a_demand_for_http_3_is_served_over_quic_without_a_record_or_a_lookup`, `a_demand_for_http_1_1_is_served_over_tcp_although_the_record_offers_h3` |
| M10 | any `RequireVersion` demand routes to QUIC | **killed** (1) | `a_demand_for_http_1_1_is_served_over_tcp_although_the_record_offers_h3` |
| M11 | `http://` may choose QUIC | **killed** (1) | `a_cleartext_origin_is_not_offered_to_quic_and_costs_no_lookup` |
| M12 | the record is always looked up under the bare host | **killed** (2) | `a_record_offering_h3_puts_the_request_on_the_quic_server`, `away_from_the_default_port_only_this_transport_asks` |
| M13 | `early_data` takes the weaker value instead of the stronger | **killed** (2) | `either_member_offering_early_data_is_enough_for_the_pair_to_offer_it`, `the_stored_answer_holds_whichever_stack_serves_the_request` |
| M14 | `full_duplex` takes the disjunction instead of the conjunction | **killed** (2) | `a_capability_only_one_member_has_is_not_promised_by_the_pair`, `the_stored_answer_holds_whichever_stack_serves_the_request` |
| M15 | `timeouts.first_byte` takes the disjunction | **killed** (2) | the same two |
| M16 | an IP literal is looked up like a name | **killed** (1) | `an_ip_literal_has_no_record_to_look_up_and_is_served_over_tcp` |
| M17 | `SelectedBody::is_end_stream` is not delegated | **killed** (1) | `both_variants_report_the_members_own_end_of_stream_and_size_hint` |
| M18 | `SelectedBody::size_hint` is not delegated | **killed** (1) | the same |
| M19 | both body variants poll the TCP one | **killed** (7) | `both_variants_yield_their_own_frames`, `a_body_error_arrives_with_its_kind_intact_from_either_variant`, and five of the QUIC-arm choice tests |

**Nineteen applied, nineteen killed, none survived.** M17 and M18 were run
against a *substituted* default body rather than a deleted method: deleting
one leaves its doc comment attached to nothing and the crate stops
compiling, which would have scored as "killed" for a reason that has nothing
to do with the tests.

---

## 6. What is not verified

- **`SelectedBody`'s `Unpin` bound is a fact about today's members**, not a
  proof. Both are `Unpin` — `hclient::Deadline` requires it of any body a
  `Client` wraps, so a body that were not could not reach a caller through
  this library at all — and the bound is what lets the projection be
  `Pin::new(&mut …)` in a crate that forbids `unsafe`. Nothing checks that a
  future member body stays `Unpin`; a member that stopped being one would be
  a compile error at the `Transport` impl, which is the honest failure but
  is not a test.
- **`Transport::to_error` is the identity here and no test distinguishes it
  from the default**, which recognises an `Error` and passes it through
  unchanged. The line states the intent where it is read and survives the
  default changing; that is the same position `hclient-native` and
  `hclient-h3` take about their own, and it is equally untested there.
- **Cancellation is not tested through this transport.** Dropping a
  `Selecting::execute` future drops the member's future, whose `Drop` is the
  one that matters, and both members' cancellation is measured in their own
  suites from the far end of the wire. What is not checked is that this
  wrapper adds nothing between them — there is no `select`, no spawn and no
  buffering in `execute`, so there is nothing that could, but "nothing could"
  is an argument rather than a measurement. **What §7.6 adds is one level
  down and in a place neither member's suite reached**: dropping an `H3`
  connect *mid-handshake* against a black hole, which is the only moment a
  race ever cancels one. It is prompt — one goodbye datagram 1.3 ms later
  and then silence. Still not through `Selecting`.
- **No live network.** Every server here is on loopback. The claim "a real
  origin publishing an HTTPS record with `h3` is reached over HTTP/3" is not
  made; what is made is that the record's ALPN decides which of two real
  servers is reached.
- **0-RTT through this transport is not exercised.** `early_data` is
  reported `Supported` and the reasoning is in §2, but no test marks a
  request with `AllowEarlyData` and watches it enter early data through
  `Selecting`. `hclient-h3`'s own suite does that for the member.

And four about the handover (§3.1), each of which is a claim this document
makes and a measurement it does not:

- **That a record cannot be paired with a request it was not fetched for is
  a property of the type, not a test.** `Prepared`'s fields are
  `pub(crate)`, its only record-bearing constructor is `Prefetch::prepare`,
  and it has no `request_mut`. Three declarations, checkable by reading
  them; no mutation can be applied to a constructor that does not exist.
  M47 is the nearest reachable defect (the *connector* fetching the wrong
  origin's record) and it is killed, which is a different claim.
- **The timing change is reasoned, not measured.** `prepare` fetches the
  record before the pool is consulted, where `execute` fetches it only when
  a connection is opened and does so beside the address lookups. Through
  `Selecting` that costs nothing new — this transport already made a query
  per request, and now it is that one — but a *direct* `Prefetch` user with
  a warm pool pays one query per request where `execute` would have paid
  one per connection. It is written where the method is; no benchmark
  stands behind it, and the ~400 ms figure v0.3 W2 measured for serialising
  discovery in front of the addresses is about a different arrangement.
- **The negative cache is read by `prepare` and not re-read at connect
  time.** A record fetched microseconds before another request's failure
  marks the origin will still be used for this one. Self-correcting and
  bounded by one request; not tested.
- **A caller that prepares and then never executes is not exercised.** The
  `Prepared` is simply dropped; there is nothing to clean up (no query is
  outstanding by then, and nothing was spawned), but no test drops one.

---

## 7. Alt-Svc and the race: what each would need

Both are named in `docs/v04-design.md` §W1 as deliverables 4 and 5, after
this one and in that order, and the order is the finding rather than a
schedule. **Alt-Svc is built** — §9. **The race is not, and its blocker has
been discharged**: it was a measurement, the measurement has been made, and
§7.1–§7.7 below say what the numbers argue for. This section is written as
prediction-then-finding for both, because in each case what the work needed
differed from what it was expected to need, and in the race's case one of
the four predictions was the reason the work had not started.

### Alt-Svc — the slow tier (deliverable 4) — **since built, see §9**

This subsection is what was predicted before the work; §9 is what the work
found. Four of the five bullets held. The one that did not is the negative
half, and it did not hold in the direction the bullet expects: the failure
cache `hclient-native` already has is **not** the one this needed, and the
one this needs is still not built — §9.3.

It is a **response header**, so it can only help the *next* connection: the
first page load at an origin is never HTTP/3 through Alt-Svc, which is
exactly why the HTTPS record exists and why it came first here. What it
needs, and none of it is in this crate:

- **A parser** for RFC 7838 §3's field value — `h3=":443"; ma=86400;
  persist=1`, a list, with quoted alt-authorities and parameters. Closest
  precedent in this tree: `hclient-cookie`'s `Set-Cookie` parsing, which is
  sans-io and clockless for the same reasons this would want to be.
- **A cache**, and this is the part that makes it deliverable 4 rather than
  3. Keyed by origin, holding the advertised protocol and alt-authority,
  with `ma` as its lifetime — a real TTL, supplied by the server, which is
  the thing `SvcbEndpoint` does not carry and the reason §3 gives for having
  no cache at all today. It needs `Timer` (the seam through which time
  reaches a transport here — `hclient-native`'s negative cache is the model,
  including its refusal to use `std::time::Instant::now()` so that a caller
  testing under `tokio::time::pause()` sees what the transport sees).
- **A negative half**, and the argument for it is already written in
  `hclient-native`'s `discovery` module: *the cache of what was advertised
  is Alt-Svc's, the cache of what failed is the connector's*, and the
  advertisement's source does not change what a blocked port costs. So the
  failure half may already have a home, and the first task is to check
  whether it does rather than to build a second one.
- **A scope decision.** `persist` and clearing on network change are in RFC
  7838 §3.1; a cache that survives a laptop moving between networks
  advertises an alt-authority that was reachable somewhere else.
- **A place to read the response header at all**, which this crate does not
  have today: `execute` hands the member's response straight back and looks
  at nothing in it. That is one match on `alt-svc` in the response head, and
  it is the smallest part of the work.

### The race — a hedge, not a chooser (deliverable 5) — **measured, and since built**

> **Built.** [`docs/v04-race.md`](v04-race.md) is the work.
> `hclient_select::Selecting::hedging(head_start)`, off until it is called.
> Everything below is what this section predicted and argued for **before**
> the staged connect existed, and it is kept because one of its central
> claims has since stopped being true — §7.6's *"a race made of two
> `Transport::execute` calls races requests, not connections"*, which was
> the reason nobody had built this. The race is not made of two `execute`
> calls; it is made of two `StagedConnect::connect` calls, neither of which
> writes a request byte. `docs/v04-race.md` §1 is how that was established
> and §2 is what it did to the head start (its floor moved to zero; its
> value did not move at all). §3 there also re-measures the TCP rows of
> §7.3 below, which predate `TCP_NODELAY`, and finds the two stacks'
> loopback order **reversed**.

P12 is emphatic that it is a **third** thing: applied *after* the choice, as
a hedge against a network that blocks UDP/443, not as a way of choosing.
Its blocker was a measurement — v0.3 W2's *"the size of the cost is
unverified"*, and `docs/v04-design.md` §W1's *"measure before choosing a
policy, not after"*. **The measurement exists now.** What follows replaces
this subsection's original four-bullet list of what the race would need
with what it does need, and each answer carries the number behind it.

The four bullets are kept, one line each, at the end — because two of them
turned out to be wrong and one of the two was the reason nobody had done
this.

#### 7.1 The fixture blocker was not a blocker, and the sentence that said so was wrong

> *"A fixture that can actually block UDP/443, which loopback cannot."*

It can, and one has been sitting in this repository since v0.3.
`crates/hclient-h3/tests/live.rs`'s `black_hole()` binds a UDP socket, never
answers, and **holds** it. Holding it is the whole thing: an *unbound* port
makes the kernel send ICMP port-unreachable, which is a firewall `REJECT`; a
bound socket that never replies emits nothing at all, which is a `DROP` —
the target a network blocking UDP/443 actually uses, and the one whose cost
the policy needs.

So the second bullet was wrong on its own terms, and the correction is not
cosmetic: it is why the measurement had not been made. No packet filter, no
namespace and no `tuntap` device is needed.

**One case loopback genuinely cannot produce, and it is not the one that
mattered.** A middlebox that *refuses* — sends an ICMP error from somewhere
along a path — cannot be staged on a single host with a real path in front
of it. The nearest reachable approximation is a port with nothing bound at
all, where the local kernel answers ICMP itself. It was measured too (§7.3,
M4b), and the result is that the distinction makes **no difference to this
client**: both cost 30.003 s. That is checked in both directions —
`/proc/net/snmp` shows `Icmp OutDestUnreachs` rising by exactly 5 for 5
datagrams sent to an unbound loopback port, and `InDestUnreachs` by 5 as
well, so the ICMP is emitted *and* delivered — and quinn ignores it: neither
`quinn-udp` 0.5.15 nor `quinn` 0.11.11 contains `IP_RECVERR`,
`ECONNREFUSED` or any other read of the socket error queue.

#### 7.2 Where the harness is and how the numbers were taken

`crates/hclient-select/tests/race_cost.rs`. Every test in it is
`#[ignore]`d and **prints rather than asserts**, which is the only shape a
timing-based harness is allowed here: the file's output is numbers, not a
verdict, so it cannot become the fourth flake. Run it with

```text
cargo nextest run -p hclient-select --test race_cost --run-ignored all \
    --no-capture -j1
```

No CI job runs it and none should; it takes about three minutes, almost all
of it spent waiting for quinn to give up. One verbatim run is kept in
`docs/measurements/v04-race-cost/`, as a receipt for the tables below and
explicitly not as a baseline anything compares against.

The fixture is two real servers behind one authority on the same port
number, the shape `tests/servers.rs` already established — and duplicated
inside `race_cost.rs` rather than shared, because a fixture two suites own
is a fixture neither can change. Where a run needs UDP to be blocked, the
QUIC server is replaced by a `black_hole` that timestamps every datagram it
receives; that log is what makes the client's own retransmission schedule
observable from outside it.

**What depends on the machine, and what does not.** Every figure was taken
twice, `--release` and debug, on an Intel i7-14700K, Linux 7.0.0-29,
`rustc` 1.97.1, over the v4 loopback. The two builds disagree by roughly an
order of magnitude on everything that is **CPU** — a QUIC handshake is
1.77 ms in release and 12.1 ms in debug — and agree to a millisecond on
everything that is a **timer**, which is every number the policy actually
turns on. The release figures are quoted below; a debug figure is given
where the difference is the point.

Two limits of loopback, stated rather than papered over. There is **no
round trip**, so every success figure is a floor: the cost with the network
removed. And there is no competing traffic, so nothing here says what the
hedge costs a loaded link. Neither limit touches the failure numbers,
because — and this is the first finding — the failure costs are not
functions of the path at all.

#### 7.3 The four measurements

**M1 — what a QUIC connect to a black hole costs today, end to end.**
`hclient_h3::H3::execute` with no `Timeouts::connect` set, three runs:
**30.005 s, 30.003 s, 30.005 s** (debug), **30.002 s, 30.002 s, 30.006 s**
(release) — a spread of 4 milliseconds across six runs on two profiles.
v0.3's 30 s is confirmed for this shape rather than reused.

The mechanism, read after measuring: `quinn_proto::TransportConfig::default()`
carries `max_idle_timeout: Some(VarInt(30_000))`, and `H3` never sets it —
it builds a fresh default `TransportConfig` when a keep-alive interval is
configured (the default 5 s) and touches nothing else. So the 30 s is
quinn's, inherited, and is the same number on every path.

The cost in bytes is **9 datagrams of 1200 each — 10.8 KB — per abandoned
attempt**, from the ladder below.

**M2 — how early is the earliest honest signal.** Two different questions,
and only one of them has an answer that scales with the network.

The Initial retransmission ladder, read off the black hole (release; the
debug ladder is the same to three decimal places, which is the point —
these are timers, not work):

```
+674.360µs  1200 bytes      the ClientHello
+  1.002s   1200 bytes  ×2  PTO 1
+  3.001s   1200 bytes  ×2  PTO 2
+  6.998s   1200 bytes  ×2  PTO 3
+ 14.991s   1200 bytes  ×2  PTO 4
  gave up after 30.005s
```

**The first PTO is at 1.001 s and it is a constant.** A client with no RTT
sample has to guess one: `quinn_proto`'s `initial_rtt` is
`Duration::from_millis(333)` — RFC 9002 §6.2.2's `kInitialRtt` — so
PTO₀ = 333 + 4×166.5 ≈ 999 ms whatever the path is. That is the earliest
moment anything in the stack has *evidence* that something is wrong;
everything before it is the client waiting exactly as it would on a slow but
working path. **A race cannot derive a head start shorter than 1 s from any
signal the QUIC stack produces**, because there is none.

Against that, what a working exchange costs, cold. Each row is n=10 within
a run; the range is the **median across three separate release runs**,
which is quoted rather than one run's figure because these move by a factor
of two between runs and the failure numbers above do not move at all — that
contrast is itself the point:

| exchange | median across three runs |
|---|---|
| QUIC by name — UDP + TLS 1.3 + h3 SETTINGS + GET + body | **1.8 – 3.3 ms** (best single sample 0.67 ms) |
| QUIC by IP literal (no resolution) | 2.1 – 3.0 ms |
| TCP by name — SYN + TLS 1.3 + GET + body | **41.8 – 42.5 ms** |
| TCP by IP literal (no resolution) | 41.6 – 42.7 ms |
| TCP with `TcpOpts { nodelay: true }` | **0.46 – 1.3 ms** |
| TCP connect to a closed port (RST) | 0.02 – 0.16 ms |

So the gap the policy is choosing against is **a couple of milliseconds
against 30.002 s** — four orders of magnitude, and only the small one moves
at all: with the run, and on a real network with the path.

**The 41 ms in the TCP rows is a finding and not a floor**, and it is the
one figure here that does not move: 40.5 to 45.5 ms in every sample of
every run, debug and release alike. It is entirely in the head —
`execute`-to-head 42.9 ms, head-to-end-of-body 8.7 µs — it is unchanged by
an IP literal, so it is not RFC 8305's `resolution_delay`, and it is
unchanged between debug and release, so it is not crypto. It is **Nagle
meeting the peer's delayed ACK**: `hclient_rt::TcpOpts::default()` is
all-off — *"the user turns nodelay on, not us"*, `caps.rs` — so every
`Native` connection this workspace makes carries Nagle, and turning it off
takes the same head to 0.72 ms. Recorded in §8 as a finding for
`hclient-native`'s neighbourhood rather than fixed here.

**M3 — what a race costs when QUIC wins.** The hedge's whole justification.
Four head starts against a live QUIC origin, both members real, counters
read **twice** — at the instant the loser is dropped and one second later,
because only the difference between the two readings distinguishes "the
loser had already got that far" from "the loser kept going" (release):

Six arms — three runs (one debug, two release) × Nagle on and off, because
one run's row would read as deterministic and this is the one measurement
here that is not:

| head start | TCP socket opened at the origin | origin got a complete request | TCP arm won outright |
|---|---|---|---|
| **0 ms** | **6 of 6** | **5 of 6** | 2 of 6 |
| **1 ms** | 5 of 6 | 4 of 6 | 1 of 6 |
| 50 ms | **0 of 6** | 0 of 6 | 0 of 6 |
| 300 ms | **0 of 6** | 0 of 6 | 0 of 6 |

The QUIC arm answered in every one of the twenty-four, in 1.1 to 7.7 ms.

**With no head start the losing arm delivers a complete, well-formed HTTP
request to the origin**, and the counters read twice are what establish it
rather than infer it. With Nagle on, the server had accepted a socket and
read no request at the instant of the drop, and had read and answered one a
second later: the bytes were in the kernel before the future died and the
close could not recall them. With `nodelay: true` the request is answered
*before* the drop instead of after it — so Nagle is the reason it arrives
late, not the reason it arrives.

At 50 ms and above the TCP arm opens no socket at all, in every run. The
boundary between "cost nothing" and "sent the request twice" is sharp, and
it sits between 1 ms and 50 ms on a machine where a QUIC handshake takes a
couple of milliseconds — which is to say it sits at roughly *one QUIC
handshake*, wherever that happens to be.

**M4 — what it costs when QUIC loses.** Both failures, and the premise that
they are different ones is wrong.

| origin | QUIC arm alone, unbounded | race, head start 0 | race, head start 300 ms |
|---|---|---|---|
| UDP black hole (`DROP`) | 30.002 s | tcp wins in 42.9 – 45.5 ms | tcp wins in 343.5 – 344.3 ms |
| nothing bound on UDP (`REJECT`, ICMP) | **30.001 – 30.003 s** | tcp wins in 45.0 – 50.9 ms | tcp wins in 343.6 – 347.2 ms |

**Both time out, at the same 30 s.** An origin with no HTTP/3 server at all
is indistinguishable to `hclient-h3` from a firewall dropping every packet
— §7.1 has the mechanism. So there is no cheap "this origin has no h3"
signal to build a policy on; a race, or a memory, or nothing.

With the race, the caller's cost is `head start + the TCP exchange`, which
is exactly additive: 44.1 ms at zero, 344.2 ms at 300 ms, 1.044 s at 1 s.
The bound is `Timeouts::connect`, and it is honoured tightly — M1b, the
same black hole with a bound set, overshoots by **0.5 to 2.0 ms** at 100 ms,
300 ms, 1 s and 3 s.

#### 7.4 The delay

**A constant, but only because nothing here can observe the thing it should
be derived from.**

The head start cannot be derived from the failure side. The failure cost is
a *constant* — 30 s, the same on every path — and the first signal within it
is also a constant, 1.001 s, because it comes from a guessed RTT and not a
measured one. Neither is a bound on anything a policy wants: a head start of
1 s would put a full second on every request to a UDP-blocked origin, which
is worse than never trying QUIC at all.

It must therefore be derived from the **success** side, and that is the one
number that scales with the path: a QUIC handshake that will succeed does so
in ≈ 1 RTT plus the 1–3 ms of crypto measured above. The head start has to
exceed that, or M3's zero-head-start row is what happens — the hedge stops
being a hedge and becomes a coin toss that duplicates requests.

The honest derived form is **an RTT observation for that origin**, and
`hclient-select` has nowhere to keep one. The Alt-Svc cache added in §9 is
keyed by origin and holds a server-supplied lifetime; an RTT is neither
server-supplied nor covered by `ma`, so it is a second store rather than a
field. Until that exists the head start is a constant, and the constant this
workspace should use is **250 ms** — not a new number, but
`hclient_proto::happy_eyeballs::HeConfig::default()`'s `attempt_delay`,
which is RFC 8305 §5's recommended Connection Attempt Delay and is already
the answer this codebase gives to the structurally identical question one
layer down ("how long do I give the preferred family before trying the
other"). Reusing it puts two orders of magnitude between the head start and
the QUIC floor, and two more between it and the failure.

**The head start is not a latency knob. It is the safety mechanism**, and
§7.6 is why.

#### 7.5 The budget

`Timeouts::connect` is one deadline, `C`. Let the head start be `H`. The
arithmetic the measurements allow:

- The QUIC arm gets `C`. The TCP arm starts at `H` and must still finish by
  `C`, so it gets **`C − H`**, not `C`. Handing each arm a full copy is the
  mistake `Client`'s `425` replay already had to avoid — *"a bound a server
  can double by answering `425` is not a bound"*.
- **`H < C` is a precondition, not a preference.** A caller setting
  `connect: Some(100ms)` with a 250 ms head start gets no hedge at all: the
  QUIC arm ends at 101.2 ms (measured, M1b) and the TCP arm has not started.
  That must be refused or documented; silently degrading to "no race" is the
  shape this workspace refuses elsewhere by name.
- `C − H` must be large enough for the fallback to complete. On this
  workspace's defaults that is **at least ~45 ms on loopback**, because of
  the 41 ms of delayed ACK in §7.3 — a fallback budget that cannot cover the
  fallback is not a budget. The fix is `nodelay`, not a larger constant.
- Nothing needs to be reserved for tearing the loser down. Both directions
  measured: the QUIC arm's goodbye is one datagram 1.3–2.4 ms after the drop,
  and `Timeouts::connect` itself overshoots by 0.5–2.0 ms.

#### 7.6 Cancellation

**The QUIC arm cancels promptly, and the spawned driver does not outlive
it.** Dropped mid-handshake at 50 ms (before the first PTO), at 1.5 s and at
3.5 s, the black hole saw in every case **exactly one further datagram,
1.3–2.4 ms after the drop, and then silence for the whole 5 s watched**.
The datagram is 1200 bytes — quinn pads any client datagram containing an
Initial (RFC 9000 §14.1) — but it is a goodbye and not a retransmission, and
the ladder settles it: a retransmission would have been due at the next PTO
and would have been followed by more, where this is followed by nothing.
Dropping the whole `H3` as well as the future changes nothing. The driver
question has an answer that is almost a tautology once stated: at that
moment there is no connection, so there is no driver — `H3` spawns one after
`connect` returns. `CancelSupport::Supported` holds where the race needs it.

**The TCP arm does not cancel in the sense the race needs, and this is the
finding.** Dropping the future stops *this side*, exactly as
`Transport::execute`'s MUST requires — and the origin still received a
complete HTTP request in nine of the twelve zero- and one-millisecond arms,
with Nagle on and with it off. That is not a defect in `hclient-native`, and
it cannot be fixed there: it is what `execute`'s own
doc says the promise excludes: *"This is a claim about this side, and
deliberately not about the server's — the request may already have arrived
and already have been acted on."*

So the fourth bullet of the original list — *"Both members already honour
it, so the work is arranging the drop rather than making it mean
something"* — is **half wrong**, and it is the half that matters. Arranging
the drop is not the work. The work is not sending in the first place:

> **A race built out of two `Transport::execute` calls races requests, not
> connections.** Browsers race *connections* and send the request on the
> winner. `Transport` has no connect-only entry point — there is nowhere in
> the seam to say "open a connection and do not send this yet" — so any race
> assembled from the members as they stand duplicates the request at the
> origin whenever the loser gets far enough.

That makes an unconditional race unsafe, and it is unsafe in the way this
workspace already has vocabulary for and already knows the vocabulary is
insufficient: `RequestBody::retry_kind()` answers *"can I send this again"*,
not *"may an attacker send this again"*, and `POST /transfer` with a
buffered body is `RetryKind::Free`. `docs/h3-research.md` §3.5's whole
argument for 0-RTT applies here unchanged.

**And a race is the second thing to break the sentence that section quotes
as the reason `hclient-native` needed no idempotency judgement.** W2's one
retry is safe because *"nothing to decide about idempotency: this is not a
second request, it is the first one, which never left"* — hands the request
back only when not a byte reached the wire. §3.5 already noted that 0-RTT
ends that exemption because its data does reach the wire. A race ends it a
second way, and a plainer one: **both requests leave, at the same time, and
neither is a retry.** So an idempotence gate is not a smaller version of the
connect-only seam — it is a judgement this codebase has twice declined to
make. That is the strongest argument that the answer is the seam.

#### 7.7 What the race does need, given the numbers

1. **A head start, and a place to derive it from.** 250 ms as a constant
   (§7.4), and an origin-keyed RTT observation as the honest form. Nothing
   in this crate observes an RTT today, and the Alt-Svc cache is not the
   place for one.

   **Since built at 250 ms, and the number stayed while its argument
   changed** — `docs/v04-race.md` §2. Its *floor* moved to zero, because a
   head start below one QUIC handshake now costs a warm pooled TCP
   connection rather than a duplicated request. The RTT store is still
   unbuilt and still the honest form.
2. **A budget rule that subtracts.** `H < C` checked rather than assumed,
   and `C − H` for the fallback arm (§7.5).

   **Since built, and two thirds of it turn out to be unwitnessable** —
   `docs/v04-race.md` §5. The QUIC arm carries `C` from an earlier start
   than any hedge, so it always reaches its deadline first and the race ends
   when it does: a hedge handed `C` instead of `C − H`, and an `H >= C` that
   raced anyway, are both **mutations that survive**, predicted and
   confirmed. The third statement — the race is charged against
   `Timeouts::connect` once — is witnessed, and getting a witness for it
   needed a TCP arm that can fail (`Tcp::Rejecting`).
3. **Either a connect-only seam or an idempotence gate** (§7.6). Without
   one of the two the race duplicates requests, and `RetryKind` alone does
   not close it.

   **Since decided, and "connect-only seam" is not what it turned out to
   be** — [`docs/connect-only-seam.md`](connect-only-seam.md), written after
   the same gap was reached twice more from v0.4 W2. Three things it settles
   that this item states loosely. It is **not** a method on `Transport`
   (`wasi:http` 0.3's client interface is one function with no connection
   resource; `hclient-fetch` declares `timeouts.connect = false` because
   `AbortSignal` is one deadline) but a staged pair on the backends that
   have a connector, which is `Prefetch`'s decision one phase later. It has
   to hand back a **handle** rather than warm a pool, because only a handle
   makes `Timeouts::connect` unspendable by the second call — §7.5's rule
   applied to this pair. And item 3 is not this seam's first customer:
   §9.3's blocker 1 is the same gap, and a staged connect removes it with no
   race at all.
4. **A failure memory**, or the head start is paid again on every request
   to a blocked origin — 250 ms × every request, for as long as the network
   blocks UDP. This is the same missing thing as §9.3's negative half, now
   with a second caller and a number.

   **Since built**, for §9.3's caller rather than for this one —
   `hclient_select::H3Failures`,
   [`docs/v04-staged-connect.md`](v04-staged-connect.md) §3. Item 3 is
   discharged too: the seam exists, on both stacks. Items 1 and 2 are where
   they were, and item 2 is *nearly* where it was — the sequential fallback
   needed the same subtraction one shape simpler, and
   `spend_connect_budget` is it.
5. ~~A fixture~~ — discharged, §7.1.
6. ~~A cancellation story for the QUIC arm~~ — discharged, §7.6, and it is
   the one part of the race that is already free.

**`DefaultTransport` does not become `Selecting`**, and that is unchanged
from the design document, but the reason has moved. It was *"it wants the
measurement above"*; the measurement is now here, and what it says is that
a race in the shape the members allow would send some requests twice. One
vertical, one claim.

> **Half of that reason expired with the shape.** A race made of two staged
> connects sends nothing on the losing arm at any head start, so *"would
> send some requests twice"* is no longer true of the built one. What is
> unchanged and is the whole of the reason now: **a default that opens UDP
> sockets is a decision about what a plain `Client::new()` does on a network
> that blocks UDP/443**, and `Selecting::hedging` is off until a caller
> turns it on. `docs/v04-race.md` §2.

---

## 8. Findings for other crates, not acted on

All are in read-only territory for this workstream and are recorded here
rather than fixed.

1. ~~**`hclient-native` has no way to be told a record has already been
   fetched**, so the type-65 query is made twice on the TCP path at the
   default port (§3). Three possible shapes are listed there.~~
   **Closed** — §3.1. The shape is `hclient_native::Prefetch`, and it is
   not any of the three as they were stated: the record is not *handed to*
   the connector, it is fetched **by** the connector and handed back
   attached to the request it was fetched for, which is what keeps a
   caller from supplying one. The kept row of the DNS table is 1.
2. **`hclient-h3`'s doc example does not compile on its own.** `cargo test
   --doc -p hclient-h3 --all-features` fails: the example calls
   `Rustls::with_webpki_roots()`, which lives behind
   `hclient-tls-rustls`'s `webpki-roots` feature, and that crate's
   dev-dependency on it enables `quic` alone. It passes under
   `--workspace` only because another member's dev-dependency turns
   `webpki-roots` on and Cargo unifies features. Found while checking that
   *this* crate's own example compiles — which needed a
   `hclient-dns-system` dev-dependency it did not have, so the same defect
   was one line away here. No CI job runs `cargo test --doc` at all, which
   is why it has been able to sit there; `AGENTS.md`'s "Running the tests"
   section now says so.
3. **`Capabilities` cannot express a per-connection answer**, which is what
   makes `full_duplex: false` the right value here and an under-claim at the
   same time: the pair really is duplex whenever the QUIC stack answers, and
   there is nowhere to say so. `docs/v04-design.md` §W2 deliverable 2 asks
   the same question from the h2 side — *"decide whether this is a
   per-response fact or a per-connection one — and do not widen the static
   floor"* — and a selecting transport is a second carrier for whatever that
   decides. It is the same shape as `version_reported`: the honest time to
   answer is after the fact.
4. ~~**Every `Native` connection carries Nagle, and on loopback that
   costs 41 ms.**~~ **Closed** — `docs/nagle-and-nodelay.md`. The
   measurement reproduces (41.8 ms against 0.82 ms, release) and the
   diagnosis is now direct rather than by elimination: the observer moved
   to the server's side of the wire, and the client's Finished record and
   its whole request arrive **together, in one 137-byte segment, 41 ms
   after the change-cipher-spec record**, where with `nodelay` they
   arrive as two segments 11 µs apart. `TCP_NODELAY` on the *server*
   changes nothing, so it is the client's write that is held, and
   plaintext `http://` — where the request head is the connection's first
   write — never stalls at all.

   The policy question this bullet handed to `hclient-rt` was answered
   the other way. `TcpOpts::default()` is **unchanged**: a set option
   there is a *refusal*, so every backend that left `TcpConnect::APPLIES`
   at its `NONE` default would start failing every connect for an option
   its caller never mentioned — a performance fix aimed at exactly the
   implementors that default was written to protect.
   `Native::new` asks for `nodelay` instead, and only where `APPLIES`
   says the runtime applies it, which is `TlsConnect::applies_ech`'s
   shape one seam over.

   **One thing it broke is recorded rather than fixed**, in §6 of that
   document: with the 41 ms gone, *this crate's* `alt_svc` suite loses a
   pooled-reuse race it never used to reach — 9 failing runs in 20 at
   `-j16` against 0 in 20 at `-j1`, on the same binary, all of them
   `Connect / hyper::Error(Shutdown, BrokenPipe)`. It is the residual
   window `hclient-native`'s `h1.rs` names in its own comment, and the
   fixture here is what makes it reachable: one response per connection,
   then a close, with no `Connection: close`.
5. **`TokioHandle` cannot be asked for `nodelay`, though it applies it.**
   `TcpConnect for TokioHandle` delegates `connect` to `Tokio::connect`,
   which applies every option — its doc says *"deliberately identical to
   `Tokio`'s, guard and all"* — but it does not restate
   `TcpConnect::APPLIES`, so it inherits the trait's `NONE`.
   `Native::tcp_opts` then refuses with an `UnsupportedTcpOpts` naming
   `nodelay`, against a runtime that does in fact apply it. Found by
   measurement rather than by reading: the first version of §7.3's control
   used `TokioHandle` and panicked on the error. It matters here because
   `TokioHandle` is the runtime `Selecting` requires — `H3` needs `Spawn` —
   so the finding above is unfixable from a selecting build until the
   constant is restated. One line in
   `crates/hclient-rt-tokio/src/handle.rs`.
   **Closed** — `handle.rs:181` declares `TcpOptsSupport::ALL`. It is
   also now load-bearing rather than merely correct: `Native::new` reads
   `APPLIES` to decide whether to ask for `nodelay` at all, so a
   `TokioHandle` that had kept the trait's `NONE` would silently keep the
   41 ms of finding 4 for every `Selecting` build.
   `hclient-native`'s `tcp_opts.rs` carries the tripwire for all three
   shipped runtimes, in `const` blocks, so a regression is a build
   failure rather than a red test.

---

## 9. Alt-Svc — the slow tier, built (deliverable 4)

`crates/hclient-select/src/altsvc.rs`, plus about thirty lines in
`src/lib.rs`. §7 above is the prediction; this is the work.

**What it is.** An origin that publishes no HTTPS record can still say
"I speak HTTP/3 here" — in a **response header**. So it can only ever help
the *next* connection: the first request to an unknown origin goes over TCP
no matter what the origin has to say, because the advertisement arrives in
the answer to it. That is not a limitation of this implementation, it is
what RFC 7838 is; the fast tier exists precisely because it is not.

**Why it matters more than its size suggests.** Before it, `Selecting` chose
HTTP/3 only for an origin publishing an HTTPS record. Alt-Svc is how every
other origin is ever chosen, and it is what browsers actually rely on.

### 9.1 The inversion, and it is the thing to read first

This crate has **no cache for the fast tier, deliberately**, and §3 gives
the reason in `hclient-native`'s words: *"this origin has no HTTPS record"
is a DNS answer with a TTL of its own, which `SvcbEndpoint` does not carry,
and inventing a lifetime for someone else's answer is how a resolver's cache
and ours drift apart.*

**That reason does not transfer, and a reader arriving from §3 will arrive
knowing the opposite rule.** RFC 7838 §3.1's `ma` parameter *is* a max-age,
given by the origin, for exactly this advertisement — *"the number of
seconds since the response was generated for which the alternative service
is considered fresh"* — with a default of 24 hours stated by the same
section. So Alt-Svc is **more** cacheable than the fast tier, not less: the
lifetime is read off the wire rather than invented, and a cache without one
would be the dishonest shape. It is also not optional the way the other
would be — without a memory the header cannot be acted on at all, since by
the time it arrives the connection it describes is the one already in use.

It is said in the module doc as well as here, because the next reader will
meet the code before the document.

The clock is `R: Timer`, which meant `Selecting::new` gained an `R`
parameter. Never `std::time::Instant::now()` — `hclient-native`'s negative
cache gives that reason for its own, and it is that `Timer` is the one seam
through which time reaches a transport here, so a caller testing under
`tokio::time::pause()` sees what the transport sees. `AltSvcCache` itself
reads no clock: `now` arrives as a parameter, the same shape as
`NegativeCache::suppressed`, which is what makes a 24-hour default testable
without waiting 24 hours.

### 9.2 The parser, and what it does with everything that is not RFC 7838

Hand-written, ~200 lines, **no new dependency**. That is a decision and not
a default: the grammar is a comma list of `token "=" quoted-string` with
two parameters, `hclient-cookie`'s `Set-Cookie` parser is the in-tree
precedent for exactly this shape, and a crate would put a third party
between this workspace and a field a remote peer controls for less code
than the crate's own manifest. `cargo deny --all-features check` is green
(advisories, bans, licenses, sources), and the graph is unchanged.

It is fed by a remote peer, so it must not panic, and what it does with bad
input has to be a decision rather than an `unwrap`. In order:

| input | what happens | why |
|---|---|---|
| a member that does not parse | **dropped, the others stand** | boundaries are known before any member is parsed, so one bad member says nothing about its neighbours. There is no whole-field rejection |
| an unterminated quoted-string | costs **only its own member** | it runs to the end of the field, so it is one member, and the members before it already ended |
| a comma inside a quoted alt-authority | **not a member boundary** | the split is quote-aware |
| an empty member (`,,`) | skipped | RFC 9110 §5.6.1.2 |
| `clear` anywhere in the field | **the whole field is `Clear`** | RFC 7838 §3 decides this: *"including those specified in the same response, in case of an invalid reply containing both 'clear' and alternative services"*. Matched case-sensitively, as `%s"clear"` requires |
| a `protocol-id` that is not a token, or a bad `%` escape | drops its member | `%` is itself a tchar, so `%zz` is a malformed escape rather than a literal |
| an alt-authority with no port, a non-numeric port, one past a `u16`, or port `0` | drops its member | zero is not a port anything is reachable at; a member naming it can only waste a connect |
| an alt-authority's `uri-host` | **not validated** | it is only ever *compared* to the origin's, so a host that is not a host cannot match one. Rejecting it early and rejecting it late are the same answer, and the late one needs no second URI parser |
| `ma` that is not `1*DIGIT` | **drops its member** | the one place a *known* parameter's bad value invalidates rather than being ignored: the alternative is to cache for the 24-hour default on the strength of a number nobody could read |
| `ma` too large for a `u64` | **saturates** | RFC 9110 §5.6.7: *"a recipient that receives a value larger than it can represent MUST use the largest value it can represent"* |
| `ma=0` | parses as zero, and the cache makes it a **removal** | see 9.4 |
| an unknown parameter | **ignored, member survives** | RFC 7838 §3: *"the values (alt-value) they appear in MUST be processed as if the unknown parameter was not present"* |
| `persist` with any value but `1` | treated as absent | RFC 7838 §3.1: *"Clients MUST ignore 'persist' parameters with values other than '1'"* — and ignoring it is what leaves the entry forgettable on a network change, the safe direction |
| a repeated parameter | **last wins** | no RFC basis either way; recorded because it is a choice |
| a parameter name in another case | **matched** | the RFC writes them lowercase and marks only `clear` case-sensitive, so this is a judgement, made where being wrong is cheaper: reading `MA=0` as unknown would leave a 24-hour entry the origin asked to expire at once |
| junk after a member's parameters, or a `;` with nothing behind it | drops its member | the grammar admits OWS only around `;` and `,` |
| an unrecognised protocol id | **parsed**, and not acted on | the parser does not know which protocols the caller speaks; the cache does, and filters where it filters |

The no-panic property is asserted rather than argued:
`no_input_makes_the_parser_panic` runs every single-byte deletion,
substitution and insertion over a valid field value, every prefix of it, a
hand-written list of hostile shapes, and three long inputs (2000 members,
5000 quotes, 5000 backslashes) — 1,400-odd inputs, deterministic so a
failure is reproducible from the file alone, asserting only that `parse`
returns.

**What the parser returns is wider than what the cache stores**, and that is
deliberate: `AltSvcCache` keeps one bit per origin — *this origin advertised
`h3` at its own authority* — because that is the only part this transport
can act on, and *"a field carried but never read is how the previous round
of this plumbing came to sit unused"* (`hclient-native`'s `discovery`).

**An alternative at another host or port is understood and not acted on**,
and this is a finding rather than a shortcut. RFC 7838 §2: *"the Host header
field … is still derived from the origin, not the alternative service (just
as it would if a CNAME were being used)"* — so honouring `h3="other:8443"`
means connecting to one authority while the request keeps another's, and
`Transport::execute` has nowhere to say that: this crate hands the request
to a member whole and the member connects to the URI's own authority. It is
the same wall the fast tier hit from the other side (§3: no record can cross
the `Transport` seam), and closing it is a change to a member.

### 9.3 The negative half: it is **not** already someone else's — and it is built now

**Built, and by the staged connect rather than by the race** —
[`docs/v04-staged-connect.md`](v04-staged-connect.md) §3. The rest of this
subsection is what was read and predicted before it; both blockers below
are discharged, and how is recorded at the end of it.


§7 predicted that *"the failure half may already have a home"* in
`hclient-native`'s connector. **Read before building, and it does not.**

`hclient_native::discovery::NegativeCache` is about a different fact. Its
subject is *"a TCP connect that used a discovered endpoint's port, hints,
ALPN or ECH failed, so stop applying that origin's HTTPS record for five
minutes"*. Three things follow, each checked in the source rather than
inferred:

- **It never sees an HTTP/3 attempt, because `Native` cannot make one.**
  Written at exactly one site (`connect.rs`, in the arm that retries without
  the record) and read at exactly one (`discovered_endpoint`, gated on
  `use_tls && port == 443`). When `Selecting` routes a request to `H3`,
  `Native` is not called at all: the cache is neither read nor written.
- **It is unreachable from here.** `mod discovery;` is private in
  `hclient-native`, `NegativeCache` is `pub(crate)`, and `Native`'s
  `svcb_failures` field has no accessor. `hclient-h3` has nothing of the
  kind — no negative cache, no suppression, no failure memory at all.
- **Its own doc's sentence is still right, and it is about the other
  half.** *"The cache of what was advertised is Alt-Svc's, the cache of what
  failed is the connector's"* — the cache of what was advertised is now
  built, here. The cache of what failed is the connector's, and the
  connector that would own an h3 failure is `hclient-h3`, which has none.

So there is no second cache and no disagreement. What there is instead is a
**gap, stated rather than filled**: if an Alt-Svc entry sends a request to
QUIC on a network that blocks UDP/443, that request fails, and this
transport remembers nothing about it. Two things stop it being built here,
and they are the two that stop the race:

1. **Without a fallback it degrades the caller rather than protecting
   them.** A windowed suppression on `hclient-native`'s model would cost one
   *failed* request per window per origin — where native's own costs none,
   because native falls back to the origin's addresses inside the same
   connect. The equivalent here is falling back from QUIC to TCP inside
   `execute`, which is request-level retry with a `RequestBody::retry_kind()`
   condition on it, and is the same mechanism deliverable 5 is about.

   **The `retry_kind()` condition is a consequence of the gap in §7.6, not
   of the fallback** — [`docs/connect-only-seam.md`](connect-only-seam.md)
   §8. Ask the QUIC stack to *connect*, and a failed connect routes a
   request that was never handed to a transport at all: no retry, no
   idempotence judgement, and `hclient-native`'s own sentence true again —
   *"this is not a second request, it is the first one, which never left."*
   That makes this blocker the staged connect's first customer and the race
   its second, which is the reverse of the order both were written in.
2. **Loopback cannot produce the failure.** UDP to a closed port on loopback
   does not reliably surface to quinn, so the arm under test would be a
   multi-second handshake timeout — a clock-driven assertion, which is the
   shape three flakes in this workspace already came from. It wants the
   `HCLIENT_REQUIRE_TUNTAP` fixture §7 names for the race.

Recorded in §6's spirit: **the failure half is not implemented and not
tested**, and where it would live if the alt-authority were ever acted on is
`hclient-native`'s cache after all, because *"the alternative's port is
blocked"* is a connector fact.

**Both blockers are discharged, and the second one was the right worry
about the wrong premise.**

1. Discharged by the staged connect, exactly as the note above predicted:
   `Selecting` asks `H3` to *connect*, and a failed connect routes a request
   that was never handed to a transport. `hclient_select::H3Failures` is the
   memory; the veto sits **after both tiers**, so a record listing `h3` at a
   UDP-blocked origin is covered too, and `RequireVersion(HTTP_3)` is
   answered before it as it is before the resolver and the cache.
2. **Not discharged by a fixture that blocks UDP — by noticing that the
   memory does not read the reason.** What it records is *the connect
   failed*. A real `quinn` server offering an ALPN this client will not
   accept fails causally in one round trip, which produces that fact without
   producing a clock-driven test. The black hole is still used once, where a
   test needs the bound *spent* rather than a connect failed — the budget
   A/B in `crates/hclient-select/tests/h3_failure.rs`.

The scope answer is `network_changed()`'s, and it is **not** the cache's
answer: the failure memory clears *everything*, with no `persist` notion,
because `persist=1` is the origin's claim about its own advertisement and
nothing ever claimed that a failure to reach it belongs to the origin
rather than to the path.

### 9.4 Scope: the network change, said at the type

RFC 7838 §2.2 asks for something this crate cannot do, and says so in the
same sentence: *"clients SHOULD remove from cache all alternative services
that lack the 'persist' flag with the value '1' when they detect such a
change, **when information about network state is available**."* To a
`Transport` it is not — nothing in `hclient-rt` reports an interface coming
up, a route changing or a VPN connecting, and inventing one would be a
runtime seam rather than a transport.

So the honest answer is given in two places rather than assumed:

- **Nothing is persisted.** The cache is a field of one `Selecting`, never
  written to disk and never shared between transports, so an advertisement
  outlives at most the transport that heard it. A caller that drops its
  client on a network change has already done the whole job —
  `a_new_transport_has_heard_nothing_and_starts_on_tcp` pins it.
- **`Selecting::network_changed()` is the event's only entry point**, public
  because the caller can usually see what the transport cannot. Until it is
  called, every entry behaves as though it carried `persist=1`, which is the
  unsafe direction — a laptop that moved networks is advertising an
  alt-authority that was reachable *somewhere else* — and is therefore
  written where the setter is rather than left to be discovered.

That also gives `persist` a real reader instead of a field parsed and
ignored: `network_changed()` keeps `persist=1` entries and drops the rest,
which is exactly §2.2. A `persist` value the RFC makes us ignore does not
survive it, because ignoring the parameter means it was never there.

### 9.5 The ordering, and why the fast tier is not made worse

The rule is about *whose statement is fresher*, not a preference between
mechanisms. A record is fetched for this request; an entry was heard on an
earlier one and may be up to its `ma` old.

1. A first-ranked record listing `h3` chooses QUIC. **The cache is not
   consulted and its lock is not taken.**
2. A first-ranked record *not* listing `h3` chooses TCP, and the cache is
   not consulted either — RFC 9460 §2.4.2 makes priority the operator's
   preference order, so an origin whose best endpoint is HTTP/2-only has
   asked for HTTP/2, and yesterday's header does not overrule it.
3. Only where there is **no record to read** — none published, the lookup
   failed, the resolver cannot ask, or the authority is an IP literal — is
   the cache consulted.

That needed `origin_offers_h3` to gain a third state. "Publishes a record
that does not offer `h3`" and "publishes no record" were both `false`
before; collapsing them would let a stale header beat a fresh record.

On the population side, a response with **no** `Alt-Svc` field leaves
`note_alt_svc` before the clock is read or the lock is taken — which is
every response from every origin that has never heard of the header. RFC
7838 §3's *"invalidates and replaces"* is about a field that is **present**;
a missing one is not an instruction.

**The IP literal is served by this tier and not by the fast one**, which
looks like an exception and is not: the fast tier skips a literal because
`_443._https.127.0.0.1` is a query with no answer, a reason about DNS rather
than about QUIC. The slow tier needs no query, so it applies, and costs
nothing — `an_ip_literal_has_no_record_to_ask_for_but_can_still_be_advertised_to`
asserts zero lookups on both hops.

### 9.6 What it costs in DNS — the §3 table, re-run and extended

`crates/hclient-select/tests/dns_cost.rs`, five arms now. **The three
measured before this deliverable are unchanged**, re-run on this branch:

| request | type-65 queries | |
|---|---|---|
| `https://origin/` chosen onto TCP | **2** | unchanged *by this deliverable* — this transport's, then `hclient-native`'s own connector's. **It is 1 now**, and §3.1 is where that happened; nothing in this section moved it |
| `https://origin/` chosen onto QUIC | **1** | unchanged — `hclient-h3` does no SVCB lookup at all |
| `https://origin:port/` | **1** | unchanged — `hclient-native` skips discovery away from the default port |
| any request carrying `RequireVersion` | **0** | unchanged |
| `http://` or an IP literal | **0** | unchanged |

And the two the slow tier adds:

| request | type-65 queries | |
|---|---|---|
| one advertised onto QUIC, at a non-default port | **1** | the same query the hop before it paid. Alt-Svc adds none |
| any request where the resolver cannot do SVCB, advertised onto QUIC | **0** | and this origin was unreachable by the fast tier at any price |

One row is **inferred rather than measured**, and is marked as such in the
file: a request the slow tier puts on QUIC at an origin's *default* port
costs **1** rather than the 2 the same request costs on TCP, because the
duplicate is `hclient-native`'s and `hclient-h3` makes no lookup — both
measured, in the first two rows. It is not measured directly for the same
reason those two rows cannot connect at all: an advertisement has to arrive
in a response, and an unprivileged test process cannot put a server on port
443 to send one.

So the slow tier adds **no query anywhere**, adds no lock to the fast tier's
path, and on one path removes a query.

### 9.7 How it is checked

The same two real servers behind one authority, both alive throughout, with
one addition: either can be made to send an `Alt-Svc` field, and the field
can be changed **between** requests — which is what makes the tier testable
at all, since an origin has to be able to advertise, then withdraw.

`crates/hclient-select/tests/`: `alt_svc.rs` (20 — end to end, request 1 on
TCP and request 2 on QUIC), `altsvc_parse.rs` (31 — the parser, no socket
and no clock), `altsvc_cache.rs` (19 — the cache, `now` handed in), and two
more arms in `dns_cost.rs` (3 → 5). With the 25 that stand unchanged
(`choice.rs` 12, `capabilities.rs` 10, `body.rs` 3): **100 tests**, all
passing, `cargo nextest run -p hclient-select --all-features`. (**104**
since §3.1 added `record_handover.rs`; this section's count is the one
§9.8's table was run against.)

Every assertion is causal. Nothing waits for a duration and concludes; the
observations are "this server answered and that one did not", read as
**deltas** across each hop because these tests make several requests to
servers that outlive them. The `ma` timescales that cannot be reached that
way — twenty-four hours — are in `altsvc_cache.rs`, where the clock is a
parameter rather than something to wait for.

Two arms exist only because the mutation table asked for them, and they are
the ones worth naming. `clear` on its own and a field offering no `h3`
produce the **same** outcome here — §3's replace rule already removes on
both — so a parser that dropped `clear` as an unreadable member would have
passed every arm in the file. The reply carrying *both* is where they part
company, and §3 decides it explicitly.

### 9.8 Mutation testing

Anchor **100 tests**, `cargo nextest run -p hclient-select --all-features`,
verified before the run and again after every restore. Each patch had to
match exactly once or the mutation was not run. The harness reads the
**names** of the failing tests and refuses to score a run where the count of
names disagrees with nextest's own `Summary` — and it caught itself doing
exactly that on the first two mutations, where a regular expression missed
nextest's `( 38/100)` progress field and would otherwise have reported two
kills as zero. Restores are `git checkout` followed by `os.utime`, because a
restore that preserves mtime leaves cargo holding the mutant.

**Twenty-five applied: twenty-four killed, one control survived as
intended, none survived unintentionally.**

| # | mutation | verdict | killed by (a selection where there are many) |
|---|---|---|---|
| M20 | the `Alt-Svc` field is parsed and then ignored | **killed** (16) | `the_first_request_is_tcp_and_the_second_is_quic`, `one_client_moves_itself_to_http_3_on_the_second_request`, `every_repeated_field_line_is_read`, and 13 more |
| M21 | the cache is never consulted, so the slow tier never chooses | **killed** (16) | the same 16 |
| M22 | the cache is consulted **before** the record | **killed** (3) | `a_record_that_does_not_offer_h3_is_not_overruled_by_an_advertisement`, `the_first_request_is_tcp_and_the_second_is_quic`, `a_request_advertised_onto_quic_asks_no_more_than_the_one_before_it` |
| M23 | the cache answers `true` before it is populated, so request 1 goes to QUIC | **killed** (40) | every arm whose first hop is TCP, plus every cache test |
| M24 | `ma` is ignored, so entries never expire | **killed** (9) | `an_entry_lives_exactly_as_long_as_its_ma_says`, `a_field_with_no_ma_lives_for_the_rfcs_twenty_four_hours`, `ma_zero_sends_the_origin_back_to_tcp`, and 6 more |
| M25 | `ma=0` is read as an absent `ma`, so the 24-hour default applies | **killed** (2) | `ma_zero_is_a_removal`, `ma_zero_sends_the_origin_back_to_tcp` |
| M26 | `clear` is not recognised | **killed** (4) | `clear_is_its_own_instruction`, `clear_beats_alternatives_in_the_same_field`, `clear_beside_an_advertisement_still_clears`, `a_clear_on_one_line_beats_an_advertisement_on_another` |
| M27 | a present field offering no `h3` leaves the stored entry standing | **killed** (8) | `a_field_that_no_longer_offers_h3_removes_what_was_stored`, `a_later_field_that_offers_no_h3_replaces_the_entry`, `clear_removes`, and 5 more |
| M28 | the expiry comparison is inclusive, so a lapsed window is still fresh | **killed** (7) | `an_entry_lives_exactly_as_long_as_its_ma_says` and 6 more cache arms |
| M29 | a stale entry is not forgotten by the lookup that found it stale | **killed** (1) | `a_stale_entry_is_forgotten_and_does_not_come_back` |
| M30 | the protocol id is not checked, so any advertisement is an `h3` one | **killed** (3) | `an_advertisement_for_another_protocol_moves_nothing`, `only_h3_at_this_origin_is_remembered`, `a_field_that_no_longer_offers_h3_removes_what_was_stored` |
| M31 | the alternative's port is not checked | **killed** (3) | `an_alternative_at_another_authority_is_not_acted_on`, `only_h3_at_this_origin_is_remembered`, `only_an_alternative_at_the_origins_own_authority_is_actionable` |
| M32 | the alternative's host is not checked | **killed** (5) | the same three, plus `the_first_actionable_h3_in_the_list_is_the_one_taken` and `the_uri_host_is_not_validated_because_it_is_only_ever_compared` |
| M33 | `network_changed` clears nothing | **killed** (4) | `a_reported_network_change_sends_the_origin_back_to_tcp`, `a_network_change_forgets_what_did_not_ask_to_persist`, `a_persist_value_the_rfc_ignores_does_not_survive_a_network_change`, `a_clone_is_the_same_cache` |
| M34 | `network_changed` clears everything, `persist=1` included | **killed** (3) | `a_persistent_advertisement_survives_a_reported_network_change`, `a_network_change_forgets_what_did_not_ask_to_persist`, `persisting_across_a_network_change_is_not_living_for_ever` |
| M35 | the origin key ignores the port | **killed** (32) | `the_key_is_the_whole_origin` and 31 more |
| M36 | the origin key keeps the host's case | **killed** (1) | `the_key_is_the_whole_origin` |
| M37 | a response with **no** field is treated as an instruction, and removes | **killed** (2) | `a_response_with_no_field_leaves_the_entry_alone`, `a_persistent_advertisement_survives_a_reported_network_change` |
| M38 | only the first `Alt-Svc` field line is read | **killed** (1) | `every_repeated_field_line_is_read` |
| M39 | the **last** actionable `h3` wins rather than the first | **killed** (1) | `the_first_actionable_h3_in_the_list_is_the_one_taken` |
| M40 | an unknown parameter invalidates its member | **killed** (1) | `an_unknown_parameter_is_ignored_and_its_member_survives` |
| M41 | `persist` is accepted for any value | **killed** (2) | `persist_is_only_ever_the_literal_one`, `a_persist_value_the_rfc_ignores_does_not_survive_a_network_change` |
| M42 | members are split on every comma, quoted or not | **killed** (2) | `a_comma_inside_the_quotes_is_not_a_member_boundary`, `an_unknown_parameter_is_ignored_and_its_member_survives` |
| M43 | an alt-authority's port `0` is accepted | **killed** (1) | `an_alt_authority_without_a_usable_port_drops_its_member` |
| **M44** | **CONTROL** — `AltSvcCache`'s `Debug` reports a constant instead of the entry count | **survived, as intended** (0) | nothing, and nothing should: no test asserts on that `Debug` output and no code path reads it. Without a control, twenty-four kills would be indistinguishable from a harness that reports "killed" unconditionally |

M22 is the one worth reading twice, because it is the mutation the ordering
rule exists to fail: with the cache consulted first, a stale header beats a
record fetched for this request, and three arms catch it — including one in
`dns_cost.rs`, which notices the *lookup that stops happening*.

### 9.9 What is not verified

Added to §6's list rather than replacing it:

- **The negative half is not built and not tested** — §9.3. An Alt-Svc entry
  that sends a request to QUIC on a network blocking UDP/443 costs that
  request, every time, with no memory of the failure.
- **No live network.** Every server here is on loopback. The claim "a real
  origin advertising `h3` is reached over HTTP/3 on the next request" is not
  made; what is made is that an advertisement decides which of two real
  servers is reached.
- **The advertisement is never acted on across an authority**, so nothing
  checks what would happen if it were — that path does not exist, by §9.2.
- **The `ma` clock is exercised through the cache and not through
  `R: Timer`.** `altsvc_cache.rs` hands `now` in directly; no test drives a
  live `Selecting` past an entry's expiry under `tokio::time::pause()`. The
  wiring in between — `Selecting::now()` reading `elapsed_since(self.epoch)`
  — is one line and is not separately pinned. What *is* pinned end to end is
  `ma=0`, which is the shortest lifetime there is.
- **`Alt-Svc` on a 1xx or on an error response** is not exercised. The
  header is read off whatever response head comes back, whatever its status;
  no test uses a non-200.
