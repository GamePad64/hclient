# Language and packaging

> Part of the [AGENTS.md](../../AGENTS.md) record, split 2026-10-05. The root
> file keeps the operational rules and the index of parts. Sections below
> keep their original headings, and a "this file" in the text named the
> whole record when it was written.

### The flag is paid in text, and the text was the only thing paying

The paragraph above ends *"the cost is paid in text"*, and being a user
showed the text is not where the cost lands. Measured from a fresh crate
outside this workspace, on the two lines the crate's own front page opens
with: `cargo add hclient` resolves, and `Client::new()` is `error[E0599]:
no associated function or constant named "new" found for struct "Client"`
— **no part of which mentions a feature**. A reader who skipped one line
of prose gets a message that reads as *this crate does not have that
function*.

**The free function beside it needs nothing at all, and that asymmetry is
what decided the shape of the fix.** `hclient::default_transport()` under
the same build is `error[E0425]` carrying rustc's own *"found an item that
was configured out"* note, which points at the `#[cfg]` line and underlines
`feature = "default-transport"`. That note is emitted for **path**
resolution and not for associated-item lookup — so a free function
announces its own gate and an inherent `fn` in a `#[cfg]`-ed-out `impl`
block does not. One stub, not a pair.

**So on the branch where there is no default transport, `Client::new`
exists** — with a where-clause nothing can satisfy and the message on the
trait it names, through `#[diagnostic::on_unimplemented]`. The obvious
spelling of that does not compile: a where-clause predicate carrying no
generic parameter is checked at the **definition** site, so
`where Self: DefaultTransportFeature` refuses to build this crate at all,
`error[E0277]` on the `where` line. A lifetime parameter the caller never
writes is what defers the predicate to the call site.

**The headline forks, because there are two reasons to be standing there
and only one of them is the feature.** `wasm32-wasip2` reaches the same
stub with `default-transport` **already on** — `hclient` does not depend on
`hclient-wasi`, so there is no branch to resolve — and telling that caller
to add a feature they have is the one wrong answer a single message would
have given. Within the stub's own gate the pair is exhaustive and mutually
exclusive by construction, which is the shape `DefaultClock`'s arms were
repaired into one section up.

`just first-five-minutes` is the check, in the `no-default` job, and it is
the reader's own instrument rather than a test written beside the code: a
crate outside this workspace, with a path dependency, built four ways. Each
arm was broken on purpose and watched — deleting the stub returns the
`E0599`, giving WASI the feature message trips the third arm, renaming
`default_transport` trips the second — and the control is the same source
compiling with the feature on, without which the recipe would be green for
a crate that refuses everything.

**What is not fixed is the first line of the error, and it cannot be.**
`Client::new()` still fails to compile; what changed is that the failure
names the flag and the command. Nothing here widens the default feature
set, for the reason the section above measures.

### The workspace was `http-ng`, and the prefix is what decided its replacement

Three objections killed the old name, and they are independent of each
other. `-ng` means *next generation of X*, so `http-ng` in a dependency list
said it superseded the `http` crate — which has **932 million downloads**
and which this workspace *depends on*. Wrong in both directions. The
`http-*` namespace is plumbing besides: `http` 932M, `http-body` 800M,
`http-types` 53M, `http-client` 7.5M are all types-and-traits crates, while
every client that found an audience is a distinctive word — `reqwest` 654M,
`ureq` 182M, `attohttpc` 32M, `isahc` 17M. And `-ng` has no Rust precedent:
`zlib-ng`, `mio-ng` and `tokio-ng` do not exist, and the single hit,
`libz-ng-sys`, is only that because the upstream C library is literally
named `zlib-ng`. It is a C convention, and it dates.

**What decided the replacement is that this publishes a family of 29, and a
family prefix should be legible rather than clever.** Fifty-four candidate
names were checked for availability; most good single words are gone, and
the survivors — `wend`, `voyage`, `portage`, `transom`, `wayfare` — all win
*distinctiveness* and lose the thing that matters here.
`hclient-dns-doh`, `hclient-rt-embassy`, `hclient-tls-rustls` tell a reader
who lands on any one of them which world they are in. `wend-dns-doh` does
not until they look `wend` up. `h` is not arbitrary either: `h2` and `h3`
are the ALPN identifiers and the names of the canonical Rust crates, so `h`
means HTTP in this domain already.

**The cost is real and is not hidden**: `hclient` is descriptive where the
successful clients are oblique. A descriptive name has nothing to say in a
sentence and it invites near-neighbours — `hclient2`, `hclient-rs` — where a
coined word does not. For a kit of 29 the legibility was judged to win.

