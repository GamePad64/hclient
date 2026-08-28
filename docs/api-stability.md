# Freezing the surface: what will force a break, and what to do while it is free

The goal is an interface a consumer writes against once. Nothing is
published, so every change below costs a rebase today and a major version
after the first upload — which is the only window in which this document
is worth acting on.

Everything here is measured against this repository's own history rather
than reasoned from principle: the changes that actually happened are the
best available evidence about the changes that will.

## The rule the whole audit turns on

`#[non_exhaustive]` protects the **reader** and blocks the **writer**. It
forbids exhaustive matching and literal construction from outside the
defining crate, and does nothing inside it.

So the question for every public type is *who is outside*:

| type crosses | reader | writer | what protects the outside |
|---|---|---|---|
| library → user (`Event`, `Capabilities`) | the user | us | `#[non_exhaustive]` alone |
| user → library (`TcpOpts`, `Timeouts`) | us | the user | `#[non_exhaustive]` **plus setters**, or nothing |
| implementor → user (`Connected`, `TlsInfo`, `SvcbEndpoint`) | the user | **an out-of-tree backend** | `#[non_exhaustive]` **plus a constructor** |

The third row is the one that is easy to get wrong: an out-of-tree backend
is a user of this library, and the attribute alone would lock it out of
building the very value it exists to produce.

## Ranked by what history says will move

### 1. `TcpOpts` and `TcpOptsSupport` — the highest-churn public structs — **done**

**Measured**: they grew from 6 fields to 10 in the last thirty-odd
commits, and `AGENTS.md` lists both among six types that took a
semver-breaking change in that window. More options exist and will be
wanted — `TCP_FASTOPEN`, DSCP/TOS, `SO_MARK`, congestion control — and
every one of them is a major bump as the types stand.

They are the **user → library** row: the caller writes the value. The
attribute alone would forbid `TcpOpts { nodelay: true, ..Default::default() }`,
which is the whole ergonomic argument `TcpOpts`' own doc records against
it. Setters answer that argument rather than losing to it:

```rust
TcpOpts::default().nodelay(true).keepalive_interval(Duration::from_secs(30))
```

One line per option at the call site, and the struct can grow for ever.
**This is the single highest-value change in this document**, and it is
made: both types carry the attribute and a full set of chained setters,
`TcpOptsSupport`'s of them `const` because a runtime writes it into an
associated constant computed with `cfg!`.

The conversion touched fifty-odd literals across ten files and every one
became shorter. The one that did not is `tests/tcp_opts.rs`'s
`every_field_it_applies`, which builds a value field by field from another
— exactly the shape a literal is for, and exactly the shape that would
have broken on the eleventh option.

`TcpOptsSupport` is the same shape with a different writer — a runtime
implementor — and takes the same treatment, because an out-of-tree runtime
is exactly who must not be broken by a tenth option.

### 2. `Event` — every new variant breaks every out-of-tree hook — **done**

**Measured**: 28 match sites in this workspace's own tests, and
`Informational` was added this year, breaking `hclient-fetch`'s suite for
six merges. That break is recorded in `AGENTS.md` as *the design worked* —
and it did, **for a backend author inside the workspace**. For somebody
who wrote a `Hooks` impl against a published version it is a break every
release.

The two audiences want opposite things and the workspace has already
invented the resolution: `Capabilities` is `#[non_exhaustive]` **and**
`every_capability_is_a_gate_or_a_report` destructures it exhaustively
*inside `hclient-core`*, where the attribute does not apply. One compile
error in one known place, and no break for anybody outside.

Applied: `Event` is marked, and `hooks::tests::every_event_is_accounted_for`
in `hclient-core` is the single exhaustive match — no `_` arm, no
assertion about behaviour, and its whole value is that it stops compiling.

Five `_` arms were added, each saying where the compile error went. **One
of them was wrong and a test caught it**: in `hclient-wasi`'s guest the
arm landed before `Event::Head`, so it swallowed the event the harness was
there to observe. A `_` arm shadows everything below it, and that is the
one hazard this change carries per site.

And the arm in `hclient-fetch`'s suite was found by `just check-targets`
rather than by the workspace run, because that crate builds for a target
the host run does not — the blind spot that job exists for, working.

### 3. The report structs an out-of-tree backend has to build — **done**

`Connected`, `Head`, `TlsInfo`, `RecvMeta`, `Datagrams`, `SvcbEndpoint` —
five to seven public fields each, no constructor, no attribute. A backend
outside this workspace builds every one of them with a literal, so a new
field breaks it, and the attribute alone would break it harder.

`Connected::remote` becoming `Option<SocketAddr>` for the Unix-socket work
is the evidence that these move: that was a field's **type**, which no
attribute protects — but it says the shape is not settled, and a new field
is the likelier next change.

The fix is `#[non_exhaustive]` plus `::new(required…)` and setters for the
rest. It is the same work as item 1 with the writer on the other side of
the seam, and it is done — for **nine** types rather than the six this
section first counted: the five hook payloads (`Connected`, `Reused`,
`Head`, `Closed`, `Informational`), `ConnectTiming` under them, `TlsInfo`,
the two UDP types and `SvcbEndpoint`.

