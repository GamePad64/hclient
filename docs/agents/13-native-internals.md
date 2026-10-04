# Native's internals

> Part of the [AGENTS.md](../../AGENTS.md) record, split 2026-10-05. The root
> file keeps the operational rules and the index of parts. Sections below
> keep their original headings, and a "this file" in the text named the
> whole record when it was written.

### The pooled-reuse race has a third test sitting on it, and it was the premise

`a_pooled_connection_the_server_closed_while_idle_is_reported_stale` waits
for the *server* to have dropped its socket and then expects the second
request to find the connection dead at checkout. Under an oversubscribed
run it failed six times in sixty with `IncompleteMessage`, and the capture
says which point of the window it reached: `accepted=1`,
`closes_seen=[(1, Ended)]`, `connects=1` — no fresh connection, and the
connection's end reported from **inside the second exchange** rather than
from the checkout.

That is the far point of `.notes/pooled-reuse-race.md`'s three, the one this
workspace documents as residual and deliberately unfixed — hyper wrote the
request and then read `EOF`, which is `Failed::Sent` and no retry. **The
client did exactly what is written down.**

What was wrong is the premise. `server_has_closed` establishes that the
peer dropped its socket; what the test needs is that *this client's
reactor* has processed the `FIN`'s readiness before `is_reusable` takes its
single non-suspending look, and those are different facts. Under load tokio
had not had its turn. A sleep after the wait is a guard on the premise, not
an assertion — and it is the same lever `Behaviour::close_delay` already
is, one field over.

Three flakes, three test-level causes, and one real defect (the h2 stream
limit below) — which is the ratio worth remembering before assuming a flake
is noise.

### Two more flakes in the same hunt, and neither was the client

The `-j96` hunt that found the h2 defect below turned up two more, and both
were the **test** measuring something the client does not promise. Worth
recording because the difference is only visible after capturing the
failure, and both first theories were wrong.

**`with_no_head_start_both_stacks_connect_and_exactly_one_request_is_sent`
— now `with_no_head_start_exactly_one_request_reaches_the_origin` — asserted
a clock wearing a counter's clothes.** Its `tcp_accepted >= 1`
says *the hedge ran*, and at a head start of zero the hedge is only
**started** — a QUIC arm that finishes first cancels it, possibly before
its `SYN` leaves. The captured failure carries the numbers: `body="h3"
quic_answered=1 tcp_accepted=0 elapsed=3.0ms`. The first fix was to widen
the wait from one second to ten on the theory that the fixture's thread was
starved; **measured, the rate was unchanged** — which is what proved the
connection never existed rather than arriving late. The assertion is gone
and the test is renamed for what it does claim; nothing is lost, because
the hedge running is asserted *causally* two tests down, against a QUIC
origin that cannot answer.

**`a_quic_arm_that_lost_the_race_teaches_the_memory` measured across a
boundary the client does not own.** `hop` takes a delta of the black hole's
datagram counter across `execute`, and the end of that future is not the
end of the abandoned QUIC arm's UDP. Measured: a hop whose hedge wins puts
**two** datagrams into the hole, and every captured failure showed hop 1
with one and hop 2 with the other. A `settled` guard between the hops fixes
it — 6 failures in 40 before, 0 in 40 after.

The second one also cost a wrong "decisive" reading first: the diagnostic
printed `two.quic_tried == 0` and that looked conclusive until the fixture
was read, where `quic_attempted` turns out never to move for a black hole.
**A counter that cannot move is not evidence**, and checking which counters
the fixture actually feeds is part of reading a capture.

### A stream opened on a guess, found by hunting a flake

`h2`'s `initial_max_send_streams` defaults to `usize::MAX`: until the
server's `SETTINGS` frame arrives a client may open as many streams as it
likes, and `SendRequest::poll_ready` — which *does* respect the counter
(`proto/streams/counts.rs`, `can_inc_num_send_streams`) — says yes to all
of them. So a caller firing a burst of concurrent requests at a fresh
multiplexed connection **races the frame that states the limit**, and a
server allowing fewer answers `RST_STREAM(REFUSED_STREAM)`.

**And this client could not repair it afterwards.** `send_request`
consumes the head, so `http2::exchange` can only report `Failed::Sent` and
`Native::run` will not retry — the weakness that module's own doc already
recorded against HTTP/1. RFC 9113 §8.7 says a `REFUSED_STREAM` reset means
the request was **never processed** and may be safely retried, which makes
the hard failure a wrong answer rather than a cautious one.

