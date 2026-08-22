//! A DoH server on loopback, and the DNS bytes it answers with.
//!
//! # Everything here is written by hand, on purpose
//!
//! The server does not use `dns_message_parser` to build its answers and
//! does not use this crate to read the queries it receives. If it did, a
//! test would be comparing the crate against itself: an encoder and a
//! decoder that agree on the same wrong thing produce a green run, and
//! nothing else in this workspace exercises the encode path. So the
//! fixtures below emit RFC 1035
//! wire format from first principles — a length-prefixed name, four
//! two-byte fields, an RDATA blob — and the assertions are about those
//! bytes.
//!
//! This is the same call `hclient-dns-system`'s `svcb.rs` tests made for
//! the same reason, and the helpers are deliberately similar to theirs.

#![allow(dead_code, reason = "each test file uses a different subset")]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

/// What the server actually received, as bytes rather than as anything
/// this crate's types would say about them.
#[derive(Debug, Clone)]
pub struct Recorded {
    pub method: String,
    /// The request target, exactly as it appeared on the request line.
    pub target: String,
    /// Lowercased names, values verbatim.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// How the fixture answers one request.
#[derive(Debug, Clone)]
pub enum Reply {
    /// `200` + `content-type: application/dns-message` + these bytes.
    Dns(Vec<u8>),
    /// That status, with a well-formed DNS body — so that a test about the
    /// status is not accidentally also a test about the body.
    Status(u16, Vec<u8>),
    /// `200`, these bytes, and that content-type.
    Typed(&'static str, Vec<u8>),
    /// Accept the connection, read the request, and never answer. For the
    /// tests that need the DoH query to fail.
    Silence,
}

pub struct Server {
    pub addr: SocketAddr,
    requests: Arc<Mutex<Vec<Recorded>>>,
}

impl Server {
    /// A server whose answer is computed from the request it received.
    pub fn spawn<F>(reply: F) -> Self
    where
        F: Fn(&Recorded) -> Reply + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        let requests: Arc<Mutex<Vec<Recorded>>> = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&requests);
        let reply = Arc::new(reply);
        std::thread::spawn(move || {
            for sock in listener.incoming() {
                let Ok(sock) = sock else { continue };
                let seen = Arc::clone(&seen);
                let reply = Arc::clone(&reply);
                std::thread::spawn(move || serve(sock, &*reply, &seen));
            }
        });
        Self { addr, requests }
    }

    /// A server that always answers with the same bytes.
    pub fn answering(body: Vec<u8>) -> Self {
        Self::spawn(move |_| Reply::Dns(body.clone()))
    }

    /// Everything received so far, oldest first.
    pub fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().expect("not poisoned").clone()
    }

    /// `http://127.0.0.1:PORT/dns-query` — a loopback cleartext endpoint,
    /// which `Doh::pinned` accepts by the rule its doc comment states.
    pub fn endpoint(&self) -> http::Uri {
        format!("http://{}/dns-query", self.addr)
            .parse()
            .expect("a valid uri")
    }
}

fn serve(
    mut sock: TcpStream,
    reply: &(dyn Fn(&Recorded) -> Reply + Send + Sync),
    seen: &Mutex<Vec<Recorded>>,
) {
    // Keep serving requests on this connection: `Native` pools, so a second
    // lookup arrives on the same socket and a server that handled one
    // request and closed would make every test about a second query a test
    // about connection reuse instead.
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let Some(request) = read_one(&mut sock, &mut buf, &mut chunk) else {
            return;
        };
        let answer = reply(&request);
        seen.lock().expect("not poisoned").push(request);
        let written = match answer {
            Reply::Dns(body) => write_response(&mut sock, 200, "application/dns-message", &body),
            Reply::Status(status, body) => {
                write_response(&mut sock, status, "application/dns-message", &body)
            }
            Reply::Typed(content_type, body) => write_response(&mut sock, 200, content_type, &body),
            Reply::Silence => {
                // Hold the socket open. The client's own bound is what has
                // to end this, which is the point of the fixture.
                std::thread::sleep(std::time::Duration::from_secs(30));
                return;
            }
        };
        if written.is_err() {
            return;
        }
    }
}

/// One request head plus its `content-length` body, or `None` at EOF.
fn read_one(sock: &mut TcpStream, buf: &mut Vec<u8>, chunk: &mut [u8]) -> Option<Recorded> {
    let head_end = loop {
        if let Some(at) = find(buf, b"\r\n\r\n") {
            break at + 4;
        }
        match sock.read(chunk) {
            Ok(0) | Err(_) => return None,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    };

    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default().to_owned();
    let mut parts = request_line.split(' ');
    let method = parts.next().unwrap_or_default().to_owned();
    let target = parts.next().unwrap_or_default().to_owned();

    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }
    }
    let length: usize = headers
        .iter()
        .find(|(n, _)| n == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);

    while buf.len() < head_end + length {
        match sock.read(chunk) {
            Ok(0) | Err(_) => return None,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let body = buf[head_end..head_end + length].to_vec();
    buf.drain(..head_end + length);
    Some(Recorded {
        method,
        target,
        headers,
        body,
    })
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn write_response(
    sock: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status} X\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\n\r\n",
        body.len()
    );
    sock.write_all(head.as_bytes())?;
    sock.write_all(body)?;
    sock.flush()
}

