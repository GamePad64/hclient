# Releasing the 26 crates

```
cargo release <patch|minor|major|VERSION>            # shows the plan, changes nothing
cargo release <patch|minor|major|VERSION> --execute  # does it
```

That is the whole procedure. This document exists for the things it does
not tell you: what the tool is doing on your behalf, the one thing that
will stop the **first** release, how to release one crate rather than
thirty afterwards, and why the order below is written down when nothing
has to follow it by hand any more.

**The first release is `0.1.0-alpha.1`, and the version is already set in
the tree**, so the first release is a publish and not a bump:

```
cargo release publish             # the plan; uploads nothing
cargo release publish --execute   # the release
```

**A stale `rmeta` will fail a release, and it fails it as a missing
API.** On 2026-08-28 `cargo release publish` and `just package-build` both
stopped at `hclient-fetch` with *cannot find `Reduced` in `hclient_core`*,
plus `mark`, `since`, `SendTransport` and `BoxSendExchange` — five items
added to `hclient-core` after `0.1.0-alpha.1` went out.

**Every obvious reading of that is wrong**, and each was checked:

- It is **not** a stale version requirement. `^0.1.0-alpha.1` matches
  `0.1.0-alpha.2` — a caret carrying a pre-release accepts higher
  pre-releases of the same version — which is why the workspace builds
  with members at `alpha.2` and requirements at `alpha.1`, and why
  `cargo release version` correctly left the member manifests alone.
- It is **not** the registry holding only `alpha.1`. The verify build's
  own `Cargo.lock` names `hclient-core 0.1.0-alpha.2`, and its checksum
  matches the tarball in `target/package/tmp-registry` byte for byte.
- It is **not** a bad tarball or a stale extraction. Both carry
  `pub trait SendTransport` and `pub enum Reduced`; deleting
  `target/package` and the overlay registry entirely changed nothing.

**The tell is the shape of the diagnostic**: the note pointed at
`transport.rs:14:0` — column **zero**, which is how rustc renders a span
recovered from *metadata* rather than read from source. The compiler was
never reading the extracted tarball; it was reusing a compiled
`libhclient_core-*.rmeta` from the shared `target/debug/deps`, keyed by a
version that had not changed. There were **89** of them.

```
cargo clean -p hclient-core     # then: 28 crates build from their own tarball
```

This is the trap `AGENTS.md` already records one line long — *a stale
`rmeta` for an unchanged version makes `package-build` fail, or worse
pass, misleadingly* — met in the failing direction, during a release, and
it costs an hour if the first three readings are chased instead. A
pre-release series makes it likely rather than exotic: the version number
does not move between packagings, so the cache key does not either.

**Not bare `cargo publish --workspace`, and the difference is not the
ordering.** Both order the uploads and both wait for each crate to reach
the index before the next — that waiting is cargo's, verified in the stable
binary, which carries the message *"due to a timeout while waiting for
published dependencies to be available"*; `-Z publish-timeout` is nightly
only for *configuring* the wait, not for having it. What only
`cargo release` does is check the **rate limit before uploading anything**:
measured, it refuses with `attempting to publish 29 new crates which is
above the rate limit: 5`, where bare `cargo publish --workspace` would
upload five and fail on the sixth — halfway through a first publication,
which is the state §2 exists to avoid.

**Neither `cargo release 0.1.0-alpha.1` nor `cargo set-version
0.1.0-alpha.1` can do it**, and the reason is worth knowing before trying:
a pre-release *precedes* its release in semver, so `0.1.0-alpha.1` is
**lower** than the `0.1.0` the tree carried, and both tools refuse a
downgrade — `Cannot downgrade from 0.1.0 to 0.1.0-alpha.1`. The guard is
right; it just does not know that `0.1.0` here was a placeholder that was
never published. So the 70 literals — one `[workspace.package].version`, 8
requirements in `[workspace.dependencies]` and 61 in crate manifests — were
edited directly, once, and `cargo check --workspace --all-features` and a
`--dry-run` publish of all 25 confirm it.

The requirements had to move with it and not merely alongside: `^0.1.0`
does **not** accept `0.1.0-alpha.1`, because a caret requirement excludes
pre-releases unless it names one itself.

Every release after this is the tool's again — `cargo release alpha` from
`0.1.0-alpha.1` gives `0.1.0-alpha.2`, measured, and it is a bump so the
downgrade guard never applies.