**`www-*` was raised afterwards and declined, on the same rule that chose
`hclient`.** It fails the prefix test hardest: `www-tls-rustls` says "web
TLS", and `www-idn`, `www-rt-tokio`, `www-rt-pair-check` carry no
information at all. It also names the whole domain rather than this thing —
the mirror of `http-ng`'s failure, *the Web itself* instead of *the
successor to `http`* — and `www` is a hostname convention, so `www-` reads
as a subdomain. The names are free; the objection is merit, not
availability. Recorded here because the question will otherwise be asked a
third time.

### The licence was a claim with no text behind it, and the root is the wrong place for one

Every crate here has declared `license = "MIT OR Apache-2.0"` since the
workspace existed, and **there was not one licence text in the
repository** — no `LICENSE-MIT`, no `LICENSE-APACHE`, at the root or
anywhere else. An SPDX expression is a claim; the text is what makes it a
grant. `cargo package` does not check this, `cargo publish --dry-run` does
not check this, and the crate would have gone out with the claim standing
alone.

**The detail that decides where the files live is that a file at the
repository root never reaches the tarball.** `cargo package` takes only
what is inside the crate's own directory, so one pair at the root would
have looked right in every git view and shipped nothing. Each of the 24
publishable crates carries its own copy, as a symlink — cargo follows one
and packs the content, verified by extracting the `.crate` rather than by
reading the file list: 18 files where there were 16, and the first line of
`LICENSE-MIT` inside the tarball is the copyright.

**One trap comes with the symlinks, and it was walked into twice.** `sed -i`
does not follow a symlink — it writes a new file and renames over it, so a
workspace-wide `git ls-files | xargs sed -i` silently turns every licence
link, and `CLAUDE.md`'s link to this file, into copies. Both renames this
week did it. The tell is `git status`'s `T` (type change), not `M`, and the
check that settles it is the **index**: `git ls-files -s | awk '$1=="120000"'`
should count **two per publishable crate, plus one** for `CLAUDE.md` — 55
today at 27 crates, and stated as a relationship rather than a number
because it was written down as 59 at 29 crates and was wrong within the
week, and as 53 at 26 until `system-resolver` arrived. `just packaging` is the gate that does not go stale, because it
asserts against the packaged file list. Restoring is one loop; noticing is
the hard part, because a copy behaves identically until it drifts.

A README is the same shape one step down, and it was the same absence: no
crate had one, `readme` was set nowhere, so 25 crates.io pages would have
carried a single line of `description`. Each crate has one now, and each
says the thing this workspace's own arguments turn on — **why it is its own
crate** — because that is the question a reader landing on
`hclient-tungstenite` actually has.

`just packaging` is the check, in the `lint` job, and it asserts against
the **packaged file list** rather than the working tree, because the tree
can hold a file the tarball drops — which is the whole defect. It fails
closed on the loop not running, and it was checked in the failing direction
by removing one README and watching it name the crate.

**`just package-build` is the other half, and it is the one that builds.**
`cargo package --workspace` does what a publish does and stops before the
upload: each `.crate` is built from the files that would ship, and then
**verified** by compiling it out of that tarball. That is the only check
here that builds a crate the way a reader would get it rather than the way
this workspace sits on disk — the same distinction the doctest job exists
for, where two examples compiled only because another member turned a
feature on.

**It failed on its first run, and the cause would have blocked the entire
publication.** `hclient-fetch` and `hclient-native` each dev-depend on
`hclient`, which depends on both — `DefaultTransport` is `Fetch` on wasm and
`Native` elsewhere. Cargo allows that cycle inside a workspace and refuses
it at package time, because a dev-dependency carrying a version has to
resolve from the registry and `hclient` cannot be there until those two
are. Nothing else could see it: not `cargo check`, not `cargo nextest`, and
not `just packaging`, which packages with `--no-verify`. Both are path-only
now, so cargo strips them from the published manifest, and the reason is
written at both sites — every other workspace dependency in those files is
`{ workspace = true }`, so the odd one out invites a tidy-up that would put
the defect straight back.

It is deliberately **not** `cargo publish --dry-run`: that also asks the
registry about ownership and version collisions, which is a different
question and one CI has no credentials for. Nothing in the workflow
publishes and there is no registry token in it.

### `#[non_exhaustive]` has three answers, and only one of them is "yes"

Publishing turns every public type into a promise, so the attribute was
decided type by type rather than swept on: **21 added**, bringing the
workspace to 35 sites. What the sweep produced is a rule, and the rule is
worth more than the list.

**It is a no-op wherever a struct has a private field**, which is most of
them — such a struct already cannot be built with a literal from outside.
So of 236 public types the question is even *live* for about 40.

**Answer 1: the caller builds it, so no.** `TcpOpts`, `Timeouts`,
`H1Opts`, `H2Opts` and `FetchOpts` exist to be written
`Struct { one: Some(n), ..Default::default() }`, and the attribute forbids
exactly that expression from outside the defining crate — functional
update included. `TcpSupport` and `TimeoutSupport` are the same answer
with a different caller: a **runtime or transport implementor** writes
those, and an implementor outside this workspace is the whole point of the
seam. `WebSocketKeepAlive` looked like this group and is **not** in it,
which is what identifies the real discriminator: `::new(every, within)` is
its only construction path, so the attribute costs nobody anything. The
shape of the type is not the test; how it is built is.