// ── DNS wire format, by hand ────────────────────────────────────────────

/// RR and QTYPE numbers, RFC 1035 §3.2.2 / RFC 3596 §2.1 / RFC 9460 §14.1.
pub const TYPE_A: u16 = 1;
pub const TYPE_AAAA: u16 = 28;
pub const TYPE_CNAME: u16 = 5;
pub const TYPE_HTTPS: u16 = 65;

/// Header flag words, RFC 1035 §4.1.1. `QR|RD|RA` is what an ordinary
/// recursive answer carries.
pub const FLAGS_NOERROR: u16 = 0x8180;
pub const FLAGS_NXDOMAIN: u16 = 0x8183;
pub const FLAGS_SERVFAIL: u16 = 0x8182;
/// `QR|TC|RD|RA`.
pub const FLAGS_TRUNCATED: u16 = 0x8380;
/// `RD` alone: no `QR`, i.e. a query where a response should be.
pub const FLAGS_QUERY: u16 = 0x0100;

/// A name in RFC 1035 §3.1 label form. `""` is the root.
pub fn name_wire(name: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for label in name.split('.').filter(|l| !l.is_empty()) {
        out.push(u8::try_from(label.len()).expect("label under 256"));
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    out
}

/// One answer record: owner name, type, TTL, RDATA.
pub struct Rr {
    pub owner: String,
    pub rtype: u16,
    pub ttl: u32,
    pub rdata: Vec<u8>,
}

impl Rr {
    pub fn a(owner: &str, ttl: u32, addr: [u8; 4]) -> Self {
        Self {
            owner: owner.to_owned(),
            rtype: TYPE_A,
            ttl,
            rdata: addr.to_vec(),
        }
    }
    pub fn aaaa(owner: &str, ttl: u32, addr: std::net::Ipv6Addr) -> Self {
        Self {
            owner: owner.to_owned(),
            rtype: TYPE_AAAA,
            ttl,
            rdata: addr.octets().to_vec(),
        }
    }
    pub fn cname(owner: &str, ttl: u32, target: &str) -> Self {
        Self {
            owner: owner.to_owned(),
            rtype: TYPE_CNAME,
            ttl,
            rdata: name_wire(target),
        }
    }
    pub fn https(
        owner: &str,
        ttl: u32,
        priority: u16,
        target: &str,
        params: &[(u16, Vec<u8>)],
    ) -> Self {
        let mut rdata = priority.to_be_bytes().to_vec();
        rdata.extend(name_wire(target));
        for (key, value) in params {
            rdata.extend_from_slice(&key.to_be_bytes());
            rdata.extend_from_slice(
                &u16::try_from(value.len())
                    .expect("value fits")
                    .to_be_bytes(),
            );
            rdata.extend_from_slice(value);
        }
        Self {
            owner: owner.to_owned(),
            rtype: TYPE_HTTPS,
            ttl,
            rdata,
        }
    }
}

/// A complete DNS response message: header, one echoed question, answers.
///
/// The ID is echoed as zero because that is what RFC 8484 §4.1 asks a DoH
/// client to send and therefore what a server echoes.
pub fn message(qname: &str, qtype: u16, flags: u16, answers: &[Rr]) -> Vec<u8> {
    message_in_class(qname, qtype, CLASS_IN, flags, answers)
}

/// RFC 1035 §3.2.4. `IN` is the only class a web client has any business
/// with; `CH` exists here so a test can send one.
pub const CLASS_IN: u16 = 1;
pub const CLASS_CH: u16 = 3;

/// The same, with the question's QCLASS chosen by the caller.
pub fn message_in_class(
    qname: &str,
    qtype: u16,
    qclass: u16,
    flags: u16,
    answers: &[Rr],
) -> Vec<u8> {
    let mut m = Vec::new();
    m.extend_from_slice(&0u16.to_be_bytes()); // ID
    m.extend_from_slice(&flags.to_be_bytes());
    m.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
    m.extend_from_slice(
        &u16::try_from(answers.len())
            .expect("few answers")
            .to_be_bytes(),
    );
    m.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
    m.extend_from_slice(&0u16.to_be_bytes()); // ARCOUNT
    m.extend(name_wire(qname));
    m.extend_from_slice(&qtype.to_be_bytes());
    m.extend_from_slice(&qclass.to_be_bytes());
    for rr in answers {
        m.extend(name_wire(&rr.owner));
        m.extend_from_slice(&rr.rtype.to_be_bytes());
        m.extend_from_slice(&1u16.to_be_bytes()); // CLASS IN
        m.extend_from_slice(&rr.ttl.to_be_bytes());
        m.extend_from_slice(
            &u16::try_from(rr.rdata.len())
                .expect("rdata fits")
                .to_be_bytes(),
        );
        m.extend_from_slice(&rr.rdata);
    }
    m
}

/// The ordinary case: a NOERROR response to `qname`/`qtype`.
pub fn noerror(qname: &str, qtype: u16, answers: &[Rr]) -> Vec<u8> {
    message(qname, qtype, FLAGS_NOERROR, answers)
}
