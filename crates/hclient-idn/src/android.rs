//! Android's own UTS 46, which is ICU4J behind the JVM.
//!
//! `android.icu.text.IDNA` has shipped with the platform since API 24 and
//! is the same ICU this crate reaches through `icuuc.dll` on Windows —
//! the same option bits, the same error names, a different language. What
//! it does not have is a C entry point: the NDK exposes no `uidna_*`, so
//! the only way to it is JNI. That is `jni` + `ndk-context`, the pair
//! `hclient-proxy` takes to read the system proxy settings, and for the
//! same reason: the thing being read lives behind managed code.
//!
//! The alternative is the bundled `idna` crate, whose Unicode tables are
//! the ~1.9 MB this crate exists to keep off a mobile target. That
//! alternative is still one feature away — `--features idna` — for a build
//! that would rather have the tables than the JVM call.
//!
//! # Two halves, and only one of them can be run here
//!
//! **This file has never been executed.** No runner in this project is an
//! Android device, and `cargo check --target aarch64-linux-android`
//! proves the JNI signatures type-check, not that they resolve. That is
//! the shape `hclient-dns-system`'s Apple arm was in when every one of its
//! live tests failed in ten milliseconds, and it is written down here
//! rather than discovered later.
//!
//! What limits the damage is the split this workspace applies to every
//! platform module: the half that talks to the platform holds no
//! decisions, and the half that holds decisions is tested on any host.
//! Here the decision is [`IGNORED`] — which of ICU's error names this
//! crate treats as *not fatal* — and `agrees_with_the_bit_mask` pins it
//! against [`crate::icu::IGNORED_ERRORS`] on the machine you are reading this
//! on.
//!
//! # Why the errors are read by name
//!
//! ICU4C reports `UIDNAInfo.errors` as a bit word and ICU4J reports
//! `IDNA.Info.getErrors()` as an `EnumSet<IDNA.Error>`. The bits and the
//! enum constants are the same list in the same order, but a `Set` cannot
//! be masked — so the six this crate forgives are matched by their enum
//! `name()`, and the constant below is the same decision
//! [`crate::icu::IGNORED_ERRORS`] states in bits.
//!
//! Reading `hasErrors()` instead would have been three JNI calls fewer
//! and a second divergence: Android would then refuse `-münchen.de`,
//! `münchen..de` and `ab--cd.münchen` where Windows and Linux accept
//! them. This crate already carries one such divergence, on Apple, and
//! records it as a cost rather than a design — adding a second knowingly
//! is not the same thing.

use crate::icu::{
    UIDNA_ERROR_DOMAIN_NAME_TOO_LONG, UIDNA_ERROR_EMPTY_LABEL, UIDNA_ERROR_HYPHEN_3_4,
    UIDNA_ERROR_LABEL_TOO_LONG, UIDNA_ERROR_LEADING_HYPHEN, UIDNA_ERROR_TRAILING_HYPHEN,
};

/// The `IDNA.Error` constants this crate forgives, beside the bit each one
/// is in [`crate::icu::IGNORED_ERRORS`].
///
/// The bit is carried so the two statements can be checked against each
/// other rather than trusted to stay in step — see the test at the
/// bottom. The names are ICU4J's, which are ICU4C's `UIDNA_ERROR_*`
/// without the prefix.
#[cfg_attr(
    not(android_backend),
    allow(
        dead_code,
        reason = "only an Android build has a JNI walk to feed this, but the POLICY is \
                  platform-independent and so is its test — gating it away here would stop \
                  the one half of this backend that can be checked without an Android"
    )
)]
const IGNORED: [(&str, u32); 6] = [
    ("EMPTY_LABEL", UIDNA_ERROR_EMPTY_LABEL),
    ("LABEL_TOO_LONG", UIDNA_ERROR_LABEL_TOO_LONG),
    ("DOMAIN_NAME_TOO_LONG", UIDNA_ERROR_DOMAIN_NAME_TOO_LONG),
    ("LEADING_HYPHEN", UIDNA_ERROR_LEADING_HYPHEN),
    ("TRAILING_HYPHEN", UIDNA_ERROR_TRAILING_HYPHEN),
    ("HYPHEN_3_4", UIDNA_ERROR_HYPHEN_3_4),
];

