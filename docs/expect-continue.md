# `Expect: 100-continue`, and why the timer cannot live in the body

`docs/informational-1xx.md` §5 ends: *"this reports a `100` when a server
sends one; it does not **ask** for one, and does not withhold a request
body waiting for it."* This is that half.

## 1. What it is for, which is not speed

A body that will be rejected costs the same to upload as one that will be
accepted. `Expect: 100-continue` is how a client asks first — and the two
things that reject a body before reading it are exactly what landed in
this tree this week: **a proxy answering `407`**, and an origin answering
`401` or `413`. RFC 9110 §10.1.1.

## 2. hyper's client does not do this, and that was checked

`Expect` appears in hyper 1.11 only on the **server** side —
`role.rs:321` parses it out of an incoming request and `conn.rs:304`
sends the `100`. The client neither withholds a body nor waits. So the
waiting is ours, and the question becomes where the two halves live.

**The reading half already exists.** `hyper::ext::on_informational` is
what `Native::watching_1xx` installs, and a `100` arrives through it —
so the signal that releases the body is the one v0.4 already wired.

**And hyper's dispatcher makes the withholding safe**, which had to be
read rather than assumed: `dispatch.rs:173-174`'s `poll_loop` calls
`poll_read` and *then* `poll_write` on every turn, so a request body that
answers `Pending` does not stop the response from being read. A client
whose dispatcher wrote before it read would deadlock on the first
`Expect`, and this one does not.

## 3. The timer cannot live in the body, and that decides the design

The obvious shape is a body that holds its own deadline. It cannot:

- A **concrete** `Pin<Box<Tm::Sleep>>` — which is what `Deadline` holds
  for the *response* body — would give `OutgoingBody` a type parameter,
  and `http::Request<OutgoingBody>` appears in a dozen signatures across
  this crate.
- Erasing it behind `Box<dyn Future>` drops auto traits, which is
  amendment C1 and the reason `hyper::upgrade::Upgraded` and
  `hyper/http2` are both unusable here. A `+ Send` to make it work is the
  bound this workspace exists not to declare.

So the body holds **only a gate** — an `Arc` with a flag and a waker, no
future inside it — and the *clock* stays where a clock already is:
`Native::execute`, which has `R: Timer` and is already racing
`Timeouts::first_byte` against the exchange. It opens the gate on expiry.
The body knows nothing about time; the transport knows nothing about
frames.

## 4. One callback, two readers

`hyper::ext::on_informational` stores **one** callback in the request's
extensions, so a second call replaces the first. The gate and
`watching_1xx`'s hook cannot each install their own: there is one
closure, and it does both — opens the gate on a `100`, reports every
`1xx` to the hooks if anything is watching.

## 5. It is an opt-in, and the default sends immediately

`Native::expect_continue(Duration)`. Without it, a request carrying the
header behaves exactly as it does today: the header goes out and the body
follows without waiting, which is legal — RFC 9110 §10.1.1 requires only
that a client *not* wait indefinitely.

**A default that waited would be a default that hangs.** A server that
ignores `Expect` — legal for HTTP/1.0, and true of some proxies — sends
no `100`, so a client that waited by default would hold every such upload
for the whole bound, on a request nobody asked to change. The knob's
owner is the person who knows how much their upload is worth, which is
the same argument `Timeouts` is built on and the reason this is not a new
`Timeouts` field: `first_byte` bounds *failure*, and this bounds
*proceeding anyway*, which is the opposite outcome from the same wait.

## 6. Deliberately not done

- **HTTP/2.** RFC 9113 permits `Expect: 100-continue` and `h2` surfaces
  the `100` through the same `poll_informational` v0.4 already reads, but
  the body there is a `SendStream` this crate drives itself rather than a
  `Body` hyper pulls, so the gate has no subject. Recorded rather than
  attempted.
- **Abandoning the body on a final response.** RFC 9110 allows a client
  that receives a final status before sending the body to stop sending
  it. hyper owns the body and will keep pulling; making it stop needs a
  seam that does not exist.