**Answer 2: exhaustiveness is the mechanism, so no.** `Event` already
carried this in writing — a new variant must be a compile error for every
backend, and *the design worked and the running of it did not* is a
sentence this file records about that exact property. The capability enums
join it, because `Client::build()` refuses a setting a transport cannot
honour, and a `_` arm there is the **silently ignored setting** defect
this project has closed four times. So do `RetryKind`, `RedirectPolicy`
and `Discovered`, each of which encodes a distinction something branches
on.

**The sharpest case in this group was found by the compiler rather than by
reading, and it splits a class this workspace had treated as uniform.**
Fourteen error enums were already `#[non_exhaustive]`, so `SvcbRecordError`
looked like the fifteenth — and `hclient-dns-system` and `hclient-dns-doh`
both refused to compile, because both **translate** it variant by variant
into their own error types. **An error's answer depends on who stands on
the other side.** One that reaches an end caller can afford the attribute:
the caller's `_` arm says *something else went wrong*, which is true. One
that crosses a seam into a translator cannot: there the `_` arm is a
*mapping*, and a new variant would quietly acquire the wrong one.
`RawParam` is the same file's other half, and its `Other(u16)` is why — an
unknown SvcParamKey never adds a variant, so a new variant means *this
crate now parses that parameter* and every converter owes it a decision.

**Answer 3: the library hands it back and the caller only reads it —
yes**, and that is the 21. Errors with public fields (`UnexpectedStatus`,
`ResponseTooLarge`, `RedirectRefused`, `InvalidBaseUrl`, the four SOCKS and
WebTransport ones), parsed values that will grow with their RFCs
(`SetCookie`, `RequestDirectives`, `ResponseDirectives`), and reports
(`Config`, `Follow`, `Disagreement`, `RecordedRequest`).

**What the attribute does not buy is worth stating, because it is what
prompted the exercise.** Of the six types that took a semver-breaking
change in the 31 commits before the trigger, `#[non_exhaustive]` would
have saved **two** — `Phase`'s new variant, and nothing else that is now
marked. `TcpOpts`, `TcpSupport`, `Timeouts` and `TimeoutSupport` are
all answer 1, and `Connected::remote` changed a field's *type*, which no
attribute has ever protected. The freedom this workspace has been spending
was never mostly about additions.

### `pin_project_lite` reaches one of five boxes, and the other four are the finding

`Pin<Box<T>>` around a **concrete** type appears at five sites here, and
each carries the same note: `tokio::time::Sleep` is `!Unpin`, this
workspace forbids `unsafe`, so there is no projection to be had and the
box is what stands in for one. The premise is false —
`pin_project_lite` is a projection with no `unsafe` at the call site, it
was already in this crate's graph, and `http3/runtime.rs`'s `SeamTimer`
had been using it for a vertical.

**`pool::Reaper` is the one that converted, and it went from two boxes to
none.** The inner box held the sleep; an outer `Box<ReaperState<..>>`
existed to make `Reaper` `Unpin` for every `R`. Neither was needed,
because `Unpin` was never the requirement: `Spawn` declares no bounds and
neither shipped runtime's impl adds one, so a spawned future may be
`!Unpin`. That is a change to a public type, and it is safe here because
the only thing anyone does with a `Reaper` is hand it to `spawn`, which
takes it by value.

**`IdleTimeout` is the one that must not, and it was measured rather than
argued.** It is the site with the most to gain — it drops and rebuilds
its sleep on **every gap between frames**, so the box is an allocation
per gap rather than per connection. Converted, it compiles and all 2259
tests pass. What it costs is invisible from in here: `IdleTimeout` is
`NativeBody`, a **public** response body, and
`http_body_util::BodyExt::frame()` is `where Self: Unpin`. A green suite
means this workspace never calls it; a consumer holding a body from
`Native::execute` directly does. `pin_project_lite` generates its own
conditional `Unpin` and offers no way to keep the hand-written one, so
the choice is binary and the box stays.

`connect::Answers` and `http2::keepalive::Phase` are the same wall from
the `&mut` side: both are polled through `&mut self` by a chain of
ordinary functions, so converting either means threading `Pin<&mut ..>`
through that chain to save two allocations per connection and one per
ping interval. `hclient-core`'s `MapErr` is a third shape — `box_body`
allocates twice per response body, and one of them would go — at the
price of putting `pin-project-lite` into `hclient-core`, measured at
13 crates to 14 for every graph that has no transport.

