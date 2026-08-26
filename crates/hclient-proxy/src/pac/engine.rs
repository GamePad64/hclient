//! The JavaScript environment a PAC file expects, over `boa_engine`.
//!
//! # Why the environment travels in a thread-local
//!
//! Boa's safe constructor for a native function is
//! `NativeFunction::from_copy_closure`, which demands `Copy + 'static` —
//! and every function here needs the caller's resolver and clock, which
//! are neither. The alternatives were `from_closure`, whose safety
//! comment is about the garbage collector tracing captured values, and
//! carrying the environment through Boa's own capture machinery, which
//! wants `Trace`.
//!
//! So the functions are plain `fn` pointers and the environment is set
//! for the duration of one call. It is a **thread-local**, and that is
//! also what removes every auto-trait bound from the seam: nothing here
//! crosses a thread, so the resolver a caller supplies is an ordinary
//! `Rc<dyn Fn>` with no `Send`, and a caller holding an `Rc` is not shut
//! out — which is the property this workspace protects everywhere else.
//!
//! # The clock is UTC, and that is a stated limitation
//!
//! `weekdayRange`, `dateRange` and `timeRange` are specified in **local**
//! time unless the script passes `"GMT"` as the last argument. Local time
//! needs a timezone database; this crate is clockless and carries none,
//! so all three read the caller's [`super::PacEnv::with_now`] as UTC. A script
//! that passes `"GMT"` gets exactly the specified behaviour, and one that
//! does not gets UTC where it expected local — which is written here, and
//! is why `now` defaults to `None` and the three answer `false`.

use std::cell::RefCell;
use std::net::IpAddr;

use boa_engine::{Context, JsResult, JsValue, NativeFunction, Source, js_string};

use super::{PacEnv, PacError};

thread_local! {
    /// The environment of the call in progress.
    ///
    /// Set by [`with_env`] around one evaluation and cleared after it, so
    /// a function reached outside a call — which cannot happen, since the
    /// context is built and dropped inside one — sees `None` and answers
    /// the ignorant answer rather than a stale one.
    static ENV: RefCell<Option<PacEnv>> = const { RefCell::new(None) };
}

/// Run `f` with `env` installed.
///
/// The previous value is restored rather than cleared, which costs
/// nothing and means a nested evaluation — a caller running one PAC file
/// from inside another's resolver, say — cannot silently take the inner
/// environment for the outer one.
fn with_env<T>(env: &PacEnv, f: impl FnOnce() -> T) -> T {
    let previous = ENV.with(|e| e.borrow_mut().replace(env.clone()));
    let out = f();
    ENV.with(|e| *e.borrow_mut() = previous);
    out
}

fn env<T>(f: impl FnOnce(&PacEnv) -> T, absent: T) -> T {
    ENV.with(|e| match &*e.borrow() {
        Some(env) => f(env),
        None => absent,
    })
}

/// Build a context with the PAC environment in it, evaluate the script's
/// top level, and hand the context to `f`.
pub(super) fn with_context<T>(
    source: &str,
    pac_env: &PacEnv,
    f: impl FnOnce(&mut Context) -> Result<T, PacError>,
) -> Result<T, PacError> {
    with_env(pac_env, || {
        let mut ctx = Context::default();
        install(&mut ctx).map_err(|e| PacError::Compile(e.to_string().into()))?;
        ctx.eval(Source::from_bytes(source))
            .map_err(|e| PacError::Compile(e.to_string().into()))?;
        f(&mut ctx)
    })
}

/// The script's entry point, checked to be callable.
pub(super) fn entry_point(ctx: &mut Context) -> Result<boa_engine::JsObject, PacError> {
    let f = ctx
        .global_object()
        .get(js_string!("FindProxyForURL"), ctx)
        .map_err(|e| PacError::Eval(e.to_string().into()))?;
    match f {
        JsValue::Object(o) if o.is_callable() => Ok(o),
        _ => Err(PacError::NoEntryPoint),
    }
}

/// `FindProxyForURL(url, host)`, as a string.
pub(super) fn call(ctx: &mut Context, url: &str, host: &str) -> Result<String, PacError> {
    let f = entry_point(ctx)?;
    let out = f
        .call(
            &JsValue::undefined(),
            &[
                JsValue::from(js_string!(url)),
                JsValue::from(js_string!(host)),
            ],
            ctx,
        )
        .map_err(|e| PacError::Eval(e.to_string().into()))?;
    out.to_string(ctx)
        .map(|s| s.to_std_string_escaped())
        .map_err(|e| PacError::Eval(e.to_string().into()))
}

