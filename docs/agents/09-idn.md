# hclient-idn

> Part of the [AGENTS.md](../../AGENTS.md) record, split 2026-10-05. The root
> file keeps the operational rules and the index of parts. Sections below
> keep their original headings, and a "this file" in the text named the
> whole record when it was written.

### `hclient-idn` had four backends, two functions and one feature

Android joined, `foundation.rs` became `apple.rs`, and the four features
became one. The shape that came out is worth more than any of the three
changes — and it is three backends now: Apple left, for the reason the
section below it gives. What is recorded here is what happened and why
the shape is the shape; the counts and the Apple row are corrected in
place.

**One feature, `idna`, off by default, and it *forces* rather than
selects.** With it on, every target answers through the bundled crate and
its Unicode tables; with it off, each platform answers from what it
already carries — `icuuc.dll` on Windows, `android.icu.text.IDNA` on
Android, and the bundled crate on the ELF unixes, wasm and Apple, because
there is no UTS 46 implementation there to ask. Linux takes `idna` either
way, which is why the switch is a forcing one: on a target where the
answer does not change, the feature buys nothing. (Apple was in the first
list until the corpus was run against Foundation; it is in the second
now, and the feature buys nothing there either.)

It replaced `platform`/`bundled`/`system-icu`/`foundation`, whose
combinations could select two backends at once or none — `build.rs`
carried the `if`s and `lib.rs` carried a `compile_error!` for the empty
case. The new rule is a total function from (feature, target) to one
backend, so exactly one cfg comes out, the `compile_error!` has no
subject, and `just features` stopped excluding this crate: the exclusion
existed because isolating one of four features was a build the crate
deliberately refused.

Measured, `cargo tree -e normal`, unique crates:

| target | without `idna` | with |
|---|---|---|
| Linux | 46 | 46 |
| Windows | **11** | 48 |
| Apple | 46 | 46 |
| Android | **24** | 59 |
| the browser | **25** | 50 |

**Four rows fork, and the browser is the one that reads wrong.** 25
against 50 is the smallest ratio here and the largest saving the crate
has anywhere — 20.7 KiB against 143.0 KiB — because what a wasm module
weighs is data rather than dependencies, and a crate count cannot see
that. Linux is the row where the feature buys nothing, which is the
table's point and is unchanged.

**Android reaches UTS 46 through the JVM, and the reason it is worth two
crates is the twenty-five it keeps out.** `android.icu.text.IDNA` is
ICU4J — the same ICU the Windows backend calls, the same option bits
under the same names — and the NDK exposes no C entry point for it, so
`jni` + `ndk-context` is the way in, exactly as `hclient-proxy` reaches
the system proxy settings. Amendment C19 covers both files now.

**The errors are read by name, and that is the one place the two ICUs
differ in shape.** ICU4C reports `UIDNAInfo.errors` as a bit word and
ICU4J reports `IDNA.Info.getErrors()` as an `EnumSet`, which cannot be
masked. Reading `hasErrors()` instead would have been three JNI calls
fewer and a second divergence — Android refusing `-münchen.de` and
`ab--cd.münchen` where Windows and Linux accept them — and this crate
already carries one such divergence, on Apple, as a recorded cost rather
than a design.

**It has been run now, and the first run refused every name.** An
emulator, API 35, no APK — a `cdylib` whose `JNI_OnLoad` registers the VM
and one `app_process` invocation. None of the failure was JNI: the VM was
registered, `android/icu/text/IDNA` resolved, `getUTS46Instance(0x3c)`
returned an instance, and `nameToASCII` answered `xn--strae-oqa.de` for
`straße.de`. The defect was one line of ours — the walk shared by both
directions had been factored out of the ASCII one and kept its closing
check, *the answer must be ASCII*, so `nameToUnicode` refused every
conversion it performed correctly, the acceptance probe's reverse half
failed, and the backend was rejected at first use.

That is `hclient-dns-system`'s Apple arm a second time, with the same
moral and a better ending: an arm that compiles is not an arm that runs,
and the only way to know is to run it. The run also caught
`NoImplementation`'s message advising a reader to *enable the `bundled`
feature*, which had been replaced by `idna`.

