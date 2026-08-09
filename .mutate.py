"""Mutation testing for the v0.3 h3 work.

Each entry names a file, an anchor that must match EXACTLY ONCE, the
replacement, and the test that must go red. The anchor count is checked
before anything is written: an anchor matching zero or several places means
the mutation is not the one described, and the run stops rather than
reporting a kill it did not earn.
"""

import pathlib
import subprocess
import sys

MUTATIONS = [
    # (id, file, anchor, replacement, nextest filter, what it claims)
    (
        "M1",
        "crates/http-ng-rt/src/udp.rs",
        "        may_fragment: true,\n    };",
        "        may_fragment: false,\n    };",
        "none_is_the_conservative_base",
        "UdpCaps::NONE's may_fragment is the WORSE answer, not the tidier one",
    ),
    (
        "M2",
        "crates/http-ng-rt/src/udp.rs",
        "    fn caps(&self) -> UdpCaps {\n        UdpCaps::NONE\n    }",
        "    fn caps(&self) -> UdpCaps {\n        UdpCaps {\n            max_send_segments: 64,\n            max_recv_segments: 64,\n            ecn: true,\n            may_fragment: false,\n        }\n    }",
        "a_default_caps_impl_reports_nothing",
        "a socket that declares nothing must not claim offloads it has not got",
    ),
    (
        "M3",
        "crates/http-ng-rt/src/udp.rs",
        "        let gso = self.segments() > caps.max_send_segments;",
        "        let gso = false;",
        "gso_beyond_the_declared_batch_is_refused_by_name",
        "a GSO batch beyond the declared one is refused, not truncated",
    ),
    (
        "M4",
        "crates/http-ng-rt/src/udp.rs",
        "            stride: 0,\n            ecn: None,\n            dst_ip: None,",
        "            stride: 1200,\n            ecn: Some(EcnCodepoint::Ect0),\n            dst_ip: None,",
        "an_unfilled_recv_slot_reports_no_ecn_rather_than_a_plausible_one",
        "an unfilled recv slot reports no ECN and no GRO run, never a plausible one",
    ),
    (
        "M5",
        "crates/http-ng-h3/src/early.rs",
        "    if req.extensions().get::<AllowEarlyData>().is_none() {\n        return false;\n    }",
        "",
        "an_unmarked_request_never_enters_early_data",
        "the caller's mark is the gate, and no body property can substitute for it",
    ),
    (
        "M6",
        "crates/http-ng-h3/src/early.rs",
        "    req.body().retry_kind() != RetryKind::Impossible",
        "    true",
        "a_marked_request_whose_body_cannot_be_replayed_is_refused",
        "RetryKind is a real precondition beneath the mark, not decoration",
    ),
    (
        "M7",
        "crates/http-ng-h3/src/runtime.rs",
        "        if !waiters.iter().any(|existing| existing.will_wake(w)) {\n            waiters.push(w.clone());\n        }",
        "        waiters.clear();\n        waiters.push(w.clone());",
        "a_socket_with_one_waker_slot_still_wakes_every_waiting_poller",
        "every waiting poller is woken, not only the most recent to register",
    ),
    (
        "M8",
        "crates/http-ng-h3/src/runtime.rs",
        "        http_ng_rt::EcnCodepoint::Ect0 => quinn::udp::EcnCodepoint::Ect0,\n        http_ng_rt::EcnCodepoint::Ect1 => quinn::udp::EcnCodepoint::Ect1,",
        "        http_ng_rt::EcnCodepoint::Ect0 => quinn::udp::EcnCodepoint::Ect1,\n        http_ng_rt::EcnCodepoint::Ect1 => quinn::udp::EcnCodepoint::Ect0,",
        "ecn_survives_the_round_trip_through_both_conversions",
        "the ECN conversion does not swap two codepoints that differ by one bit",
    ),
    (
        "M9",
        "crates/http-ng-h3/src/lib.rs",
        "            keep_alive: Some(DEFAULT_KEEP_ALIVE),",
        "            keep_alive: None,",
        "an_idle_connection_survives_only_because_of_the_keep_alive",
        "a pooled connection is kept alive by default, or it dies between requests",
    ),
    (
        "M10",
        "crates/http-ng-h3/src/lib.rs",
        "            early_data: wants_early,\n        };",
        "            early_data: false,\n        };",
        "early_data_is_offered_only_to_a_request_the_caller_marked",
        "early data is part of the pool key, so a marked request is not served by a connection built without it",
    ),
    (
        "M11",
        "crates/http-ng-h3/src/lib.rs",
        "            && p.conn.close_reason().is_none()",
        "            && true",
        "an_idle_connection_survives_only_because_of_the_keep_alive",
        "the pool checks a connection is still alive before handing it out",
    ),
    (
        "M12",
        "crates/http-ng-h3/src/lib.rs",
        "            return Ok(p.send.clone());",
        "            let _ = p;",
        "two_requests_share_one_connection",
        "the pool is actually used: a second request reuses the first connection",
    ),
    (
        "M13",
        "crates/http-ng-rt-tokio/src/udp.rs",
        "        t.reject_unsupported(self.caps)?;",
        "",
        "the_socket_refuses_a_gso_batch_it_cannot_send",
        "the tokio socket enforces its own capability report rather than only publishing it",
    ),
    (
        "M14",
        "crates/http-ng-rt-tokio/src/udp.rs",
        "            ecn: ecn_is_really_on(&io),",
        "            ecn: true,",
        "ecn_is_reported_from_the_kernel_not_assumed",
        "ECN support is read back from the descriptor, not assumed from the platform",
    ),
    (
        "M15",
        "crates/http-ng-h3/src/lib.rs",
        "    c.full_duplex = false;",
        "    c.full_duplex = true;",
        "capabilities_describe_this_implementation_not_the_protocol",
        "full_duplex describes what execute does, not what HTTP/3 permits",
    ),
]


def run(cmd):
    return subprocess.run(cmd, shell=True, capture_output=True, text=True)


def main():
    only = sys.argv[1:] if len(sys.argv) > 1 else None
    results = []
    for mid, path, anchor, repl, test, claim in MUTATIONS:
        if only and mid not in only:
            continue
        p = pathlib.Path(path)
        original = p.read_text()
        n = original.count(anchor)
        if n != 1:
            print(f"{mid}: ANCHOR MATCHES {n} TIMES in {path} — not run")
            results.append((mid, f"anchor x{n}", claim))
            continue
        p.write_text(original.replace(anchor, repl, 1))
        r = run(f"cargo nextest run --workspace --all-features -E 'test(={test})' 2>&1")
        p.write_text(original)
        out = r.stdout + r.stderr
        if r.returncode == 0 and "0 tests run" not in out:
            verdict = "SURVIVED"
        elif "0 tests run" in out or "did not match any" in out:
            verdict = "NO SUCH TEST"
        else:
            verdict = "killed"
        print(f"{mid}: {verdict:12} {test}")
        results.append((mid, verdict, claim))

    print("\n--- summary ---")
    for mid, v, claim in results:
        print(f"{mid} {v:12} {claim}")


if __name__ == "__main__":
    main()
