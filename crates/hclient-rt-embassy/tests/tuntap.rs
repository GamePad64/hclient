//! The acceptance: `hclient::Client` → `Native` → hyper →
//! `hclient-rt-embassy` → embassy-net, over a real TAP link, inside
//! `embassy_executor::Executor`. It **runs**, it does not merely compile.
//!
//! # What is real here and what is not
//!
//! Real: the TCP state machine is smoltcp's, driven by `embassy-net`, with
//! no `std::net` socket anywhere on the client path — the request is
//! assembled by hyper, handed to `EmbassyIo`, turned into segments by
//! smoltcp and written to `/dev/net/tun` as Ethernet frames. The far end is
//! an ordinary blocking `std::net::TcpListener` on the kernel side of the
//! tap, so every assertion below is made by a normal Linux socket about
//! what it actually saw.
//!
//! Not real, and said plainly: the readiness of the tap file descriptor
//! comes from `async-io`'s reactor thread inside `embassy-net-tuntap`,
//! because on Linux something has to poll a file descriptor. On a device
//! that is an interrupt. And nothing here has run on an MCU — the W7
//! research measured `riscv32imc-esp-espidf` as `cargo check` only, and
//! xtensa as not buildable on upstream rustc at all.
//!
//! # Why it re-executes itself
//!
//! Creating a TAP device needs `CAP_NET_ADMIN`, which an unprivileged user
//! has inside a user+network namespace of their own. So the outer half of
//! each test re-runs *this same test binary* under `unshare -Ur --net`,
//! with [`SCENARIO`] set, and asserts on the child's exit status; the inner
//! half sees the variable, builds the tap and the stack, and runs the
//! scenario under embassy's executor. `Executor::run` never returns, so the
//! inner process ends with an explicit `exit(0)` after its assertions, and
//! a failed assertion inside a task unwinds out of `run` and becomes a
//! non-zero status the outer half reports.
//!
//! # Skipping, and the marker that makes skipping impossible
//!
//! Unprivileged user namespaces are disabled on some hosts (Ubuntu's
//! `kernel.apparmor_restrict_unprivileged_userns`, Docker without
//! `--privileged`). There the outer half prints a `NOTICE` and returns —
//! **unless `HCLIENT_REQUIRE_TUNTAP` is set**, in which case it panics.
//! That is the same shape as `hclient-wasi`'s `require_wasmtime`, and for
//! the same reason: a laptop that cannot run this should say so, and a CI
//! job that promised to run it must not go green having checked nothing.
//! The job does not exist yet (`.github/workflows/ci.yml` is out of scope
//! for W7), which is written down in the W7 report rather than left to be
//! discovered.
//!
//! # Seeing what the stack did
//!
//! `HCLIENT_EMBASSY_TRACE=1` installs a logger and turns on embassy-net's
//! and smoltcp's own tracing in the inner process, down to every packet
//! decision. That is how `sockets.rs`'s `Inner::reclaim_finished` was
//! diagnosed, and the quickest way to diagnose the next one.
#![cfg(target_os = "linux")]

use embassy_executor::Executor;
use embassy_net::tcp::TcpSocket;
use embassy_net::{Config, Ipv4Address, Ipv4Cidr, Stack, StackResources, StaticConfigV4};
use embassy_net_tuntap::TunTapDevice;
use embassy_time::Timer;
use hclient_core::Timeouts;
use hclient_dns::IpLiteralOnly;
use hclient_native::Native;
use hclient_rt::{TcpConnect, TcpOpts};
use hclient_rt_embassy::{Embassy, SocketPool};
use hclient_tls::NoTls;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::sync::mpsc;
use std::time::Duration;

/// Set on the child process to say which scenario it is: its presence is
/// also how the inner half knows it is the inner half.
const SCENARIO: &str = "HCLIENT_EMBASSY_SCENARIO";
/// Set by a CI job that has promised this test can run; turns a skip into
/// a failure.
const REQUIRE_MARKER: &str = "HCLIENT_REQUIRE_TUNTAP";

/// The client is smoltcp's, at `.2`; the server is the kernel's, at `.1`.
const TAP: &str = "tap0";
const CLIENT_IP: [u8; 4] = [192, 168, 69, 2];
const SERVER_IP: &str = "192.168.69.1";
const PREFIX: u8 = 24;

/// Ceiling for the whole inner process. A regression that stops the stack
/// moving must be a failed test with a diagnosis, not a stuck CI job —
/// the same watchdog rule as `two_runtimes.rs` and `tests/h1.rs`.
const BOUND: Duration = Duration::from_secs(30);

/// How long the server waits for the client end to go away before
/// concluding it is still there. Identical in role, and in value, to
/// `hclient-native/tests/cancel.rs`'s constant of the same name: both
/// verdicts come out of the same `read`, so the two cases cannot disagree
/// about what "long enough" means.
const OBSERVATION_WINDOW: Duration = Duration::from_millis(100);

const BODY: &str = "hello from the tap";

// ---------------------------------------------------------------- tests