**Thirteen cases agree with `idna` on the device after the fix**,
including every error name the backend forgives — `a..b` came back
`[EMPTY_LABEL]`, `a-.de` `[TRAILING_HYPHEN]`, `ab--cd.de` `[HYPHEN_3_4]`,
all forgiven and all matching — and the one it does not, `xn--zzzz.test`
with `[PUNYCODE]`. The option constants were confirmed to be ICU4C's, so
the crate's `0x3c` means the same thing in both.

It is **not** a CI job: an emulator boot needs minutes and KVM, and what
the run established is that the code is correct rather than that it will
stay so. The cheaper half of staying so is already there —
`check-targets` compiles the backend in both feature settings and
`graph-idn-backend` asserts the tables stay off Android — and
`.notes/android-idn-live.md` is the other half, a run anyone can repeat
in ten minutes.

**Two public functions**, `domain_to_ascii` and `domain_to_unicode`, plus
the error their `Result` needs. Everything else — the option word, the
error mask, the deny list — became `pub(crate)` or moved behind the
`#[doc(hidden)] testing` seam the differential corpus and the fuzz
targets already used. `Handle::name` went with them and is not replaced:
four strings, one per backend, that nothing branched on, and the one test
that read a name was really asking whether the acceptance gate had
passed. `testing::has_platform()` is that question with one answer.

**`domain_to_unicode` is the platform's reverse direction**, like the
forward one: `uidna_nameToUnicodeUTF8` on Windows,
`IDNA.nameToUnicode` on Android, `idna::domain_to_unicode` in the
bundled build, and `NSURLComponents::host` on Apple — where `NSURL::host`
hands out the A-label and `NSURLComponents` splits the same host into
`encodedHost`, which is that A-label, and `host`, which is decoded. The
naming misleads exactly as the existing macOS test's doc comment records:
for an IDN the *encoded* getter is the ASCII one. Nobody here has a Mac,
so the acceptance probe runs in **both** directions and a build where
that getter is not what Apple documents answers `NoImplementation`
instead of a plausible wrong name.

### Deleting the policy layer broke two backends, and only CI could see it

The layer went because it answered a question that was not this crate's —
*may this host be contacted* — and that was right. What went with it was
not: `AsciiDenyList::URL`, applied by the bundled backend alone, and the
half of UTS 46 that `NSURL` does not do. Neither is visible from Linux,
where the backend **is** `idna` and the whole class disappears, so both
shipped and both were found by the first CI run over seventeen commits.

**The deny list is one line and it is `idna`'s, not a policy.** ICU and
Foundation take no such list, so `domain_to_ascii("a<b.com")` began
erroring on Linux and answering on Windows and macOS. It is applied once
now, in `domain_to_ascii`, **on the converted name** — the ordering the
fuzzer established, since `">\u{338}"` composes to `≯` and a check before
mapping refuses a character UTS 46 removes. `bundled` drops to
`AsciiDenyList::EMPTY` so there is exactly one statement of it: with two,
removing the shared one left Linux green, and a mutation nothing can kill
on the only platform this workspace runs is not a check.

**The Apple half was eight rows, and the cause is that `NSURL` is a URL
parser.** It maps and converts a Unicode host and then stops: it does not
case-fold ASCII (`EXAMPLE.COM`, `XN--MNCHEN-3YA.DE` came back as written)
and it does not validate an ACE label (`xn--zzzz.test`, `xn--a.de`,
`xn--.de`, `xn--a-.de` came back unchanged where `idna` refuses them).
Two more are the mirror: `""` and `a"b.com` are legal names UTS 46 leaves
alone and are not URLs a parser will take, so Foundation refused what
`idna` answers.

**Apple left, came back, and left again, and the third decision is the
one with a rule behind it.** Closing those eight rows took
a punycode decoder and a conversion sequence — fold, decode the ACE
labels, skip the parser for an all-ASCII name, convert, check the answer
is about the name that was given — which is this crate reimplementing the
thing it exists to avoid reimplementing. The rule that decides it is
narrower than *the OS ships something*: it is **the OS carries a UTS 46
implementation**, and `NSURL` is a URL parser that happens to call ICU.
So Apple took the bundled tables for one commit, and `apple.rs`,
`ace.rs` and `objc2-foundation` went with it — **and came back one
question later**, when the browser turned out to be the same shape and
worth 86%. What this paragraph gets right is Foundation; what it gets
wrong is the conclusion drawn from it, and the section below is that.