The reason for a pre-release is not doubt about the code — 19 CI jobs on
three platforms are green — it is that the week before it moved six public
surfaces, and `0.1.0` would freeze twenty-nine of them at the moment they
were last seen moving. A pre-release claims the names and promises nothing:
`cargo add hclient` will not select it unless asked, so another week of
changes costs `-alpha.2` rather than a major version across the family.
Subsequent pre-releases are `cargo release alpha`, which increments the
`.1`.

`0.1.0` follows when the seams stop moving on their own, and it is an
ordinary `cargo release 0.1.0` when it does — an upgrade from any
`-alpha.N`, so the tool handles that one.

## 1. What each half does, measured on this workspace

**`cargo publish --workspace` is native since cargo 1.90 and works here.**
Dry-run on this tree: 29 packaged, 29 **verified**, exit 0, and the upload
order computed by cargo itself. So publishing is not the part that needed a
tool.

**The bump is.** `[workspace.package].version` is one number, and beside it
are **69 literal version requirements** — 8 in the root
`[workspace.dependencies]` and 61 in crate manifests — which must move with
it. (The header says 70: that is these 69 plus the workspace version
itself. Both counts were measured; an earlier "68 / 7" here was a narrower
grep than the one that set the version, and a document contradicting itself
two sections apart is the count-in-prose defect this project keeps
finding.) Cargo offers no way to write `version.workspace = true` inside a
dependency requirement, so the repetition is forced and nothing checked
that the copies agreed. `cargo release version minor` does it:

```
Upgrading workspace to version 0.2.0
Upgrading hclient-dns from 0.1.0 to 0.2.0 (inherited from workspace)
 Updating hclient's dependency from 0.1.0 to 0.2.0
 Updating hclient-dns-doh's dependency from 0.1.0 to 0.2.0
 …
```

`cargo release <level> --execute` then runs bump → commit → publish in
dependency order → tag → push, so the two halves are one command.

## 2. The first release will be refused, and that is correct

```
error: attempting to publish 29 new crates which is above the rate limit: 5
```

crates.io rate-limits **new** crates far harder than new versions of
existing ones — a burst of 5, then roughly one per ten minutes — and
`cargo-release` knows both numbers and refuses rather than getting halfway.
Two ways past it:

- **Ask crates.io to raise it** (`help@crates.io`) for this one burst, and
  then set the tool's copy to match:

  ```toml
  [workspace.metadata.release.rate-limit]
  new-packages = 29
  ```

- **Or publish in batches** of five and wait, which is what the limit
  enforces anyway.

**Do not raise `new-packages` before crates.io raises the real limit.** The
number is not a preference: setting it high without the grant turns a check
that works into one that cannot fire, and the failure it was preventing —
stopping halfway through a first publication — is the expensive one.

Later releases do not meet this: `existing-packages` is 30, above the 29
here. **A pre-release does not dodge it** — `0.1.0-alpha.1` is still 29
new crates as far as the registry is concerned.

## 3. The order, and why it is still written down

Nothing has to follow this by hand — cargo computes it. It is here because
it was derived **independently**, from `cargo metadata`, before either tool
was consulted, and the two agree exactly. That agreement is the evidence
the wave count is a fact about the graph rather than a guess:

| wave | crates |
|---|---|
| 1 | `hclient-core`, `hclient-idn` |
| 2 | `hclient-dns`, `hclient-fetch`, `hclient-mock`, `hclient-proto`, `hclient-rt`, `hclient-tls`, `hclient-webtransport` |
| 3 | `hclient-dns-hickory`, `hclient-dns-system`, `hclient-proxy`, `hclient-rt-smol`, `hclient-rt-tokio`, `hclient-tls-rustls`, `hclient-winhttp` |
| 4 | `hclient-native`, `hclient-tls-native-tls` |
| 5 | `hclient`, `hclient-dns-doh` |
| 6 | `hclient-otel`, `hclient-tower`, `hclient-tungstenite`, `hclient-urlsession`, `hclient-wasi` |
| 7 | `hclient-cli` |

