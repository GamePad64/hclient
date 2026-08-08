//! `Client::new()` on `wasm32-unknown-unknown` — the first browser tests in
//! this crate.
//!
//! Everything here runs in a real browser under `wasm-pack test`. That
//! matters more than usual: `cargo test --workspace` being green says
//! nothing about any line below, because a host run never touches these
//! tests at all. CI does execute them, on Chrome and Firefox, in the
//! `browser` job — it did not when this file was written, which is why an
//! earlier version of this comment said no CI job ran a browser test.
//!
//! Run them with
//!
//! ```text
//! wasm-pack test --headless --chrome crates/http-ng --features default-transport,test-util
//! ```
//!
//! The feature flags are `wasm-pack`'s own arguments. The `cargo test`-style
//! `-- --features ...` is rejected outright with "unexpected argument
//! '--features'", which is worth stating because it looks like the form that
//! would work.
//!
//! `#![cfg(all(feature = "default-transport", target_family = "wasm",
//! target_os = "unknown"))]` — the same three conditions that gate
//! `DefaultTransport` and `Client::new()` themselves. Without the feature,
//! or on any other target, this file compiles to nothing rather than to a
//! build error.

#![cfg(all(
    feature = "default-transport",
    target_family = "wasm",
    target_os = "unknown"
))]

use http_ng::{Capabilities, Client, RedirectPolicy, RedirectSupport};
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

/// A URL that is guaranteed to be fetchable, offline, from inside the test
/// harness: the harness's own already-loaded page.
///
/// **Not the `data:` URL this task's brief wrote** (`client.get("data:text/
/// plain,portable")`). That code cannot reach `Fetch` at all through
/// `http_ng::Client`, for two independent reasons — both already measured
/// and recorded one crate over, in `http-ng-fetch/tests/transport.rs`,
/// where the identical line in that task's brief had to be replaced for the
/// identical reasons:
///
/// 1. `http::Uri` cannot parse `"data:text/plain,portable"` in the first
///    place (`InvalidUri(InvalidFormat)`): the `http` crate's grammar
///    covers HTTP's own request-target forms, not RFC 3986 at large, and
///    the comma is what breaks it. `RequestBuilder::new` parses the string
///    into an `http::Uri` before `http-ng-fetch` is ever reached, so this
///    fails a whole crate away from the transport.
/// 2. Even given a parse, `http-ng-fetch`'s `convert::checked_url` rejects
///    every scheme but `http`/`https` as `ErrorKind::Unsupported`,
///    deliberately and documented as such.
///
/// Re-verified rather than inherited: `Client::new().get("data:text/plain,
/// portable")` fails in the browser, because `fetch()` rejects a `data:` URL
/// for a same-origin-credentialled request rather than serving it inline the
/// way the plan's example assumed. Using the loaded page instead keeps the
/// exchange fully offline — no Internet access needed, which is what the `data:` URL was
/// reaching for — while actually exercising the network path a `data:` URL
/// skips entirely in a browser.
fn harness_page_url() -> String {
    web_sys::window()
        .expect("wasm_bindgen_test_configure!(run_in_browser) guarantees a window")
        .location()
        .href()
        .expect("the currently loaded page always has an href")
}

/// The vertical's actual acceptance claim: the same two lines that work on
/// native in vertical 2, unchanged, in a browser. No transport named, no
/// `Result` on the constructor, no `#[cfg]` in the caller.
#[wasm_bindgen_test]
async fn the_two_line_example_from_the_readme_works_in_a_browser() {
    let client = Client::new();
    let resp = client.get(&harness_page_url()).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let text = resp.collect().await.unwrap().text().unwrap();
    assert!(
        !text.is_empty(),
        "the harness's own already-loaded page must come back non-empty"
    );
}

