# Platform dependencies

> Part of the [AGENTS.md](../../AGENTS.md) record, split 2026-10-05. The root
> file keeps the operational rules and the index of parts. Sections below
> keep their original headings, and a "this file" in the text named the
> whole record when it was written.

### The floor moved under a green tree, and the fuzzer found something else

Rust 1.98 landed on 2026-08-18 and CI takes `channel = "stable"`, so the
first push after it met a compiler this workspace had never seen. Two things
came out of that, and only one of them is a lint.

**`clippy::result_large_err` is new here and fires twice**, on
`hclient-native`'s and `hclient-h3`'s private `stage`, whose `Err` is
`(Error, http::Request<RequestBody>)`. Measured rather than boxed: the pair
is **288 bytes, of which 264 are `http::Request<RequestBody>`** — a foreign
type — and 24 are `Error`. Boxing there would silence the lint and shrink
nothing a caller sees, because the public form is `connect`'s
`Result<Self::Staged, Refused>`, the same 288 bytes, which clippy does not
flag only because it is a trait implementation. So both sites carry an
`#[allow]` with that reasoning beside them, and shrinking `Refused` is left
as a seam decision for whoever needs it rather than a lint fix.

**The fuzzer found a real disagreement, and it is not about 1.98.**
`idn_policy_vs_idna` failed on
`"xn--qqqqqqqqqqHJJJJJJ'ｗJJJJJJJJJJJi-0dJd"`: the policy layer answers
`None` where `idna` answers
`Some("xn--qqqqqqqqqqhjjjjjj'wjjjjjjjjjjji-0djd")`. Reproduced locally, and
narrowed far enough to say what it is *not* — the apostrophe alone, the
fullwidth `ｗ` alone, `xn--` with either alone, and three short combinations
all agree. What is left is a long `xn--` label whose UTS 46 mapping leaves
it still undecodable as punycode, where `idna` passes it through and this
crate's own hand-written decoder rejects it.

**Why it matters more than a fuzz crash usually does**: on Linux
`domain_to_ascii` *is* `idna`, so the shipped path there agrees with the
oracle and the difference is invisible. The layer is what runs on Windows
and macOS over ICU and Foundation. So this is a **host that would be
contacted on one platform and refused on another**, which is the one thing
`hclient-idn` exists to prevent — its own claim is that the tables move and
the answer does not.

Not caused by anything in the rename or the dependency bumps: the fuzzer
simply had a different random walk than the 9,739-input corpus that found
zero differences.

**Fixed, and the diagnosis is one line: UTS 46 maps before it looks for an
ACE label, and this layer did it the other way round.** §4 maps first, so
`xn--` is only meaningful on a label mapping has already made ASCII. The
fullwidth `ｗ` maps to an ASCII `w`; decoding before that handed punycode —
which is defined over ASCII — a non-ASCII payload, where it could only
fail. The layer has no mapper of its own, so a label that is not yet ASCII
is now pushed through untouched and the backend does the whole of §4 on it.
What identified the *ordering* rather than the characters is that
substituting the mapped `w` by hand made both sides answer the same string,
before any change. `an_ace_label_is_decoded_after_mapping_and_not_before`
pins it, checked in the failing direction.

**The fuzzer then found a second disagreement that is not a defect, and
narrowing the target is the more interesting half.** On
`xn--xn--aaaaaaax*-nlw` the layer refuses and `idna` accepts — because
`idna` accepts an ACE label whose own `domain_to_unicode` output it then
**refuses to re-encode**. The layer verifies by round-tripping: it emits
the backend's answer only once that answer re-encodes to the label it was
given, so a backend that will not confirm its own output is one it is right
to decline.

So `assert_eq!` was the wrong contract — the layer was never transparent,
it is *confirming* — and the target now asserts the real one: the layer may
refuse where the backend does not, may **never** invent a different answer,
and where it refuses, `idna` must fail its own round trip. That is narrower
and still sharp: the ordering bug above is a case where `idna` round-trips
perfectly, so the new assertion fires on it exactly as `assert_eq!` did.
Checking that before narrowing is what makes this a contract rather than a
silenced alarm.