#[test]
fn a_request_goes_out_over_embassy_net_and_the_response_comes_back() {
    scenario("request");
}

/// Six requests through a two-slot pool, with `Native`'s own connection
/// reuse turned off so every one of them has to take a socket out of the
/// pool and give it back. A backend that leaked a slot would hang on the
/// third; a backend that reclaimed a slot before its FIN was on the wire
/// would be caught by the cancellation pair below, not here.
#[test]
fn more_requests_than_pool_slots_reuse_the_slots_instead_of_running_out() {
    scenario("slots");
}

/// `Timer::sleep` on this runtime, exercised where `Native` actually uses
/// it: `with_connect_timeout`, racing a connect that can never finish.
///
/// Two things at once, and neither is checkable on the host. First, that
/// `embassy_time` works at all in that position — embassy's integrated
/// timer queue panics on a waker it did not make, and the reclaim path
/// inside `TcpConnect::connect` is polled through exactly such a waker
/// (see `sockets.rs`'s `CLOSING_TIMEOUT`). Second, that a connect
/// abandoned mid-flight releases its pool slot: the request after the
/// timeout has to get a socket, and it does.
#[test]
fn connect_timeout_is_enforced_over_this_runtimes_clock() {
    scenario("connect-timeout");
}

/// The seam's one obligation, checked where it is owed: **inside
/// `connect`**, not next to it.
///
/// `src/lib.rs`'s unit tests call `TcpOpts::reject_unsupported` directly,
/// which proves the check computes the right answer and nothing about
/// whether `connect` ever calls it. Deleting the
/// `opts.reject_unsupported(Self::APPLIES)?` line from `Embassy::connect`
/// — a backend that silently ignores every option it cannot apply, the one
/// answer `TcpConnect::connect`'s doc says is unavailable — passed the
/// whole crate suite, 14/14 (W7 mutation M6). This scenario is what makes
/// that mutation fail.
#[test]
fn options_this_runtime_cannot_apply_are_refused_by_connect_itself() {
    scenario("refuse-opts");
}

/// The W1 contract, observed from the far end: dropping the `execute`
/// future closes the connection the server can see.
#[test]
fn dropping_the_execute_future_closes_the_connection_the_server_sees() {
    scenario("cancel");
}

/// The control for the test above: the same request against the same
/// server for the same window, with the future kept alive, must leave the
/// connection open — otherwise "closed" could be the passage of time, a
/// timeout of our own, or a bug that drops sockets early.
#[test]
fn holding_the_execute_future_leaves_the_connection_open() {
    scenario("cancel-control");
}

/// The closing list's **wait**, exercised in the only state where it does
/// anything: a slot reclaimed while its FIN is still merely queued.
///
/// The pair above proves the socket is kept alive past the drop. Neither
/// of them reaches `sockets::finish_closing` at all — measured, with an
/// `eprintln!` at the top of that function: **zero** calls across all
/// seven scenarios, because every one of them lets the stack run (an
/// `await` on the server's verdict, a `Timer`) between releasing a socket
/// and asking for the next, by which time `Inner::reclaim_finished` finds
/// it already `Closed` and the closing list is never popped. So deleting
/// `finish_closing`'s `sock.flush().await` — which leaves it aborting a
/// FIN that has not been transmitted, exactly the failure the closing list
/// exists to prevent — passed the whole crate suite, 15/15 (W7 mutation
/// M7).
///
/// This scenario is what makes that mutation fail. `N = 1`, so the second
/// request cannot get a slot of its own; and the release and the next
/// `acquire` happen in **one executor turn**, with nothing in between for
/// the stack task to run in, so the socket really is still closing when
/// `finish_closing` gets it.
#[test]
fn a_slot_reclaimed_while_still_closing_waits_for_its_fin() {
    scenario("reclaim");
}

/// The observer's third verdict, and the only scenario that produces one.
///
/// `Eof` and `Reset` are worth keeping apart only if something can tell
/// them apart. Every other scenario here ends in a FIN, so folding the
/// `ConnectionReset` arm of [`observe`] into `Eof` — an instrument that
/// accepts an RST wherever it is owed an orderly close — passed the whole
/// crate suite, 16/16 (W7 mutation M16). This is the case that puts a real
/// RST on the wire, and it is also case 2 of the research's `cancel2`
/// spike, which until now lived only in an untracked directory:
/// `abort()` with the stack given the one poll `TcpSocket::drop` denies
/// it.
#[test]
fn an_abort_with_a_stack_poll_is_seen_as_a_reset() {
    scenario("abort");
}

/// The measurement the whole design rests on: embassy's own teardown —
/// `close()` immediately followed by dropping the `TcpSocket`, which is
/// exactly what `embassy_net::tcp::client::TcpConnection::drop` does — is
/// invisible to the server, because `TcpSocket::drop` removes the socket
/// from smoltcp before the stack can turn the queued FIN into a packet.
///
/// If this test ever fails, embassy has fixed that, and
/// `hclient-rt-embassy`'s closing list can be deleted. That is a good
/// failure and the comment is here to make it actionable rather than
/// puzzling.
#[test]
fn embassys_own_socket_teardown_is_invisible_to_the_server() {
    scenario("naive");
}