`Native` now asks for **one** stream until the peer has stated a number.
The cost is one round trip once per connection — the peer's SETTINGS
arrives in its first flight, and `counts.rs` overwrites the guess the
moment it does, even where the frame names no limit at all. It is
deliberately **not** an `H2Opts` field: every field there is `None` meaning
*whatever h2 chooses*, and this is a correctness choice, not a knob. A
caller who guessed high would be choosing a failure they cannot retry.

**How it was found is the part worth copying.** It was a flake —
`beyond_the_peers_stream_limit_requests_queue_on_one_connection` failing
once in a while under a full-workspace run. Sixty runs at `-j16` and
`-j28` were clean and proved nothing; **oversubscribing to `-j96`** turned
it into 4 failures in 40, with identical captures every time:
`Reset(StreamId(5), REFUSED_STREAM, Remote)`. Every run's whole output went
to its own file — the session that lost two earlier sightings lost them to
a `grep` in the pipeline.

**The test is deterministic now, not statistical.** `DelayFirstWrite` holds
the server's *first* write for 300 ms, which is the frame carrying
`MAX_CONCURRENT_STREAMS` — the first write and no other, since delaying
every write would also delay the `RST_STREAM`s the test is about. Without
the fix it fails three times in three with the same reset on stream 5; the
assertion is the server's high-water mark, a count, so a slow machine
changes nothing.

That makes four: `.notes/v03-acceptance.md` records three timing-based
assertions in this workspace that turned out to be flakes, one of them
hiding a real defect. This is the fourth, and it was hiding one too.

### Unix-domain sockets, and the sibling trait that could not exist

`Native::unix_socket(path)` — curl's `--unix-socket`, for reaching a local
daemon that speaks HTTP over a socket rather than a port. The URI still
carries a host, because HTTP needs one for `Host:` and for the pool.

**`docs/competitive-gaps.md` expected a sibling of `TcpConnect` and there
cannot be one.** `Native`'s IO type *is* `TcpConnect::Stream`, so a second
trait would have to produce the same associated type — at which point it is
`TcpConnect` with an extra method. Putting `R: UnixConnect` on `Native`
would tax every runtime with no file descriptors, and the `fn`-pointer
trick that keeps `Spawn` off `Native`'s signature does not work here:
`spawn` returns `()` where this returns a future, and boxing it drops auto
traits (amendment C1).

So it is `TcpConnect::connect_unix`, a **defaulted method** whose default
is a refusal, beside `SUPPORTS_UNIX` defaulted to `false` — `reports_alpn`
and `applies_ech`'s shape: a constant defaulted to the understating value,
read by the layer above to decide whether to *ask*. Both shipped runtimes
compute it with `cfg!(unix)`, and each holds an enum internally
(`TokioIo`'s `Socket`, and `hclient-rt-smol`'s, inside `SmolIo`) because one
associated type must cover both.

**It is `connect_ipc(&IpcAddr)` now, and the reason is the freeze.** A
method per kind carried its own associated future type, and an associated
type cannot have a default on stable Rust — so the second kind, Windows
named pipes (where Docker, containerd and the gRPC daemons listen), would
have broken every `TcpConnect` implementor at once, after `hclient-rt` had
promised not to. `IpcAddr` is a `#[non_exhaustive]` enum with `Unix` today,
`TcpConnect::IPC_SUPPORT` an `IpcSupport` in `TcpSupport`'s shape, and a
runtime's `match` has to carry a wildcard arm. That arm used to *be* the
refusal, a `RefuseIpc` future naming the kind — and that made IPC the one
seam refusing by which arm ran rather than by what the report said. So a
runtime now calls `IpcAddr::reject_unsupported(IPC_SUPPORT)` on entry, the
way it calls `TcpOpts::reject_unsupported` on a connect and
`Datagrams::reject_unsupported` on a send, and the wildcard is an
`unreachable!` naming the disagreement between report and `match`.
`RefuseIpc` is gone with nothing lost: it was a ready error, which any
`async` block is. The two shipped runtimes lost
their `cfg`-ed pair of items with it: one type on every target, and the
`cfg` on the Unix arm. `Native::unix_socket` did not move.

