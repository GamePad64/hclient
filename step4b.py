def edit(path, pairs):
    s = open(path).read()
    for old, new in pairs:
        assert old in s, f"{path}: not found:\n{old!r}"
        s = s.replace(old, new, 1)
    open(path, 'w').write(s)
    print("ok", path)


edit('crates/http-ng-fetch/src/caps.rs', [
    ("""use http_ng_core::{
    CancelSupport, Capabilities, RedirectSupport, ReuseSupport, TimeoutSupport, TlsSupport,
    UpgradeSupport,
};""",
     """use http_ng_core::{
    CancelSupport, Capabilities, RedirectSupport, ReuseSupport, TimeoutSupport, TlsSupport,
};"""),
])

edit('crates/http-ng-h3/src/lib.rs', [
    ("""    RedirectSupport, RequestBody, ReuseSupport, TimeoutSupport, Timeouts, TlsSupport,
    UpgradeSupport, unversioned::Transport,
};""",
     """    RedirectSupport, RequestBody, ReuseSupport, TimeoutSupport, Timeouts, TlsSupport,
    unversioned::Transport,
};"""),
])

edit('crates/http-ng-native/src/lib.rs', [
    ("""    CancelSupport, Capabilities, Error, ErrorKind, Phase, RedirectSupport, RequestBody,
    ReuseSupport, TimeoutSupport, Timeouts, UpgradeSupport,
};""",
     """    CancelSupport, Capabilities, Error, ErrorKind, Phase, RedirectSupport, RequestBody,
    ReuseSupport, TimeoutSupport, Timeouts,
};"""),
])

edit('crates/http-ng-wasi/src/lib.rs', [
    ("""    CancelSupport, Capabilities, Error, RedirectSupport, RequestBody, ReuseSupport, TimeoutSupport,
    Timeouts, TlsSupport, UpgradeSupport,
};""",
     """    CancelSupport, Capabilities, Error, RedirectSupport, RequestBody, ReuseSupport, TimeoutSupport,
    Timeouts, TlsSupport,
};"""),
    ("""/// `upgrade` — no protocol upgrade support (`Capabilities::upgrade` is
/// already `UpgradeSupport::None`); `host` — the host computes it itself""",
     """/// `upgrade` — no protocol upgrade support, and this crate implements
/// neither `wasi:http`'s `HTTP-upgrade-failed` nor
/// `http_ng_core::unversioned::WebSocketConnect`, which is how a backend
/// says it can now (there is no capability field to read: see that
/// trait's own module doc); `host` — the host computes it itself"""),
])

# ------------------------------------------------------------------ http-ng
edit('crates/http-ng/src/lib.rs', [
    ("""// naming to hand-assemble your own `Capabilities` for""",
     """// naming to hand-assemble your own `Capabilities` for"""),
    ("""// `RetryKind` is the
// invariant of `RequestBody::retry_kind()` and the field
// `mock::RecordedRequest::retry_kind`. `RedirectSupport`/`TlsSupport`/
// `TimeoutSupport`/`UpgradeSupport` are `Capabilities` fields that need
// naming to hand-assemble your own `Capabilities` for""",
     """// `RetryKind` is the
// invariant of `RequestBody::retry_kind()` and the field
// `mock::RecordedRequest::retry_kind`. `RedirectSupport`/`TlsSupport`/
// `TimeoutSupport`/`EarlyDataSupport` are `Capabilities` fields that need
// naming to hand-assemble your own `Capabilities` for"""),
    ("""// `AllowEarlyData` and `EarlyDataSupport` are on this list for the reason
// every other capability type is, and one of their own: `AllowEarlyData` is
// a value the CALLER puts on a request — it is the only thing that can
// admit a request to 0-RTT, and `Client::run`'s `425` branch is what takes
// it back off a replay — so a facade that names `TimeoutSupport` and
// `UpgradeSupport` while making callers reach past it into `http-ng-core`
// for this one would be arbitrary.""",
     """// `AllowEarlyData` and `EarlyDataSupport` are on this list for the reason
// every other capability type is, and one of their own: `AllowEarlyData` is
// a value the CALLER puts on a request — it is the only thing that can
// admit a request to 0-RTT, and `Client::run`'s `425` branch is what takes
// it back off a replay — so a facade that names `TimeoutSupport` while
// making callers reach past it into `http-ng-core` for this one would be
// arbitrary.
//
// `UpgradeSupport` used to be on this list and is gone from the workspace
// (v0.3 W4 step 4): four variants, every backend answering `None`, and no
// caller decision turning on it. WebSocket is a trait a backend implements
// (`http_ng_core::unversioned::WebSocketConnect`) rather than a capability
// anyone reads, which is why there is nothing to re-export in its place —
// see `docs/w4-upgrade-seam.md` §3."""),
    ("""    AllowEarlyData, Capabilities, DecompressionSupport, EarlyDataSupport, Error, ErrorKind, Phase,
    RedirectSupport, RequestBody, RetryKind, RewindFactory, TimeoutSupport, TlsSupport,
    UnsupportedCapability, UpgradeSupport,
};""",
     """    AllowEarlyData, Capabilities, DecompressionSupport, EarlyDataSupport, Error, ErrorKind, Phase,
    RedirectSupport, RequestBody, RetryKind, RewindFactory, TimeoutSupport, TlsSupport,
    UnsupportedCapability,
};"""),
])