// ------------------------------------------------------- outer / inner

fn scenario(name: &str) {
    match std::env::var(SCENARIO) {
        Ok(v) if v == name => inner(name),
        // A child running a *different* scenario cannot happen: the outer
        // half always names one test.
        Ok(v) => panic!("child asked for scenario {v} while running {name}"),
        Err(_) => outer(name),
    }
}

/// Re-run this binary's own test for `name` inside a fresh user+network
/// namespace.
fn outer(name: &str) {
    if !namespaces_available() {
        if std::env::var_os(REQUIRE_MARKER).is_some() {
            panic!(
                "`unshare -Ur --net` does not work here even though {REQUIRE_MARKER} is set: the \
                 environment is broken, not deliberately limited."
            );
        }
        eprintln!(
            "NOTICE: unprivileged user+network namespaces are unavailable — skipping the live \
             embassy-net run `{name}`. Nothing about the embassy backend was checked by this \
             test in this environment."
        );
        return;
    }
    let exe = std::env::current_exe().expect("current_exe");
    let out = std::process::Command::new("unshare")
        .args(["-Ur", "--net", "--"])
        .arg(&exe)
        .args(["--exact", "--nocapture", "--test-threads", "1"])
        .arg(test_fn_name(name))
        .env(SCENARIO, name)
        .output()
        .expect("running the scenario under unshare");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "scenario `{name}` failed inside the namespace ({}):\n--- stdout ---\n{stdout}\n--- \
         stderr ---\n{stderr}",
        out.status
    );
    // The inner process prints what it measured; without this the outer
    // test would be a green light with nothing behind it.
    print!("{stdout}");
    eprint!("{stderr}");
}

/// libtest selects by the test function's path, and the scenario name is
/// not it. Kept as one table so a renamed test fails here loudly instead
/// of running nothing and passing.
fn test_fn_name(scenario: &str) -> &'static str {
    match scenario {
        "request" => "a_request_goes_out_over_embassy_net_and_the_response_comes_back",
        "slots" => "more_requests_than_pool_slots_reuse_the_slots_instead_of_running_out",
        "connect-timeout" => "connect_timeout_is_enforced_over_this_runtimes_clock",
        "refuse-opts" => "options_this_runtime_cannot_apply_are_refused_by_connect_itself",
        "cancel" => "dropping_the_execute_future_closes_the_connection_the_server_sees",
        "cancel-control" => "holding_the_execute_future_leaves_the_connection_open",
        "reclaim" => "a_slot_reclaimed_while_still_closing_waits_for_its_fin",
        "abort" => "an_abort_with_a_stack_poll_is_seen_as_a_reset",
        "naive" => "embassys_own_socket_teardown_is_invisible_to_the_server",
        other => panic!("unknown scenario {other}"),
    }
}

fn namespaces_available() -> bool {
    std::process::Command::new("unshare")
        .args(["-Ur", "--net", "--", "true"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// The inner half: build the link, build the stack, run the scenario under
/// embassy's own executor. Never returns.
fn inner(name: &str) -> ! {
    if std::env::var_os("HCLIENT_EMBASSY_TRACE").is_some() {
        struct L;
        impl log::Log for L {
            fn enabled(&self, _: &log::Metadata<'_>) -> bool {
                true
            }
            fn log(&self, r: &log::Record<'_>) {
                eprintln!("[{}] {}", r.target(), r.args());
            }
            fn flush(&self) {}
        }
        log::set_boxed_logger(Box::new(L)).ok();
        log::set_max_level(log::LevelFilter::Trace);
    }
    watchdog();
    setup_tap();

    let device = TunTapDevice::new(TAP).expect("opening /dev/net/tun for tap0");
    // `Box::leak` once, for the life of the process — the stack's resources
    // are exactly what a device would put in a `static`.
    let resources: &'static mut StackResources<4> = Box::leak(Box::new(StackResources::new()));
    let config = Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(Ipv4Address::from(CLIENT_IP), PREFIX),
        gateway: None,
        dns_servers: heapless::Vec::new(),
    });
    let (stack, runner) = embassy_net::new(device, config, resources, 0x0123_4567_89ab_cdef);
    let executor: &'static mut Executor = Box::leak(Box::new(Executor::new()));
    executor.run(|spawner| {
        // `embassy-executor` 0.10 moved the fallible half: the `#[task]`
        // macro's function now returns `Result<SpawnToken<_>, _>` — the
        // pool being exhausted is known when the token is made — and
        // `Spawner::spawn` returns `()`. So the `expect` moves one call to
        // the left rather than disappearing.
        spawner.spawn(net_task(runner).expect("a stack task token"));
        spawner.spawn(scenario_task(stack, name.to_owned()).expect("a scenario task token"));
    })
}

#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static, TunTapDevice>) -> ! {
    runner.run().await
}