### One line put the Linux build behind the policy, and three defects fell out

`bundled_to_ascii` called `idna::domain_to_ascii_cow` **directly**, so on
Linux and wasm the shared policy layer never ran. The ICU path's own doc
had the argument against that and it had simply not been applied here —
*"the alternative is two statements of one contract and the newer one is
always the one that rots."* Routing it through cost one expression and
surfaced three defects at once, none of them new.

**The tell was a test that stayed green.** `ä..de` resolves under `idna`
and under Windows's ICU and is refused by Apple's Foundation — a host
reachable on two of this project's three platforms and not the third,
which is the one thing `hclient-idn` exists to prevent. The rule refusing
an empty label went into the shared policy, and `hclient-proto`'s corpus
**stayed green on Linux**: a rule that refuses `ä..de` cannot leave a row
pinning `xn--4ca..de` passing, so the layer was being bypassed. A fix that
changes nothing is a fix in the wrong place.

Refusing is the direction available — nothing here can make Foundation
accept — and the safe one, since an empty label is not a legal DNS label.
A single trailing empty label is the root and stays legal.

**A label can also become empty during mapping, so the rule holds of the
answer.** UTS 46 maps a soft hyphen to nothing, so `"\u{ad}.\u{ad}"`
arrives with two non-empty labels and leaves as `"."`. The fuzzer found it
in under a minute.

**The deny list ran before mapping, where UTS 46 validates after.**
`">\u{338}"` is `>` followed by a combining long solidus overlay, which
composes to `≯` — so the forbidden character is not in the name by the time
§4 validates, and `idna` answers `xn--hdh`. This is the ordering defect
above met a second time, one field over.

**Moving that check to the end was wrong and a test said so in a minute.**
`xn--%-0fa.de` decodes to `%ä`, because punycode preserves the basic code
points verbatim and a literal `%` rides through. The check that caught it
carried a comment reading *"this cannot fire — checked rather than
trusted"*. It could fire, and that comment is what nearly justified
deleting it: **checked rather than trusted is what saved it.** So the check
is narrowed rather than moved — judged on the decoded label, where punycode
could have carried one, and on the converted output, which is the string
that decides which host is contacted.

**Step 6: what this crate emits, this crate accepts.** Steps 1-5 confirm an
ACE label the caller *gave*; they said nothing about one the crate
*produced*, and a label carrying a character that only maps to ASCII is
pushed through untouched by design, so the platform can answer with an
`xn--` label nothing examined. `xn--xn--kd--kd-xn--kd--kdijaakkkx` resolved
on the way out and was refused on the way back — and **the second parse is
the one a redirect hop makes**, so a host reached once became unreachable
mid-chain. The confirmation is that second parse, run once, with `confirm`
a parameter rather than a recursive call because the inner pass must not
confirm its own answer.

All three were reachable before this week and none was reached, because
the fuzz target was asserting `idna`'s behaviour rather than this layer's
wherever the developer was sitting.

### Two versions of `quinn-udp` coexist, and that is the seam paying out

A dependency bump asked for `quinn-udp` 0.6.1. The first look said it was
unreachable — it arrives through `quinn`, and `quinn` 0.11.11 is the newest
release and still requires `^0.5`. That was wrong about *where* it arrives:
it is a **direct** dependency of `hclient-rt-tokio` and `hclient-rt-smol`,
unconditional since their `udp` feature went. Ours can move; quinn's cannot.