**Two limits of the macro are worth knowing before the next attempt,
because both look like a mistake in your own code.** In 0.2.17 a bound
list is one predicate at a time — `I: Read + Write` is `no rules expected
"+"` — and a `///` on any **field** is `no rules expected "="`. The
second is why `SeamTimer` had no field docs and read as a style choice.
Nothing is lost by writing them `//` where the fields are private, since
rustdoc does not render those anyway.

### No crate for a job the standard library now does, and the gate is not `cargo deny`

`cfg-if` and `assert_matches` are gone — `core::cfg_select!` (stable
1.95) and `std::assert_matches!` (stable 1.96) replace them, at 98 call
sites for the second. Neither crate is banned for anything it did: both
are small, well made, and were the right answer when they were taken.
What changed is underneath them, so each now buys a name that `core`
already exports.

**The gate reads the manifests rather than the resolved graph, and
`cargo deny` was tried before it was rejected.** It is already wired into
`just supply-chain` and its `[bans]` table has a `use-instead` field that
says exactly what this rule wants to say. Both entries were written and
run, and the result separates the two crates:

- `assert_matches` is **not in the resolved graph at all**, so the entry
  is silent and means precisely *nobody here may declare it*;
- `cfg-if` fails at once — `error[banned]: crate 'cfg-if = 1.0.4' is
  explicitly banned` — because it arrives through **nineteen**
  third-party parents: `ring`, `sha2`, `js-sys`, `openssl`,
  `encoding_rs`, `getrandom`, `chacha20`, `parking_lot_core` and the
  rest, not one of them ours.

`wrappers` would take the second case at the price of a list of somebody
else's parents that goes stale on their next release. But the reason to
reject the tool is not that one crate is awkward: **`[bans]` reads what a
build resolves, and this rule is about what this workspace declares.**
The quiet entry is quiet by luck — the day any dependency takes up
`assert_matches`, the identical entry starts failing for something nobody
here did, which is `cfg-if` today arriving early. A check that cries wolf
gets silenced, which this file treats as the mirror of a check that
cannot fail.

So `scripts/no-crate-for-what-std-does.sh` reads every dependency table
in every workspace manifest — `dev-` and `build-` included, their
`[target.<cfg>]` forms, and the root's `[workspace.dependencies]`, where a
re-entry would most likely be staged. `tomllib` rather than `grep`,
because `system-resolver`'s own manifest argues about `cfg-if` in prose
for a paragraph and a grep cannot tell an argument from a dependency. A
renamed dependency is caught by its `package` key. Checked in three
failing directions: a plain dependency, a renamed dev-dependency, and a
run over zero manifests, which fails closed.

The message names the replacement and the release that made it possible,
which is the one idea worth taking from `cargo deny`: a reader who meets
the refusal is told what to write instead of being told no.

### `cfg_select!` pays for a module and charges for a function body

`system-resolver`'s `sys/mod.rs` took it first, `hclient-idn` second —
four sites there where a condition had been written twice, once negated:
the *at least one backend* guard, `backend()`'s tail, and the two
`testing` functions whose bodies were two `#[cfg]` blocks in a row,
mutually exclusive only because `build.rs` says so. Both `testing` arms
are written with **two positive arms and no `_`**, so a third platform
backend is *none of the predicates in this `cfg_select` evaluated to
true* at the line that would have been wrong, rather than a silent
routing into Foundation's body.

**Then the workspace was swept for the same shape, and the sweep is worth
more than the conversions.** Seventeen sites write a condition twice with
one negated. Thirteen are a single feature — `#[cfg(feature = "cookies")]`
beside `#[cfg(not(feature = "cookies"))]` — where the pair is already
minimal and symmetric and a macro would be five lines for two attributes.
Two more are `#[cfg(unix)]`/`#[cfg(not(unix))]`. `hclient-otel`'s enum
variants and `decompress`'s match arms cannot be selected by this macro
at all, and their features are **additive**, so an ordered select would be
the wrong shape twice over. `client.rs`'s default-transport gate repeats
its condition for a reason written beside it — *the branch a reader is
standing in should say what it excludes.*

**Two conversions were built, measured and reverted**, and that is the
rule. `ecn_is_really_on` in both UDP runtimes is a six-platform list
written twice, the second time negated — the textbook case — and
`hclient-proxy`'s `platform()` is the same with three. Both were
converted and both were undone, because **rustfmt does not descend into a
macro arm**: checked in the failing direction by mangling the indentation
inside a converted arm and watching `cargo fmt --check` stay green over
it. So the conversion silently removes 17 lines from `just fmt-check`'s
reach in the first case and about 150 in the second — and the second is
platform code nobody on this host compiles, which is exactly where
formatting drift would go unnoticed longest.

So the line is what an arm *contains*: a module declaration
(`sys/mod.rs`), a short expression or an empty block (`hclient-idn`, 25
lines in total and eleven of them a `compile_error!` string) is on the
paying side; a function body is on the charging side. A check that
quietly stops covering code is this file's recurring defect with the
subject changed.

