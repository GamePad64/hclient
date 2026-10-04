# The parsers

> Part of the [AGENTS.md](../../AGENTS.md) record, split 2026-10-05. The root
> file keeps the operational rules and the index of parts. Sections below
> keep their original headings, and a "this file" in the text named the
> whole record when it was written.

### Two date parsers were reported as identical, and the duplication was twenty lines

The premise was wrong and the measurement is worth more than the fix.
the cache's `date.rs` and the jar's share **no parsing at
all**: the first reads RFC 9110 §5.6.7's three fixed `HTTP-date` forms,
the second RFC 6265 §5.1.1's position-free algorithm, and their function
inventories intersect nowhere. What was genuinely duplicated is the
**civil-date arithmetic** — `days_from_civil` byte for byte, `is_leap`
and `days_in_month` differing only in integer type. Twenty lines of the
402.

**So the split that landed is: winnow parses the grammar, jiff answers
the calendar.** It maps onto the finding rather than onto the report —
the halves that differ stay per-crate and the half that was copied is
delegated, which removes the copy from both crates rather than moving it
to a third.

**Neither library is asked to parse, and that decided which objections to
either of them survived.** A date crate's `strftime` validates the
weekday against the date and refuses a mismatch, which §5.6.7 does not
require and this workspace deliberately does not do — a server with an
off-by-one weekday would lose its `Date` entirely. It also reads a
literal space as *any run of whitespace, including none*: measured on
chrono, `06Nov1994 08:49:37 GMT` yields an ordinary 1994 timestamp, as do
double spaces, tabs, a lowercase month, `+1994`, `-1994`, and one digit
wherever the grammar writes two. For a format §5.6.7 fixes down to the
character, that is a different grammar rather than leniency.

The asymmetry is why that is refused rather than weighed. `None` here
means **already stale** (§5.3), and this parser also reads `Date`, which
feeds the `Age` arithmetic — so a surplus refusal is safe and a surplus
*acceptance* mints a freshness lifetime out of a string no conforming
sender produced. The `httpdate` differential would have had to record
eight new divergences of the form *we accept what the oracle refuses*,
turning a test that pins one deliberate decision into a list of what a
dependency happens to do.

winnow keeps every one of those properties exactly:
`take_while(n..=n, ..)` is the `fixed_digits` this file used to hand-roll,
each separator is a literal that means itself, and the day name is
consumed by a parser that never shows a date library a weekday. Nothing
in either crate's behaviour changed — 62 cache tests and 95 cookie tests
pass unaltered, the differential among them.

**Three of the four objections to `jiff` were objections to its parser,
and the fourth was an artifact of the route taken to the answer.** With
winnow parsing, the weekday check and the POSIX `%y` window have no
subject. The one that looked structural was `jiff::Timestamp::MAX` —
`9999-12-30T22:00Z`, which cannot represent `Expires: Fri, 31 Dec 9999
23:59:59 GMT`, a real "never expires" idiom this parser has always read
as `253402300799`. That is true of `Timestamp` and **not** of the civil
types: `civil::Date::MAX` is `9999-12-31`, and
`DateTime::duration_since` against a civil epoch answers `253402300799`
exactly. Measured, after the first measurement said the opposite for the
wrong reason.

What survives is a trap rather than a defect: `civil::date(..)` and
`Date::at(..)` **panic** on a value the calendar does not have, one
function name from the `Date::new`/`Time::new` that return `Result`. Both
crates use the fallible pair, and the single panicking constructor is
inside a `const` — where an impossible date is a compile error rather
than something a header could trigger.

chrono was measured too and is equivalent for this half, agreeing with
the existing parser on every probed point. jiff is what landed, at the
same crate count — `jiff` + `jiff-core` against `chrono` + `num-traits`
— and with **no build script**, where `num-traits` carries one and an
`autocfg` build-dependency with it.

**The honest cost, because one of the three numbers went the wrong way.**

| file | code lines | |
|---|---|---|
| `cache/date.rs` | 138 → **118** | the calendar left; the grammar stayed |
| `cookie/date.rs` | 122 → **96** | same, plus §5.1.1's productions read better as combinators |
| `cookie/parse.rs` | 142 → **152** | **longer** |

Graphs, while the two were still crates of their own: `hclient-cache`
10 → 13 crates, `hclient-cookie` 11 → 14.
`default-features = false` on jiff is what makes it affordable in a
clockless leaf — its default `tz-system` reaches for the platform
timezone. Both build for `wasm32-unknown-unknown` and `wasm32-wasip2`,
checked directly rather than inferred, since `cargo nextest run
--workspace` builds for neither.

**`parse.rs` growing is the result worth keeping.** RFC 6265bis §5.2 is
not a grammar — it is a sequence of *cuts*, "the characters up to the
first `;`", then the same again — and a combinator that expresses a cut
costs more text than `position(|b| *b == b';')`. The productions in
§5.1.1 are a grammar and shrank; the algorithm around them is a search
and is still written as one. That is the same line the whole change is
drawn on, met from the far side: a parser combinator library pays where
there is a grammar, and charges where there is not.