**What it costs is measured and it is the crate's own number**: 13 crates
on `aarch64-apple-darwin` become 46, because the ICU tables arrive with
`idna`. That number put the backend back twice — sixty-five lines of
arithmetic against thirty-seven crates of tables is not a close call —
and what settled it in the end was neither the number nor the rule above
but the browser, which is the same shape and could not be given up. **`ä..de` and
`VerifyDnsLength` stay gone either way** — that was the URL validation,
and it is not this crate's question.

### `wasi:http` will not take a U-label, so the tables ride in every component

Asked whether the transport seam could carry the URL as a `&str` instead
of an `http::Uri`, and let each host do the IDN — which on WASI would
spare a component about 140 KiB, multiplied by however many components a
deployment ships, because nothing there shares a library.

**It buys WASI nothing, and that is measured rather than argued.**
`wasi:http`'s `set-authority` takes a string and validates it: *fails if
the string given is not a syntactically valid URI authority*, and RFC
3986's `reg-name` is ASCII. Asked of wasmtime 47 through the live guest —
`set-authority("münchen.de")` is `Err`, `set-authority("xn--mnchen-3ya.de")`
is `Ok`, the second being the control that says the first measured the
U-label rather than a broken call. It is
`a_unicode_authority_is_refused_by_the_host_and_the_a_label_is_not` now,
because a claim about somebody else's interface is exactly as perishable
as the check behind it: a `wasi:http` that grows IDN support fails that
line rather than leaving a stale paragraph.

The one place IDNA appears in WASI at all is `wasi:sockets`'
`resolve-addresses` — *Unicode domain names are automatically converted
to ASCII using IDNA encoding* — and it is unreachable three ways:
`hclient-wasi` does not import sockets (the `wasi:http` world has no such
import), the function hands back addresses rather than labels, and
"IDNA encoding" does not say which of 2003, 2008 or UTS 46. That is
glibc's `AI_IDN` again, one platform over.

**So the seam change would buy the browser 17 KiB and nobody else**,
because the browser is the only host that both does the conversion and
will accept a U-label — and it already has a backend at 20.7 KiB. What it
would cost is `http::Request` itself: `uri` is an `http::Uri`, the type
sits in the public API of ten crates here, and everything that reads a
host reads it — cookie domain matching, redirect origins, the cache key,
the pool key, `Host`, the TLS server name, the HTTPS-record name, and
`resolve_reference`, which needs a parsed base. Parsing once and carrying
the parsed form is what the type is for.

So WASI carries the tables, and what that changes is the documentation
rather than the code: `hclient-wasi`'s own module doc now says the weight
is per component and names the lever, which is `--no-default-features` on
`hclient-proto` and a caller that converts once. It is the right trade
there far more often than anywhere else this workspace builds for.

### One direction, and the crate stopped answering a question nobody asked

`hclient-idn` had two public functions and has one. `domain_to_unicode`
is gone, every backend is `find` + `to_ascii` + a `Handle`, and the
acceptance probe asks one question instead of two.

**Nothing needed it, and the reason is structural rather than a survey.**
An HTTP client converts U to A because `http::Uri` refuses a non-ASCII
authority — measured, `"https://münchen.de/".parse::<http::Uri>()` is
`Err(invalid uri character)` — and never converts back, because nothing
downstream takes a U-label. A grep agrees: the only callers in this
workspace were the crate's own tests.

**What it cost to keep was four implementations of a direction with no
caller**, and one target could not supply it at all: no JS API performs
ToUnicode. That had already grown machinery — a `REVERSES` constant on
every backend, a second half of the acceptance probe, a refusal path in
the public function — to describe a narrowness that existed only because
the surface did. All of it goes with the surface.

