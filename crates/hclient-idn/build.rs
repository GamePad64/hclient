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
//! | Apple | Foundation, through `NSURL` | `apple_backend` |
//! | Windows | the system ICU, `icuuc.dll` | `icu_backend` |
//! | Android | `android.icu.text.IDNA`, over JNI | `android_backend` |
//! | everything else, Linux included | the bundled `idna` crate | `idna_backend` |
//!
//! Linux takes `idna` with the feature off as well as on, which is why
//! the feature is a *forcing* switch rather than a selector: there is no
//! system UTS 46 to reach for on an ELF unix, so the row is the same
//! either way and the feature buys nothing there.
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
//! `any(feature = "idna", not(any(target_vendor = "apple", windows,
//! target_os = "android")))`, and the fourth copy of that expression is
//! where the platform list quietly stops matching the manifest's.
//!
//! The `[target.…]` tables in `Cargo.toml` decide which *dependency* is
//! pulled in and carry the same predicate. The two must agree, which is
//! why the predicate below is written in the same form and why
//! `Cargo.toml` points here.

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rustc-check-cfg=cfg(icu_backend)");
    println!("cargo::rustc-check-cfg=cfg(apple_backend)");
    println!("cargo::rustc-check-cfg=cfg(android_backend)");
    println!("cargo::rustc-check-cfg=cfg(idna_backend)");

    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let vendor = std::env::var("CARGO_CFG_TARGET_VENDOR").unwrap_or_default();
    let forced = std::env::var_os("CARGO_FEATURE_IDNA").is_some();

    let apple = vendor == "apple";
    let windows = os == "windows";
    let android = os == "android";

    // Ordered, and the order is the rule: the feature is asked first
    // because it is a promise about *every* target, and the platforms
    // follow in the order their arms appear in `Cargo.toml`. The final
    // `else` is the ELF unixes and wasm, which have no system UTS 46 to
    // reach for — so it is the same branch the feature forces, reached
    // for a different reason.
    let cfg = if forced || !(apple || windows || android) {
        "idna_backend"
    } else if apple {
        "apple_backend"
    } else if windows {
        "icu_backend"
    } else {
        "android_backend"
    };
    println!("cargo::rustc-cfg={cfg}");
}