### Every error type lives in a file called `error.rs`

A reader asking *what can this crate refuse* had to read the crate. Error
types sat wherever the code that raises them sits — `lib.rs`, `body.rs`,
`keepalive.rs` — so the answer was a grep rather than a file. They are all
in an `error.rs` now, in every crate that has one, and the convention is
held by `just invariants` rather than by memory.

**The rule is about the file *name*, not about one file per crate.** A
self-contained subsystem keeps its own: `hclient-native/src/http2/error.rs`
and `.../http3/error.rs` are legal, and so is `hclient/src/cookie/error.rs`.
What the check forbids is an error type beside the code that raises it,
which is the state that made the question a grep. The boundary is written
in each `error.rs`'s own module doc, so the next type has somewhere to look
rather than a precedent to copy.

**A type goes by what it is, not by which half of a `Result` it appears
in.** `hclient-mock`'s `RecordedBody` is not an error and stays where the
recorder is; `hclient-native`'s `ConnectTimedOut` is one and moves — and
there are two of that name, in the crate root and under `http3/`, bounding
a TCP connect and a QUIC handshake respectively. That is the boundary
working rather than a collision.

**No consumer's `use` line moved.** Every type is re-exported under the
path it already had, and a type that was private becomes `pub(crate)` —
the smallest visibility change that compiles, never `pub`, because the
convention is about where a maintainer looks and not about widening a
surface.

**Ordering inside the file is by how far the request got** — what a
builder refuses, then what a connect refuses, then what a body loses —
because that is the order a reader debugging a failure arrives in, and
alphabetical order is the order of a type nobody is looking for.

**The check is `scripts/errors-live-in-error-rs.sh`, and it is two greps
rather than one.** The survey that sized this work looked for
`#[derive(thiserror::Error)]` alone and undercounted by three crates:
`hclient-cli`, `hclient-tls-rustls` and `hclient-tower` write
`impl Display` + `impl Error` by hand, and those were exactly the types
nobody had looked at recently — the check finds a class it would otherwise
have been blind to by construction. A `#[cfg(test)]` module is exempt, by
brace counting rather than by *after the first `#[cfg(test)]`*, so a test
module in the middle of a file does not blind the check to everything
below it; the exemption is about who reads the type, since a fixture that
exists to make one test fail has no caller asking what the crate refuses.
It fails closed on finding fewer than 15 `error.rs` files, because a
`find` that matched nothing and a tidy tree print the same thing
otherwise — this file's own recurring defect.

**Two things went wrong doing it and both are worth knowing.** A carve
script that moves a `pub struct` by walking back over `///` and `#[` lines
stops at the *first* line ending in `)]`, so a multi-line `#[error(..)]`
below a `#[derive(..)]` is left behind attached to nothing — three types
here, caught by the compiler. And moving a hand-written error moves *three*
items, not one: the struct, its `Display` and its `Error` impl. A script
matching only the struct leaves two orphans that still compile, so nothing
fails — it was the new check that found them, on the same run that was
meant to confirm the work was finished.

### The rendered docs had no check, and the prose is the product

`just docs` — `RUSTDOCFLAGS="-D warnings" cargo doc --workspace
--all-features --no-deps`, in the `lint` job. It was run for the first
time when publishing made the output visible, and it reported **96
warnings across 17 crates**. Four kinds, in rising order of harm, and the
order is the point: the cheapest is cosmetic and the dearest is
invisible.

**Two unclosed HTML tags, which silently delete the rest of the sentence
from the page.** `<S>` and `<usize>` escaped their code spans, and one of
them for a reason no reader would find by looking: a doc line beginning
`+ Unpin` starts a **markdown list**, which closes the code span opened
on the line above, so every backtick after it pairs one off and
`Stream<S>` lands outside any span. Counting the backticks says the line
is balanced. It is the parser that disagrees.

**Twelve unresolved links from a single shape** — a link target wrapped
across two `///` lines. rustdoc does not rejoin the path, so the target
is `crate::` followed by a newline. Searching for the *shape* rather than
working the warning list found a thirteenth the warnings had classified
differently, which is the argument for fixing a class rather than a list.

**Forty-eight links from public prose to private items**, which docs.rs
renders as literal `[`brackets`]`. These are the maintainer's cross
references — `crate::pool`, `crate::hooks`, `Native::run` — and a reader
of the published page meets them as punctuation.

The remaining fourteen were answered one at a time rather than silenced:
six named a crate that is genuinely not a dependency (one of them a
*dev*-dependency, which resolves in a test and not in a doc build), three
only needed a path, one named a private helper under a name it does not
have, and one — `[RFC5987]` — is a verbatim quotation from RFC 7578 and
is escaped so it stays the quote it is.

