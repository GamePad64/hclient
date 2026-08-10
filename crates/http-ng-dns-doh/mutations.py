#!/usr/bin/env python3
"""Hand-applied mutations for http-ng-dns-doh, with anchor counts.

Each entry names a file, a literal `find` string, the `replace` that
mutates it, and how many places the `find` is expected to match. The count
is checked BEFORE the edit: a mutation that matched zero or several places
is reported as such rather than scored, which is the convention the h3
work established.
"""

import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SRC = ROOT / "crates/http-ng-dns-doh/src"

MUTATIONS = [
    (
        "M1 the response is parsed but the answer ignored",
        "wire.rs",
        "    let mut answer = Answer::default();\n    for rr in &dns.answers {",
        "    let mut answer = Answer::default();\n    for rr in &[] as &[RR] {",
        1,
    ),
    (
        "M2 the TTL is ignored",
        "wire.rs",
        "ttl: Some(Duration::from_secs(u64::from(a.ttl))),",
        "ttl: None,",
        2,
    ),
    (
        "M3 an error RCODE is treated as an empty answer",
        "wire.rs",
        "            return Err(DohError::ResponseCode { rcode: rcode as u8 });",
        "            let _ = rcode;\n            return Ok(Answer::default());",
        1,
    ),
    (
        "M4 supports_svcb is flipped",
        "lib.rs",
        "    fn supports_svcb(&self) -> bool {\n        true\n    }",
        "    fn supports_svcb(&self) -> bool {\n        false\n    }",
        1,
    ),
    (
        "M5 NXDOMAIN is treated as a failure rather than an answer",
        "wire.rs",
        "        RCode::NXDomain => return Ok(Answer::default()),",
        "        RCode::NXDomain => return Err(DohError::ResponseCode { rcode: 3 }),",
        1,
    ),
    (
        "M6 the HTTP status is not checked",
        "lib.rs",
        "        if status != http::StatusCode::OK {",
        "        if false {",
        1,
    ),
    (
        "M7 the content-type is not checked",
        "lib.rs",
        "        if content_type.as_deref() != Some(DNS_MESSAGE) {",
        "        if false {",
        1,
    ),
    (
        "M8 the QR bit is not checked",
        "wire.rs",
        "    if !dns.is_response() {",
        "    if false {",
        1,
    ),
    (
        "M9 the TC bit is not checked",
        "wire.rs",
        "    if dns.flags.tc {",
        "    if false {",
        1,
    ),
    (
        "M10 the echoed question is not checked",
        "wire.rs",
        "    check_question(&dns, name, query)?;",
        "    let _ = check_question(&dns, name, query);",
        1,
    ),
    (
        "M11 a failed lookup with no fallback becomes an empty answer",
        "lib.rs",
        "        if recovered.is_empty() {\n            vec![Err(failure.into())]\n        } else {\n            recovered\n        }",
        "        let _ = failure;\n        recovered",
        1,
    ),
    (
        "M12 the fallback is consulted even when DoH answered",
        "lib.rs",
        "            Ok(answer) => answer.addrs.into_iter().map(Ok).collect(),\n            Err(e) => self.recover(name, query, e).await,",
        "            Ok(_) => {\n                self.recover(name, query, DohError::NotAResponse).await\n            }\n            Err(e) => self.recover(name, query, e).await,",
        1,
    ),
    (
        "M13 recursion desired is not set on the query",
        "wire.rs",
        "            rd: true,",
        "            rd: false,",
        1,
    ),
    (
        "M14 the DNS ID is not zero",
        "wire.rs",
        "        id: 0,",
        "        id: 0x1234,",
        1,
    ),
    (
        "M15 a cleartext endpoint anywhere is accepted",
        "lib.rs",
        "fn check_confidential(uri: &Uri, host: Option<IpAddr>) -> Result<(), EndpointError> {\n    if uri.scheme_str() == Some(\"https\") {",
        "fn check_confidential(uri: &Uri, host: Option<IpAddr>) -> Result<(), EndpointError> {\n    let _ = (&uri, &host);\n    if true {",
        1,
    ),
    (
        "M16 pinned accepts a name",
        "lib.rs",
        "        let Some(addr) = ip_literal(host) else {\n            return Err(EndpointError::NotAnIpLiteral {\n                host: host.to_owned(),\n            });\n        };",
        "        let addr = ip_literal(host).unwrap_or(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));",
        1,
    ),
    (
        "M17 bootstrapped accepts a literal",
        "lib.rs",
        "        if let Some(_addr) = ip_literal(host) {\n            return Err(EndpointError::IsAnIpLiteral {\n                host: host.to_owned(),\n            });\n        }",
        "",
        1,
    ),
    (
        "M18 an SVCB failure is routed to the fallback like an address failure",
        "lib.rs",
        "                Err(e) => vec![Err(Error::from(e))],",
        "                Err(_) => Vec::new(),",
        1,
    ),
    (
        "M19 the ECHConfigList length prefix is dropped",
        None,  # lives in http-ng-dns, see below
        "                prefixed.extend_from_slice(&len.to_be_bytes());\n",
        "",
        1,
    ),
    (
        "M20 a ServiceMode root TargetName is not replaced by the owner name",
        None,
        "        target: if binding.target.is_empty() {\n            binding.owner.clone()\n        } else {\n            binding.target.clone()\n        },",
        "        target: binding.target.clone(),",
        1,
    ),
    (
        "M21 an unrecognised mandatory key no longer disqualifies a record",
        None,
        "        if !RECOGNISED_KEYS.contains(key) {\n            return Ok(None);\n        }",
        "",
        1,
    ),
]

SHARED = ROOT / "crates/http-ng-dns/src/svcb.rs"


def path_for(name):
    return SHARED if name is None else SRC / name


def run_suite():
    """The whole doh suite plus the shared crate's own, since M19-M21 live there."""
    return subprocess.run(
        [
            "cargo",
            "nextest",
            "run",
            "-p",
            "http-ng-dns-doh",
            "-p",
            "http-ng-dns",
            "-p",
            "http-ng-dns-system",
            "--no-fail-fast",
        ],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )


def failing_tests(out):
    names = []
    for line in (out.stdout + out.stderr).splitlines():
        if "FAIL" in line and "::" in line:
            names.append(line.split()[-1].strip())
    return sorted(set(names))


def main():
    only = sys.argv[1:]
    for label, filename, find, replace, expected in MUTATIONS:
        if only and not any(label.startswith(o) for o in only):
            continue
        path = path_for(filename)
        original = path.read_text()
        count = original.count(find)
        if count != expected:
            print(f"{label}: ANCHOR MISMATCH — matched {count}, expected {expected}")
            continue
        path.write_text(original.replace(find, replace))
        try:
            out = run_suite()
            if out.returncode == 0:
                print(f"{label}: SURVIVED (anchors {count})")
            else:
                dead = failing_tests(out)
                if not dead:
                    print(f"{label}: KILLED — build failure (anchors {count})")
                else:
                    print(f"{label}: KILLED by {', '.join(dead[:4])} (anchors {count})")
        finally:
            path.write_text(original)


if __name__ == "__main__":
    main()