**Re-derived on 2026-08-30 with the script below, and three rows had
drifted.** `hclient-winhttp` and `hclient-cli` were missing entirely and
`hclient-tls-native-tls` had moved from wave 3 to wave 4 — none of which
anything forced, because this is a table and not a check, and the sentence
above about re-deriving after a dependency change is the only thing that
was ever going to move it. `hclient-otel` is the crate that prompted the
re-derivation and it changed **nothing**: it joins wave 6 with the other
terminal crates, because its only normal dependency is `hclient-core` and
its only version-carrying dev-dependency is `hclient`. **A crate that adds
no wave is the ordinary case**, and saying so is the point of checking.

**It is seven and not five, and that is two questions rather than a
miscount.** Five is the *normal* dependency graph; `cargo publish` must
also satisfy **dev-dependencies that carry a version**. Cargo's own
ordering includes those edges, which is how the two derivations were
checked against each other. Wave 7 is `hclient-cli`, which dev-depends on
nothing but is the one crate depending on `hclient-tungstenite`.

The chokepoints are one crate wide and each is a real edge:
`hclient-native` needs both runtimes, the system resolver and
`hclient-tls-rustls`;
`hclient-native` needs that plus both runtimes and the system resolver,
and `hclient` needs `hclient-native`.

**That last edge is why `hclient-native` and `hclient-fetch` carry their
dev-dependency on `hclient` path-only, with no version.** Cargo allows the
cycle inside a workspace and refuses it at package time, because a
versioned dev-dependency has to resolve from the registry. `just
package-build` is what catches it.

`hclient-rt-pair-check` is `publish = false` and is not in the count — it
must depend on `hclient-rt-tokio` **and** `hclient-rt-smol` at once, with
`udp` on both, which no shipped crate may do.

Re-derive after any dependency change:

```python
import json, subprocess
md = json.loads(subprocess.run(["cargo","metadata","--format-version","1","--no-deps"],
    capture_output=True, text=True).stdout)
pkgs = {p["name"]: p for p in md["packages"]}
names, pub = set(pkgs), {n for n, p in pkgs.items() if p.get("publish") != []}
need = {}
for n in pub:
    r = set()
    for d in pkgs[n]["dependencies"]:
        if d["name"] in names and (
            d["kind"] in (None, "build")
            or (d["kind"] == "dev" and d.get("req") not in (None, "*"))
        ):
            r.add(d["name"])
    need[n] = r & pub
done, wave = set(), 0
while len(done) < len(pub):
    w = sorted(n for n in pub - done if need[n] <= done)
    assert w, f"cycle among {sorted(pub - done)}"
    wave += 1
    print(f"wave {wave}: {', '.join(w)}")
    done |= set(w)
```

## 4. The configuration, and why each line is not a default

`[workspace.metadata.release]` in the root `Cargo.toml`. **An unknown key
there is a hard parse error, not a silent no-op** — checked on purpose
before writing it, because a release configuration that ignores a typo is
the shape this project refuses everywhere else.

- `shared-version = true` — all 23 crates carry `version.workspace = true`,
  and this tells the tool the same thing.
- `consolidate-commits = true` — one commit for the bump, not thirty.
- `allow-branch = ["main"]` — the default is every branch except `HEAD`,
  which would let a release happen from a feature branch.
- `pre-release-commit-message` and `tag-message` — the defaults are
  `chore: Release …`, and this repository does not write conventional
  commits: **nought of the last twenty-five subjects** are in that form,
  because its commit messages are the record of *why*. A release commit has
  no argument to make, so it says what it did and stops.

## 5. Releases after the first: one version, everything published

**The policy is to publish all 23 crates on every release**, off the one
shared version:

```
cargo release patch
```

The argument for it is that it removes a question rather than answering
one. Selecting crates means knowing which changed, and *knowing* means a
step that can be skipped, got wrong, or forgotten — the failure this
document spent §5a building a tool against. Publishing everything cannot
forget anything. The cost is 24 uploads for a one-line fix and a version
history with no gaps in it, which for crates this size is cosmetic.

**It is also not what guarantees compatibility, and that is worth saying
because the intuition is wrong.** What guarantees it is the *requirement*:
the published `hclient` asks `^0.1.0-alpha.1` of each neighbour, and
semver is what makes the set resolve. Matching version numbers are a
consequence, not the mechanism — under `dependent-version = "fix"` the
numbers on crates.io are free to differ and the set still works.

**Selecting by name still works and the configuration still supports it**,
because the policy may change:

```
cargo release -p hclient-native patch
```

