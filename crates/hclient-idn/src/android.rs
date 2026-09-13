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
//! # It has been run, and the first run found it broken
//!
//! This file was written from Android's documentation and type-checked
//! for `aarch64-linux-android`, which proves the JNI signatures compile
//! and not that they resolve — the state `hclient-dns-system`'s Apple arm
//! was in when every one of its live tests failed in ten milliseconds.
//!
//! **It has since been executed on an emulator, API 35, and the first run
//! refused every name.** Every JNI step was correct: the VM was
//! registered, `android/icu/text/IDNA` resolved, `getUTS46Instance(0x3c)`
//! returned an instance, and `nameToASCII` answered `xn--strae-oqa.de`
//! for `straße.de`. What was wrong was one line of ours — see
//! [`imp::Answer`]: the shared walk had been factored out of the ASCII
//! direction and kept its closing check, *the answer must be ASCII*, so
//! `nameToUnicode` refused every conversion it performed correctly. The
//! acceptance probe's reverse half then failed, and the backend reported
//! `NoImplementation` for every name.
//!
//! After the fix, thirteen cases agree with `idna` on the device:
//! `straße.de` and `faß.de` non-transitionally, `EXAMPLE.COM`, `a..b`,
//! `ä..de`, `a-.de`, `ab--cd.de`, the refusals of `xn--zzzz.test` and
//! `a<b.com`, and all three reverse conversions.
//!
//! What limited the damage before that run is the split this workspace
//! applies to every platform module: the half that talks to the platform
//! holds no decisions, and the half that holds decisions is tested on any
//! host. Here the decision is [`IGNORED`] — which of ICU's error names
//! this crate treats as *not fatal* — and the device confirmed every one:
//! `a..b` came back `[EMPTY_LABEL]`, `a-.de` `[TRAILING_HYPHEN]`,
//! `ab--cd.de` `[HYPHEN_3_4]`, all forgiven and all matching `idna`, and
//! `xn--zzzz.test` `[PUNYCODE]`, not forgiven.
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
pub(crate) use imp::{Android, find, to_ascii};

/// The name every backend module exports, so that `lib.rs` can select one
/// with `cfg_select!` and then name no platform at all.
#[cfg(android_backend)]
pub(crate) type Handle = Android;

#[cfg(android_backend)]
mod imp {
    use jni::errors::Error;
    use jni::objects::{JObject, JString, JValue};
    use jni::{Env, JavaVM, jni_sig, jni_str};

    /// Nothing to carry: the class is part of the platform and the JVM
    /// handle comes from `ndk_context` at each call. The type exists so
    /// the backend has the same shape as the other two.
    #[derive(Debug)]
    pub(crate) struct Android;

    /// Why a conversion stopped — a JNI failure, or this crate refusing
    /// the answer.
    ///
    /// **It exists because jni 0.22 asks the callback for a `Result` and
    /// the honest `E` is not [`jni::errors::Error`].** That type is the
    /// JVM's vocabulary: `WrongObjectType`, `ClassNotFound`,
    /// `CaughtJavaException`. Three of the stops on this path are *ours*
    /// — the answer was not ASCII, it carried a byte
    /// [`crate::is_forbidden_domain_byte`] refuses, ICU4J reported an
    /// error name this crate treats as fatal — and pushing those through
    /// `Error::NullPtr` would file a decision of ours under a null
    /// pointer that never existed.
    ///
    /// Nothing reads the distinction today: [`with_env`] collapses both
    /// arms onto `None`, which is this backend's contract and the reason
    /// its doc gives. What the type buys is that the collapse happens in
    /// **one** place, visibly, rather than by every call site borrowing
    /// a JNI variant to mean something else.
    #[derive(Debug)]
    enum Stop {
        /// The JVM refused, or the call could not be made.
        Jni(Error),
        /// This crate refused the answer.
        Refused(&'static str),
    }

    impl From<Error> for Stop {
        fn from(e: Error) -> Self {
            Self::Jni(e)
        }
    }

