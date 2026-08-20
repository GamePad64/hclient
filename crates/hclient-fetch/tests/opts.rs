//! `Fetch::opts`, read back off the `web_sys::Request` the browser built.
//!
//! **No network, and that is the honest shape here rather than a
//! shortcut.** What is claimed is that a value the caller set reaches the
//! `Request` — and the browser itself answers that, since `Request` has a
//! getter for each of the four. What a test could *not* honestly arrange
//! is the thing these members are for: `credentials: "include"` against a
//! cross-origin server that sets a cookie, or `no-cors` producing an
//! opaque response, both need a second origin a headless run does not
//! have. Asserting the member is on the request is the whole of what this
//! crate is responsible for; what the browser then does with it is the
//! browser's.
#![cfg(target_arch = "wasm32")]
use wasm_bindgen_test::*;
wasm_bindgen_test_configure!(run_in_browser);

use hclient_core::RequestBody;
use hclient_fetch::opts::FetchOpts;
use web_sys::{ReferrerPolicy, RequestCache, RequestCredentials, RequestMode};

fn built(opts: &FetchOpts) -> web_sys::Request {
    let caps = hclient_fetch::Fetch::new().capabilities_for_test();
    let req = http::Request::builder()
        .uri("https://example.test/x")
        .body(RequestBody::Empty)
        .expect("request");
    hclient_fetch::testing::to_web_request_with_opts(req, &caps, opts).expect("conversion")
}

/// **The browser's own defaults are what an unset field means**, which is
/// the control every other test here needs: without it, a test that set
/// `credentials: Include` and read `Include` back would pass for a
/// transport that hard-coded it.
#[wasm_bindgen_test]
fn an_unset_field_leaves_the_browsers_default_in_place() {
    let r = built(&FetchOpts::default());
    assert_eq!(r.mode(), RequestMode::Cors);
    assert_eq!(r.credentials(), RequestCredentials::SameOrigin);
    assert_eq!(r.cache(), RequestCache::Default);
    // `""` is the empty policy, which the standard makes *the default for
    // this request's client* — a `Request` built with no policy reports
    // it rather than reporting a concrete one.
    assert_eq!(r.referrer_policy(), ReferrerPolicy::None);
}

/// **Each of the four reaches the request**, set one at a time so that a
/// setter wired to the wrong member fails its own line rather than being
/// masked by its neighbours.
#[wasm_bindgen_test]
fn each_member_the_caller_sets_reaches_the_request() {
    let r = built(&FetchOpts {
        mode: Some(RequestMode::SameOrigin),
        ..FetchOpts::default()
    });
    assert_eq!(r.mode(), RequestMode::SameOrigin);
    assert_eq!(
        r.credentials(),
        RequestCredentials::SameOrigin,
        "and the others are untouched"
    );

    let r = built(&FetchOpts {
        credentials: Some(RequestCredentials::Include),
        ..FetchOpts::default()
    });
    assert_eq!(r.credentials(), RequestCredentials::Include);
    assert_eq!(r.mode(), RequestMode::Cors, "and the others are untouched");

    let r = built(&FetchOpts {
        cache: Some(RequestCache::NoStore),
        ..FetchOpts::default()
    });
    assert_eq!(r.cache(), RequestCache::NoStore);

    let r = built(&FetchOpts {
        referrer_policy: Some(ReferrerPolicy::NoReferrer),
        ..FetchOpts::default()
    });
    assert_eq!(r.referrer_policy(), ReferrerPolicy::NoReferrer);
}

/// All four together, because a per-field `if let` chain is exactly the
/// shape in which one arm can shadow or overwrite another.
#[wasm_bindgen_test]
fn all_four_survive_being_set_at_once() {
    let r = built(&FetchOpts {
        mode: Some(RequestMode::NoCors),
        credentials: Some(RequestCredentials::Omit),
        cache: Some(RequestCache::Reload),
        referrer_policy: Some(ReferrerPolicy::Origin),
    });
    assert_eq!(r.mode(), RequestMode::NoCors);
    assert_eq!(r.credentials(), RequestCredentials::Omit);
    assert_eq!(r.cache(), RequestCache::Reload);
    assert_eq!(r.referrer_policy(), ReferrerPolicy::Origin);
}

/// **The capability set does not move**, which is the claim `redirect`'s
/// absence rests on: `Fetch` with opts reports what `Fetch` without them
/// reports, so a `Client` built on either refuses and permits the same
/// settings.
#[wasm_bindgen_test]
fn configuring_the_options_changes_no_capability() {
    use hclient_core::unversioned::Transport;
    let plain = hclient_fetch::Fetch::new();
    let configured = hclient_fetch::Fetch::new().opts(FetchOpts {
        mode: Some(RequestMode::NoCors),
        credentials: Some(RequestCredentials::Include),
        cache: Some(RequestCache::NoStore),
        referrer_policy: Some(ReferrerPolicy::NoReferrer),
    });
    let (a, b) = (plain.capabilities(), configured.capabilities());
    assert_eq!(a.redirects, b.redirects);
    assert_eq!(
        b.redirects,
        hclient_core::RedirectSupport::Internal,
        "and it is still `Internal`, which is why `redirect` is not one of \
         the members offered — see `opts`'s module doc"
    );
    assert_eq!(a.owns_cookie_jar, b.owns_cookie_jar);
    assert_eq!(a.owns_cache, b.owns_cache);
    assert_eq!(a.forbidden_request_headers, b.forbidden_request_headers);
}