#[embassy_executor::task]
async fn scenario_task(stack: Stack<'static>, name: String) {
    stack.wait_config_up().await;
    println!("stack up: {:?}", stack.config_v4().map(|c| c.address));
    match name.as_str() {
        "request" => request(stack).await,
        "slots" => slots(stack).await,
        "connect-timeout" => connect_timeout(stack).await,
        "refuse-opts" => refuse_opts(stack).await,
        "cancel" => cancel(stack, true).await,
        "cancel-control" => cancel(stack, false).await,
        "reclaim" => reclaim(stack).await,
        "abort" => abort(stack).await,
        "naive" => naive(stack).await,
        other => panic!("unknown scenario {other}"),
    }
    println!("OK");
    // `Executor::run` never returns, so this is how the inner process
    // reports success.
    std::process::exit(0);
}

/// Turns a hang into a failure with a diagnosis. A thread, not a timer
/// task: the failure mode being guarded against is precisely "the executor
/// stopped making progress".
fn watchdog() {
    std::thread::spawn(|| {
        std::thread::sleep(BOUND);
        eprintln!(
            "hclient-rt-embassy: the scenario did not finish within {BOUND:?} — the stack or the \
             socket pool stopped making progress. Failing instead of hanging."
        );
        std::process::exit(2);
    });
}

