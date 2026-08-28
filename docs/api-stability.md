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

### 1. `TcpOpts` and `TcpOptsSupport` — the highest-churn public structs

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
**This is the single highest-value change in this document.**

`TcpOptsSupport` is the same shape with a different writer — a runtime
implementor — and takes the same treatment, because an out-of-tree runtime
is exactly who must not be broken by a tenth option.

### 2. `Event` — every new variant breaks every out-of-tree hook

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

Apply the same to `Event`: mark it, and put a single exhaustive match in
`hclient-core`. The cost is `_` arms in the backends' own test files,
which is where the property is currently duplicated 28 times.

### 3. The report structs an out-of-tree backend has to build

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
the seam.

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
