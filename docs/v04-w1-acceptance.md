# v0.4 W1 — a transport that chooses: what it does, what it costs, and what it does not do

`docs/v04-design.md` §W1, deliverables 2 and 3. Deliverable 1
(`RedirectSupport`) landed independently and is written up in the design
document itself. Deliverables 4 (Alt-Svc) and 5 (the race) are **not
built**, and the last section here says what each would need rather than
leaving the reader to infer it from their absence.

Written the way the other acceptance documents are: every claim carries how
it is known, and a claim that is not pinned by a test says so.

---

## 1. Where it lives, and why a crate

**`crates/http-ng-select`**, one public type: `Selecting<R, T, D>`, owning a
`Native<R, T, D>` and an `H3<R, T, D>`.

A crate rather than a feature of either member, for the reason `http-ng-h3`
is not a feature of `http-ng-native`: **Cargo's features are additive**. A
`http-ng-native/select` feature would put the whole QUIC stack — and the
`UdpBind + Spawn` bounds it needs from the runtime — into every build in
any graph in which any crate switched it on, including the builds that will
only ever speak HTTP/1.1. A crate is opt-in by being named.

Measured, `cargo tree -p http-ng-select -e normal --prefix none`, unique
crates: **66**, against `http-ng-h3`'s 57. The 9 are what
`http-ng-native` adds that `http-ng-h3` did not already have.

P2 is the condition, and it is inherited rather than declared here:
`http-ng-rt-tokio` needs its `udp` feature, or `TokioHandle` does not
implement `UdpAdoptStd` and `Selecting<TokioHandle, …>` cannot be named at
all. That is not this crate's dependency — it is `H3`'s — and the failure is
a compile error at the call site rather than a weaker transport.

**The members are concrete rather than `A: Transport, B: Transport`.** A
generic pair would have to be *told* which member speaks HTTP/3 — nothing in
`Transport` or `Capabilities` says so — and a caller could hand it two
HTTP/1.1 stacks and get a transport that "chooses" between them on the
strength of a record neither honours. With the two named, that is
unrepresentable. The generality is not free to add later either, but it is
also not free of a decision: a third member (`http-ng-urlsession`, §W3)
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
the two the design document's examples name.** Two of its examples were
fixed under it while it was being written (`RedirectSupport::Configurable`
deleted in `b2289c4`; `version_select` turned on for both in `4e9805f`), and
four of the six were never in it.

| field | `http-ng-native` | `http-ng-h3` | stored | why |
|---|---|---|---|---|
| `full_duplex` | `false` | `true` | **`false`** | `false` asks the caller to assume less and forbids nothing. This is the answer `http-ng-native` already gives one level down for the same question — HTTP/1.1 cannot do duplex, HTTP/2 can, one transport reports one value — beside the cost that decided it: over-claiming deadlocks a caller structured for bidirectional streaming, under-claiming costs a buffered copy. The rule is imported from the crate that already had to make it |
| `response_trailers` | `false` | `true` | **`false`** | same shape: `false` costs a caller trailers it would not have looked for, `true` would have it look for trailers on a connection that cannot carry them |
| `client_certs` | `false` | `true` | **`false`** | same shape |
| `timeouts.first_byte` | `true` | `false` | **`false`** | and this is the field where the two disagree in the *other* direction. Declaring a bound one stack silently ignores is the exact no-op v0.2 W4 created `TimeoutSupport` to prevent |
| `timeouts.between_bytes` | `true` | `false` | **`false`** | same |
| `early_data` | `None` | `Supported` | **`Supported`** | the one field whose *stronger* value is the true one — see below |

`timeouts.connect` is `true` on both and stays `true` on the pair; a
conjunction that returned `false` for everything would satisfy every row
above, so `the_stored_answer_holds_whichever_stack_serves_the_request`
asserts it, along with `streaming_request_body`, `version_select`,
`version_reported`, `redirects`, `cancel_on_drop`, `connection_reuse` and
`tls_config`.

### Why `early_data` is the stronger value and not an exception

`EarlyDataSupport::Supported` says the transport **can** place a request the
caller marked with `AllowEarlyData` into early data. It promises nothing
about any particular request — `http-ng-h3` alone already does not place the
*first* request to an origin there, because there is no session ticket yet.
So "this transport can offer early data for a marked request" is true of the
pair, and `None` — "this transport never offers early data" — is false of
it.