Measured on this tree with a tag planted one commit back: the workspace
version moves to `0.1.1` in every manifest, `hclient-native` is
published, and every other crate is skipped — *"disabled by user, skipping
hclient-core, despite being unpublished"*.

**What makes that legal is `dependent-version = "fix"`.** The default,
`upgrade`, rewrites every dependent's requirement to the new number, so
`hclient` 0.1.1 would require `hclient-core` 0.1.1 — and a
requirement is a demand: that version must then exist in the index whether
or not anything in it changed. `fix` touches a requirement only when it
must, so they stay at `^0.1.0` and the already-published 0.1.0 keeps
satisfying them.

**cargo-release does not work out which crates changed** — it was measured
and it does not: with a tag one commit back and one crate touched, a plain
`cargo release patch` still planned all 24 uploads. Under §5's policy that
is the wanted behaviour rather than a shortcoming; under the `-p` form,
selecting is yours, and §5a is what tells you what to select.
`cargo-smart-release` is the tool that does compute the set, and it is not
used here for reasons in §8.

**Two consequences the `-p` form has and this policy does not**, recorded
because they are the argument for publishing everything and because they
read as mistakes if met without warning:

- **An unpublished crate's version runs ahead of the index.**
  `hclient-core` could be 0.1.5 in this tree and 0.1.0 on crates.io,
  because nothing in it changed. Correct, and not a drift to "fix".
- **Published versions go sparse per crate.** A crate released at 0.1.1
  and again at 0.1.4 would have no 0.1.2 or 0.1.3, because those releases
  were other crates'. Cargo does not care; a reader might.

Publishing everything removes both: every crate is at every version, and
the tree and the index agree. That is the second argument for the policy,
after the one §5 gives — the first removes a step that can be forgotten,
this one removes two explanations a reader would otherwise need.

Both artefacts fall out of one shared version number, which is the trade
`[workspace.package].version` was chosen for: 23 hand-maintained numbers
could drift, and one cannot.

## 5a. Knowing which crates have unreleased changes

Under §5's policy nothing has to answer this — publishing everything
cannot leave a crate behind. It is kept for the two cases that remain:
seeing what has accumulated before deciding a version level, and the day
the policy changes back to selecting with `-p`. `just release-pending`
is that:

```
just release-pending
```

For each publishable crate it reads the last version in the crates.io
sparse index, finds the git tag naming that version, and diffs the
crate's directory between that tag and `HEAD`. Three answers, and the
third is the one worth having:

- **unchanged** — nothing in the directory moved since it was published.
  Do not release it; `dependent-version = "fix"` is what makes that legal.
- **CHANGED (n files)** — it has unreleased content. The recipe prints a
  ready `cargo release -p … -p … <level>` line at the end, which is the
  selecting form §5 keeps rather than the policy it uses.
- **NO TAG — cannot compare** — the anchor is missing, and the recipe
  refuses to guess a commit rather than answering wrongly.

**The anchor is a git tag and it is not optional.**
`[workspace.metadata.release]` sets `tag-name = "v{{version}}"`, so every
release cargo-release makes leaves one; with `shared-version` that single
tag covers whichever crates went out under it, which is enough, because
the index says *which version* each crate is at and the tag says *which
commit* that version was.

**As of this writing there is no such tag.** All 23 crates are published
at `0.1.0-alpha.1` and `git tag` is empty, so the recipe answers
"cannot compare" for every one of them — the first release was made
without cargo-release, or with its tagging off. Plant it once on the
commit that was published:

```
git tag -a v0.1.0-alpha.1 <commit> -m "hclient 0.1.0-alpha.1"
git push origin v0.1.0-alpha.1
```

From then on the tags maintain themselves.

**It is not in `just ci`, deliberately.** It asks crates.io over the
network — the kind of flakiness a gate must not have — and the answer is
only wanted before a release. It was checked in the discriminating
direction rather than trusted: with a tag planted six commits back it
reported 20 changed and 3 unchanged, not one blanket answer.

**What it does not catch**, said here because the boundary is real: a
change *outside* a crate's directory that still alters what it publishes
— the workspace `Cargo.toml`'s lints or a `[workspace.dependencies]`
version bump. Those move every crate at once, and the honest handling is
to treat a workspace-manifest change as touching everything.

## 6. Keywords and categories: how twenty-nine crates stay one family

Every publishable crate carries the keyword **`hclient`**, and that is the
only mechanism that groups them on crates.io — it is a clickable, indexed
tag, where a name prefix is only a string that happens to sort together.