fn setup_tap() {
    for args in [
        vec!["tuntap", "add", "dev", TAP, "mode", "tap"],
        vec!["addr", "add", &format!("{SERVER_IP}/{PREFIX}"), "dev", TAP],
        vec!["link", "set", TAP, "up"],
    ] {
        let out = std::process::Command::new("ip")
            .args(&args)
            .output()
            .unwrap_or_else(|e| panic!("running `ip {}`: {e}", args.join(" ")));
        assert!(
            out.status.success(),
            "`ip {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

// ------------------------------------------------------------ scenarios

async fn request(stack: Stack<'static>) {
    // The verdict is not read here: this client keeps its connection in
    // `Native`'s pool, so the far end correctly sees it stay open.
    let (addr, _ends) = spawn_http_server(1);
    let t = transport::<2>(stack, true);
    let started = embassy_time::Instant::now();
    let text = get(&t, &format!("http://{addr}/"), None)
        .await
        .expect("send");
    println!(
        "embassy-net + hyper + hclient: body={text:?} in {}",
        started.elapsed()
    );
    assert_eq!(text, BODY);
}

async fn slots(stack: Stack<'static>) {
    const REQUESTS: usize = 6;
    let (addr, ends) = spawn_http_server(REQUESTS);
    // `without_pool`: with `Native`'s own connection reuse on, requests 2
    // to 6 would travel over the connection request 1 opened and the
    // socket pool would never be asked for a second slot — i.e. the test
    // would pass without checking anything about slots.
    let t = transport::<2>(stack, false);
    for i in 1..=REQUESTS {
        let text = get(&t, &format!("http://{addr}/"), None)
            .await
            .expect("send");
        assert_eq!(text, BODY, "request {i}");
        // Every connection ends the way a connection should — the FIN this
        // pool queues in `Drop` and waits for before handing the slot on.
        // `Reset` here would mean a slot was recycled by tearing the
        // connection down instead of closing it, which every one of these
        // requests would survive and no other assertion would notice.
        let end = recv_async(&ends, "the server's verdict").await;
        assert_eq!(end, ClientEnd::Eof, "request {i} ended as {end:?}");
        println!("request {i} ok (pool N=2), server saw {end:?}");
    }
}

async fn connect_timeout(stack: Stack<'static>) {
    const DEADLINE: Duration = Duration::from_millis(300);
    // Nobody answers here: `.9` is on the tap subnet and does not exist, so
    // the ARP for it goes unanswered and the connect never completes. A
    // closed port would not do — that is a refusal, which arrives at once.
    const BLACK_HOLE: &str = "http://192.168.69.9:80/";

    let pool = SocketPool::<2, 1536, 1536>::leak(stack);
    let rt = Embassy::new(stack, pool);
    let t = Native::new(rt, NoTls, IpLiteralOnly);
    let bound = Timeouts {
        resolve: None,
        connect: Some(DEADLINE),
        ..Default::default()
    };

    let started = embassy_time::Instant::now();
    let err = get(&t, BLACK_HOLE, Some(bound))
        .await
        .expect_err("nothing answers at .9");
    let waited = started.elapsed();
    println!("connect to a black hole: {err} after {waited}");
    assert!(
        matches!(
            err.kind(),
            hclient_core::ErrorKind::Timeout(hclient_core::Phase::Connect)
        ),
        "expected a connect timeout, got {:?}",
        err.kind()
    );
    // The deadline was this runtime's clock, not a coincidence: the whole
    // point is that `Timer::sleep` fired. Generous upper bound, tight lower
    // bound — a timer that fired early would be the interesting failure.
    assert!(
        waited >= embassy_time::Duration::from_millis(300),
        "fired early, after {waited}"
    );
    assert!(waited < embassy_time::Duration::from_secs(5), "{waited}");

    // The abandoned connect must have handed its slot back, or this
    // request cannot start at all. Both numbers, not just the sum: a
    // socket that was neither freed nor closing would be one this pool
    // can never hand out again.
    let (free, closing) = rt.sockets().counts();
    println!("pool after the timeout: free={free} closing={closing}");
    assert_eq!(free + closing, 2, "a slot went missing");

    let (addr, _ends) = spawn_http_server(1);
    let t2 = embassy_time::Instant::now();
    // A deadline of its own, and a generous one, because this request has
    // to pay for the previous one's failure: smoltcp rate-limits ARP to
    // one request per second **for the whole interface**, and the
    // black-hole attempt above just spent that budget. 300ms would be
    // measuring the neighbour cache, not the pool.
    let generous = Timeouts {
        resolve: None,
        connect: Some(Duration::from_secs(10)),
        ..Default::default()
    };
    let text = get(&t, &format!("http://{addr}/"), Some(generous))
        .await
        .expect("send after the timeout");
    assert_eq!(text, BODY);
    println!(
        "a request after the timeout still gets a socket, in {}",
        t2.elapsed()
    );
}

/// Every option this runtime cannot apply is refused **by `connect`**, and
/// the two it can apply still connect.
///
/// Both halves are needed. Without the second, a `connect` that refused
/// everything unconditionally would pass; without the first, the silent
/// ignore is invisible — which is exactly what mutation M6 measured.
async fn refuse_opts(stack: Stack<'static>) {
    let pool = SocketPool::<2, 1536, 1536>::leak(stack);
    let rt = Embassy::new(stack, pool);
    let addr = spawn_accept_only();

    let cases: [(&str, TcpOpts); 4] = [
        (
            "local_address",
            TcpOpts {
                local_address: Some(std::net::IpAddr::from(CLIENT_IP)),
                ..TcpOpts::default()
            },
        ),
        (
            "send_buffer_size",
            TcpOpts {
                send_buffer_size: Some(64 * 1024),
                ..TcpOpts::default()
            },
        ),
        (
            "recv_buffer_size",
            TcpOpts {
                recv_buffer_size: Some(64 * 1024),
                ..TcpOpts::default()
            },
        ),
        (
            "reuse_address",
            TcpOpts {
                reuse_address: true,
                ..TcpOpts::default()
            },
        ),
    ];

    for (name, opts) in cases {
        let before = rt.sockets().counts();
        let Err(err) = TcpConnect::connect(&rt, addr, &opts).await else {
            panic!("`connect` must refuse {name}, not ignore it and connect anyway");
        };
        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported, "{name}: {err}");
        assert!(
            err.to_string().contains(name),
            "the refusal must name {name}, got: {err}"
        );
        // Refused *before* a slot was taken — `connect` checks the options
        // as its first act. A refusal that cost a slot would turn a
        // caller's bad option into a leak of this pool's scarcest
        // resource.
        assert_eq!(
            rt.sockets().counts(),
            before,
            "{name}: the refusal must not cost a pool slot"
        );
    }

    // And the pair it does apply is not refused: `connect` really reaches
    // the socket. Without this the test above would pass against a
    // `connect` whose first line was `return Err(...)`.
    let io = TcpConnect::connect(
        &rt,
        addr,
        &TcpOpts {
            nodelay: true,
            keepalive: Some(Duration::from_secs(30)),
            ..TcpOpts::default()
        },
    )
    .await
    .expect("nodelay and keepalive are the two this runtime applies");
    println!("four options refused by name, and nodelay+keepalive connected: {io:?}");
}

async fn cancel(stack: Stack<'static>, drop_it: bool) {
    let (addr, seen, verdict) = spawn_silent_server();
    let t = transport::<2>(stack, true);
    // Owned, not `pin!`: dropping a `Pin<&mut F>` would drop the borrow
    // and leave the future itself alive on the stack, which is the one
    // thing this test must not do.
    let url = format!("http://{addr}/");
    let mut fut = Some(Box::pin(get(&t, &url, None)));
    // Drop at a determined moment — once the server has the whole request
    // head — rather than after a guessed sleep, which could land before
    // there is anything on the wire to cancel.
    loop {
        if seen.try_recv().is_ok() {
            break;
        }
        let f = fut.as_mut().expect("still held");
        if let std::task::Poll::Ready(r) =
            std::future::poll_fn(|cx| std::task::Poll::Ready(f.as_mut().poll(cx))).await
        {
            panic!(
                "the server never answers, so `send` must not complete: ok={}",
                r.is_ok()
            );
        }
        Timer::after_millis(1).await;
    }
    if drop_it {
        fut = None;
    }
    let end = loop {
        match verdict.try_recv() {
            Ok(v) => break v,
            Err(mpsc::TryRecvError::Empty) => {
                // Keep the held future being polled in the control case,
                // so "still there" is not an artefact of nobody driving
                // it. And never block the thread: the executor is
                // single-threaded, and a blocking `recv` here would stop
                // the stack task that has to put our FIN on the wire.
                if let Some(f) = fut.as_mut() {
                    let _ = std::future::poll_fn(|cx| std::task::Poll::Ready(f.as_mut().poll(cx)))
                        .await;
                }
                Timer::after_millis(1).await;
            }
            Err(e) => panic!("the server thread went away: {e}"),
        }
    };
    println!("future dropped: {drop_it}, server saw: {end:?}");
    // `Eof`, not "gone somehow": the socket pool closes the connection, so
    // the far end must see an orderly FIN. A future that stopped the
    // exchange by resetting would report `Reset` and fail here.
    let expected = if drop_it {
        ClientEnd::Eof
    } else {
        ClientEnd::StillThere
    };
    assert_eq!(end, expected);
}

/// Cancel a request on a one-slot pool, then immediately make another:
/// the second request has to take the socket the first one is still
/// closing, and the first connection must *still* end with an orderly FIN.
///
/// The timing is the whole test, so it is arranged rather than hoped for.
/// `drop(fut.take())` releases the socket — `PooledSocket::drop` queues a FIN
/// and wakes the stack task, but waking is not running — and the very next
/// statement starts a request whose first act, on the same poll of this
/// same task, is `SocketPool::acquire`. Nothing between them awaits, so
/// the stack task has not had a turn and the FIN is still in smoltcp's
/// send queue. `finish_closing` therefore gets a socket that has not
/// finished closing, which is the one case in which its `flush()` is
/// load-bearing: without it the next line is `abort()`, and an abort
/// before the FIN is dispatched replaces it, so the far end learns nothing
/// — the same silent teardown as `naive`, arrived at from the other
/// direction.
async fn reclaim(stack: Stack<'static>) {
    let (silent, seen, verdict) = spawn_silent_server();
    let (answering, _ends) = spawn_http_server(1);
    // `N = 1` is not a tuning choice: with two slots the second request
    // takes the free one and never looks at the closing list.
    let t = transport::<1>(stack, false);

    let url = format!("http://{silent}/");
    let mut fut = Some(Box::pin(get(&t, &url, None)));
    // Same shape as `cancel`: drop once the server has the whole head, so
    // there is something on the wire to cancel.
    loop {
        if seen.try_recv().is_ok() {
            break;
        }
        let f = fut.as_mut().expect("still held");
        if let std::task::Poll::Ready(r) =
            std::future::poll_fn(|cx| std::task::Poll::Ready(f.as_mut().poll(cx))).await
        {
            panic!(
                "the silent server never answers, so `send` must not complete: ok={}",
                r.is_ok()
            );
        }
        Timer::after_millis(1).await;
    }

    // --- the two statements that must share one executor turn ---
    drop(fut.take());
    let text = get(&t, &format!("http://{answering}/"), None)
        .await
        .expect("the second request must get the one slot back");
    // ------------------------------------------------------------
    assert_eq!(text, BODY, "the reclaimed socket carries a real exchange");

    let end = recv_async(&verdict, "the cancelled connection's verdict").await;
    println!("slot reclaimed while closing, the cancelled connection ended as {end:?}");
    assert_eq!(
        end,
        ClientEnd::Eof,
        "a slot reclaimed before its FIN was dispatched must still deliver that FIN, not replace \
         it with an abort — see this scenario's doc comment"
    );
}

/// `abort()`, then let the stack run, then drop: the far end sees an RST.
///
/// The sibling of `naive`, and deliberately its mirror image. Both end a
/// connection without a FIN; the difference is the single `await` between
/// the teardown and the drop, which is the whole of what
/// `SocketPool`'s closing list buys and what a synchronous `Drop` cannot
/// arrange. `naive` shows the packet never leaves; this one shows it does
/// once the stack gets a turn — so "the server saw nothing" is a fact
/// about the missing poll, not about the tap link swallowing packets.
async fn abort(stack: Stack<'static>) {
    let (addr, seen, verdict) = spawn_silent_server();
    let rx: &'static mut [u8] = Box::leak(Box::new([0u8; 1536]));
    let tx: &'static mut [u8] = Box::leak(Box::new([0u8; 1536]));
    let mut sock = TcpSocket::new(stack, rx, tx);
    let SocketAddr::V4(v4) = addr else {
        panic!("the test server binds v4")
    };
    sock.connect(v4).await.expect("connect");
    let head = format!("GET / HTTP/1.1\r\nHost: {addr}\r\n\r\n");
    let mut sent = 0;
    while sent < head.len() {
        sent += sock.write(&head.as_bytes()[sent..]).await.expect("write");
    }
    sock.flush().await.expect("flush");
    recv_async(&seen, "the request head").await;
    sock.abort();
    // The one poll `naive` never gets. `Timer`, not a bare yield: the
    // stack task has to be scheduled and run, and this is the same clock
    // every other scenario waits on.
    Timer::after_millis(20).await;
    drop(sock);
    let end = recv_async(&verdict, "the server's verdict").await;
    println!("abort + a stack poll, server saw: {end:?}");
    assert_eq!(
        end,
        ClientEnd::Reset,
        "an abort that reached the wire must be visible as an RST, or `observe` cannot tell a \
         reset connection from a closed one"
    );
}

async fn naive(stack: Stack<'static>) {
    let (addr, seen, verdict) = spawn_silent_server();
    // Buffers leaked once for this one socket: the point of the scenario
    // is the teardown, not the allocation.
    let rx: &'static mut [u8] = Box::leak(Box::new([0u8; 1536]));
    let tx: &'static mut [u8] = Box::leak(Box::new([0u8; 1536]));
    let mut sock = TcpSocket::new(stack, rx, tx);
    let SocketAddr::V4(v4) = addr else {
        panic!("the test server binds v4")
    };
    sock.connect(v4).await.expect("connect");
    let head = format!("GET / HTTP/1.1\r\nHost: {addr}\r\n\r\n");
    let mut sent = 0;
    while sent < head.len() {
        sent += sock.write(&head.as_bytes()[sent..]).await.expect("write");
    }
    sock.flush().await.expect("flush");
    recv_async(&seen, "the request head").await;
    // `embassy_net::tcp::client::TcpConnection::drop`, line for line:
    // queue a FIN, then drop the socket. The drop removes it from
    // smoltcp's `SocketSet` (`embassy-net-0.9.1/src/tcp.rs:466`) before
    // the stack runs again, so the FIN never becomes a packet.
    sock.close();
    drop(sock);
    let end = recv_async(&verdict, "the server's verdict").await;
    println!("naive teardown (close + drop), server saw: {end:?}");
    assert_eq!(
        end,
        ClientEnd::StillThere,
        "embassy's own teardown became visible to the server — see this test's doc comment"
    );
}

// -------------------------------------------------------------- helpers

/// `Native` over the embassy runtime, with an `N`-slot socket pool.
///
/// `N` is a parameter because the scenarios need two different kinds of
/// pressure: `2` is enough to prove slots are recycled at all, and only
/// `1` forces a request to reclaim a socket that is *still closing*, which
/// is the state `finish_closing` exists for.
fn transport<const N: usize>(
    stack: Stack<'static>,
    reuse: bool,
) -> Native<Embassy<N, 1536, 1536>, NoTls, IpLiteralOnly> {
    let pool = SocketPool::<N, 1536, 1536>::leak(stack);
    let rt = Embassy::<N, 1536, 1536>::new(stack, pool);
    let t = Native::new(rt, NoTls, IpLiteralOnly);
    if reuse { t } else { t.without_pool() }
}

/// A GET through the transport, with the body collected.
///
/// **This file used to build an `hclient::Client` here, and cannot any
/// more.** `Client` names no type parameters, so it boxes its transport as
/// `Send + Sync`; this runtime is `RefCell` throughout — structurally,
/// because embassy's executor is single-threaded — so `RefCell<embassy_net
/// ::Inner>: !Sync` refuses at `Client::builder`. The embedded path is
/// `Transport` directly, which is what `hclient-native/examples/minimal.rs`
/// already documents as the constrained-target path.
///
/// What the acceptance still covers is unchanged in the part that is about
/// *this crate*: `Native` -> hyper -> `embassy-net` over a real TAP device.
/// What it no longer covers is the `Client` layer above — redirects, the
/// cookie jar, the cache, timeout merging — and that is target-independent
/// logic exercised by the rest of the suite on every other backend.
async fn get<T>(t: &T, url: &str, timeouts: Option<Timeouts>) -> Result<String, hclient_core::Error>
where
    T: hclient_core::unversioned::Transport<Error = hclient_core::Error>,
    T::Body: http_body::Body<Data = bytes::Bytes> + Unpin,
    <T::Body as http_body::Body>::Error: Into<hclient_core::Error>,
{
    let mut req = http::Request::builder()
        .method(http::Method::GET)
        .uri(url)
        .body(hclient_core::RequestBody::Empty)
        .expect("request");
    if let Some(tm) = timeouts {
        req.extensions_mut().insert(tm);
    }
    let resp = t.execute(req).await?;
    let bytes = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .map_err(Into::into)?
        .to_bytes();
    Ok(String::from_utf8(bytes.to_vec()).expect("utf-8"))
}

/// Answers `count` requests, one connection each, and then reports how the
/// client ended each of those connections.
///
/// The verdicts are what makes this more than "six responses arrived": a
/// client that reused its slots by resetting them instead of closing them
/// would answer every request just as happily, and the only place that
/// difference is visible is the far end's `read`.
fn spawn_http_server(count: usize) -> (SocketAddr, mpsc::Receiver<ClientEnd>) {
    let l = std::net::TcpListener::bind((SERVER_IP, 0)).expect("bind");
    let addr = l.local_addr().expect("local_addr");
    let (verdict_tx, verdict_rx) = mpsc::channel();
    std::thread::spawn(move || {
        for _ in 0..count {
            let (mut s, _) = l.accept().expect("accept");
            s.set_read_timeout(Some(BOUND)).expect("read timeout");
            read_head(&mut s);
            let _ = s.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{BODY}",
                    BODY.len()
                )
                .as_bytes(),
            );
            // Sending the verdict must not fail the thread: the scenarios
            // that do not read them (the client keeps the connection in
            // `Native`'s own pool, so there is nothing to see) drop the
            // receiver.
            let _ = verdict_tx.send(observe(&mut s, OBSERVATION_WINDOW));
        }
    });
    (addr, verdict_rx)
}

