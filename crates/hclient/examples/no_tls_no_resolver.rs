//! A `Client` with neither TLS nor a resolver — the smallest thing this
//! library assembles into.
//!
//! For a target that has `std` and little else: a static musl binary, a
//! scratch container, embedded Linux, a device that talks to one address it
//! was configured with. Swapping in `NoTls` and `IpLiteralOnly` drops
//! rustls, native-tls, OpenSSL and the system resolver out of the
//! dependency graph entirely — check with
//! `cargo tree -p hclient --no-default-features -e normal`.
//!
//! This is the "assembled from parts" half of the project's mission made
//! literal: nothing here is a stripped-down mode of the full client. It is
//! the same `Client`, with two of its four seams filled by parts that do
//! nothing, and it says so honestly rather than failing later.
//!
//! Run: `cargo run -p hclient --example no_tls_no_resolver`
//!
//! Native-only, and gated as such. `cargo test --target
//! wasm32-unknown-unknown` — what the `browser` job runs through
//! `wasm-pack` — builds every `[[example]]` in this crate, and the parts
//! assembled here (`hclient-native`, `hclient-rt-smol`) are
//! `cfg(not(target_family = "wasm"))` dev-dependencies, so on wasm the
//! file cannot resolve them. That is not a gap to paper over: this example
//! is about a target that has `std` and nothing else, and a browser build
//! of it would be meaningless rather than merely unsupported.

#[cfg(not(target_family = "wasm"))]
fn main() {
    use hclient::Client;

    use hclient_core::{ErrorKind, TlsSupport, unversioned::Transport};
    use hclient_dns::IpLiteralOnly;
    use hclient_native::Native;
    use hclient_tls::NoTls;

    let transport = Native::new(hclient_rt_smol::Smol, NoTls, IpLiteralOnly);

    // The capability is read BEFORE a request, which is the point of the
    // registry. Before `TlsConnect::tls_support` existed, `Native` reported
    // `Full` here regardless of what was plugged in, and a caller branching
    // on it was told it had TLS right up until the connect failed.
    assert_eq!(transport.capabilities().tls_config, TlsSupport::None);

    let client = Client::builder(transport)
        .build()
        .expect("the default configuration asks for nothing this stack lacks");

    // A hostname is refused, and the error says what is missing rather than
    // timing out or resolving to nothing.
    let err = futures_executor::block_on(client.get("http://example.com/").send())
        .expect_err("there is no resolver to turn a name into an address");
    assert_eq!(*err.kind(), ErrorKind::Resolve);
    println!("hostname  -> {err}");

    // An IP literal is what this build is for. Port 1 on loopback is
    // closed, so this fails at connect — and note the KIND: `Connect`, not
    // `Resolve`. The address was understood; nothing answered. Loopback
    // rather than a documentation address because it is deterministic
    // everywhere: a documentation range can be intercepted by a proxy, and
    // this example is meant to be run.
    let err = futures_executor::block_on(client.get("http://127.0.0.1:1/").send())
        .expect_err("port 1 on loopback is closed");
    assert_eq!(*err.kind(), ErrorKind::Connect);
    println!("v4 literal -> {err}");

    // `https://` cannot work here, and fails with a typed TLS error rather
    // than a confusing connect failure on port 443.
    let err = futures_executor::block_on(client.get("https://192.0.2.1/").send())
        .expect_err("this build has no TLS");
    assert_eq!(*err.kind(), ErrorKind::Tls);
    println!("https      -> {err}");

    // What this build KEEPS. Stripping TLS and the resolver costs nothing
    // else: the redirect stage is still there and still configurable,
    // because `Native` follows redirects itself rather than delegating them
    // — so a policy is honoured here and would be refused only by a backend
    // that follows them internally, like the browser's `fetch`.
    let with_policy = Client::builder(Native::new(hclient_rt_smol::Smol, NoTls, IpLiteralOnly))
        .redirect(hclient::redirect::Forbid)
        .build();
    assert!(
        with_policy.is_ok(),
        "a redirect policy is honourable on this stack; only an Internal backend refuses one"
    );
    println!("redirect policy accepted: the stubs cost only what they replace");
}

// The browser has no socket to open and no TLS to decline — `fetch` is the
// transport and the host owns both ends of that choice. There is nothing
// for this example to show there, so this half exists only to keep the
// file compiling where the suite builds it.
#[cfg(target_family = "wasm")]
fn main() {}
