//! Resolves the backend, in one auditable place.
//!
//! **One feature and four targets, and the feature wins.** `idna` is the
//! only switch this crate has: turn it on and every target answers
//! through the bundled `idna` crate and its Unicode tables. Leave it off —
//! which is the default — and the answer comes from whatever the platform
//! already carries:
//!
//! | target | backend | cfg |
//! |---|---|---|
//! | Windows | the system ICU, `icuuc.dll` | `icu_backend` |
//! | Android | `android.icu.text.IDNA`, over JNI | `android_backend` |
//! | the browser (`wasm32-unknown-unknown`) | `new URL()`, through `web-sys` | `web_backend` |
//! | everything else, Linux and Apple included | the bundled `idna` crate | `idna_backend` |
//!
//! Linux takes `idna` with the feature off as well as on, which is why
//! the feature is a *forcing* switch rather than a selector: there is no
//! system UTS 46 to reach for on an ELF unix, so the row is the same
//! either way and the feature buys nothing there.
//!
//! **Apple is in that row and used to have one of its own.** Foundation
//! is reached through `NSURL`, which is a URL parser rather than a UTS 46
//! implementation: it does not case-fold ASCII and it does not validate
//! an ACE label, so eight rows of the differential corpus came back as
//! themselves. Making it conform took a punycode decoder and a sequence
//! of this crate's own — which is a reimplementation of the thing the
//! crate exists to avoid reimplementing. The bundled crate is what a
//! target without a real UTS 46 gets, and that is what Apple is.
//!
//! **Exactly one cfg comes out**, which the previous scheme could not
//! promise — it had three features whose combinations could set two at
//! once, and `src/lib.rs` carried a `compile_error!` for the case where
//! they set none. Both of those have no subject now: the rule below is a
//! total function from (feature, target) to one backend, so the source
//! reads one cfg per site and the "no backend" build does not exist.
//!
//! This is the same shape as `hclient`'s `DefaultTransport`: the default
//! is decided by the target, not by a feature the user has to pick, and
//! the resolution is written down once rather than repeated at every use
//! site. Without it, every `#[cfg]` in `src/` would spell out
//! `any(feature = "idna", not(any(windows, target_os = "android",
//! all(target_arch = "wasm32", target_os = "unknown"))))`, and the fourth
//! copy of that expression is where the platform list quietly stops
//! matching the manifest's.
//!
//! The `[target.…]` tables in `Cargo.toml` decide which *dependency* is
//! pulled in and carry the same predicate. The two must agree, which is
//! why the predicate below is written in the same form and why
//! `Cargo.toml` points here.

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rustc-check-cfg=cfg(icu_backend)");
    println!("cargo::rustc-check-cfg=cfg(android_backend)");
    println!("cargo::rustc-check-cfg=cfg(web_backend)");
    println!("cargo::rustc-check-cfg=cfg(idna_backend)");

    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let forced = std::env::var_os("CARGO_FEATURE_IDNA").is_some();

    let windows = os == "windows";
    let android = os == "android";
    // The browser, and only the browser: `wasm32-wasip2` is `target_os =
    // "wasi"` and has no `URL`, `wasm32-unknown-emscripten` is
    // "emscripten". The predicate is the one `web-sys` itself builds
    // under, and `Cargo.toml`'s target table carries the same words.
    let web = arch == "wasm32" && os == "unknown";

    // Ordered, and the order is the rule: the feature is asked first
    // because it is a promise about *every* target, and the platforms
    // follow in the order their arms appear in `Cargo.toml`. The final
    // `else` is the ELF unixes, WASI and Apple, which have no UTS 46 to
    // reach for — so it is the same branch the feature forces, reached
    // for a different reason.
    let cfg = if forced || !(windows || android || web) {
        "idna_backend"
    } else if windows {
        "icu_backend"
    } else if android {
        "android_backend"
    } else {
        "web_backend"
    };
    println!("cargo::rustc-cfg={cfg}");
}
