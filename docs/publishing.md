# Releasing the crates

```
just release-pending      # what has changed since each crate last published
release-plz update        # edit versions and changelogs locally, publish nothing
release-plz release       # publish
```

**The tool is `release-plz`, and it was `cargo-release` until
2026-09-07.** What changed with it is the policy: cargo-release published
every crate on every release, because one `[workspace.package].version`
served all of them and one number cannot advance for some. Every crate
now carries its own literal version, and release-plz publishes the crates
that changed.

**What that does not buy is worth reading before the first release under
it**, because the obvious expectation is wrong and was measured rather
than assumed. A run today still bumps all 27 crates — and that is the
tool being right. Asked crate by crate on a pristine tree it answers
`hclient-otel: already up to date`, and the same for `hclient-tower` and
`hclient-webtransport`; its detection is exact. What produces the sweep is
**dependency propagation**: `hclient-core` changed, every crate here
depends on it transitively — 8 changed, **30 affected, 0 untouched** — and
a dependent must bump so its requirement can name a version that exists.

So the saving arrives for a change confined to a leaf (`hclient-cli`,
`hclient-winhttp`) and not for one that touches the core. That is a fact
about this dependency graph rather than about release-plz.

**`just release-pending` is still the thing to run first.** It answers
which crates have changed since they last published, anchored on a git
tag rather than on commit messages, and it reaches the network — which is
why it is diagnostics rather than a CI gate.

**Bump levels now come from conventional commits.** This repository did
not write them: nought of the twenty-five subjects before the migration
parsed as one, because a commit message here is the record of *why*. The
prefix goes in front of that sentence rather than replacing it.

**Semver is release-plz's now.** `just semver` has come out of CI, because
that recipe *fails* on a breaking change where release-plz *bumps the
major* — both would mean a breaking change fails CI and is released
anyway. The recipe still exists for running by hand. What it also did and
release-plz does not is fail closed on a run that executed zero lints,
which is the state inside a pre-release; that blind spot is live while
this family is on `-alpha`.

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

Every release after this is the tool's again. That read
`cargo release alpha` until 2026-09-07 and is `release-plz` now, which
derives the level from the commits rather than taking it as an argument;
either way it is a bump, so the downgrade guard never applies.

The reason for a pre-release is not doubt about the code — 19 CI jobs on
three platforms are green — it is that the week before it moved six public
surfaces, and `0.1.0` would freeze twenty-nine of them at the moment they
were last seen moving. A pre-release claims the names and promises nothing:
`cargo add hclient` will not select it unless asked, so another week of
changes costs `-alpha.2` rather than a major version across the family.

`0.1.0` follows when the seams stop moving on their own. Under release-plz
that is `release-plz update --version 0.1.0` for the crates concerned
rather than a level word — an upgrade from any `-alpha.N` either way.

## 1. What each half does, measured on this workspace

**`cargo publish --workspace` is native since cargo 1.90 and works here.**
Dry-run on this tree: 29 packaged, 29 **verified**, exit 0, and the upload
order computed by cargo itself. So publishing is not the part that needed a
tool, and that was true under cargo-release and is true under release-plz.

**The bump is**, and what it involves changed with the migration. Every
crate now carries its own literal version — there is no
`[workspace.package].version` to move — and beside those are dozens of
literal version requirements, counted by `just versions-agree` rather than
written down here. Cargo offers no way to write `version.workspace = true`
inside a dependency requirement, so the repetition is forced and nothing
but that gate checks the copies agree.

`release-plz update` does the bump: it downloads each published crate,
compares, decides a level from the commits since, writes the new version
and rewrites every requirement that names it. `release-plz release` then
publishes. Both are safe to run and inspect — `update` edits the working
tree and uploads nothing.


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

`release-plz.toml` at the repository root, which replaced
`[workspace.metadata.release]` in `Cargo.toml`.

- `semver_check = true` — release-plz runs cargo-semver-checks and turns a
  breaking change into a **major bump**. `just semver` used to run the same
  tool as a CI **gate** that failed instead; the two cannot both hold, so
  the CI step is gone. The recipe remains for running by hand, and the note
  where the step was records what that costs: it failed closed on a run
  that executed zero lints, which is exactly the pre-release state this
  family is in.
- `changelog_update = true` — new. cargo-release wrote no changelog,
  because prose subjects give a generator nothing to parse. Conventional
  commits do.
- `release_always = true` — releases happen when the command is run, rather
  than on merging a release PR. This repository has no PR flow.