It also reaches the two crates a prefix could never honestly cover.
`hclient-tower` and `hclient-tungstenite` are named after the foreign
library each wraps, which is deliberate — `hclient-ws-tungstenite` was
renamed *away* from a seam-shaped name because the seam crate it implied
must not exist — and they are exactly the two that get lost in a list of
thirty. A keyword picks them up; `hclient-transport-*` never would.

The second keyword carries the role. **`transport` is on all eight**:
`hclient-native`, `-h3`, `-fetch`, `-wasi`, `-urlsession`, `-select`,
`-mock` and `-tower`. Likewise `runtime` on the four `-rt*`, `tls` on the
four TLS crates, `dns` and `resolver` on the four resolvers.

Categories are the curated axis and are chosen from crates.io's own list,
verified against its API rather than guessed:
`web-programming::http-client` for anything a caller sends requests with,
`network-programming`, `asynchronous`, `wasm` for the two browser/WASI
backends, `cryptography` for the TLS family, `os::macos-apis` for
`hclient-urlsession`, `development-tools::testing` for `hclient-mock`,
`internationalization` and `encoding` for `hclient-idn`,
`web-programming::websocket` for `hclient-tungstenite`.

`cargo package --workspace` accepts every one of them without a warning,
which is the check: an unknown category is reported there rather than at
upload.

**Why this rather than an `hclient-transport-*` rename.** A
`hclient-<seam>-<impl>` name is legitimate here only when a
`hclient-<seam>` crate exists to hold something `hclient-core` must not —
`hclient-rt` and `hclient-tls` hold `hyper`, `hclient-dns` holds a DNS
codec. `Transport` lives *in* `hclient-core` and needs nothing extra, so
`hclient-transport` would be an empty crate and the name would promise
one the dependency rule forbids. That is the same defect that renamed
`hclient-ws-tungstenite`.

## 7. What is checked before any of this runs

One command, `just release-check`, which is `ci` plus the three below in
the order a failure is cheapest to find. Not part of `ci` itself:
`package-build` is minutes of work for a question only a release asks,
and `release-pending` reaches the network.

- `just package-build` — `cargo package --workspace`, which builds each
  `.crate` from the files that would ship and then **verifies** it by
  compiling out of that tarball. The only check here that builds a crate
  the way a reader would get it.
- `just packaging` — the licence texts and READMEs are in the **packaged
  file list**, not merely in the working tree. Its floor is derived from
  `cargo metadata` rather than written down, for the reason
  `package-build` derives its own: a literal goes stale the next time a
  crate is added or folded in, and a stale floor is a check that passes
  for a run that did less than it should. It was a literal until the jar
  and the cache became modules — 25 to 23 — which is the edit the
  derivation removes.
- `just release-pending` — §5a, and **diagnostics rather than a gate**:
  under §5's policy nothing has to answer which crates changed.

Neither can catch a wrong publish *order*, because `cargo package
--workspace` makes every member available to every other through a local
overlay. That used to matter; it no longer does, because the order is the
tool's to compute rather than a human's to remember.

## 8. Why `cargo-release` and not the other two

Both alternatives **infer** — what to release and how far to bump — from
git history, and this repository's history is the wrong shape for it.
`release-plz`'s own description says "conventional commits", and **nought
of the last twenty-five subjects here** are in that form: the commit
messages are the record of *why*, and feeding them to a `feat:`/`fix:`
parser would mean flattening them.

`cargo-smart-release` aims precisely at §5's problem — it uses git tags to
know whether a crate changed at all and skips the ones that did not — and
is the tool to revisit if selecting by hand becomes tiresome. Three things
kept it out for now, all from its own README: it derives the version from
conventional commits too; detecting whether a change is breaking is an
open item, so "downstream breakage impossible" rests on the commit being
labelled correctly; and pre-release versions like `1.0.0-beta.1` are
listed as not handled, which rules it out for a cautious first release.
Its author recommends `cargo-release` in the same file.

Its other half is separable and worth remembering: `cargo changelog`
writes changelogs non-destructively "leaving the release workflow to
cargo-release", so changelogs can be adopted later without moving the
release path.

## 9. Irreversible

A published version can be **yanked** but never replaced or deleted, so
`0.1.0` is spent whatever happens. Re-running a crate that already went out
fails with a version collision and changes nothing.