**Two smaller things fell out, and both are the same shape.** Windows'
`uidna_nameToUnicodeUTF8` import and the `accepts_back` probe were dead
the moment the direction was; and Android's `Answer` enum, which said
whether a result had to be ASCII, has one reachable variant with one
direction. That enum is worth its epitaph: it cost the backend its first
run on a device, because the shared walk was factored out of the ASCII
direction and kept its closing check, so `nameToUnicode` refused every
conversion it performed correctly.

**And Apple is back with it.** The backend was removed on the rule *the
OS must carry a conformant converter*, which Foundation is not — and the
rule was right about Foundation and wrong about the conclusion, because
the same is true of the browser and the browser is worth 86%. What the
two share is that they are reached through a URL parser; what `ace.rs`
supplies is the case folding and the ACE validation such a parser leaves
out.

**The difference was said to be scale and there is none**, which is the
next section's finding and is corrected here rather than left standing:
the browser needed `ace.rs` in full too, and only Firefox hid it.

So the rule that decides a backend is neither *the OS ships something*
nor *the OS carries a conformant converter*. It is **the OS carries the
tables**, and a backend supplies whatever of UTS 46 the platform's entry
point leaves out — nothing for `icuuc.dll` and ICU4J, and for a URL
parser the whole ASCII half, whichever parser it is. Five backends, and
the ICU tables stay off four targets.

### The browser is the case Apple was not, and one objection nearly closed it

The same rule, asked of the last candidate: `wasm32-unknown-unknown`.
`new URL()` converts an IDN host, and it is reached exactly as `NSURL`
was — build a URL, read the host back. **What separates them is that the
standard says so**: the WHATWG URL Standard *defines* host parsing as
UTS 46 with named parameters, where Foundation's conversion is an
undocumented side effect.

**Measured before anything was written**, on the same 38 rows that caught
Foundation, in headless Firefox: **37 agree with `idna`**, against
Foundation's 30. Of the eight rows Foundation answered as itself, that
engine gets seven right — including all four invalid `xn--` labels and
both case-folding rows. The one divergence is the empty name, because
`https:///` is not a URL any engine parses, and it cost **one line**
against Apple's sixty-five.

**That ratio was read as the rule working and it was one engine's.**
`browser (chrome)` is red on the push that landed the backend, and
Chrome answers **six** of the same rows differently: it does not validate
an ACE label at all, so `xn--zzzz.test`, `xn--a.de`, `xn--.de` and
`xn--a-.de` come back unchanged where Firefox refuses them, and it
percent-encodes `a b.com` and `a\u{a0}b.de` rather than refusing them.
So the browser backend applies `ace.rs` **in full**, exactly as Apple
does, and *sixty-five lines against one* was never a fact about a URL
parser — it was a fact about Firefox.

**What survives is the rule and not the arithmetic**, which is worth
separating because the arithmetic is what made the case: the OS carries
the tables and the backend supplies the rest, and *the rest* is the
engine's business rather than the standard's. The half WHATWG defines is
the half that needs the tables — mapping, punycode, CheckBidi, ContextJ,
non-transitional processing — and every engine gets that right. The ASCII
half is where they differ, and it is free to supply, because
`ace::to_ascii_over` answers an all-ASCII name without asking the parser
anything at all.

**The defect is a measurement generalised past its sample**, which is a
shape this file records from every other direction and not yet this one.
Nothing was stale and nothing drifted: 37 of 38 was true in Firefox on
the day, is true now, and was written down as *the browser*. The check
that caught it existed from the first commit and ran both engines on
every push — `tests/web_corpus.rs` — so the cost was one red job rather
than a host reachable in one browser and refused in another. Its
divergence list is now a **union over engines**, annotated with which
engine each row belongs to, and no single run can exhaust it; what
replaces the closed-set assertion is that every divergence a run finds
must be *repaired*, so the list can record where an engine falls short
and never where this crate does.