# ------------------------------------------------------------------ facade.rs
edit('crates/http-ng/tests/facade.rs', [
    ("""/// `RedirectSupport`/`TlsSupport`/`TimeoutSupport`/`UpgradeSupport` are
/// `Capabilities` fields. Needed not just for reading (`Capabilities` is
/// readable even without them — `#[non_exhaustive]` on the struct doesn't
/// block access to `pub` fields), but for WRITING: you can't assemble your
/// own `Capabilities` for a mock transport (e.g.
/// `MockTransport::with_capabilities`) without them — the field's type has
/// to be nameable on the caller's side.""",
     """/// `RedirectSupport`/`TlsSupport`/`TimeoutSupport`/`EarlyDataSupport` are
/// `Capabilities` fields. Needed not just for reading (`Capabilities` is
/// readable even without them — `#[non_exhaustive]` on the struct doesn't
/// block access to `pub` fields), but for WRITING: you can't assemble your
/// own `Capabilities` for a mock transport (e.g.
/// `MockTransport::with_capabilities`) without them — the field's type has
/// to be nameable on the caller's side.
///
/// The fourth used to be `UpgradeSupport`, which v0.3 W4 step 4 deleted
/// from the workspace: four variants, `None` from every backend, and no
/// caller decision turning on it. What this test pins is the plumbing —
/// that a `Capabilities` field's type is nameable and writable from the
/// facade — not upgrade, so it needs a different field rather than one
/// fewer.
///
/// `EarlyDataSupport` is that field, and it was picked over the two other
/// candidates on the same grounds:
///
/// - It is a `Capabilities` field with an enum type re-exported from
///   `http-ng`, and its `Capabilities::none()` value (`None`) differs from
///   the value set here (`Supported`) — so a fixture can set it to
///   something distinguishable, which is what makes the assertion mean
///   anything.
/// - Nothing else in this crate's tests names it. `DecompressionSupport`,
///   the other enum-typed field, is already named from `http_ng::` by
///   `tests/compression_capability.rs`, so pointing this test at it would
///   have duplicated an existing guard and left `EarlyDataSupport`'s
///   re-export unexercised. `tests/too_early.rs` reaches for
///   `http_ng_core::AllowEarlyData` directly rather than through the
///   facade, so the whole early-data corner was the one with no facade
///   check at all.
/// - `ReuseSupport` and `CancelSupport` are the remaining enum-typed
///   fields and are deliberately NOT re-exported from `http-ng` — pointing
///   the test at one of them would have meant adding a re-export to make a
///   test compile, which is the reasoning this project rejects.
///
/// The check is a compile-time one as much as a runtime one: delete
/// `EarlyDataSupport` from `http-ng`'s `pub use` and this file — an
/// external consumer — stops compiling. That is the mutation that proves
/// the re-pointing is not vacuous, and it was run."""),
    ("""    caps.upgrade = http_ng::UpgradeSupport::H1;""",
     """    caps.early_data = http_ng::EarlyDataSupport::Supported;"""),
    ("""    assert_eq!(caps.upgrade, http_ng::UpgradeSupport::H1);""",
     """    assert_eq!(caps.early_data, http_ng::EarlyDataSupport::Supported);"""),
])
print("done")