/// The twelve functions of the PAC environment.
fn install(ctx: &mut Context) -> JsResult<()> {
    let mut add =
        |name: &str,
         arity: usize,
         f: fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>| {
            ctx.register_global_callable(js_string!(name), arity, NativeFunction::from_fn_ptr(f))
        };
    add("isPlainHostName", 1, is_plain_host_name)?;
    add("dnsDomainIs", 2, dns_domain_is)?;
    add("localHostOrDomainIs", 2, local_host_or_domain_is)?;
    add("isResolvable", 1, is_resolvable)?;
    add("isInNet", 3, is_in_net)?;
    add("dnsResolve", 1, dns_resolve)?;
    add("myIpAddress", 0, my_ip_address)?;
    add("dnsDomainLevels", 1, dns_domain_levels)?;
    add("shExpMatch", 2, sh_exp_match)?;
    add("weekdayRange", 2, weekday_range)?;
    add("dateRange", 1, date_range)?;
    add("timeRange", 2, time_range)?;
    Ok(())
}

/// One argument as a string, or `""`.
///
/// A PAC file calling a helper with the wrong type is a broken script,
/// and every implementation answers rather than throwing — a throw would
/// turn a bad branch into a failed request.
fn arg(args: &[JsValue], i: usize, ctx: &mut Context) -> String {
    args.get(i)
        .and_then(|v| v.to_string(ctx).ok())
        .map(|s| s.to_std_string_escaped())
        .unwrap_or_default()
}

// --- the name functions -------------------------------------------------

fn is_plain_host_name(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::from(!arg(args, 0, ctx).contains('.')))
}

fn dns_domain_is(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let host = arg(args, 0, ctx).to_ascii_lowercase();
    let domain = arg(args, 1, ctx).to_ascii_lowercase();
    Ok(JsValue::from(host.ends_with(&domain)))
}

/// `localHostOrDomainIs(host, hostdom)` — the host is the whole name, or
/// it is the plain-name prefix of it.
///
/// The second half is what the function exists for: `www` matches
/// `www.example.com`, so a script can name intranet hosts by their short
/// name and their long one at once.
fn local_host_or_domain_is(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let host = arg(args, 0, ctx).to_ascii_lowercase();
    let hostdom = arg(args, 1, ctx).to_ascii_lowercase();
    let plain = !host.contains('.');
    Ok(JsValue::from(
        host == hostdom || (plain && hostdom.starts_with(&format!("{host}."))),
    ))
}

fn dns_domain_levels(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::from(arg(args, 0, ctx).matches('.').count() as u32))
}

/// `shExpMatch(str, shexp)` — a **shell glob**, not a regular expression.
///
/// `*` and `?`, and every other character means itself: a `.` is a dot
/// and a `+` is a plus. Implemented rather than translated into a regex
/// because the translation is where implementations get it wrong — an
/// unescaped `.` in the pattern quietly becomes *any character*, and a
/// script excluding `10.0.0.1` starts excluding `10x0y0z1` too.
fn sh_exp_match(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let s = arg(args, 0, ctx);
    let pattern = arg(args, 1, ctx);
    Ok(JsValue::from(glob(pattern.as_bytes(), s.as_bytes())))
}

/// Wildcard matching with backtracking, iterative so a pathological
/// pattern cannot exhaust the stack.
fn glob(pattern: &[u8], s: &[u8]) -> bool {
    let (mut p, mut i) = (0, 0);
    let (mut star, mut resume) = (None, 0);
    while i < s.len() {
        match pattern.get(p) {
            Some(b'*') => {
                star = Some(p);
                resume = i;
                p += 1;
            }
            Some(b'?') => {
                p += 1;
                i += 1;
            }
            Some(c) if *c == s[i] => {
                p += 1;
                i += 1;
            }
            _ => match star {
                // Backtrack: the last `*` swallows one more character.
                Some(at) => {
                    p = at + 1;
                    resume += 1;
                    i = resume;
                }
                None => return false,
            },
        }
    }
    // Trailing `*`s match the empty rest.
    while pattern.get(p) == Some(&b'*') {
        p += 1;
    }
    p == pattern.len()
}

// --- the functions that ask the world -----------------------------------

fn dns_resolve(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let host = arg(args, 0, ctx);
    Ok(match resolve(&host) {
        Some(ip) => JsValue::from(js_string!(ip.to_string())),
        // `null`, which is what an unresolvable name looks like to a PAC
        // file — and the honest answer from a client given no resolver.
        None => JsValue::null(),
    })
}

fn is_resolvable(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let host = arg(args, 0, ctx);
    Ok(JsValue::from(resolve(&host).is_some()))
}

fn my_ip_address(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let ip = env(|e| e.my_ip, IpAddr::from([127, 0, 0, 1]));
    Ok(JsValue::from(js_string!(ip.to_string())))
}

/// An address literal is itself; anything else goes to the resolver.
///
/// Both halves matter: `isInNet` is called with a host name *and* with
/// the output of `dnsResolve`, and a version that always resolved would
/// ask the resolver about `10.0.0.1`.
fn resolve(host: &str) -> Option<IpAddr> {
    if let Ok(ip) = host.trim_start_matches('[').trim_end_matches(']').parse() {
        return Some(ip);
    }
    env(|e| (e.resolve)(host), None)
}