**The saving is the largest this crate has anywhere**, because a wasm
module has almost nothing else in it: **20.7 KiB against 143.0 KiB**
through the full `wasm-pack` pipeline, 86%. It read 17.4 against 159.1
and both numbers moved at once — `ace.rs` is 5.7 KiB of the new figure,
measured against the same program calling the parser directly at 15.0
KiB, and the rest is a pipeline nothing pins: `wasm-opt`'s flags, the
toolchain, `idna`'s own release. **The share is the durable figure**, and
it moved by three points. Measure after
`wasm-bindgen`: the raw `.wasm` carries a custom section of descriptors
that the shim generator consumes and nothing ships, and it made the
browser build look 158 KiB **larger** than the one carrying ICU. That
number was nearly reported.

**And the first push of it took `hclient-proto`'s sans-io property away**,
which is worth more than the backend. `web-sys` has `Url` ready made and
pulls `js-sys`, whose optional `futures-core-03-stream` feature anything
streaming from JS switches on — `wasm-streams`, through `hclient-fetch` —
and Cargo unifies features, so the sans-io leaf grew `futures-util` on
`wasm32-unknown-unknown` and on no other target. `just
graph-proto-sans-io` failed on that push: *this crate must stay sans-io
on every target, not just the host*, which is the guard saying exactly
what it was written to say.

The repair is ten lines of `#[wasm_bindgen]` declaring `URL`'s
constructor and its `hostname` getter — amendment C20, and the thinnest
`unsafe` here, since edition 2024 makes every `extern` block one and
nothing in this file is dereferenced. It took `web-sys` and `js-sys` out
of the graph with it: the browser build of `hclient-idn` went from 25
crates to **17**. `graph-idn-backend` refuses both by name on that target
now, so reaching for the convenient dependency is a failure rather than a
decision.

**One direction only, and it is declared rather than discovered.**
`URL.hostname` hands back the A-label whatever went in, and no JS API
performs ToUnicode. That was this backend's one narrowness while the
crate had a reverse direction — a `REVERSES` constant on every backend, a
second half of the acceptance probe, a refusal path in the public
function — and the section below is what became of all of it: there is
one direction now, so the narrowness has no subject and every backend
supplies exactly what every other does.

**And the objection that nearly ended it was right about the browser and
did not reach this code.** *In a browser the client goes through `fetch`,
and the browser resolves IDN itself, so this buys nothing.* True of
`fetch`, and the conversion happens one layer earlier: `http::Uri`
**refuses** a non-ASCII authority — measured,
`"https://münchen.de/".parse::<http::Uri>()` is `Err(invalid uri
character)` — and every URL a caller gives goes through
`hclient_proto::uri::parse` in `Client`'s `effective_uri` before any
transport exists. `Transport::execute` takes an `http::Request`, so
`fetch` never sees the U-label. Without the conversion a browser build
answers `NonAsciiHost` for `client.get("https://münchen.de/")`, which is
a capability lost rather than a saving — and `config.rs` already recorded
that, in a comment about the inconsistency this client used to have.

**The check the Apple backend never had is in place from the first
commit.** `tests/web_corpus.rs` runs the corpus in both engines on every
push, and the corpus itself moved to `tests/shared/corpus.rs` so the host
and browser binaries read one copy rather than two agreeing with each
other.

**The rule the owner set, and it is the one the evidence already
supported.** This crate is a *micro-optimisation for a platform that
carries a reliable, conformant converter*. Where the platform is
troublesome and the code around it reads as props, the bundled crate is
the answer — the saving is not worth being wrong about which host is
contacted. Measured against that rule the line is not close:

| backend | what it calls | is that UTS 46? |
|---|---|---|
| Windows | `uidna_openUTS46`, `uidna_nameToASCII`, `uidna_nameToUnicodeUTF8` | yes — ICU4C's entry point, named after the standard |
| Android | `android.icu.text.IDNA.getUTS46Instance`, `nameToASCII`, `nameToUnicode` | yes — the same in ICU4J |
| Apple | `NSURL::URLWithString`, `NSURL::host`, `NSURLComponents::host` | **no** — a URL parser; there is no UTS 46 entry point to call |