/// What the server saw at its end of the connection, which is the only
/// thing these scenarios ever assert on.
///
/// `Eof` and `Reset` are kept apart rather than folded into one "gone":
/// both mean the peer went away, but only the first means it closed. A
/// backend that swapped every FIN for an RST would still stop the exchange
/// and would still be caught here, which is the point — the pool waits for
/// the FIN precisely so the far end sees an orderly close.
#[derive(Debug, PartialEq, Eq)]
enum ClientEnd {
    /// `read` returned `Ok(0)`: the peer closed its side.
    Eof,
    /// `read` failed with `ConnectionReset`: the peer sent an RST.
    Reset,
    /// `read` hit its window with the connection intact.
    StillThere,
}

/// Watch a server-side socket for `window` and report what happened to the
/// client end. Shared by every scenario so none of them can disagree about
/// what "the peer went away" means.
fn observe(s: &mut std::net::TcpStream, window: Duration) -> ClientEnd {
    s.set_read_timeout(Some(window)).expect("read timeout");
    let mut buf = [0u8; 64];
    match s.read(&mut buf) {
        Ok(0) => ClientEnd::Eof,
        Ok(n) => panic!("the peer sent {n} bytes where nothing was expected"),
        Err(e)
            if e.kind() == std::io::ErrorKind::WouldBlock
                || e.kind() == std::io::ErrorKind::TimedOut =>
        {
            ClientEnd::StillThere
        }
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => ClientEnd::Reset,
        Err(e) => panic!("observing the client end: {e}"),
    }
}