**What makes the split safe is a decision made long before, for a different
reason.** `hclient-rt` declares its own `EcnCodepoint` rather than
re-exporting `quinn_udp`'s, and the doc comment beside it says so. The
consequence only became visible here: the two sides never exchange a
`quinn-udp` type at all. A runtime converts *our* codepoint into its
`quinn_udp::EcnCodepoint`; `hclient-quinn` converts *our* codepoint into
`quinn::udp::EcnCodepoint`, which is quinn's own copy. Our type is the
interchange format, so the versions on either side of it are free to differ
— and no code changed to get 0.6.1, at any site.

The cost is one duplicate crate, in a build that uses one of these
runtimes and quinn — which is the realistic HTTP/3 configuration.
`cargo deny` has `multiple-versions = "warn"`, so it says so without
failing. **Every crate count in this file is unchanged**, because the
duplicate exists only in a workspace-wide all-features graph and never in
any single crate's.

What a Linux run cannot settle is whether the two agree about ECN on a
platform where the answer differs. **There is no CI job for that**, and
this sentence originally said there was — see the ECN story in the root
`AGENTS.md`,
where the job was withdrawn before it ever ran because the mutation is
unkillable everywhere. What exists is `test (macos-latest)`, which runs
the ordinary UDP suite on macOS, and that is what would catch a 0.6
regression there.

**0.6.2 then deprecated `UdpSocketState::send`, and the attribute found
an asymmetry between the two runtimes that nothing here had noticed.**
The note reads *silences I/O errors; use `try_send` instead*, and
reading `unix.rs:225` rather than taking its word says how much: `send`
answers `Ok(())` for `EMSGSIZE` and, after logging, for **every** error
that is not `WouldBlock`. `hclient-rt-smol` had called `try_send` since
it was written; `hclient-rt-tokio` had not. So one runtime surfaced a
send failure and the other swallowed it, and `UdpDatagrams::try_send`'s
own `# Errors` promises the caller *"whatever else the OS's send call
answers"* — which the tokio half was quietly not keeping.

Both call `try_send` now. **What that hands back is the decision rather
than the error**: the `EMSGSIZE` quinn's comment calls *expected for MTU
probes* goes to quinn, through `http3/runtime.rs`'s
`AsyncUdpSocket::try_send`, which is quinn's own interface and quinn's
own MTU logic. The deprecated spelling had our runtime absorbing a probe
result on quinn's behalf.

And the smol comment beside it was right for a smaller reason than the
one that makes it right: it described the 0.6.1 difference — `send`
looping on `EINTR` — where the deprecation names error swallowing.
Corrected there rather than left, because a comment that survives the
fact it explains is this file's recurring defect.

**Two more bumps came with it, and the refusal is the interesting one.**
`embassy-executor` 0.10 renamed `arch-std` to `platform-std` and moved the
fallible half of spawning: the `#[task]` macro's function now returns
`Result<SpawnToken<_>, _>` where `Spawner::spawn` returns `()`, so an
`expect` moves one call to the left. Verified by *running* the gated TAP
suite rather than compiling it. `smoltcp` 0.14 was **refused**:
`embassy-net` 0.9.1 pins 0.13, so taking it would put two copies of a
wire-format parser in one test binary, one of them handing types to a stack
built against the other. A duplicate is cheap for a crate whose types never
cross a seam and wrong for one whose whole job is the types.

### `jni` 0.22 is a redesign wearing a minor version, and the run on the device is what settled it

Dependabot's bump from 0.21.1 to 0.22.4 failed `cross-target-check`, and
the gap is why: 0.21.1 is from **March 2023** and 0.22 landed **February
2026**, with 0.22.0 and 0.22.1 both yanked. Two crates here call into the
JVM — `hclient-idn`'s `android.rs` for `android.icu.text.IDNA` and
`hclient-proxy`'s `jvm.rs` for the system proxy properties — and five
separate API changes reached them.

**`JNIEnv` split into `Env` and `EnvUnowned`, and the alias points at the
FFI-safe half.** `jni::JNIEnv` is now deprecated and resolves to
`EnvUnowned`, which carries none of the calling API — so every
`find_class`, `call_method` and `new_string` in this workspace stopped
resolving with *no method named … for `&mut EnvUnowned`*. Upstream says
in the diagnostic that this is deliberate: *"there will be clear compiler
errors if trying to access the real `Env` API through the `EnvUnowned`
type."* The repair is `Env` at both `with_env` helpers.