- `git_tag_name = "{{ package }}-v{{ version }}"` — **the tag has to name
  the crate now**, and that is the shared version ending rather than a
  preference. cargo-release wrote `v{{version}}` and one tag covered every
  crate because one version did; with versions sparse per crate that is
  ambiguous the first time two sit at different numbers. `just
  release-pending` reads these tags, and its own header says a crate whose
  published version has no tag cannot be compared against anything — so the
  existing `v0.1.0-alpha.3` tags stay meaningful for what was released
  under them.
- three `[[package]] release = false` entries — `hclient-rt-pair-check`,
  `hclient-rt-nal` and `hclient-rt-embassy`. They are `publish = false` in
  their own manifests already; naming them here stops release-plz
  version-bumping and changelogging crates that never go out.


## 5. Releases after the first: what changed, and only that

**The policy is that a release publishes the crates that changed.** It was
the opposite until 2026-09-07 — every crate on every release, one shared
version — and the argument for that is worth keeping because it was a good
one: selecting means knowing which crates changed, knowing means a step
that can be forgotten, and publishing everything cannot forget. What ended
it is a tool that *computes* the set rather than asking a human for it.

```
just release-pending      # diagnostics: what has changed, and since when
release-plz update        # the plan, written into the tree; uploads nothing
release-plz release       # the upload
```

**In practice this publishes everything anyway, today, and that is
correct.** `hclient-core` sits under every other crate, so any change to
it propagates: measured, 8 crates changed and **30 affected, 0 untouched**.
release-plz is not failing to skip — asked crate by crate it answers
`already up to date` for the ones that are. The saving is real for a
change confined to a leaf and absent for one that touches the core.

**What the old policy removed and this one brings back is drift.** With
one version, requirements could not disagree with the crates they named.
With versions sparse per crate, they can — so `just versions-agree` stops
being a convenience and becomes the thing that catches it. It resolves
every in-workspace requirement to the crate it names and compares against
**that** crate's version, which it has done since `system-resolver` left
the shared version, so it needed no change for this migration.

**Two things that used to read as mistakes and no longer are.** An
unpublished crate's version running ahead of the index, and published
versions going sparse per crate: the first is still ordinary, and the
second is now the intended shape rather than a symptom.


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

- **unchanged** — nothing in the directory moved since it was published,
  so release-plz will answer `already up to date` for it.
- **CHANGED (n files)** — it has unreleased content. The recipe suggests
  `release-plz update` and nothing more specific: release-plz computes the
  set itself, and a crate list printed here would be a second opinion
  about which crates to publish — the one that rots.
- **NO TAG — cannot compare** — the anchor is missing, and the recipe
  refuses to guess a commit rather than answering wrongly.

**The anchor is a git tag and it is not optional.**
`release-plz.toml` sets `git_tag_name = "{{ package }}-v{{ version }}"`, so
every release leaves one per crate — it was `v{{version}}` under
cargo-release, where one tag covered every crate because one version did;
that single
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

## 8. Why `release-plz`, and what this section said before

**This section argued for `cargo-release` and against `release-plz`, and
the objection it raised was correct at the time.** It read: both
alternatives *infer* what to release and how far to bump from git history,
and this repository's history is the wrong shape — release-plz wants
conventional commits, and nought of the last twenty-five subjects were in
that form, because the commit messages are the record of *why*.

That is still an accurate description of the trade. What changed is the
answer to it: the repository writes conventional-commit prefixes now, in
front of the sentence rather than instead of it, which is a cost paid
deliberately in exchange for computed release sets and changelogs.

**What cargo-release could not do is the reason.** It has no change
detection at all — measured, with a tag one commit back, a plain
`cargo release patch` still planned all 23 uploads. Its `-p` flag selects,
which is knowing-which-crates-changed by hand, the step §5's old policy
existed to remove. So the choice was between publishing everything for
ever and moving to a tool that computes the set.

**`cargo-smart-release` was the other candidate and is still not it**, for
the reasons this section already recorded from its own README: it derives
versions from conventional commits too, detecting whether a change is
breaking is an open item there, and pre-release versions like
`1.0.0-beta.1` are listed as not handled — which rules it out while this
family is on `-alpha`.


## 9. Irreversible

A published version can be **yanked** but never replaced or deleted, so
`0.1.0` is spent whatever happens. Re-running a crate that already went out
fails with a version collision and changes nothing.
