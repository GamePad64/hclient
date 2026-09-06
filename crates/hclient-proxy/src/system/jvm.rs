//! The four JNI calls that fetch a JVM system property, and nothing else.
//!
//! **This file holds no rules**, which is the split `read.rs`'s own header
//! describes: every decision about what a property *means* — which key is
//! which scheme, how `nonProxyHosts` splits, what a missing port implies —
//! is in [`super::read::from_jvm_properties`], which is pure and is tested
//! on this workspace's Linux hosts. What is here cannot run on them.
//!
//! # Why the JVM at all
//!
//! Android has no environment variables for a proxy and no registry. What
//! it has is `System.getProperty("http.proxyHost")` and its four
//! neighbours, which the framework fills in from the active network's
//! settings — and which `java.net`'s `DefaultProxySelector` reads to
//! decide where a request goes. Reading the same properties is what makes
//! this client agree with every other one in the process.
//!
//! # Where the `JavaVM` comes from, and what happens when it does not
//!
//! `ndk_context::android_context()`, which is the handle an Android
//! application registers once — `android_activity`, `ndk-glue` and
//! `winit` all do it, and so does any app that already carries
//! `rustls-platform-verifier`, since that needs the same thing to reach
//! the platform trust store.
//!
//! **An unregistered context is not an error, it is silence**: every
//! failure here answers `None`, which reaches
//! [`SystemProxies`](super::SystemProxies) as *the machine named no
//! proxy*. That is the honest reading — this crate cannot tell an app
//! that registered no context from a device with no proxy, and the
//! alternative would be an error on every non-Android-app use of a
//! library that is also linked into tests and command-line tools.

use jni::errors::Error;
use jni::objects::{JObject, JString, JValue};
use jni::{Env, JavaVM, jni_sig, jni_str};

/// One JVM system property, or `None` for anything that went wrong.
///
/// The collapse of every failure onto `None` is deliberate and is the
/// module doc's subject: a missing context, a JVM that will not attach, a
/// property that is not set and an exception all mean the same thing to
/// the caller — *nothing was read* — and distinguishing them would put
/// four error paths on a reader whose whole answer is a string.
#[allow(
    unsafe_code, // unsafe-code-exception: amendment-C19
    reason = "JavaVM::from_raw over the pointer the application registered with ndk_context"
)]
pub(super) fn system_property(name: &str) -> Option<String> {
    let ctx = ndk_context::android_context();
    if ctx.vm().is_null() {
        return None;
    }
    // SAFETY: the pointer is the one the application registered with
    // `ndk_context`, which is the `JavaVM` the Android runtime created
    // for this process.
    //
    // **The null check above is load-bearing rather than polite, and it
    // became so in jni 0.22.** `from_raw` used to answer a `Result` and
    // now answers `Self` over an internal `assert!(!ptr.is_null())` — so
    // what was a `None` on a bad pointer is a panic, and this check is
    // what keeps it unreachable.
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }; // unsafe-code-exception: amendment-C19

    // `attach_current_thread` takes a callback in 0.22 where it handed
    // back a guard, and the callback must answer a `Result` — so the
    // `Option`-per-step style below becomes `ok_or(Error::NullPtr(..))`
    // at the boundary and nowhere else. The outer `.ok()` puts it back
    // to the `None` this module's doc argues for: every failure here is
    // *nothing was read*, and a caller whose whole answer is a string
    // has no use for four error paths.
    vm.attach_current_thread(|env| read_property(env, name))
        .ok()
}

/// The four JNI calls, with the env the attach handed over.
///
/// Split out because 0.22's callback owns the `Env` for its own scope:
/// inlining this into the closure above would work and would put the
/// module's one interesting sequence inside a lambda, where the next
/// reader has to find it.
fn read_property(env: &mut Env<'_>, name: &str) -> Result<String, Error> {
    let key: JString<'_> = env.new_string(name)?;
    // **`jni_str!` rather than a plain literal, and it is 0.22's doing.**
    // A class or method name crosses this boundary as MUTF-8, and 0.21
    // converted a `&str` at every call; 0.22 asks for the encoded form
    // in the type, and the macro does it in a `const` — so the same
    // literal costs an allocation there and nothing here.
    let value = env
        .call_static_method(
            const { jni_str!("java/lang/System") },
            const { jni_str!("getProperty") },
            const { jni_sig!("(Ljava/lang/String;)Ljava/lang/String;") },
            &[JValue::Object(&JObject::from(key))],
        )?
        .l()?;
    if value.is_null() {
        // The property is not set, which is the ordinary answer on a
        // device with no proxy. An error rather than an `Ok(None)`
        // because the caller collapses both onto `None` anyway, and one
        // shape through the callback is fewer than two.
        return Err(Error::NullPtr("System.getProperty returned null"));
    }
    // **A checked cast where 0.21 took a bare `.into()`**, and the check
    // is the upgrade rather than ceremony: `cast_local` asks the runtime
    // whether the object really is a `java.lang.String` and answers
    // `Error::WrongObjectType` where it is not, in place of a conversion
    // that could not fail and could be wrong.
    let s: JString<'_> = env.cast_local::<JString<'_>>(value)?;
    s.try_to_string(env)
}