**`attach_current_thread` takes a callback where it handed back a
guard**, and the callback must answer a `Result`. That is the change with
consequences past its call site: both helpers here were written in the
`Option`-per-step style, so an error type had to arrive.

**`JavaVM::from_raw` stopped being fallible** — it answers `Self` over an
internal `assert!(!ptr.is_null())`. Both call sites already null-checked
the pointer from `ndk_context` with a comment reading *"null-checked
above rather than trusted"*, so nothing changed behaviourally; what
changed is that the check went from **polite to load-bearing**, since the
failure it prevents is now a panic rather than a `None`. Said at both
sites.

**Class, method and signature names want their encoded types.** A `&str`
no longer coerces: `jni_str!` and `jni_sig!` encode MUTF-8 in a `const`,
so a literal costs nothing at run time where 0.21 converted on every
call. The one name that cannot use the macro is `through`'s `method`
parameter, which is chosen at run time — `JNIString::from` is the
run-time half, and it is the single allocation this path gained.

**And `JObject → JString` stopped being a `From`.** `env.cast_local` is
the replacement and it is a **checked** cast: it asks the runtime whether
the object really is a `java.lang.String` and answers
`Error::WrongObjectType` otherwise, in place of a conversion that could
not fail and could be wrong.

**The error type is this workspace's own, and refusing to reuse
`jni::errors::Error` is the one judgement here.** 0.22 wants
`Result<T, E>` from the callback, and the obvious `E` is theirs — which
would have filed *"the answer was not ASCII"*, *"the answer carried a
forbidden byte"* and *"ICU4J reported a fatal error"* under
`Error::NullPtr` for a null pointer that never existed. `Stop::{Jni,
Refused}` keeps the two apart. Nothing reads the distinction — `with_env`
collapses both onto `None`, which is this backend's contract — so the
compiler called both payloads dead, and the honest answer was a `Display`
that prints the JVM's own message or this crate's reason rather than an
`#[allow(dead_code)]` over data nobody can see.

**`--all-features` is the wrong invocation for this crate, which nearly
cost the whole check.** `hclient-idn`'s Android backend compiles only
with the `idna` feature **off** — the feature forces the bundled tables
on every target — so `cargo check -p hclient-idn --target
aarch64-linux-android --all-features` is green over a backend it never
built. The justfile already runs the pair, both ways round, for exactly
this reason; reading it before trusting a green check is what caught it.

**And then it was run on a device, because this crate's own history says
a compiling Android arm is not a working one.** The last time this
backend changed it type-checked, passed `cross-target-check`, and
**refused every name** on the emulator — one line of ours, kept from the
wrong direction. So: emulator, API 36, no APK, a `cdylib` whose
`JNI_OnLoad` registers the VM with `ndk_context` and one `app_process`
invocation, against the corpus `.notes/android-idn-live.md` recorded from
that run. **Twelve of twelve agree**, including the four ICU4J error
names the backend forgives (`EMPTY_LABEL`, `TRAILING_HYPHEN`,
`HYPHEN_3_4`), the one it does not (`PUNYCODE`), and the deny list.

The probe was then checked in the failing direction, which is what makes
the twelve mean anything: forcing the closing `is_ascii` check to reject
takes the run from **12/12 to 2/12** and reproduces the exact failure
mode the previous migration shipped. A live run that cannot fail is worth
no more than a type-check.

One thing the run costs and does not buy: it is **not** a CI job, for the
reason `.notes/android-idn-live.md` already gives — an emulator boot
needs minutes and KVM. What is checkable from here is unchanged:
`check-targets` compiles the backend in both feature settings and
`graph-idn-backend` asserts the tables stay off Android.