/// `Client::new()` returns `Self`, not `Result<Self, _>`, and this is where
/// that is checked rather than merely asserted in a doc comment: the
/// annotation is the test. It compiles only while the browser constructor
/// is infallible.
#[wasm_bindgen_test]
async fn client_new_is_infallible_on_this_target() {
    let client: Client = Client::new();
    assert_eq!(
        client.capabilities().redirects,
        RedirectSupport::Internal,
        "and the thing it built really is the browser transport"
    );
}

/// The dependency `Client::new()`'s `.expect(...)` rests on, checked
/// against the capabilities the transport **actually advertises** rather
/// than a hand-written `Capabilities` literal that happens to say
/// `Internal`. If `Fetch`'s advertised capabilities change, this test
/// changes with them; a literal would keep passing while the claim it
/// stands for quietly stopped being true.
#[wasm_bindgen_test]
async fn client_news_own_config_passes_the_real_fetch_capabilities() {
    let client = Client::new();
    let caps: &Capabilities = client.capabilities();

    assert!(
        client.config().redirect.is_none(),
        "the whole reason `Client::new()` can panic-free `expect` a successful `build()`"
    );
    assert_eq!(
        caps.redirects,
        RedirectSupport::Internal,
        "the premise: this backend follows redirects itself"
    );
    http_ng::check_supported(client.config(), caps, "fetch")
        .expect("`Client::new()`'s own configuration must be supported by its own transport");
}

/// The other half, and the one that proves the test above isn't passing
/// because the check is dead: the same real capabilities, the same real
/// transport, plus a policy the caller actually asked for — and now it is
/// an error, named.
#[wasm_bindgen_test]
async fn a_configured_redirect_policy_is_rejected_by_the_real_browser_transport() {
    let err = Client::builder(http_ng_fetch::Fetch::new())
        .redirect(RedirectPolicy::Limited(5))
        .build()
        .unwrap_err();
    assert_eq!(err.what, "redirect_policy");
    assert!(err.backend.contains("Fetch"), "{}", err.backend);
}

/// The per-request path, in a real browser, all the way out of `send()`.
///
/// `limit: 0` — "do not follow redirects" — is the branch `act`'s
/// `http-client` component takes when `follow_redirects` is false, and it
/// is precisely what fetch can never honour: its `redirect: "follow"`
/// default is not overridable through `http-ng-fetch`. A silent no-op here
/// would mean a consumer asking not to follow redirects and being followed
/// anyway, with nothing said.
///
/// Named for what this body actually checks. The stronger claim — that the
/// rejection happens *before* a request goes out — is pinned by the host
/// sibling `tests/redirect.rs`'s
/// `a_per_request_policy_against_an_internal_backend_fails_at_send`, which
/// asserts `requests().is_empty()` and dies to a mutation moving the check
/// after `execute`. This test cannot: there is no `MockTransport` in a
/// browser, so nothing here can observe whether a request was issued, and a
/// name claiming otherwise would be the exact defect this project keeps
/// finding.
#[wasm_bindgen_test]
async fn a_per_request_redirect_policy_is_rejected_as_unsupported() {
    let client = Client::new();
    let err = client
        .get(&harness_page_url())
        .redirect(RedirectPolicy::Limited(0))
        .send()
        .await
        .unwrap_err();
    assert_eq!(*err.kind(), http_ng::ErrorKind::Unsupported, "{err}");
    assert!(err.to_string().contains("redirect_policy"), "{err}");
}

/// The failure mode the `Option` exists to prevent, checked where it would
/// actually bite: an ordinary browser request, with no redirect policy
/// anywhere, must not be rejected for a setting nobody made. This is the
/// same property as the test above it inverted, and it is the one that
/// would have gone unnoticed had `Config.redirect` stayed a bare
/// `RedirectPolicy`.
#[wasm_bindgen_test]
async fn a_request_that_mentions_no_redirect_policy_is_not_rejected() {
    let client = Client::new();
    let resp = client.get(&harness_page_url()).send().await.unwrap();
    assert_eq!(resp.status(), 200);
}