/// `isInNet(host, pattern, mask)` — v4, under a dotted mask.
///
/// v6 is **false rather than an error**: the function's third argument is
/// a dotted-quad mask, so the specification has no v6 form, and a script
/// that reaches this with a v6 address wants the other branch rather than
/// a failure.
fn is_in_net(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let host = arg(args, 0, ctx);
    let pattern = arg(args, 1, ctx);
    let mask = arg(args, 2, ctx);
    let (Some(IpAddr::V4(host)), Ok(IpAddr::V4(pattern)), Ok(IpAddr::V4(mask))) =
        (resolve(&host), pattern.parse(), mask.parse())
    else {
        return Ok(JsValue::from(false));
    };
    let (h, p, m) = (host.octets(), pattern.octets(), mask.octets());
    Ok(JsValue::from((0..4).all(|i| h[i] & m[i] == p[i] & m[i])))
}

// --- the calendar functions ---------------------------------------------

/// Seconds since the epoch, or `None` where the caller gave no clock.
fn now_secs() -> Option<i64> {
    let now = env(|e| e.now, None)?;
    now.duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64)
}

/// `weekdayRange(from [, to])`, in UTC — see this module's doc.
fn weekday_range(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    const DAYS: [&str; 7] = ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"];
    let Some(secs) = now_secs() else {
        return Ok(JsValue::from(false));
    };
    // 1970-01-01 was a Thursday, which is index 4.
    let today = ((secs.div_euclid(86_400) + 4).rem_euclid(7)) as usize;
    let index = |s: &str| DAYS.iter().position(|d| d.eq_ignore_ascii_case(s));

    let from = arg(args, 0, ctx);
    let Some(from) = index(&from) else {
        return Ok(JsValue::from(false));
    };
    let to = arg(args, 1, ctx);
    let Some(to) = index(&to) else {
        // One argument: that day alone. The second may also be "GMT",
        // which names no day and lands here — correctly, since this
        // implementation is UTC either way.
        return Ok(JsValue::from(today == from));
    };
    Ok(JsValue::from(if from <= to {
        (from..=to).contains(&today)
    } else {
        // A range that wraps the week, `FRI` to `MON`.
        today >= from || today <= to
    }))
}

/// `timeRange(h1, h2)` — the two-argument hour form, in UTC.
///
/// The specification also has four- and six-argument forms with minutes
/// and seconds. They answer `false` here rather than being guessed at,
/// which is this module's rule everywhere: an implementation that read
/// `timeRange(8, 30, 17, 0)` as hours would proxy nothing between 8 and
/// 17 on a script that meant 08:30 to 17:00.
fn time_range(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let Some(secs) = now_secs() else {
        return Ok(JsValue::from(false));
    };
    let hour = secs.rem_euclid(86_400) / 3600;
    let number = |i: usize, args: &[JsValue], ctx: &mut Context| -> Option<i64> {
        arg(args, i, ctx).trim().parse::<i64>().ok()
    };
    let named = args.len() > 2 && number(2, args, ctx).is_some();
    if named {
        return Ok(JsValue::from(false));
    }
    let (Some(from), Some(to)) = (number(0, args, ctx), number(1, args, ctx)) else {
        // One argument: that hour alone.
        return Ok(JsValue::from(
            number(0, args, ctx).is_some_and(|h| h == hour),
        ));
    };
    Ok(JsValue::from(if from <= to {
        (from..=to).contains(&hour)
    } else {
        hour >= from || hour <= to
    }))
}

/// `dateRange` — **not implemented, and it answers `false`**.
///
/// Its argument list is the most overloaded in the environment: one to
/// six arguments, where a number is a day *or* a year depending on its
/// magnitude and on what sits beside it, and a string is a month. Reading
/// one form as another is the failure mode, and it is silent — a script
/// meaning *the first of January* would be answered for *the year 1*. So
/// this is the one place the module declines rather than approximates,
/// and a caller who needs it has `PacEnv::now` and can pre-evaluate the
/// branch themselves.
fn date_range(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::from(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_glob_is_a_shell_glob_and_not_a_regex() {
        assert!(glob(b"*.example.com", b"a.example.com"));
        assert!(glob(b"*.example.com", b".example.com"));
        assert!(!glob(b"*.example.com", b"example.com"));
        // A `.` means a dot. The regex translation this avoids would
        // match `aXexample1com` here.
        assert!(!glob(b"*.example.com", b"aXexample1com"));
        assert!(glob(b"ho?t", b"host"));
        assert!(!glob(b"ho?t", b"hoost"));
        assert!(glob(b"*", b""));
        assert!(glob(b"", b""));
        assert!(!glob(b"", b"x"));
        // Backtracking: the first `*` must give characters back.
        assert!(glob(b"*a*b", b"xaybzab"));
        assert!(!glob(b"*a*b", b"xayb z"));
        // Every other character is literal, including regex punctuation.
        assert!(glob(b"a+b", b"a+b"));
        assert!(!glob(b"a+b", b"aab"));
    }
}