**And the three reports became one shape, which found a defect and a
promise nobody could keep.** `TcpOptsSupport`, `UdpCaps` and `IpcSupport`
are `TcpSupport`, `UdpSupport` and `IpcSupport`, declared as
`TCP_SUPPORT`, `UdpDatagrams::support()` and `IPC_SUPPORT`, checked by a
`reject_unsupported` on the request, refused as `UnsupportedTcp`,
`UnsupportedUdp` and `UnsupportedIpc` — each with `names()` and one shared
message formatter. `hclient-rt`'s crate doc carries the table. The one
difference is real: UDP support is a property of one socket on one kernel,
measured at bind, so it is a method where the other two are constants.

**Making the three consistent found that TCP was the one not checking.**
The rule is *a runtime checks its request on entry, and a transport checks
again at configuration so a caller meets the refusal early*. UDP's
`try_send` did the first, embassy's `connect` did, and `hclient-rt-tokio`
and `hclient-rt-smol`'s `connect` did not — the check lived only in
`Native::tcp_opts`. So a caller reaching either runtime directly had
`bind_device` silently dropped on macOS and Windows, where `build_socket`'s
`cfg` skips it: the *silently ignored setting* this workspace refuses
everywhere else, in the two runtimes that ship. Both check first now, and a
test gated to exactly the targets that lack the option asserts the refusal
— which on Linux is vacuous, so the pin is `test (macos-latest)`.

**`TcpSupport::ALL` is gone, and it had done the damage it predicts.**
`TokioHandle` declared `ALL` while delegating every connect to `Tokio`,
which declares a per-target set — so on macOS and Windows it claimed
`bind_device` and `user_timeout`, `reject_unsupported` let them through,
and nothing applied them. It declares `<Tokio as
TcpConnect>::TCP_SUPPORT` now: a delegate's claim is its delegate's. And a
public *every field* would claim the next field too, the day one is added
under a stable version, so every report starts from `NONE`.

**`Datagrams::reject_unsupported` promised an ECN check it could not
make.** Its doc said a socket claiming `ecn: true` and handed a codepoint
it would not apply is refused; the code said `let ecn = false`, because
nothing in a send and a declaration says whether the kernel applied a
mark, and the error carried an `ecn` field no path could set — kept alive
by a test that built the value by hand. Both went; the claim is checked
where it can be, on receive.

**And then IPC left `TcpConnect` for a trait of its own**, reversing the
argument this section opens with. `IpcConnect: TcpConnect` — the
supertrait is the one real constraint, since a same-machine connect must
hand back the stream a TCP one does — and `hclient-rt`'s modules follow the
seams: `tcp.rs`, `ipc.rs`, `udp.rs`, and `spawn.rs` for `Spawn` and
`Blocking`. The objection was that a stored `fn` pointer returning a boxed
future drops its auto traits (amendment C1), so `Native` would have had to
bound every runtime on the trait. It drops them only if the box declares
none: `Native::unix_socket` now keeps an `IpcRoute` — the address and a
pointer to `dial_ipc::<R>`, whose box **declares** `Send` — and carries the
bound itself, which is `Native::http3`'s arrangement (amendment C15). So
nothing but that constructor names `IpcConnect`, and a runtime with no
file descriptors — `hclient-rt-embassy`, a NAL stack, every test double —
implements nothing instead of naming a refusal. The cost is one
allocation per same-machine connect.

**It replaces the whole resolve → discovery → Happy Eyeballs → connect
block, which is `Proxy`'s slot exactly** — and a proxy and a socket
together are a **refusal**, because both answer *where does this connection
go* and a precedence rule between them would be one nobody could guess.
The two orders are not symmetrical: `unix_socket` returns a `Result` and
refuses politely, `proxy` panics, because it changes `P` and cannot hand
back a `Result<Native<.., P2>, _>` without costing every caller who never
touches a socket a `?`. Said where each is.

**`Connected::remote` became `Option<SocketAddr>` for it, and that is the
sharper half.** There is no address, and a fabricated `0.0.0.0:0` would
give a hook a *wrong* answer where the absence gives it a missing one — the
argument `Head::version` already settled one event over. Emitting no
`Connected` at all was the alternative and is worse: the `Closed` that
follows would announce the end of a connection whose beginning was never
announced, which is the defect this file records about building one out of
`wasi:http`'s error codes.

No `TcpOpts` are applied, and that is not an omission: every field of it is
a TCP or IP option `AF_UNIX` does not have. `https://` still works — the
handshake is unchanged and the server name comes from the URI.

The socket path is in the pool key, sharing `proxy`'s field since at most
one can be set, and **that correctness is unobservable** — the second
mutation control here, for the same structural reason as the proxy's:
`unix_socket` is constant within one `Native`, so two requests through one
transport cannot disagree about it.

