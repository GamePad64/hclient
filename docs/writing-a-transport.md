# Writing a transport

`Transport` is this project's one extension point: it is what makes the
same application code run on a socket, in a browser, on a WASI host or on
a platform HTTP stack. This is what it takes to write one from outside
the workspace.

Everything below was measured by doing it — a scratch crate depending on
`hclient` and `hclient-core` by path and reading nothing else. The walls
it hit are the reason this document exists.

## The whole of it

Two impls and about fifteen lines.

```rust
use bytes::Bytes;
use hclient_core::unversioned::{SendTransport, Transport, BoxSendExchange};
use hclient_core::{Capabilities, Error, RequestBody};

pub struct Echo { caps: Capabilities }

impl Transport for Echo {
    type Body = EchoBody;
    type Error = Error;

    async fn execute(&self, _req: http::Request<RequestBody>)
        -> Result<http::Response<Self::Body>, Self::Error>
    {
        Ok(http::Response::builder().status(200)
            .body(EchoBody(Some(Bytes::from_static(b"hi")))).unwrap())
    }

    fn capabilities(&self) -> &Capabilities { &self.caps }
}

// Required by `hclient::Client`, and by nothing else.
impl SendTransport for Echo {
    fn execute_send(&self, req: http::Request<RequestBody>)
        -> BoxSendExchange<'_, Self::Body, Self::Error>
    { Box::pin(self.execute(req)) }
}
```

`Self::Body` is any `http_body::Body<Data = Bytes>`. `Self::Error` is any
`std::error::Error`; using `hclient_core::Error` directly lets you
override `to_error` with the identity, which is what the backends here do
so that a classified error is not re-wrapped as `Other`.

## The two things that are not obvious

### `Client` needs `SendTransport`, and `Transport` deliberately does not

`Transport::execute` returns `impl Future`, which has **no name** — so a
consumer that must prove its own future `Send` cannot ask for this one to
be. `SendTransport` is a second trait whose one method hands back a box
that does name it. At a concrete type its body is
`Box::pin(self.execute(req))` and `Send` is *inferred* rather than proved:
proof is only ever owed by generic code, which is the asymmetry the whole
design rests on.

So implement it if your transport can cross a thread, and **do not** if it
cannot — a browser one, or a runtime whose IO is `!Send`. `Transport`
alone still works; only `hclient::Client` is out of reach, and that is a
promise withheld rather than a seam closed.

Since 2026-08-28 the compiler prints the impl to write, with the paths
filled in, at the line where `Client::builder` refused. That attribute is
on `BoxedTransport` rather than on `SendTransport`, because the blanket
impl makes `BoxedTransport` the bound rustc reports as unsatisfied — which
was found by reading what it actually printed rather than by reasoning
about which trait was missing.

### Capabilities are a promise, and the floor is the safe direction

`capabilities()` returns a `&Capabilities`, so the answer is stored at
construction. Two rules keep it honest.

**Report the floor.** If your transport might negotiate HTTP/1.1 or
HTTP/2, report what holds on the worse of the two. An over-claimed
`full_duplex` deadlocks a caller; an under-claimed one costs a buffered
copy.

**A gate refuses, a report informs.** Some fields make `Client::build()`
refuse a setting the caller made — `owns_cookie_jar`, `owns_cache`,
`redirects`, `response_decompression`, `version_select`, `timeouts`,
`forbidden_request_headers`. Others state a fact nothing at the client
level could refuse, `proxy` among them. Setting a gate `true` when you do
not honour it turns a caller's setting into one that is silently ignored,
which is the defect this project has closed four times.

`Capabilities::default()` understates everything, so a transport that
sets nothing is honest and merely modest.

## What `execute` owes

Three obligations, all stated on the trait and worth repeating because
they are easy to get wrong:

- **Dropping the future cancels the exchange.** No further request bytes,
  no waiting for a response, and whatever carries it is torn down. A drop
  is never a way to detach a request into the background.
- **`Timeouts` in `req.extensions()` is present on every request that
  comes through `Client`**, including when the caller set none — it will
  be a `Timeouts` with every field `None`. Read it field by field;
  branching on `.is_some()` as *"the caller asked for timeouts"* is always
  true and always wrong.
- **Only honour what your `Capabilities` claim.** The two are one promise
  read from two directions.

## Testing it

`hclient-mock` is the double for the *other* side — code that consumes a
`Client`. For a transport of your own, the useful checks are the ones this
workspace applies to its own:

- an exchange completes and the body arrives whole;
- dropping the future partway leaves nothing running, observed from the
  far side of whatever carries it;
- each capability you set `true` is honoured, and each you leave `false`
  makes `Client::build()` refuse the matching setting;
- the request reaches the wire with what the caller put on it.

## What you do not have to write

`Client` supplies redirects, the cookie jar, the response cache,
decompression, digest auth, the `425` replay and the whole-operation
timeout — every one of them above the seam, so a transport gets them by
existing. That is the trade the seam makes: `execute` is the poorest shape
of the four ambient APIs it was taken from, and everything richer is built
once, above it.
