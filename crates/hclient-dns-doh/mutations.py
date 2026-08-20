#!/usr/bin/env python3
"""Hand-applied mutations for hclient-dns-doh, with anchor counts.

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
SRC = ROOT / "crates/hclient-dns-doh/src"

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
        "            Ok(answer) => answer.addrs.into_iter().map(Ok).collect(),\n            Err(e) => self.recover(name, family, e).await,",
        "            Ok(_) => self.recover(name, family, DohError::NotAResponse).await,\n            Err(e) => self.recover(name, family, e).await,",
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
        None,  # lives in hclient-dns, see below
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
    (
        "M22 an IP literal is sent to the DoH server as a name",
        "lib.rs",
        "        if let Some(addr) = ip_literal(name) {\n            return match (addr, family) {",
        "        if let Some(addr) = ip_literal(name).filter(|_| false) {\n            return match (addr, family) {",
        1,
    ),
    (
        "M23 an IP literal of the wrong family is an address rather than an empty stream",
        "lib.rs",
        "                (IpAddr::V4(_), Family::V4) | (IpAddr::V6(_), Family::V6) => {\n                    vec![Ok(ResolvedAddr { addr, ttl: None })]\n                }\n                _ => Vec::new(),",
        "                _ => vec![Ok(ResolvedAddr { addr, ttl: None })],",
        1,
    ),
    (
        "M24 an IP literal is asked for an HTTPS record",
        "lib.rs",
        "            if ip_literal(&name).is_some() {\n                return Vec::new();\n            }",
        "",
        1,
    ),
    (
        "M25 the response body is read without a size limit",
        "lib.rs",
        "http_body_util::Limited::new(response.into_body(), MAX_RESPONSE_BYTES)",
        "http_body_util::Limited::new(response.into_body(), usize::MAX)",
        1,
    ),
    (
        "M26 no timeouts are put in the request's extensions",
        "lib.rs",
        "        req.extensions_mut().insert(self.timeouts);",
        "",
        1,
    ),
    (
        "M27 the configured timeouts are ignored and the defaults used",
        "lib.rs",
        "        req.extensions_mut().insert(self.timeouts);",
        "        req.extensions_mut().insert(DEFAULT_TIMEOUTS);",
        1,
    ),
    (
        "M28 the request is a GET rather than a POST",
        "lib.rs",
        "        *req.method_mut() = http::Method::POST;",
        "        *req.method_mut() = http::Method::GET;",
        1,
    ),
    (
        "M29 the Accept header is not sent",
        "lib.rs",
        "        req.headers_mut().insert(\n            http::header::ACCEPT,\n            http::HeaderValue::from_static(DNS_MESSAGE),\n        );",
        "",
        1,
    ),
    (
        "M30 the endpoint's path is dropped and the query posted to the root",
        "lib.rs",
        "        *req.uri_mut() = self.endpoint.clone();",
        "        *req.uri_mut() = {\n            let mut parts = self.endpoint.clone().into_parts();\n            parts.path_and_query = Some(http::uri::PathAndQuery::from_static(\"/\"));\n            Uri::from_parts(parts).expect(\"still a uri\")\n        };",
        1,
    ),
    (
        "M31 the default timeout values are minutes rather than seconds",
        "lib.rs",
        "    connect: Some(Duration::from_secs(2)),\n    first_byte: Some(Duration::from_secs(5)),\n    between_bytes: Some(Duration::from_secs(5)),\n};",
        "    connect: Some(Duration::from_secs(120)),\n    first_byte: Some(Duration::from_secs(120)),\n    between_bytes: Some(Duration::from_secs(120)),\n};",
        1,
    ),
    (
        "M32 the request content-type is not sent",
        "lib.rs",
        "        req.headers_mut().insert(\n            http::header::CONTENT_TYPE,\n            http::HeaderValue::from_static(DNS_MESSAGE),\n        );",
        "",
        1,
    ),
    (
        "M33 the echoed question's CLASS is not checked",
        "wire.rs",
        "        || question.q_class != QClass::IN",
        "",
        1,
    ),
    (
        "M34 the question name is compared case-sensitively",
        "wire.rs",
        "    if !got_name.eq_ignore_ascii_case(name.trim_end_matches('.'))\n",
        "    if got_name != name.trim_end_matches('.')\n",
        1,
    ),
    (
        "M35 a trailing dot on the name asked for is not stripped before comparison",
        "wire.rs",
        "    if !got_name.eq_ignore_ascii_case(name.trim_end_matches('.'))\n",
        "    if !got_name.eq_ignore_ascii_case(name)\n",
        1,
    ),
    (
        "M36 a mandatory key the record lacks is ignored instead of failing",
        None,
        "            return Err(SvcbRecordError::MandatoryKeyAbsent { key: *key });",
        "            return Ok(None);",
        1,
    ),
    (
        "M37 the v6 family consults the fallback's v4 stream",
        "lib.rs",
        "            Family::V6 => self.fallback.lookup_ipv6(name).collect().await,",
        "            Family::V6 => self.fallback.lookup_ipv4(name).collect().await,",
        1,
    ),
]

SHARED = ROOT / "crates/hclient-dns/src/svcb.rs"


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
            "hclient-dns-doh",
            "-p",
            "hclient-dns",
            "-p",
            "hclient-dns-system",
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