### `Capabilities` has two kinds of field, and one of them is not a gate

`docs/competitive-gaps.md` §7 asked whether `Capabilities::proxy` "should
have a reader at all" — a producer, no reader, and the `upgrade` deletion
sitting there as a precedent. The answer is that the question was the wrong
one: `proxy` is one of **eleven** fields nothing branches on, and six of
them (`proxy`, `client_certs`, `tls_config`, `early_data`,
`connection_reuse`, `cancel_on_drop`) had no doc comment either.

**A gate** guards a setting a caller made on the `Client`, and `build()`
refuses when the transport cannot honour it — `redirects`,
`response_decompression`, `owns_cookie_jar`, `owns_cache`,
`version_select`, `timeouts`, `forbidden_request_headers`. A gate with no
branch is the *silently ignored setting* defect, and this project has
closed four of them.

**A report** states a fact about the transport, and nothing at the client
level could refuse it, because the setting it describes is configured *on
the transport*. `proxy` is a report: what it would guard is
`Native::proxy`, which is on the object that answers the question. So it
will never be a gate, and that is structural rather than an omission.

**A report is not a dead field**, which is where `upgrade` differs: those
four variants encoded a distinction with one reachable side, where a report
has both values reachable and answers a question only it can answer — *will
my requests go through a proxy* is a diagnostic's question.

`informational_1xx` is the one gate in the other direction: no `Client`
setting turns it on, and what it guards is a **claim** — `Native::hooks`
clears it, because a transport reporting `true` while reporting nothing is
a capability that lies.

The classification is enforced rather than described.
`every_capability_is_a_gate_or_a_report` lives in `hclient-core`, because
`#[non_exhaustive]` allows an exhaustive destructure only inside the
defining crate (amendment C6) — so a field added later is a compile error
in **two** places until somebody decides which kind it is. Checked by
adding one and watching both fail.

### The response head is bounded on both protocols, and one setter can refuse

`Native::h1_opts(H1Opts { max_headers, max_buf_size })`, beside
`H2Opts::max_header_list_size`. **A response head is the one part of a
response a client must buffer whole before it can act on any of it**, so it
is the one part a hostile server can make expensive without ever sending a
body — and neither half is complete without the other, because a transport
that negotiates ALPN speaks whichever protocol the server picked.

Two bounds and not one: a count alone does not bound the bytes, since a
server can send one field with a megabyte of value and stay under any
count.

**`h1_opts` is fallible where `h2_opts` is not, and the difference is who
would refuse the value.** A `SETTINGS` frame is written by this crate and
there is nobody to say no; `max_buf_size` is handed to hyper, which
`assert!`s below 8192. A caller's number reaching a `panic!` inside a
connect is not a refusal they can act on, so it is checked at the setter
and named — and the boundary itself is accepted, which a check written
`<=` would not do.

The failures come back as `ErrorKind::Connect` rather than `Body`, and
that is hyper's classification rather than a judgement made here: a head
that cannot be parsed means nothing usable came off the connection, so
there is no response for a body error to attach to.

### More than one proxy, chosen by scheme, first match wins

`Native::and_proxy` appends to a list and `Proxy::only_for(ProxyScheme)`
restricts an entry to `http://` or `https://`. The case is the ordinary
corporate one — an `HTTP_PROXY` and an `HTTPS_PROXY` at different hosts —
which one `Option` could not hold.

**First-match-wins, not most-specific-wins.** A precedence rule has to be
learned; an ordered list is read off the builder chain that wrote it. So
an unrestricted proxy placed first shadows a narrower one after it, and
that is asserted rather than warned about — the same reason `bypass`
refuses to invent the precedence that every `NO_PROXY` implementation
disagrees about.

**One `P` per transport, stated rather than worked around.** A caller
wanting SOCKS5 for `https` and an HTTP proxy for `http` cannot say so;
lifting it means erasing `P`, and erasing `P` erases the IO with it, which
is the objection that disqualified `Box<dyn ProxyProtocol>` in the first
place. `and_proxy` therefore does not change `P`, unlike `proxy` — and it
is uncallable before `proxy` for free rather than by a check, since
`NoProxy` is an empty enum and there is no `Proxy<NoProxy>` to pass.

