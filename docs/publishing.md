# Publishing the 29 crates

Derived from the manifests rather than recalled — `cargo metadata
--no-deps`, every workspace member, every edge. Re-derive it after any
dependency change with the script in §6 rather than trusting this list.

## 1. Eight waves, not five

`AGENTS.md` said **five waves**, and that number is the *normal* dependency
graph. `cargo publish` also has to satisfy **dev-dependencies that carry a
version**, of which there are 32 here, and they add three more waves. Both
numbers are right about different questions; the one that governs a publish
is eight.

Within a wave the crates are independent — any order, and they can go out
back to back. A wave may not start until the previous wave is **in the
index**; `cargo publish` waits for that itself, so back-to-back invocations
are fine.

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

`hclient-rt-pair-check` is `publish = false` and is not in the count. It
must depend on `hclient-rt-tokio` **and** `hclient-rt-smol` at once, with
`udp` on both, which no shipped crate may do.

## 2. Why waves 4, 5 and 8 hold one crate each

They are the chokepoints, and each is a real dependency rather than an
accident:

- **`hclient-tls-rustls`** is alone in wave 4 because it depends on
  `hclient-tls-quic` — the `quic` feature, the second TLS seam. Nothing
  else in wave 3 is below it.
- **`hclient-native`** is alone in wave 5 because it needs
  `hclient-tls-rustls` plus both runtimes and the system resolver. It is
  the widest normal dependency set in the workspace.
- **`hclient-select`** is last because it needs `hclient-h3`, which is in
  wave 7.

## 3. The rate limit is the real schedule, not the graph

crates.io rate-limits **new** crate publication far more tightly than new
versions of an existing crate — a small burst and then roughly one per ten
minutes. Twenty-nine new crates therefore takes hours of wall clock, not
minutes, and the graph above is not what makes it slow.

**Check the current limit before starting, and ask crates.io to raise it
for this burst if needed** (help@crates.io). Being rate-limited midway is
not harmful — a wave can be resumed — but it turns a one-sitting job into
several.

## 4. What is checked, and the one thing no local check can catch

`just packaging` and `just package-build` both pass, and between them they
cover the file list, the licence symlinks, the READMEs, and that each crate
**compiles out of its own tarball**.

**Neither can catch a wrong publish order.** `cargo package --workspace`
makes every member available to every other at once through a local
overlay, so a crate whose dependency is not yet on crates.io still builds.
The order in §1 only bites on a real, sequential publish, where each crate
resolves its dependencies from the index. That is why this file exists
rather than a check.

The failure mode is benign and visible: `cargo publish` refuses at the
verify step with the missing crate named, nothing is uploaded, and the fix
is to publish the wave below first.

## 5. Running it

Per crate, in wave order:

```
cargo publish -p <crate>
```

`--dry-run` is deliberately **not** the rehearsal to rely on: it asks the
registry about ownership and version collisions, which needs credentials,
and it does not answer the ordering question either — see §4. The rehearsal
that does work is `just package-build`, already green.

If a wave is interrupted, re-running a crate that already went out fails
with a version collision and changes nothing. Publishing is otherwise
irreversible: a version can be **yanked** but never replaced or deleted, so
`0.1.0` is spent whatever happens.

## 6. Re-deriving this

```python
import json, subprocess
md = json.loads(subprocess.run(["cargo","metadata","--format-version","1","--no-deps"],
    capture_output=True, text=True).stdout)
pkgs = {p["name"]: p for p in md["packages"]}
names = set(pkgs)
pub = {n for n, p in pkgs.items() if p.get("publish") != []}
need = {}
for n in pub:
    r = set()
    for d in pkgs[n]["dependencies"]:
        if d["name"] not in names:
            continue
        # a normal or build dep always; a dev dep only when it carries a
        # version, since cargo strips a path-only one from the manifest
        if d["kind"] in (None, "build") or (
            d["kind"] == "dev" and d.get("req") not in (None, "*")
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

## 7. Collapsing the waves is possible and is refused

Dropping the `version` from those 32 dev-dependencies would make cargo
strip them from the published manifests, and the eight waves become
**five** — the number `AGENTS.md` had. It is refused, and the reason is not
the effort:

a published `.crate` with a version-carrying dev-dependency can have its
own test suite built and run by whoever downloads it. Distribution
packagers do exactly that. Strip the version and the dev-dependency
disappears from the tarball entirely, so `cargo test` on the unpacked crate
cannot compile — the crate becomes untestable by anyone who did not clone
this repository.

Three waves of waiting against that is not a trade worth making, especially
when §3 says the rate limit dominates the clock anyway.

**Two dev-dependencies are path-only on purpose and are not part of this**:
`hclient-fetch` and `hclient-native` each dev-depend on `hclient`, which
depends on both. Cargo allows that cycle inside a workspace and refuses it
at package time. Those two are the exception the paragraph above does not
cover, and the reason is written at both sites so that a tidy-up does not
put the defect back.
