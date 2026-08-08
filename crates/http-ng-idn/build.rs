//! Resolves the backend **by target**, in one auditable place.
//!
//! This is the same shape as `http-ng`'s `DefaultTransport`: the default
//! is decided by the target, not by a feature the user has to pick, and
//! the resolution is written down once rather than repeated at every use
//! site. Without this, every `#[cfg]` in `src/` would have to spell out
//! `all(feature = "platform", any(windows, all(unix, not(target_vendor =
//! "apple"))))`, and the fourth copy of that expression is where the
//! platform list quietly stops matching `icu::candidates`.
//!
//! Two cfgs come out, and the source reads only these:
//!
//! - `icu_backend` — the platform's ICU is compiled in.
//! - `idna_backend` — the bundled `idna` crate is compiled in.
//!
//! Both, one, or neither; `src/lib.rs` turns "neither" into a
//! `compile_error!`.
//!
//! The corresponding `[target.…]` tables in `Cargo.toml` decide which
//! *dependency* is actually pulled in, and they carry the same predicate.
//! The two must agree, which is why the predicate below is written in the
//! same form and why `Cargo.toml` points here.

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rustc-check-cfg=cfg(icu_backend)");
    println!("cargo::rustc-check-cfg=cfg(idna_backend)");

    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let family = std::env::var("CARGO_CFG_TARGET_FAMILY").unwrap_or_default();
    let vendor = std::env::var("CARGO_CFG_TARGET_VENDOR").unwrap_or_default();

    // The targets with a system UTS 46 this crate can reach: Windows
    // (`icu.dll`/`icuuc.dll`) and ELF unixes (`libicuuc.so.NN`). Apple is
    // excluded because `libicucore.dylib` is private and Foundation's
    // conversion is only reachable through a whole URL parse; wasm has no
    // dynamic loader at all. See the crate docs for both in full.
    let platform_has_icu =
        os == "windows" || (family.split(',').any(|f| f == "unix") && vendor != "apple");

    let feature = |name: &str| std::env::var_os(format!("CARGO_FEATURE_{name}")).is_some();
    let (platform, bundled, system_icu) = (
        feature("PLATFORM"),
        feature("BUNDLED"),
        feature("SYSTEM_ICU"),
    );

    // `platform` takes whichever this target can use; an explicit
    // feature asks for one by name. Both are gated on `platform_has_icu`
    // because the DEPENDENCY is: asking for `bundled` on Linux enables a
    // `dep:idna` that this target's tables do not supply, so there is no
    // `idna` to compile against.
    //
    // No `cargo::error` here, deliberately. Cargo features must be
    // additive, and `--workspace --all-features` — the command this
    // project's CI and its contributors run — turns on all three at once
    // on every OS. Erroring would make the standard invocation fail on
    // Linux and Windows. A request that genuinely leaves no backend at
    // all (`--no-default-features --features bundled` on Linux) sets
    // neither cfg, and `src/lib.rs`'s `compile_error!` says so by name.
    if (system_icu || platform) && platform_has_icu {
        println!("cargo::rustc-cfg=icu_backend");
    }
    if (bundled || platform) && !platform_has_icu {
        println!("cargo::rustc-cfg=idna_backend");
    }
}