**A bypass belongs to the proxy that carries it**, so a bypassed host
falls through to the next proxy and goes direct only when the list runs
out. The global `NO_PROXY` reading is worse *because* the list exists: a
host bypassed on an `https`-only proxy would take an `http://` request
direct, past an `http` proxy that was never in the running and never
mentioned it. With one proxy the two rules coincide exactly, which is why
this was invisible until there could be two. The first test written for it
asserted the global rule and failed — correctly.

**The pool key asks the list rather than taking the first entry, and that
correctness is unobservable** — this change's mutation control. `choose`
is a pure function of `(use_tls, host, port)` and `PoolKey` already
carries all three, so two requests that agree on the key cannot disagree
on the proxy. It is written correctly anyway for the reason the `proxy`
field is in `PoolKey` at all: a pool shared between transports ends the
coincidence.

**SOCKS4 and SOCKS4a are one type**, and that is the wire's decision rather
than ours: 4a is 4's own extension, signalled inside a SOCKS4 request by a
`DSTIP` of `0.0.0.x` — invalid as an address, therefore meaning *a hostname
follows the userid*. There is no version byte to tell them apart and no
handshake to negotiate in, so a second type would be a choice nobody can
make. The hostname form goes out always, because that is what
`ProxyProtocol::tunnel` is handed: resolving locally would leak the DNS a
proxy user is often there to hide, which is the same reason a proxy is not
a `TcpConnect` decorator.

`Socks5` is the answer unless a server forces otherwise, for the protocol's
own reasons: **no IPv6**, the address field being four bytes, and **no
authentication**, only a `USERID` the proxy may check against an identd.
The `USERID` is deliberately not marked sensitive — marking it would claim
a secrecy the protocol does not have. Two details every implementation gets
wrong once, both pinned: the reply's version byte is **zero**, not four,
and the grant is `CD = 90`, not `0`.

### Four more socket options, and `TCP_SUPPORT` stopped being a constant

`TcpOpts` gains `bind_device`, `keepalive_interval`, `keepalive_retries`
and `user_timeout`, with the matching `TcpSupport` bools — the
field-per-field mirror exists precisely so the error can name the option a
caller set, so growing it is the designed-for change.

**The consequence worth knowing is that `Tokio::TCP_SUPPORT` and
`Smol::TCP_SUPPORT` are no longer `TcpSupport::ALL`.** `SO_BINDTODEVICE`
is Linux/Android/Fuchsia, `TCP_USER_TIMEOUT` those plus Cygwin, and
`TcpKeepalive::with_retries` is missing on three more. A constant claiming
all of them everywhere would be a capability that lies on macOS and
Windows, so `TCP_SUPPORT` is `cfg!`-computed now. `ALL` still means *every
field*; it is simply no longer a value any real runtime can claim on every
target it builds for. The direction matters: an understated `TCP_SUPPORT`
costs a caller a named `Unsupported` error, an overstated one costs them an
option silently not applied.

**Keepalive is one setting in three parts and the field names do not say
so**, which is why the type says it: `set_tcp_keepalive` switches
`SO_KEEPALIVE` on, so setting *any* of the three enables it and each part
left `None` keeps the OS's value. A caller who sets only the interval has
switched keepalive on with the OS's idle time — asserted, because it reads
as a surprise otherwise.

**`bind_device` is not `local_address` renamed.** An address binds the
*source address* and the kernel still routes by its table, so a request can
leave through a different interface that happens to hold the same address.
This binds the interface, which is what a caller on a multi-homed host or
inside a VRF means. Its test asserts an outcome consistent with the claim
rather than success: `SO_BINDTODEVICE` needs `CAP_NET_RAW`, so either the
socket reports the interface back or the connect fails `EPERM` — what must
never happen is a silent success with nothing bound.

**`user_timeout` is the one that catches a peer that vanished
mid-transfer**, where keepalive catches only an idle one: probes go out
when nothing is in flight, so a connection with unacknowledged data sits in
retransmission for minutes with keepalive never firing. It overlaps
`Timeouts::between_bytes` without replacing it — the kernel's, on a socket
rather than an exchange, and the only one of the two a build with no
`Client` above it can reach.

**Two test defects surfaced, both from names rather than behaviour.**
`each_unappliable_option_is_named_on_its_own` compared the error's
*message* against every other option name as a substring — which worked
while no two names shared a prefix, and reported that a withheld
`keepalive_interval` had also named `keepalive`, which the message never
did. It compares `names()` as data now, which is what its neighbour four
lines down already did. And a fixture called `all_six_set` with a
`[&str; 6]` beside it: a count in a name and a length in a type are both
things to remember when a struct grows, and it grew.
