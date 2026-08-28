# hclient-mock

**`MockTransport` — a `Transport` that answers from a queue.**

It is how every capability refusal in this workspace is tested, because "a
jar against a jar-owning backend is refused at `build()`" is a fact about a
type that never sends anything. It records what it was asked for, so a test
can assert on the request as well as script the response.

```rust
let mock = MockTransport::new();
mock.push_response(http::Response::builder().status(200).body(r#"{"id":7}"#)?);

// `Clone` shares the queue and the log, so the handle stays usable after
// `Client::builder` has taken one by value.
let client = hclient::Client::builder(mock.clone()).build()?;

my_code::create_user(&client, "alice").await?;

let seen = mock.requests();
assert_eq!(seen[0].uri.path(), "/users");
assert_eq!(seen[0].body.text(), Some(r#"{"name":"alice"}"#));
assert_eq!(mock.queued(), 0);       // and no scripted response went unused
```

Responses come back in the order they were pushed, whatever was asked for.
That is deliberate: the flows this exists to test — a redirect chain, a
`425` replay, a retry — are **ordered**, and a matcher would let a test
pass while the code made its requests in the wrong order. Assert on
`requests()` instead.

`RecordedBody` has four cases rather than being an `Option<Bytes>`,
because "there was no body", "a body this mock will not read for you" and
"a body nothing can read twice" are different facts. A `Rewindable` body
is handed back as its **factory**: calling it is one more call than the
code under test made, and a test that counts calls must not have the
double counted in.

Part of [hclient](https://github.com/GamePad64/hclient) — an HTTP client
complete enough to build a new curl on, or a browser. See the repository
for the whole shape, and `AGENTS.md` in it for why this piece is its own
crate.

## Licence

MIT or Apache-2.0, at your option.