/// A listener that accepts and then simply holds the connection open.
///
/// Deliberately not [`spawn_http_server`]: that one calls `read_head`,
/// which asserts the client sends a request, and `refuse_opts` connects
/// without ever sending one.
fn spawn_accept_only() -> SocketAddr {
    let l = std::net::TcpListener::bind((SERVER_IP, 0)).expect("bind");
    let addr = l.local_addr().expect("local_addr");
    std::thread::spawn(move || {
        // One connect reaches here — the four refused ones never open a
        // socket at all, which is half of what the scenario asserts.
        let _held = l.accept().expect("accept");
        std::thread::sleep(BOUND);
    });
    addr
}

/// A server that accepts one connection, reads one request, and then never
/// answers — cancellation is only observable while there is something to
/// cancel.
fn spawn_silent_server() -> (SocketAddr, mpsc::Receiver<()>, mpsc::Receiver<ClientEnd>) {
    let l = std::net::TcpListener::bind((SERVER_IP, 0)).expect("bind");
    let addr = l.local_addr().expect("local_addr");
    let (seen_tx, seen_rx) = mpsc::channel();
    let (verdict_tx, verdict_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let (mut s, _) = l.accept().expect("accept");
        s.set_read_timeout(Some(BOUND)).expect("read timeout");
        read_head(&mut s);
        seen_tx.send(()).expect("report the request head");

        let verdict = observe(&mut s, OBSERVATION_WINDOW);
        verdict_tx.send(verdict).expect("report the verdict");
        // Held open deliberately: closing here would end the exchange from
        // the server side, and the control scenario — still polling its
        // future — would get a connection error instead of its assertion.
        std::thread::sleep(BOUND);
    });
    (addr, seen_rx, verdict_rx)
}

/// Wait for a value from a server thread **without blocking the executor**.
///
/// `Receiver::recv` would block this thread, and this thread is the whole
/// executor: the stack task would stop running, and the FIN whose arrival
/// the verdict reports would never leave. Polling once a millisecond is
/// bounded by the watchdog and costs nothing on a link this idle.
async fn recv_async<T>(rx: &mpsc::Receiver<T>, what: &str) -> T {
    loop {
        match rx.try_recv() {
            Ok(v) => return v,
            Err(mpsc::TryRecvError::Empty) => Timer::after_millis(1).await,
            Err(e) => panic!("waiting for {what}, the server thread went away: {e}"),
        }
    }
}

fn read_head(s: &mut std::net::TcpStream) {
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    loop {
        let n = s.read(&mut buf).expect("reading the request head");
        assert_ne!(n, 0, "the client closed before sending a request at all");
        head.extend_from_slice(&buf[..n]);
        if head.windows(4).any(|w| w == b"\r\n\r\n") {
            return;
        }
    }
}