Two backends call the function the standard names. The third has no such
function, so one was synthesised out of a URL parser and sixty-five lines
of ours — and needed three repairs in one week: the reverse direction,
the case folding, the ACE check. **The sharpest evidence is that our own
reliability gate could not see any of it**: the acceptance probe passed
Foundation and the corpus failed eight rows, which is the difference
between *converts the probe* and *implements UTS 46*.

**The check that replaces the backend is a graph guard rather than a
runner.** `graph-idn-backend` asserts from Linux that Apple pulls `idna`
and links no `objc2`, next to the identical pair for Android, and names
`aarch64-apple-ios` as well — the decision was "mac and iOS", and a
predicate is not a check. iOS is where the tables cost most, so a
Foundation backend returning for phones alone is the version of this
mistake nobody would look for. macOS left the
`idn-platform-agrees-with-idna` matrix with the backend: it would have
run the same bundled path as the Linux leg under a job name promising a
comparison there is nothing left to make.

**And the two lints beneath all of it had never run.** `lint-idn` sits
after `test-idn` in the same recipe, so while the tests failed the lints
were never reached: `Icu::name` had outlived `Handle::name`, and
`icu/mod.rs` was dead code on every backend that does not speak ICU4C.
Both are checkable from here — `cargo clippy -p hclient-idn --target <t>
--all-targets -- -D warnings` on the three platform triples — and neither
was checked. A recipe that stops at the first failure hides every later
one, which is this file's *a check that cannot fail* with the subject
changed to *a check that does not run*.

### `hclient-idn` leaves the shared version too, and the gates derive their own lists

It is published like `system-resolver`: its own version, its own
`cargo-release` group, its own compiler floor, independent of the
family's release. Nothing in it depends on `hclient`'s mechanisms — two
public functions, an error type, and a use outside HTTP entirely — and
`cargo tree -i` names `hclient-proto` as its only in-workspace consumer.

**The version left pre-release for the reason `system-resolver`'s did**,
and it has since gone further: `0.1.0` on 2026-09-02 and **`0.2.0` on
2026-09-05**, the second because dropping `domain_to_unicode` is a
breaking change and `cargo semver-checks` said so on the first try.
Inside a pre-release that tool executes none of its lints, because every
step out of one is a major step. Leaving pre-release is what gives the
gate a subject; versioning separately is what lets this crate do that
without promising the same of a family whose seams are still moving.

**Its floor is 1.95.0, measured rather than picked**: 1.94 rejects
`core::cfg_select!` as an unstable library feature and 1.95 accepts it,
so that macro is the single binding constraint — every dependency's own
floor is far below (`thiserror` 1.31, `windows-sys` 1.71,
`objc2-foundation` 1.71, `idna` 1.57) and edition 2024's 1.85 is lower
still. On 1.95.0 it builds `--all-targets` and passes its tests.

**What is new is that neither gate names a crate any more.** `just msrv`
finds every manifest that declares a literal `rust-version` — the same
gesture that takes a crate out of the shared version — and checks each on
its own toolchain, and `just msrv-toolchains` installs the floors it
finds, so the workflow carries no copy of them either. `just semver`
asks crates.io for each publishable crate's newest version and sorts it
into one of **five**: **no library target**, so there is no API surface
a compatibility tool could speak about; no release at all; a
**pre-release baseline** where no lint can run and that is not a defect;
a **deliberate major step** — a working tree whose version has moved out
of the published one's compatible range, where breaking is exactly what
was intended and no lint runs either — or a stable baseline, where
checks are required. A further crate is covered by all of them the day
it is written, because they read the registry and the targets rather
than a list.

**The no-library-target bucket is `hclient-cli`'s, and it was added the
day that crate went stable — before the release rather than after, which
is the only reason it reads as a decision instead of a breakage.** A binary has
no `lib`, and `cargo semver-checks` does not merely skip such a crate: it
**never names it**. Asked about several crates at once it exits `0` and
prints one fewer `N checks:` line than it was asked for, with no error
and no skip note — so the zero-check guard has nothing to see and the
count guard fires instead, reporting that the gate and the tool disagree
about what was checked. That would have surfaced only once the stable
version reached the index, presenting as *the release broke the gate*,
with the cause nowhere in the message.