/// Whether an error set this crate has read by name is fatal.
///
/// Pure, and the whole of the Android backend's policy: an error nobody
/// listed is fatal, and a name in [`IGNORED`] is not. Separated from the
/// JNI walk for the reason the module doc gives — this half runs in the
/// test suite on every platform, and the half above it runs nowhere this
/// project can reach.
#[cfg_attr(
    not(android_backend),
    allow(
        dead_code,
        reason = "see `IGNORED` above — the policy is tested everywhere"
    )
)]
pub(crate) fn is_fatal_by_name<'a>(mut errors: impl Iterator<Item = &'a str>) -> bool {
    errors.any(|e| !IGNORED.iter().any(|(name, _)| *name == e))
}

#[cfg(android_backend)]
pub(crate) use imp::{Android, convert, find};

/// The name every backend module exports, so that `lib.rs` can select one
/// with `cfg_select!` and then name no platform at all.
#[cfg(android_backend)]
pub(crate) type Handle = Android;

#[cfg(android_backend)]
mod imp {
    use jni::JavaVM;
    use jni::objects::{JObject, JString, JValue};

    /// Nothing to carry: the class is part of the platform and the JVM
    /// handle comes from `ndk_context` at each call. The type exists so
    /// the backend has the same shape as the other two.
    #[derive(Debug)]
    pub(crate) struct Android;

    impl Android {
        pub(crate) fn name(&self) -> &str {
            "android.icu.text.IDNA (ICU4J, over JNI)"
        }
    }

    /// `Some` where a JVM is registered and `android.icu.text.IDNA`
    /// resolves.
    ///
    /// Both halves are real questions rather than ceremony. A registered
    /// JVM is absent in a unit test binary and in a command-line tool
    /// built for Android, which is `hclient-proxy`'s
    /// `ndk_context` note one crate over; the class is absent below API
    /// 24. Either way the caller falls back the way it does for a Windows
    /// with no `icuuc.dll` — see [`crate::backend`].
    pub(crate) fn find() -> Option<Android> {
        with_env(|env| {
            env.find_class("android/icu/text/IDNA")
                .map(|_| Android)
                .ok()
        })?
    }

    /// The A-label form of `domain`, or `None` if ICU4J refused it for a
    /// reason this crate treats as fatal.
    pub(crate) fn convert(_a: &Android, domain: &str) -> Option<String> {
        // Before the call, never after — the same rule and the same
        // reason as `apple.rs`: a denied byte here is one the platform
        // would consume as a delimiter, silently changing which host
        // comes back.
        if domain.bytes().any(crate::is_forbidden_domain_byte) {
            return None;
        }
        with_env(|env| {
            // `IDNA.getUTS46Instance(int)` takes the same option bits
            // ICU4C does, which is why `crate::icu::OPTIONS` is passed
            // straight through rather than translated.
            let options = i32::try_from(crate::icu::OPTIONS).ok()?;
            let idna = env
                .call_static_method(
                    "android/icu/text/IDNA",
                    "getUTS46Instance",
                    "(I)Landroid/icu/text/IDNA;",
                    &[JValue::Int(options)],
                )
                .ok()?
                .l()
                .ok()?;

            let src: JString<'_> = env.new_string(domain).ok()?;
            let dest = env.new_object("java/lang/StringBuilder", "()V", &[]).ok()?;
            let info = env
                .new_object("android/icu/text/IDNA$Info", "()V", &[])
                .ok()?;

            env.call_method(
                &idna,
                "nameToASCII",
                "(Ljava/lang/CharSequence;Ljava/lang/StringBuilder;Landroid/icu/text/IDNA$Info;)\
                 Ljava/lang/StringBuilder;",
                &[
                    JValue::Object(&JObject::from(src)),
                    JValue::Object(&dest),
                    JValue::Object(&info),
                ],
            )
            .ok()?;

            if errors_are_fatal(env, &info)? {
                return None;
            }

            let text = env
                .call_method(&dest, "toString", "()Ljava/lang/String;", &[])
                .ok()?
                .l()
                .ok()?;
            let out: String = env.get_string(&JString::from(text)).ok()?.into();

            // The same closing check `apple.rs` makes, for the same
            // reason: what comes back has to be a host, and a
            // conversion that did not happen shows up as a non-ASCII
            // answer.
            if !out.is_ascii() || out.bytes().any(crate::is_forbidden_domain_byte) {
                return None;
            }
            Some(out)
        })?
    }

