# Releasing the 30 crates

```
cargo release <patch|minor|major|VERSION>            # shows the plan, changes nothing
cargo release <patch|minor|major|VERSION> --execute  # does it
```

That is the whole procedure. This document exists for the three things it
does not tell you: what the tool is doing on your behalf, the one thing
that will stop the **first** release, and why the order below is written
down when nothing has to follow it by hand any more.

## 1. What each half does, measured on this workspace

**`cargo publish --workspace` is native since cargo 1.90 and works here.**
Dry-run on this tree: 29 packaged, 29 **verified**, exit 0, and the upload
order computed by cargo itself. So publishing is not the part that needed a
tool.

**The bump is.** `[workspace.package].version` is one number, and beside it
are **68 literal `version = "0.1.0"` requirements** — 7 in the root
`[workspace.dependencies]` and 61 in crate manifests — which must move with
it. Cargo offers no way to write `version.workspace = true` inside a
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
here.

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

## 5. What is checked before any of this runs

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

## 6. Irreversible

A published version can be **yanked** but never replaced or deleted, so
`0.1.0` is spent whatever happens. Re-running a crate that already went out
fails with a version collision and changes nothing.