The classification is derived from the **target** and happens **before**
the registry lookup, so it rests on a durable fact rather than on
today's index, and it reads the same before and after a publish.

**And the fix contained the defect it was written to prevent, which is
worth more than the fix.** Its first form asked
`cargo metadata --manifest-path <that crate>` and searched the output for
a `lib` kind — but `--manifest-path` on a *workspace member* returns
every package in the workspace (`--no-deps` bounds dependencies, not
members). Measured: thirty packages come back, so a neighbour's library
matched and the skip silently did nothing. Found only by running the
failing direction; it filters by package name now, and with the filter
the answer is `False` where without it the answer is `True`.

**The deliberate-major-step bucket is `hclient-idn`'s, and it was needed
the day the crate broke on purpose.** Removing `domain_to_unicode` from a published
`0.1.0` is what `cargo semver-checks` is for and it caught it; the answer
is a major bump rather than a smaller change, and after one the tool
permits everything and reports nothing. Without a bucket for that, a
deliberate break and a gate that silently stopped looking are the same
green run. It was verified in the failing direction at `0.1.1`, where the
checks come back.

**And the count is now per crate rather than in total**, which is the
half that would otherwise have gone quietly wrong: 196 checks from one
crate and zero from another add up to a green run over a crate nothing
examined.

**And the sentence above aged correctly, which is rare enough here to be
worth the line.** It predicted that the exemption would end by itself the
day a stable version was published, with no edit to the recipe, because
the recipe reads the registry. That day came: `hclient-idn` 0.2.0 went up
on 2026-09-05, and the gate went from `196 checks across 1 crate(s) with
a stable baseline` to **`392 checks across 2`** — the crate's own 196
running for the first time, and passing — without a line changing. Every
other claim in this file that turned out to be perishable was written as
though it were permanent; this one named the condition that would retire
it.

Two smaller things the wiring cost, both worth knowing. `[ -n "$x" ] &&
echo …` exits a recipe under `set -euo pipefail` whenever `$x` is empty,
which is the common case and killed the gate silently. And two manifests
align their `name` with their neighbours — `name         = "…"` — so a
`^name = "` pattern matched neither, which under `set -e` stopped the
loop after the first candidate rather than skipping two.

### Is the crate worth it, or should this just be `idna`? Measured: 148 KiB

Its whole justification is binary weight, and that had never been
weighed — the figures in this file were crate counts and megabytes of
*vendored source*, neither of which is what ships. Measured on one
program that converts one name, `opt-level = "z"`, fat LTO,
`panic = "abort"`, stripped:

| build | binary |
|---|---|
| a stand-in with no tables | 287.6 KiB |
| `idna` with `compiled_data` | **435.5 KiB** |
| the same with `idna_adapter` pinned to 1.1.0, the unicode-rs backend | **561.2 KiB** |

**So `idna` costs 148 KiB, and the cheap alternative to this crate is not
cheaper.** This file has recorded for two verticals that pinning
`idna_adapter` to the unicode-rs backend is *one `cargo update`, needs no
code and collapses the graph to 11 crates*. The crate count is right and
the conclusion does not follow: unicode-rs is **126 KiB larger** than the
ICU one in a stripped binary. A count of crates is not a count of bytes,
which is the same lesson this file draws about vendored source.

**And on Android it is measured on the real target rather than by
proxy**, now that there is an NDK to link with. The same `cdylib` the
live run used, `opt-level = "z"`, fat LTO, `panic = "abort"`, stripped:

| target | platform backend | `--features idna` | saved |
|---|---|---|---|
| `aarch64-linux-android` | 304.5 KiB | 443.5 KiB | **139.0 KiB — 31%** |
| `x86_64-linux-android` | 334.9 KiB | 478.3 KiB | **143.3 KiB — 30%** |

**A third of the library.** That is the number the crate lives or dies
by, and it is a share rather than an absolute because the denominator is
what a small native library actually weighs: 139 KiB against `hc`'s
5.2 MiB is 2.8% and not worth a crate, but against 443 KiB of `.so`
shipped per ABI it is the largest single item in it. `aarch64` is the ABI
almost every real device takes.