**This is the third shape of the rule this file keeps recording.**
`test-doc` was a recipe nothing called; `test-no-default` was a recipe
that printed `error:` and exited zero; this was **no check at all**, for
the artifact this project's whole method rests on. The recipe fails
closed twice — on rustdoc's warnings, and on there being no sign rustdoc
ran, because printing nothing and finding nothing are indistinguishable —
and it was checked in the failing direction rather than assumed: a
deliberate `[`ThisDoesNotExist`]` takes it to exit 101.

`--all-features` is not thoroughness, it is the same fact as the docs.rs
metadata beside it: `default` is empty or near-empty in every crate here
by design, so a doc build without it checks the smallest part of the
surface and publishes the same.

### `clippy::pedantic` is on, and the two lints refused are refused for reasons of this workspace's own

`[workspace.lints.clippy]` sets `pedantic = { level = "warn", priority =
-1 }`. `warn` rather than `deny` because `just lint` already passes
`-D warnings`, so the gate fails on a pedantic finding exactly as it
fails on any other, and an editor shows the same set with no flag
anybody has to remember. `priority = -1` is what lets a single lint be
given back by name without the group overriding it.

**Measured before it was decided: 1309 findings, of which 1024 carried a
machine-applicable suggestion.** `cargo clippy --fix` took those in one
pass. The three biggest groups are the ones a reader meets rather than
the ones a compiler does — `doc_markdown` at 282, `must_use_candidate`
at 179, `semicolon_if_nothing_returned` at 86 — and the 285 that
remained are the ones that wanted a decision.

**What the group is actually worth here is the documentation half.** 68
`missing_errors_doc` and 48 `missing_panics_doc` are public functions
that return a `Result` or can panic and never told a caller when. Many
already explained the failure in prose — `check_version`'s doc states
its exact error type and condition in its second paragraph — so the fix
there is the `# Errors` heading rustdoc looks for, over text that was
already right. The rest were genuine gaps on a crate that had just
frozen its public interface.

**`large_futures` is given back, and it is the one that would have been
a second check disagreeing with a first.** It fires 210 times and **not
once in library source**: every site is a test awaiting a client future.
This workspace already bounds those, with ceilings it measured rather
than a constant somebody picked — 6 KiB in `hclient/tests/future_size.rs`
and 24 KiB in `hclient-native`'s, both derived from the 1.81x an extra
`async fn` layer was measured to cost. Clippy's default is 16 KiB, so
the lint would refuse `Native::execute`'s deliberate 15,480-byte future
while this workspace's own guard passes it. Two checks disagreeing about
one fact is what those guards exist to prevent, and the guard is the one
that fails closed on the defect.

**`must_use_candidate` is given back because the attribute would stop
meaning anything.** `#[must_use]` says *ignoring this is a bug*. That is
true of `Timeouts::or` and false of `Capabilities::redirects` — reading
a capability to log it is not a mistake. Applied to all 136 source
sites it would be noise on the ones that carry a real claim, and it is
already where it earns its place: on the `const fn` builders, where
dropping the answer really is the error.

**The sharpest finding is not a lint at all — it is which crates the
group could not see.** Seven crates spell out their own `[lints.rust]`
for the `unsafe_code` exemption, and a crate that opts out of
`[lints] workspace = true` opts out of **every** workspace lint table,
clippy's included. So each needed its own `[lints.clippy]` block. Worse,
most of them build only for a target the workspace run never touches:
`hclient-fetch` on wasm, `hclient-urlsession` on Apple, `hclient-winhttp`
on Windows. Checked on their own targets they carried **443 further
findings** that `cargo clippy --workspace` reports zero of. That is this
file's *a green `--workspace` run is a claim about the workspace, not
about any crate in it*, met from a fourth direction — and the reason the
per-crate and per-target recipes exist rather than the sweep alone.

**What the cast lints found, checked one at a time rather than
blanket-allowed.** `cast_possible_truncation` fires on wire-format and
FFI arithmetic, which is where a truncation is a defect rather than a
style question. SOCKS5's four are all genuinely bounded — `password_auth`
refuses a credential over 255 bytes at the setter with a test beside it,
and `HostTooLong` guards the other — so they take an `#[allow]` naming
the check that bounds them. `hclient-urlsession`'s was not: `statusCode`
is an `NSInteger` and `as u16` would wrap an out-of-range value into a
plausible status, so it is `u16::try_from` now, falling back where a
delegate with no readable status always fell back. **An `#[allow]` that
names the bound is a claim somebody can check; a bare one is a claim
nobody can** — 117 allows landed and every one carries its reason.

**Two lints ask for opposite spellings of one expression, which is worth
knowing before obeying either.** `redundant_closure_for_method_calls`
wants `is_some_and(|e| e.is::<T>())` written as the method path, and the
only path that names it is `<(dyn Error + 'static)>::is::<T>` — which
rustc's own `unused_parens` then rejects. The closure stays, with an
`#[allow]` naming the pair, because a lint that cannot be satisfied is a
lint to answer rather than to chase.