False in the direction that matters, too: **nothing in `http-ng` reads this
field** (grep `crates/http-ng/src` for `early_data`: no matches), so
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
`http-ng-h3`'s `Supported`, and `ReuseSupport::None` is not a weaker
`Supported`: it says every request gets a fresh connection, which is false
the moment the QUIC stack answers one.
`a_pooling_disagreement_is_refused_at_construction_naming_the_field` reads
the field name and both values off the error, and
`the_same_two_stacks_with_the_pool_on_are_one_transport` is its control.

The other arms are pinned through `http_ng_select::combine`, which is public
for exactly this reason: a rule whose arms can only be exercised by a member
that does not exist yet would otherwise ship unpinned.

### A field added to `Capabilities` later

There is no compile-time guard. `Capabilities` is `#[non_exhaustive]`, so a
destructuring `let` outside `http-ng-core` needs `..` and cannot be made
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

Two of those need a note because `http-ng-native` decided them differently.

**The prefixed name.** `http-ng-native`'s connector refuses to build it, and
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

Counted, not reasoned about — `crates/http-ng-select/tests/dns_cost.rs`
counts the calls a resolver received.

| request | type-65 queries |
|---|---|
| `https://origin/` chosen onto TCP | **2** — this transport's, and then `http-ng-native`'s own connector's |
| `https://origin/` chosen onto QUIC | **1** — `http-ng-h3` does no SVCB lookup at all |
| `https://origin:port/` | **1** — `http-ng-native` skips discovery away from the default port |
| any request carrying `RequireVersion` | **0** |
| `http://` or an IP literal | **0** |

§W1's *"costs no new discovery at all — only the acting"* is true of the
**mechanism** and not of the count. **The duplicate is a finding, not a
defect of this crate, and closing it is a change to `http-ng-native`:**
`discovery::lookup` is `pub(crate)`, `Endpoint` is a private type, and no
record can cross the `Transport` seam. Three shapes would close it, and
none of them is this crate's to make:

- a way to hand `Native` an already-fetched `SvcbEndpoint` for this request
  (a request extension would fit the existing vocabulary — `Timeouts`,
  `AllowEarlyData` and `RequireVersion` all travel that way);
- `Native` skipping its own discovery when told the caller has done it;
- a memoising `Resolve` adapter, which is the one a *user* can already build
  today without any crate here changing, and the reason there is none in
  this crate is in the next paragraph.

**There is no cache here, deliberately.** One remembered answer per origin
would turn one query per request into one per origin, and the reason not to
is `http-ng-native`'s own, about the other half of the same problem: *"this
origin has no HTTPS record" is a DNS answer with a TTL of its own, which
`SvcbEndpoint` does not carry, and inventing a lifetime for someone else's
answer is how a resolver's cache and ours drift apart.* A resolver that
caches (`http-ng-dns-hickory`, with the real TTLs) already removes the cost;
one that does not (`http-ng-dns-doh`, by an explicit decision of its own)
would not have it removed by a second cache here that no caller can turn
off.

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

`crates/http-ng-select/tests/`: `capabilities.rs` (10), `choice.rs` (12),
`dns_cost.rs` (3), `body.rs` (3), plus the two fixtures. **28 tests.**

---

## 5. Mutation testing

Anchor **28 tests**, `cargo nextest run -p http-ng-select --all-features`,
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
  proof. Both are `Unpin` — `http_ng::Deadline` requires it of any body a
  `Client` wraps, so a body that were not could not reach a caller through
  this library at all — and the bound is what lets the projection be
  `Pin::new(&mut …)` in a crate that forbids `unsafe`. Nothing checks that a
  future member body stays `Unpin`; a member that stopped being one would be
  a compile error at the `Transport` impl, which is the honest failure but
  is not a test.
- **`Transport::to_error` is the identity here and no test distinguishes it
  from the default**, which recognises an `Error` and passes it through
  unchanged. The line states the intent where it is read and survives the
  default changing; that is the same position `http-ng-native` and
  `http-ng-h3` take about their own, and it is equally untested there.
- **Cancellation is not tested through this transport.** Dropping a
  `Selecting::execute` future drops the member's future, whose `Drop` is the
  one that matters, and both members' cancellation is measured in their own
  suites from the far end of the wire. What is not checked is that this
  wrapper adds nothing between them — there is no `select`, no spawn and no
  buffering in `execute`, so there is nothing that could, but "nothing could"
  is an argument rather than a measurement.
