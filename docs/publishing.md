# Releasing the 30 crates

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
`--dry-run` publish of all 29 confirm it.

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
| 1 | `hclient-core`, `hclient-cache`, `hclient-cookie`, `hclient-idn` |
| 2 | `hclient-dns`, `hclient-fetch`, `hclient-mock`, `hclient-proto`, `hclient-rt`, `hclient-tls`, `hclient-webtransport` |
| 3 | `hclient-dns-hickory`, `hclient-dns-system`, `hclient-quinn`, `hclient-rt-smol`, `hclient-rt-tokio`, `hclient-tls-native-tls`, `hclient-tls-quic` |
| 4 | `hclient-tls-rustls` |
| 5 | `hclient-native` |
| 6 | `hclient`, `hclient-dns-doh` |
| 7 | `hclient-h3`, `hclient-rt-embassy`, `hclient-tower`, `hclient-tungstenite`, `hclient-urlsession`, `hclient-wasi` |
| 8 | `hclient-select` |

**It is eight and not five, and that is two questions rather than a
miscount.** Five is the *normal* dependency graph; `cargo publish` must
also satisfy **dev-dependencies that carry a version**, of which there are
32 here. Cargo's own ordering includes those edges, which is how the two
derivations were checked against each other.

The chokepoints are one crate wide and each is a real edge:
`hclient-tls-rustls` needs `hclient-tls-quic`, the second TLS seam;
`hclient-native` needs that plus both runtimes and the system resolver;
`hclient-select` needs `hclient-h3`.

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

- `shared-version = true` — all 30 crates carry `version.workspace = true`,
  and this tells the tool the same thing.
- `consolidate-commits = true` — one commit for the bump, not thirty.
- `allow-branch = ["main"]` — the default is every branch except `HEAD`,
  which would let a release happen from a feature branch.
- `pre-release-commit-message` and `tag-message` — the defaults are
  `chore: Release …`, and this repository does not write conventional
  commits: **nought of the last twenty-five subjects** are in that form,
  because its commit messages are the record of *why*. A release commit has
  no argument to make, so it says what it did and stops.

## 5. Releases after the first: publish what changed, not all thirty

`cargo release <level>` publishes **every** crate. For an ordinary fix that
is thirty releases for one change, so name the crate instead:

```
cargo release -p hclient-select patch
```

Measured on this tree with a tag planted one commit back: the workspace
version moves to `0.1.1` in all thirty manifests, `hclient-select` is
published, and every other crate is skipped — *"disabled by user, skipping
hclient-core, despite being unpublished"*.

**What makes that legal is `dependent-version = "fix"`.** The default,
`upgrade`, rewrites every dependent's requirement to the new number, so
`hclient-select` 0.1.1 would require `hclient-core` 0.1.1 — and a
requirement is a demand: that version must then exist in the index whether
or not anything in it changed. `fix` touches a requirement only when it
must, so they stay at `^0.1.0` and the already-published 0.1.0 keeps
satisfying them.

**cargo-release does not work out which crates changed** — it was measured
and it does not: with a tag one commit back and one crate touched, a plain
`cargo release patch` still planned all 29 uploads. Selecting is yours,
with `-p`. `cargo-smart-release` is the tool that does compute the set, and
it is not used here for reasons in §7.

Two consequences that read as mistakes and are not:

- **An unpublished crate's version runs ahead of the index.**
  `hclient-core` can be 0.1.5 in this tree and 0.1.0 on crates.io, because
  nothing in it changed. Do not "fix" it.
- **Published versions are sparse per crate.** A crate released at 0.1.1
  and again at 0.1.4 has no 0.1.2 or 0.1.3, because those releases were
  other crates'. Cargo does not care; a reader might.

Both fall out of one shared version number, which is the trade
`[workspace.package].version` was chosen for: thirty hand-maintained
numbers could drift, and one cannot.

## 6. What is checked before any of this runs

- `just package-build` — `cargo package --workspace`, which builds each
  `.crate` from the files that would ship and then **verifies** it by
  compiling out of that tarball. The only check here that builds a crate
  the way a reader would get it.
- `just packaging` — the licence texts and READMEs are in the **packaged
  file list**, not merely in the working tree.

Neither can catch a wrong publish *order*, because `cargo package
--workspace` makes every member available to every other through a local
overlay. That used to matter; it no longer does, because the order is the
tool's to compute rather than a human's to remember.

## 7. Why `cargo-release` and not the other two

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

## 8. Irreversible

A published version can be **yanked** but never replaced or deleted, so
`0.1.0` is spent whatever happens. Re-running a crate that already went out
fails with a version collision and changes nothing.
