# `1xx` responses: one capability, two structurally different routes

`Capabilities::informational_1xx` had been inert since v0.1 — set by
nobody, read by nobody — which is the shape `Capabilities::proxy` had
until this week and `UpgradeSupport` had before it was deleted. This is
the counterpart.

## 1. It is an event, not a response

`Transport::execute` resolves exactly once, and a `1xx` is not that once.
So it goes through the hooks seam (v0.4 W2) as `Event::Informational`,
which is also what makes it useful: `103 Early Hints` exists so a client
can start fetching subresources **while the origin is still thinking**,
and a caller told about it after the response has been told nothing it
can act on.

`Informational` carries `id`, `status` and `headers`, and **no
`version`** — deliberately. The connection's protocol was already
reported by the `Connected` or `Reused` that opened this exchange, both
of which carry a plain `Version` rather than an `Option`, because only a
transport that owns a connection emits either. A third place to be wrong
about one fact is a third place to be wrong.

## 2. The bound, and why it is on a constructor

The two protocols reach the same capability by routes that share nothing:

| | HTTP/1 | HTTP/2 |
|---|---|---|
| API | `hyper::ext::on_informational`, a callback in the request's extensions | `h2::client::ResponseFuture::poll_informational`, a poll |
| bound | `F: Fn(..) + Send + Sync + 'static`, stored as `Arc<dyn .. + Send + Sync>` | none |
| who drives it | hyper, from inside its own dispatcher | the same future that awaits the response |

**That `Send + Sync` is the third time hyper's auto-trait requirements
have shaped this crate.** The first ruled out `hyper/http2` in v0.2 — a
sealed `Http2ClientConnExec` whose executor is handed the connection. The
second ruled out `hyper::upgrade::Upgraded` for the WebSocket work — a
`Rewind<Box<dyn Io + Send>>`. Here it collides with a property this
workspace documents as supported: **a hook may hold an `Rc`**
(`http-ng-core/tests/shape.rs`, P13), which is why the seam declares no
auto traits at all.

The difference is that this time there is a pattern for absorbing it.
`Native::multiplexed()` puts `Spawn`'s bound on the opt-in constructor and
stores a `fn` pointer in a field that demands nothing of `R`. The same
here: `Native::watching_1xx()` carries `H: Clone + Send + Sync + 'static`,
the field is `Option<fn(&H, &mut Request<..>, ConnectionId)>`, and **no
signature a single-threaded hook meets gains a bound**. A hook holding an
`Rc` gets `E0277` on the line where it asked for `1xx`, and nowhere else.

HTTP/2 needs none of that, and the reason is worth naming: the callback
there is a `&dyn Fn(StatusCode, &HeaderMap)` built one stack frame up and
called from inside the future that awaits the response, so it neither
outlives the call nor crosses a thread — and nothing below has to name
`H`. **One switch turns both on**, because the capability reports the
floor: a `true` that held on h2 alone would be a claim an HTTP/1
connection could not keep.

## 3. Three defects this found, all of them in the writing

- **The poll order in h2 was wrong, and the comment justifying it was
  confidently wrong too.** The first version polled for interim heads at
  the top of the loop, *before* the connection was driven — i.e. before
  the frames carrying them had been read — with a comment arguing that
  interim heads must be reported "first". They must be reported before
  `resp_fut` **resolves**, not before the IO is driven, and the loop
  returns on `Ready` so there is no second chance. Measured, not
  reasoned: written that way, the `103` never arrived.
- **`Native::hooks` left the capability behind.** It drops the installer
  pointer, because the pointer's type names `H` — the same trap
  `.multiplexed()` has — but it carried `caps` across unchanged, so
  `.watching_1xx().hooks(h)` reported nothing while claiming
  `informational_1xx == true`. **A capability lying**, which is worse
  than the silent downgrade it accompanies, because a caller can act on a
  capability. Found by the pair-of-orders test, not by reading.
- **The shared HTTP/2 path was wired and unreached.** The mutation that
  removes its poll survived the entire suite. That is a gap rather than a
  control, and it is closed:
  `a_1xx_on_a_shared_connection_reaches_the_hook` runs two concurrent
  requests over **one** accepted connection and asserts one interim head
  each.

## 4. Mutations

Anchor 270 (`http-ng-native --all-features`), `--no-fail-fast`.

| # | mutation | verdict | killed |
|---|---|---|---|
| M1 | the HTTP/1 callback is never installed | killed | 2 |
| M2 | `exchange` does not poll for interim heads | killed | 1 |
| M3 | `watching_1xx` does not set the capability | killed | 1 |
| M4 | the h2 poll moves back above the connection drive | killed | 1 |
| M5 | **control** — `hooks()`'s two capability lines are rewritten as a block expression with identical semantics | **survived, as intended** | 0 |
| M6 | `exchange_shared` does not poll | **survived, then killed** | 0 → 1 |

M6 is the row worth reading: it survived on the first run and that was
the finding, not the verdict. A fixture now reaches that path.

## 5. Not covered

- **`Informational::id` is not asserted against the `Connected` that
  opened the same connection.** The field is populated from `est.id()` on
  both protocols, and no test reads it — a caller correlating several
  in-flight exchanges is relying on something unmeasured.
- **`http-ng-h3` reports none**, and that is not argued here either way:
  RFC 9114 permits `1xx`, `h3`'s client surface was not examined, and
  the transport's capability says `false`, which is the honest value for
  a path nobody wrote.
- **No `Expect: 100-continue` request-side behaviour.** This reports a
  `100` when a server sends one; it does not *ask* for one, and does not
  withhold a request body waiting for it.