    /// **The one reader, and it exists so that the payloads are not dead
    /// weight.** Without it both fields are `dead_code` — a `Debug`
    /// derive does not count as a use — and the honest choices then are
    /// to carry the data with an `#[allow]` over it or to drop it and
    /// keep a unit enum that says less than its name promises.
    ///
    /// This is what a `#[allow(dead_code)]` would have papered over, and
    /// it is two lines: the failure prints as itself, so anyone reaching
    /// for a `dbg!` or a log line on a device gets the JVM's own message
    /// or this crate's reason rather than a discriminant.
    impl std::fmt::Display for Stop {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Jni(e) => write!(f, "{e}"),
                Self::Refused(why) => f.write_str(why),
            }
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
            env.find_class(const { jni_str!("android/icu/text/IDNA") })?;
            Ok(Android)
        })
    }

    /// The A-label form of `domain`, or `None` if ICU4J refused it for a
    /// reason this crate treats as fatal.
    pub(crate) fn to_ascii(_a: &Android, domain: &str) -> Option<String> {
        through("nameToASCII", domain)
    }

    /// The conversion, with the method name a parameter.
    ///
    /// **It took two directions and takes one**, and the parameter that
    /// went with the second is worth a line: an `Answer` said whether the
    /// result had to be ASCII, because `nameToUnicode`'s does not. That
    /// distinction cost the backend its first run — `through` was
    /// factored out of the ASCII direction and kept its closing check, so
    /// `nameToUnicode` refused every conversion it performed correctly
    /// and the backend reported `NoImplementation` on a real device.
    /// With one direction the check is unconditional and the enum has no
    /// subject.
    ///
    /// `method` stays a parameter rather than being inlined, because it
    /// is what a second direction would need again and it costs nothing.
    fn through(method: &str, domain: &str) -> Option<String> {
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
            let options = i32::try_from(crate::icu::OPTIONS)
                .map_err(|_| Stop::Refused("OPTIONS does not fit an i32"))?;
            let idna = env
                .call_static_method(
                    const { jni_str!("android/icu/text/IDNA") },
                    const { jni_str!("getUTS46Instance") },
                    const { jni_sig!("(I)Landroid/icu/text/IDNA;") },
                    &[JValue::Int(options)],
                )?
                .l()?;

            let src: JString<'_> = env.new_string(domain)?;
            let dest = env.new_object(
                const { jni_str!("java/lang/StringBuilder") },
                const { jni_sig!("()V") },
                &[],
            )?;
            let info = env.new_object(
                const { jni_str!("android/icu/text/IDNA$Info") },
                const { jni_sig!("()V") },
                &[],
            )?;

            // **The method name is a runtime parameter and every other
            // name here is a literal**, which is what keeps `jni_str!`
            // from covering it: the macro encodes at compile time, and
            // `method` is `through`'s argument. `JNIString::from` is the
            // run-time half of the same conversion, and it is the one
            // allocation 0.22 asks for on this path.
            env.call_method(
                &idna,
                jni::strings::JNIString::from(method),
                const {
                    jni_sig!(
                        "(Ljava/lang/CharSequence;Ljava/lang/StringBuilder;\
                         Landroid/icu/text/IDNA$Info;)Ljava/lang/StringBuilder;"
                    )
                },
                &[
                    JValue::Object(&JObject::from(src)),
                    JValue::Object(&dest),
                    JValue::Object(&info),
                ],
            )?;

            if errors_are_fatal(env, &info)? {
                return Err(Stop::Refused(
                    "IDNA reported an error this crate treats as fatal",
                ));
            }

            let text = env
                .call_method(
                    &dest,
                    const { jni_str!("toString") },
                    const { jni_sig!("()Ljava/lang/String;") },
                    &[],
                )?
                .l()?;
            let text: JString<'_> = env.cast_local::<JString<'_>>(text)?;
            let out: String = text.try_to_string(env)?;

            if !out.is_ascii() {
                return Err(Stop::Refused("the answer was not ASCII"));
            }
            if out.bytes().any(crate::is_forbidden_domain_byte) {
                return Err(Stop::Refused("the answer carried a forbidden byte"));
            }
            Ok(out)
        })
    }

    /// Walks `IDNA.Info.getErrors()` and asks [`super::is_fatal_by_name`].
    ///
    /// `None` for a JNI failure, which the caller turns into a refusal —
    /// an error set this crate could not read is not one it may forgive.
    fn errors_are_fatal(env: &mut Env<'_>, info: &JObject<'_>) -> Result<bool, Error> {
        let set = env
            .call_method(
                info,
                const { jni_str!("getErrors") },
                const { jni_sig!("()Ljava/util/Set;") },
                &[],
            )?
            .l()?;
        let iter = env
            .call_method(
                &set,
                const { jni_str!("iterator") },
                const { jni_sig!("()Ljava/util/Iterator;") },
                &[],
            )?
            .l()?;
        let mut names = Vec::new();
        while env
            .call_method(
                &iter,
                const { jni_str!("hasNext") },
                const { jni_sig!("()Z") },
                &[],
            )?
            .z()?
        {
            let item = env
                .call_method(
                    &iter,
                    const { jni_str!("next") },
                    const { jni_sig!("()Ljava/lang/Object;") },
                    &[],
                )?
                .l()?;
            let name = env
                .call_method(
                    &item,
                    const { jni_str!("name") },
                    const { jni_sig!("()Ljava/lang/String;") },
                    &[],
                )?
                .l()?;
            let name: JString<'_> = env.cast_local::<JString<'_>>(name)?;
            names.push(name.try_to_string(env)?);
        }
        Ok(super::is_fatal_by_name(names.iter().map(String::as_str)))
    }

    /// Attaches to the process's JVM and runs `f` with an env.
    ///
    /// Every failure collapses to `None`, which is `hclient-proxy`'s
    /// `jvm.rs` decision and its reason: a missing context, a JVM that
    /// will not attach and a class that is not there all mean the same
    /// thing to the caller — *this backend cannot answer* — and four
    /// error paths would reach a caller whose whole answer is a host.
    #[allow(
        unsafe_code, // unsafe-code-exception: amendment-C19,
        reason = "JavaVM::from_raw over the pointer the application registered with ndk_context"
    )]
    fn with_env<T>(f: impl FnOnce(&mut Env<'_>) -> Result<T, Stop>) -> Option<T> {
        let ctx = ndk_context::android_context();
        if ctx.vm().is_null() {
            return None;
        }
        // SAFETY: the pointer is the one the application registered with
        // `ndk_context`, which is the `JavaVM` the Android runtime
        // created for this process.
        //
        // **The null check above is load-bearing rather than polite, and
        // it became so in jni 0.22.** `from_raw` used to answer a
        // `Result` and now answers `Self` over an internal
        // `assert!(!ptr.is_null())` — so what was a `None` on a bad
        // pointer is a panic, and this check is what keeps it
        // unreachable.
        let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }; // unsafe-code-exception: amendment-C19

        // **The callback shape is 0.22's, and it is why every body below
        // answers `Result` where it used to answer `Option`.**
        // `attach_current_thread` handed back a guard and now takes a
        // closure returning `Result<T, E>` — which is a real improvement
        // rather than churn: the attachment's scope is the callback, so
        // there is no guard whose lifetime a caller could get wrong. The
        // `.ok()` here is what puts the two back together, and the
        // collapse of every failure onto `None` is unchanged and is this
        // function's own doc.
        vm.attach_current_thread(f).ok()
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