On Linux, wasm **and Apple** it changes nothing at all, because there the
bundled crate *is* the backend. Apple joined that list after Foundation
was measured against the corpus, which narrows where this crate pays to
Android and Windows — and Android is where it pays most.

What it costs is now small enough to weigh against that: **707 lines of
code** across three backends, an acceptance probe and two public
functions, after the policy layer went and Apple with it. Before it went the crate was
2,700 lines and answered a question that was not its own, and the honest
verdict then would have been different.

### And then the policy layer was deleted, because the crate is not a URL validator

`hclient-idn` is a smaller-binary `idna` — the same answers, from
whatever UTS 46 the platform already carries — and it had grown a layer
of its own that made it **stricter** than `idna` on six of twenty-two
probed inputs, `a..b` and `ä..de` among them. Answering *may this host be
contacted* is not this crate's question, and the layer is gone: 1,565
lines with its tests, and the dispatch now calls the backend directly.

**Two measurements settle that it was the layer's mistake rather than a
service.** Neither `url::Url::parse` — the WHATWG reference
implementation in Rust — nor `http::Uri`, which every request in this
workspace carries, refuses any of those names; both are conformant in
accepting them, since the URL Standard's forbidden-domain set excludes
`*` and domain-to-ASCII without `beStrict` checks neither empty labels
nor DNS length. And `hclient-proto`'s own consumer already said so in
prose: *this is still literally `idna::domain_to_ascii_cow(host,
AsciiDenyList::URL)`* — which the layer had quietly stopped being true
of.

**Removing it moved a divergence onto the oracle rather than off it.**
`tests/uri_resolution.rs` compares 96 pairs against `url`, and its
`DIVERGENCES` list carried one entry that was ours by decision:
`https://ä..de/x`. It is gone from the list, the row now pins `url`'s own
answer, and what is left is the RFC-versus-WHATWG set and nothing else.

**Both fuzz targets went with it**, and for the crate's definition rather
than for maintenance: on the Linux runner any fuzzer uses, the bundled
backend *is* `idna`, so a differential target compares `idna` with itself
and an idempotence target measures `idna`'s. What they were aimed at —
whether the platform's ICU answers what `idna` answers — only Windows can
be asked now, and `tests/differential.rs` asks it on every push.

**What survives is the acceptance probe, and it is the whole contract —
and its limit is now measured too.** A backend is used only after
answering the transitional pair the way `idna` does, in both directions;
one that does not is refused, and the crate reports `NoImplementation`
rather than a different host. Foundation *passed* that probe and failed
eight rows of the corpus, which is the difference between *converts the
probe* and *implements UTS 46*: the probe is a floor against a backend
that is wrong about the thing the crate is for, not a conformance suite.
The corpus is the conformance suite, and it is why Apple no longer has a
backend.

One property was lost and is `idna`'s own: the two directions do not
accept the same names. `domain_to_ascii` takes `AsciiDenyList::URL` and
refuses `a<b.com`; `idna::domain_to_unicode` takes no deny list and
answers it. A test asserted they agree, and it passed only while the
layer forced both through one path.

**`cfg_select!` selects a module, which is the shape this file measured
as the macro's paying side.** Each backend module exports `find`,
`convert` and a `Handle`, so `lib.rs` names one `platform` and nothing
past that line names an operating system: one `OnceLock`, one acceptance
probe, one dispatch shared by both directions. Three copies of that gate
existed before — the ICU module had its own and the two platform
backends had theirs written out in `lib.rs` — which is three chances for
one of them to stop asking. There is no `_` arm, so a fifth backend is a
compile error at the alias rather than a silent fall-through.

**The UIDNA constants moved into `icu`, and the second ICU backend is
why.** They were the crate root's while Windows was the only caller;
Android reads the same option word and forgives the same errors, so the
vocabulary belongs to the thing the two share. That made `mod icu`
unconditional with only its Windows binding gated — and drew the line the
move exposed: `UIDNA_*` and `IGNORED_ERRORS` are *what ICU decides* and
compile everywhere, where `U_ZERO_ERROR`, `UIDNAInfo`'s size and the
first-try buffer are *how one binding asks* and are gated with it.
