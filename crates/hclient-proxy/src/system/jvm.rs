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

use jni::JavaVM;
use jni::objects::{JObject, JString, JValue};

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
    // for this process. Null-checked above rather than trusted.
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }.ok()?; // unsafe-code-exception: amendment-C19
    let mut env = vm.attach_current_thread().ok()?;

    let key: JString<'_> = env.new_string(name).ok()?;
    let value = env
        .call_static_method(
            "java/lang/System",
            "getProperty",
            "(Ljava/lang/String;)Ljava/lang/String;",
            &[JValue::Object(&JObject::from(key))],
        )
        .ok()?
        .l()
        .ok()?;
    if value.is_null() {
        // The property is not set, which is the ordinary answer on a
        // device with no proxy.
        return None;
    }
    let s: JString<'_> = value.into();
    env.get_string(&s).ok().map(Into::into)
}