**The rule about allows is a gate now, and writing it found four that
were not.** *An allow naming its bound is a claim somebody can check; a
bare one is a claim nobody can* was held by habit alone, which is this
file's own recurring defect with the subject changed:
`scripts/every-allow-names-its-reason.sh` reads all 123 and refuses one
with nothing beside it. Three spellings count — a comment above, a
trailing one after, and rustc's own `reason = ".."` — because a rule that
refuses a legible form invites the bare allow it exists to prevent. It
found four on its first run, and the sharpest was a reason `cargo fmt`
had moved to the line *below* its attribute, where a reader arriving at
the attribute meets nothing. What it deliberately does not check is
whether a reason is **true**; nothing can. What it ends is the allow
there is no way to argue with.

**And then the suppressions became `#[expect]`, which is the rule checking
itself.** `#[expect]` (stable 1.81) is `allow` plus one property this
workspace wanted from the start: an expectation whose lint **no longer
fires is a warning at the site**, and `-D warnings` makes it an error —
so a suppression that has outlived its reason fails the gate instead of
sitting there silently, which is exactly the allow nobody can argue with,
aged. All 267 attributes converted in one pass; 42 reverted, and the
pattern in what reverted is the finding: **an expectation is the wrong
answer exactly when the lint's firing is a fact about the build rather
than about the code** — `dead_code` that is per-binary in a shared test
module, or per-feature in a `#[cfg]`'d backend (system-resolver's
`sys/mod.rs`, where a build compiles exactly one platform reader; the
shared TLS test fixtures, where each binary uses its own subset;
`hclient-rt-embassy`'s smoltcp endpoint, where "no IPv6" is an absent
enum variant rather than a runtime branch). Each such site keeps
`allow`, and its `reason` names where the lint does fire — which is the
same claim the expectation would have made, stated about the family of
builds instead of the one at hand. Found mechanically, by looping
`-D warnings` clippy over all-features and the no-default combinations
and reading the unfulfilled sites back, rather than by enumerating the
exceptions from prose. One instrument lesson: the unfulfilled span
points at the lint name *inside* a multi-line attribute, so a converter
keyed on the reported line missed them until it walked up to the
opening. The gate reads both spellings, so the census does not depend on
which one a site took.

**And wiring it up found that four of the eight invariant gates had no CI
step at all** — `errors-in-error-rs`, `no-crate-for-what-std-does` and
`versions-agree` besides this one, the first three since they were
written. They ran for anyone typing `just invariants` and on no push.
**`ci-mirrors-just` could not see it, and that is the interesting half**:
it asserts every `run:` names a recipe that exists, which is the converse
of every gate having a `run:`. A check with a direction has a blind side,
and this one's is the side the gates live on. All eight have a step now,
49 calls across 49 steps.

**A `#[cfg(not(feature = ..))]` twin fires `unused_self` and
`unused_async`, and obeying that would fork a signature on a feature.**
Ten such stubs across `hclient` and `hclient-native` exist to keep the
call sites free of a `#[cfg]` — the feature-on half needs the `&self` and
the `async`, the twin does not, and the point of the pair is that a caller
cannot tell them apart. They carry an `#[expect]` saying so. **Every one is
invisible to `--all-features`**, which is the workspace run: they surface
only under `just test-no-default`, the recipe this file records as having
once printed `error:` and exited zero.

**And the sharpest cost was a defect the sweep itself introduced.**
`ref_option` is a good lint — `&Option<T>` really should be
`Option<&T>` — and applying it to `config::effective_redirect` and
`check_redirect_supported` changed two signatures and left **six call
sites stale**, so `hclient` stopped compiling. `cargo clippy` was green
over it, because clippy checks the crate it is given and those callers
live behind `--all-targets`' test build. What caught it was
`cargo nextest run --workspace`, run because a clean lint is not a claim
that the code builds. The same pass changed
`TungsteniteWebSocket::new`'s `read_buf` from `Bytes` to `&Bytes` —
**public API**, for a borrow the body's own `to_vec()` makes pointless;
that one is reverted with the reason written where the parameter is.
**A lint that rewrites a signature is a refactor, and a refactor is
checked by building, not by re-running the linter.**

### A capability that answers yes or no is a `bool`

`Capabilities` carried eleven `bool` fields and four two-variant enums —
`CancelSupport`, `ReuseSupport`, `EarlyDataSupport`, `DecompressionSupport`,
each `None | Supported` — for the same shape. All four are `bool` now.

**What the enums bought was nothing.** None had an `impl` block. The one
comparison at a call site was `== EarlyDataSupport::None`, which is what
`!` does. And no doc anywhere explained why an enum rather than a `bool`:
three carried a *"why two variants and not three"* section, which answers
a different question while eleven `bool` siblings sat in the same struct.