### Three header grammars, and one of them needed the backtracking

The same line the date parsers were split on, applied to the parsers that
were left — and it came out three different ways, which is what makes it a
rule rather than a preference.

**`WWW-Authenticate` is the case that pays, and it pays in a defect
rather than in lines.** RFC 9110 §11.6.1 lets one value carry several
challenges separated by commas — *the same commas that separate a
challenge's own parameters*. Nothing local tells them apart, so the
hand-written splitter looked ahead: *a token then whitespace begins a new
challenge, a token then `=` is a parameter*. A combinator does not need
the lookahead at all — the parameter list stops where `auth-param` fails
to match, and the outer list takes the comma. `token68` is the arm that
keeps a `Negotiate YWJj==` beside a `Digest` one from derailing the
value: `auth-param` matches its `YWJj`, finds `=` with nothing behind it,
and the alternative swallows the whole thing. **294 code lines to 261,
and four hand-written helpers gone** — `split_challenges`,
`digest_params`, `unescape` and `split_outside_quotes`.

**`Cache-Control` is an ordinary win**: 196 to 185, RFC 9111 §5.2's
`token [ "=" ( token / quoted-string ) ]` written as itself.

**`charset` grew, 231 to 240**, which is the third sighting of the rule
and the second time it has been recorded against this workspace's own
hopes. Finding one parameter past a media type is a *cut*, and a cut
costs more as a combinator than as `position`. It is kept converted
anyway, because what it buys is not lines — see below.

**What it buys is that "split on a separator outside a quoted-string" is
now written zero times where it was written three.** The cache's
`directives.rs`, `hclient`'s `digest.rs` and its `response.rs` each
carried a copy, differing only in separator and in `&[u8]` versus `&str`.
They were **measured against each other first, on twelve inputs** —
unterminated quote, escaped separator, trailing backslash, empty input —
and agreed on all twelve, so this was tidy-up rather than a defect, and
worth saying because the same investigation could have found the
opposite.

Two of the three quoted-string parsers hand back a **borrow and do not
unescape**, and that is a decision rather than a shortcut: a
`Cache-Control` argument that is quoted at all is a field-name list and a
`charset` is an encoding label, neither of which can contain a quote, so
an unescape would allocate on every directive to change nothing. A
`quoted-pair` is still consumed, so a `\"` cannot end the value early.
`digest.rs`'s does unescape, because a `realm` is free text a deployment
chooses — which is the same split this workspace already had between
those two modules, now stated in the parsers instead of beside them.


### `1xx` responses, and the third time hyper's `Send` shaped this crate

`Native::watching_1xx()`, and `Event::Informational` on the hooks seam —
because `Transport::execute` resolves exactly once and a `1xx` is not that
once. It carries `id`, `status` and `headers` and **no `version`**: the
connection's protocol was already reported by the `Connected` or `Reused`
that opened the exchange, and a third place to be wrong about one fact is
a third place to be wrong.

**The two protocols reach the same capability by routes that share
nothing.** HTTP/2 is `ResponseFuture::poll_informational`, a poll on the
same future that awaits the response, needing no bound at all. HTTP/1 is
`hyper::ext::on_informational`, whose callback must be
`Send + Sync + 'static` and is stored as `Arc<dyn .. + Send + Sync>` —
**the third time hyper's auto-trait requirements have shaped this
crate**, after the sealed `Http2ClientConnExec` that ruled out
`hyper/http2` in v0.2 and the `Rewind<Box<dyn Io + Send>>` inside
`hyper::upgrade::Upgraded` that ruled it out for the WebSocket work. Here
it collides with a property this workspace documents as supported: a hook
may hold an `Rc`.

What is different this time is that a pattern for absorbing it already
exists. The bound sits on the opt-in constructor and the field is a `fn`
pointer, which is `multiplexed()`'s shape exactly, so **no signature a
single-threaded hook meets gains a bound** and an `Rc`-holding hook gets
`E0277` on the line where it asked. One switch turns both protocols on,
because the capability reports the **floor** — a `true` that held on h2
alone would be a claim an HTTP/1 connection could not keep.

Three defects came out of the writing rather than the design, and the
middle one is the sharpest: **`Native::hooks` dropped the installer
pointer but carried the capability**, so `.watching_1xx().hooks(h)`
reported nothing while claiming `informational_1xx == true` — a
capability lying, which is worse than the silent downgrade it accompanies
because a caller can act on a capability. The h2 poll was also written
above the connection drive, asking for interim heads before the frames
carrying them had been read, under a comment arguing for the wrong
ordering; and the **shared** h2 path was wired and unreached, its
mutation surviving the whole suite until a fixture reached it.
`.notes/informational-1xx.md`, including what is still unmeasured —
`Informational::id` is populated and never asserted.