**The split is the same everywhere: what a value cannot be without goes in
`new`, and what a backend may not know goes in a setter.** So `Head::new`
takes four parameters and `version` is a setter, because a backend that
does not learn the protocol must say so rather than answer `HTTP/1.1`, and
a required parameter would invite exactly that. `Connected::remote` is a
setter for the same reason — a Unix socket has no peer address, and a
fabricated `0.0.0.0:0` is a wrong answer where the absence is a missing
one.

Two things the conversion taught. **A functional update from a base value
is what `..base` expressed and setters replace**, which is why the
required fields have setters too — `service_record().port(Some(p))` is
that expression, and without those two it does not compile. And a
converter that rewrites `Type { .. }` must not match `-> Type {`: a
function signature followed by its body looks exactly like a struct
literal, and one that does not check also rewrote quinn's own `RecvMeta`,
which has no constructor at all.

### 3a. `bon` was weighed for the constructors above, and measured out

A builder macro is the obvious way to write item 3, and `bon` is the
obvious crate — 52.6M downloads, one direct dependency, a `no_std` mode,
released three days before this was written. Three measurements decided
against it, in rising order of weight.

**It cannot express `TcpOptsSupport` at all.** `TcpConnect::APPLIES` is an
associated **constant** computed with `cfg!`, and a builder call chain is
not a constant expression — measured, `E0015: cannot call non-const
associated function`. The `const fn` setters that type now carries are the
only shape that works there, whatever else is decided.

**It would make `TcpOpts` worse.** Every field is optional and `Default`
is meaningful, so there is no required-field checking to gain, and
`TcpOpts::builder().nodelay(true).build()` is longer than
`TcpOpts::default().nodelay(true)` by exactly the `.build()`.

**It costs eight crates in the one that everything inherits.**
`hclient-core` goes from 13 to 21 — `bon`, `bon-macros`, `darling`,
`darling_core`, `darling_macro`, `ident_case`, `prettyplease`, `strsim`.

And the rule this workspace already applies to dependencies settles it.
`md-5` and `sha2` were taken at nine crates because *a hash is two hundred
lines whose defects are silent*; `url` was dropped and RFC 3986 resolution
hand-written because the tables cost more than the code; `proxy_cfg` was
refused at 28 crates. The line is **whether a wrong hand-written answer is
silent**, and a missing setter is a compile error — the loudest failure
available.

**The sharpest point is that it does not serve the goal.** A builder buys
ergonomics and code volume; it does not buy stability. Adding an
*optional* field is additive under either shape, and adding a *required*
one breaks callers under either — bon's typestate demands it exactly as a
`new()` signature does. This document exists to stop the surface moving,
and `bon` does not move that.

Worth reconsidering if `hclient-core` ever drops its `no_std` aspiration
*and* the report structs grow past a handful of fields, which is the
configuration where thirty hand-written setters stop being cheaper than
eight crates.

### 4. `RequestBody` and `RetryKind` are matched by every backend

Neither is `#[non_exhaustive]`. A fifth `RequestBody` — a file-backed
body, say — breaks every transport ever written against this crate. The
same tension as `Event` and the same resolution, with one difference worth
weighing: a backend that silently ignored a new body kind would send the
wrong bytes, where a hook that ignored a new event merely reports less.
That argues for keeping `RequestBody` exhaustive and accepting the break,
and it is a decision rather than an oversight either way.

### 5. Runtime seams: every method is required, and that is already handled

`TcpConnect` 2 of 2, `Spawn` 1 of 1, `Blocking` 1 of 1, `UdpBind` 1 of 1.
Adding a method to any of them breaks every implementor — and this
workspace already has the pattern that makes additions free: a **defaulted
method beside a constant defaulted to the understating value**.
`SUPPORTS_UNIX`/`connect_unix`, `reports_alpn`, `applies_ech` and
`APPLIES` are all that shape.

Nothing to change; what is missing is that it is a **policy** rather than
four coincidences. Written here so the next method follows it: *a new seam
method arrives defaulted, and the layer above asks a constant before it
asks the method.*

## One thing that looks like a limitation and is not

`TcpConnect::Stream` carries **no lifetime**, which is why an embedded
stack has to hand out a `'static` connection — `embassy-net` puts its
buffers in a `StaticCell`, and `hclient-rt-nal` takes a `&'static S`.

That is not a slip to be fixed with a GAT. `hclient::Client` erases its
transport into a `'static` box, so it could not hold a borrowed transport
whatever `TcpConnect` said; adding `type Stream<'a>` would propagate a
lifetime through every signature in the workspace and buy a capability the
facade structurally cannot use. The `'static` requirement is inherited
from erasure, not imposed by this seam, and it should stay.

## What none of this protects against

A field's **type** changing, a method's signature changing, a trait
gaining a required item. No attribute helps with those; only not doing
them does. The list of what already moved is in `AGENTS.md`'s Status
section, and it is the honest measure of how settled this is: six public
types in thirty-one commits, before this document existed.