**The usual argument is a third variant later, and this crate's own
history runs the other way.** `RedirectSupport` *lost* two variants, under
the rule that a variant exists only if a caller decision turns on it.
These shrink rather than grow, and each removed enum's doc said so about
itself: the third value it named had no producer and no reader.

`DecompressionSupport` was held back one commit on the reading that
`None | Internal` names *who* decodes rather than *whether* — and it is
still binary, because there are exactly two parties. Either the transport
hands over decoded bytes (and owns `Accept-Encoding`), or it hands over
the wire bytes and the client decodes. No third party can decode a body
nobody else has.

**What stays an enum is what has a third state that is really reachable.**
`RedirectSupport` is `None | Transparent | Internal`, and the middle one
exists because *the backend does not follow redirects* and *the backend
follows them where we cannot see* are different facts a caller acts on
differently. `TlsSupport` is the same shape. `TimeoutSupport` is a struct
of four `bool`s because it answers four questions rather than one.

The rule: **a capability field is a `bool` unless a third state is
reachable and a caller branches on it.** Ask which caller decision the
third variant serves; if the answer is a hypothetical backend, it is a
`bool`.

### Erasure is named `Box*` and lives beside the trait it erases

Two conventions, and both were settled by noticing the crate already
followed them almost everywhere.

**A boxed form of a trait is called `Box<Trait>`** — `BoxBody`,
`BoxSleep`, `BoxInstant`, `BoxFlow`, `BoxExchange`, `BoxCacheStore`. Not
`Boxed*`, not `Any*`, not `Erased*`. Three prefixes for one idea was three
things to learn, and `futures_core`'s `BoxFuture`/`BoxStream` is the name
a Rust reader already has. Thirteen types were renamed to reach it.

**The object-safe trait a `dyn` is taken of is `Dyn*`**, and that is a
layering rather than a synonym: `Box*` is what a caller names, `Dyn*` is
the shape that makes boxing possible. `BoxCacheStore(Box<dyn
DynCacheStore + …>)` is the pattern, and `BoxInstant = Box<dyn
DynInstant>` is the same thing one crate down.

**Three traits carried the wrong half of that pair for a week**, which is
what asking *what is actually named `Box`* found. `BoxTransport`,
`BoxTimer` and `BoxInstantOf` are object-safe traits — `SharedTransport`
is literally `dyn BoxTransport + Send + Sync` — so each was a trait
wearing the name of the box taken of it, in a workspace whose own rule
two paragraphs up says otherwise. They are `DynTransport`, `DynTimer` and
`DynInstant` now — and asking the question with a grep that did not
assume `pub` found two more, `BoxStaged` and `BoxStagedConnect` in
`hclient-native`'s QUIC arm, `pub(crate)` and the same shape again
(`Box<dyn BoxStaged<'a> + Send>`). Five in total, not three, which is why
the sweep was worth running rather than reasoning about.

**The `Of` suffix is the tell worth keeping.** `BoxInstantOf` was named
that way to dodge a collision with the `BoxInstant` alias beside it, and
a suffix that exists only to make a wrong name compile is the wrong name
announcing itself. Under the convention there is no collision to dodge:
the trait is `DynInstant` and the alias is `Box<dyn DynInstant>`. The
rule's own sentence had said this — *giving them one name is a compile
error rather than a style question* — and the error had been answered
with a suffix instead of the rule.

**It cost nothing because `hclient-core` is at an unpublished
`0.1.0-alpha.8`**, which is the same window the stable-version reversal
used. None of the three is re-exported from a crate root, so the change
is 26 references across seven files and no consumer's `use` line.

`Shared*` stays for what is genuinely shared rather than boxed —
`SharedTransport` and `SharedTimer` are unsized `dyn` behind an `Arc`, and
`Box<SharedTransport>` is a real use site. A name that says `Box` where an
`Arc` is meant would be worse than the inconsistency.

**And the boxed form lives beside its trait, not in a module of its own.**
`hclient-core` had an `erased` module holding both halves of the `Client`
erasure, on the argument that what unites them is *why* they exist — one
concrete facade type instead of two type parameters — rather than what
they erase. That argument is real and it lost to a simpler one: the crate
was already doing the opposite everywhere else. `BoxFlow` sits in `auth`
beside `AuthFlow`, `BoxSendExchange` in `transport`, and `hclient`'s
`SharedRetryPolicy` beside the policy it shares. `erased` was the outlier,
and `futures_core` keeps `BoxFuture` in `future` rather than in an
`erased`.

Dissolving it also ended a collision worth naming: **`hclient` has its own
`erased` module**, holding the store wrappers, so the family had two
public modules of one name and different contents.

What the split cost is one cross-module doc link that had to be qualified,
which is the boundary announcing itself. What it buys is that a reader of
`transport` meets `DynTransport` where the reason for it is, and a reader
of `timer` never meets it at all.
