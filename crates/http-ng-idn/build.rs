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
    println!("cargo::rustc-check-cfg=cfg(foundation_backend)");
    println!("cargo::rustc-check-cfg=cfg(idna_backend)");

    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let vendor = std::env::var("CARGO_CFG_TARGET_VENDOR").unwrap_or_default();

    // **Windows and nothing else.** The rule is not "does this OS ship an
    // ICU somewhere" but "is there a stable, statically linkable ABI whose
    // version comes with the OS":
    //
    // - Windows: yes. `icuuc.dll` is part of the OS from 10 1703, its
    //   exports are unsuffixed (`U_DISABLE_RENAMING`), `windows-sys`
    //   declares them from Microsoft's own metadata, and the import is
    //   resolved at link time.
    // - Linux and other ELF unixes: no, and this is the case that was
    //   built and then deliberately removed. Both the soname and every
    //   symbol carry the ICU major version (`libicuuc.so.78`,
    //   `uidna_openUTS46_78`), so reaching them at all meant `dlopen` plus
    //   a version search — and what came back was whatever ICU that
    //   machine happens to carry, on a Unicode version nobody chose and
    //   nothing reports. For IDN a Unicode version difference is a
    //   different host, so that is a correctness risk taken on for a size
    //   saving. Removed; see the crate docs.
    // - wasm: nothing to link.
    let platform_has_icu = os == "windows";

    // Apple is the second platform that satisfies the same rule by a
    // different route: Foundation is part of the OS, is linked, and its
    // UTS 46 arrives with the OS rather than with whatever the user
    // installed. It is reached through `NSURL` rather than a `uidna_*`
    // entry point — `libicucore.dylib`, Apple's own ICU, stays out of
    // reach — so it is a separate backend, not a second ICU one.
    let platform_has_foundation = vendor == "apple";

    let feature = |name: &str| std::env::var_os(format!("CARGO_FEATURE_{name}")).is_some();
    let (platform, bundled, system_icu, foundation) = (
        feature("PLATFORM"),
        feature("BUNDLED"),
        feature("SYSTEM_ICU"),
        feature("FOUNDATION"),
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
    if (foundation || platform) && platform_has_foundation {
        println!("cargo::rustc-cfg=foundation_backend");
    }
    if (bundled || platform) && !platform_has_icu && !platform_has_foundation {
        println!("cargo::rustc-cfg=idna_backend");
    }
}