    /// Walks `IDNA.Info.getErrors()` and asks [`super::is_fatal_by_name`].
    ///
    /// `None` for a JNI failure, which the caller turns into a refusal —
    /// an error set this crate could not read is not one it may forgive.
    fn errors_are_fatal(env: &mut jni::JNIEnv<'_>, info: &JObject<'_>) -> Option<bool> {
        let set = env
            .call_method(info, "getErrors", "()Ljava/util/Set;", &[])
            .ok()?
            .l()
            .ok()?;
        let iter = env
            .call_method(&set, "iterator", "()Ljava/util/Iterator;", &[])
            .ok()?
            .l()
            .ok()?;
        let mut names = Vec::new();
        while env
            .call_method(&iter, "hasNext", "()Z", &[])
            .ok()?
            .z()
            .ok()?
        {
            let item = env
                .call_method(&iter, "next", "()Ljava/lang/Object;", &[])
                .ok()?
                .l()
                .ok()?;
            let name = env
                .call_method(&item, "name", "()Ljava/lang/String;", &[])
                .ok()?
                .l()
                .ok()?;
            names.push(String::from(env.get_string(&JString::from(name)).ok()?));
        }
        Some(super::is_fatal_by_name(names.iter().map(String::as_str)))
    }

    /// Attaches to the process's JVM and runs `f` with an env.
    ///
    /// Every failure collapses to `None`, which is `hclient-proxy`'s
    /// `jvm.rs` decision and its reason: a missing context, a JVM that
    /// will not attach and a class that is not there all mean the same
    /// thing to the caller — *this backend cannot answer* — and four
    /// error paths would reach a caller whose whole answer is a host.
    #[allow(
        unsafe_code, // unsafe-code-exception: amendment-C19
        reason = "JavaVM::from_raw over the pointer the application registered with ndk_context"
    )]
    fn with_env<T>(f: impl FnOnce(&mut jni::JNIEnv<'_>) -> T) -> Option<T> {
        let ctx = ndk_context::android_context();
        if ctx.vm().is_null() {
            return None;
        }
        // SAFETY: the pointer is the one the application registered with
        // `ndk_context`, which is the `JavaVM` the Android runtime
        // created for this process. Null-checked above rather than
        // trusted.
        let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }.ok()?; // unsafe-code-exception: amendment-C19
        let mut env = vm.attach_current_thread().ok()?;
        Some(f(&mut env))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The two statements of one decision have to agree**, and this is
    /// the only part of the Android backend that can be checked without an
    /// Android. `IGNORED_ERRORS` is what the Windows backend masks out of
    /// ICU4C's bit word; `IGNORED` is what this file matches out of
    /// ICU4J's enum names. Adding a forgiven error to one and not the
    /// other is how the two platforms would start answering differently
    /// for the same host, which is the divergence this crate exists to
    /// prevent.
    #[test]
    fn the_ignored_names_are_the_ignored_bits() {
        let from_names = IGNORED.iter().fold(0, |acc, (_, bit)| acc | bit);
        assert_eq!(
            from_names,
            crate::icu::IGNORED_ERRORS,
            "the names this backend forgives and the bits the ICU backend forgives are the \
             same decision, and they have drifted"
        );
    }

    /// An error nobody listed is fatal, which is the direction that keeps
    /// a name unreachable rather than reachable when this crate meets an
    /// ICU error it has never heard of.
    #[test]
    fn an_unlisted_error_is_fatal_and_a_listed_one_is_not() {
        assert!(is_fatal_by_name(["DISALLOWED"].into_iter()));
        assert!(is_fatal_by_name(["EMPTY_LABEL", "PUNYCODE"].into_iter()));
        assert!(!is_fatal_by_name(["EMPTY_LABEL", "HYPHEN_3_4"].into_iter()));
        assert!(!is_fatal_by_name(std::iter::empty()));
    }
}