- **No live network.** Every server here is on loopback. The claim "a real
  origin publishing an HTTPS record with `h3` is reached over HTTP/3" is not
  made; what is made is that the record's ALPN decides which of two real
  servers is reached.
- **0-RTT through this transport is not exercised.** `early_data` is
  reported `Supported` and the reasoning is in §2, but no test marks a
  request with `AllowEarlyData` and watches it enter early data through
  `Selecting`. `http-ng-h3`'s own suite does that for the member.

---

## 7. Alt-Svc and the race: what each would need

Neither is built. Both are named in `docs/v04-design.md` §W1 as deliverables
4 and 5, after this one and in that order, and the order is the finding
rather than a schedule.

### Alt-Svc — the slow tier (deliverable 4)

It is a **response header**, so it can only help the *next* connection: the
first page load at an origin is never HTTP/3 through Alt-Svc, which is
exactly why the HTTPS record exists and why it came first here. What it
needs, and none of it is in this crate:

- **A parser** for RFC 7838 §3's field value — `h3=":443"; ma=86400;
  persist=1`, a list, with quoted alt-authorities and parameters. Closest
  precedent in this tree: `http-ng-cookie`'s `Set-Cookie` parsing, which is
  sans-io and clockless for the same reasons this would want to be.
- **A cache**, and this is the part that makes it deliverable 4 rather than
  3. Keyed by origin, holding the advertised protocol and alt-authority,
  with `ma` as its lifetime — a real TTL, supplied by the server, which is
  the thing `SvcbEndpoint` does not carry and the reason §3 gives for having
  no cache at all today. It needs `Timer` (the seam through which time
  reaches a transport here — `http-ng-native`'s negative cache is the model,
  including its refusal to use `std::time::Instant::now()` so that a caller
  testing under `tokio::time::pause()` sees what the transport sees).
- **A negative half**, and the argument for it is already written in
  `http-ng-native`'s `discovery` module: *the cache of what was advertised
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

### The race — a hedge, not a chooser (deliverable 5)

P12 is emphatic that it is a **third** thing: applied *after* the choice, as
a hedge against a network that blocks UDP/443, not as a way of choosing.
What it needs:

- **The measurement first, and this is the blocker.** v0.3 W2 recorded that
  *"the size of the cost is unverified"* — how long a client waits before
  concluding UDP is blocked, and what that costs on a network where it is
  not. A policy chosen before that number is a guess with a timer in it.
- **A fixture that can actually block UDP/443**, which loopback cannot: a
  packet filter, a namespace, or the `tuntap` device the workspace already
  gates a CI job on (`HTTP_NG_REQUIRE_TUNTAP`). Without it the arm that
  matters is untestable and the race is a code path nobody exercises.
- **A cancellation story.** Two in-flight connects, one of which must be
  torn down when the other wins, with `Transport::execute`'s MUST — a drop
  is a cancellation, never a detach — applying to the loser. Both members
  already honour it, so the work is arranging the drop rather than making it
  mean something.
- **A budget.** `Timeouts::connect` is *one* deadline for the whole race on
  both members today; a race that gave each arm the full budget would double
  the bound a caller set, which is the mistake `Client`'s `425` replay
  already had to avoid ("a bound a server can double by answering `425` is
  not a bound").

**`DefaultTransport` does not become `Selecting`**, and that is unchanged
from the design document. Making a plain `Client::new()` open UDP sockets is
a decision about what happens on a network that blocks UDP/443, and it wants
the measurement above. One vertical, one claim.

---

## 8. Findings for other crates, not acted on

Both are in read-only territory for this workstream and are recorded here
rather than fixed.

1. **`http-ng-native` has no way to be told a record has already been
   fetched**, so the type-65 query is made twice on the TCP path at the
   default port (§3). Three possible shapes are listed there.
2. **`http-ng-h3`'s doc example does not compile on its own.** `cargo test
   --doc -p http-ng-h3 --all-features` fails: the example calls
   `Rustls::with_webpki_roots()`, which lives behind
   `http-ng-tls-rustls`'s `webpki-roots` feature, and that crate's
   dev-dependency on it enables `quic` alone. It passes under
   `--workspace` only because another member's dev-dependency turns
   `webpki-roots` on and Cargo unifies features. Found while checking that
   *this* crate's own example compiles — which needed a
   `http-ng-dns-system` dev-dependency it did not have, so the same defect
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
