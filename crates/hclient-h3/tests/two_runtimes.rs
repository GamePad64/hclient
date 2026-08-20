//! HTTP/3 under two runtimes, from one generic body.
//!
//! The counterpart of `crates/hclient/tests/two_runtimes.rs`, which does
//! this for HTTP/1 over TCP, and it exists for the same reason: **"the
//! runtime seam is real" is one of the four things this project set out to
//! prove**, and until the UDP half had a second implementation the HTTP/3
//! path could not make that claim at all.
//!
//! `fetch_once<R>` below is the one body. It is instantiated twice — once
//! under `hclient_rt_tokio::TokioHandle` inside a real `tokio::runtime::
//! Runtime`, once under `hclient_rt_smol::Smol` on a bare
//! `futures_executor::block_on` — and there is no `#[cfg]` in it, no
//! boxing, and no bound naming either runtime. The file's only conditional
//! is the `#![cfg(not(target_family = "wasm"))]` that keeps its native
//! dev-dependencies off wasm targets.
//!
//! # Why one function and not two test files
//!
//! `docs/v03-design.md` §W1 states the trap in as many words: *"An h3 pair
//! written as two copies would prove that both compile and nothing else."*
//! What makes `two_runtimes.rs` worth what it is next door is that the
//! **same** function is instantiated twice, and that its sensitivity was
//! demonstrated rather than assumed.
//!
//! # It is sensitive, and here is the demonstration
//!
//! Adding `R::Instant: PartialEq<std::time::Instant>` to `fetch_once`'s
//! where-clause breaks instantiation on **one** runtime and not the other:
//! `TokioHandle`'s `Timer::Instant` is `tokio::time::Instant`, a wrapper
//! that derives `PartialEq<Self>` only, so tokio fails with `E0277: can't
//! compare tokio::time::Instant with std::time::Instant`, while `Smol`'s is
//! `std::time::Instant` itself and compiles. Run while writing this file,
//! and it is the same probe `hclient-rt-pair-check`'s `pair_property.rs`
//! and `hclient`'s `two_runtimes.rs` already use — so a green run here is
//! evidence about the seam, not merely about whether the file compiles.
#![cfg(not(target_family = "wasm"))]

mod server;

use hclient_core::RequestBody;
use hclient_core::unversioned::Transport;
use hclient_dns::IpLiteralOnly;
use hclient_h3::{H3, H3Runtime};
use http_body_util::BodyExt;
use server::Behaviour;
use std::fmt;

/// The one shared body: build the transport over `rt`, speak HTTP/3 to a
/// real QUIC server on loopback, twice on the same connection.
///
/// Two requests rather than one, because the second is what needs the
/// runtime for something the first does not: it is served from the pool,
/// which only exists because the connection's driver was **spawned**
/// through `R` and kept alive by a keep-alive timer built from `R`'s
/// `Timer`. A single request would exercise `UdpBind` and little else.
///
/// The three `where` lines below are `quinn`'s bounds, not this
/// workspace's, and they are repeated here for the same reason `H3`'s own
/// impl block repeats them: `R: H3Runtime` does not tell the compiler
/// anything about `R::Sleep` or `R::Socket`, because a supertrait list
/// cannot carry associated-type bounds on behalf of an implementer.
async fn fetch_once<R>(
    rt: R,
    cert: &rustls::pki_types::CertificateDer<'static>,
    addr: std::net::SocketAddr,
) -> usize
where
    R: H3Runtime,
    R::Sleep: Send + 'static,
    R::Socket: fmt::Debug + Send + Sync + 'static,
{
    let t = H3::new(rt, server::client_tls(cert), IpLiteralOnly).expect("H3::new does no I/O");

    let mut bodies = 0;
    for path in ["/one", "/two"] {
        let req = http::Request::builder()
            .uri(format!("https://{addr}{path}"))
            .body(RequestBody::Empty)
            .unwrap();
        let resp = t.execute(req).await.expect("h3 request");
        assert_eq!(resp.status(), 200);
        // Not incidental: an HTTP/1.1 or HTTP/2 client gets nothing at all
        // from this server, and `HTTP_3` is read off the response rather
        // than assumed from the crate name.
        assert_eq!(resp.version(), http::Version::HTTP_3);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&bytes[..], b"hello over h3");
        bodies += 1;
    }
    bodies
}

#[test]
fn http3_over_a_real_socket_under_tokio() {
    // A real `tokio::runtime::Runtime` built here rather than
    // `#[tokio::test]`, so that the two arms of this file are the same
    // shape: each one owns its executor and calls the same function.
    let s = server::start(Behaviour::Echo);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime");
    let bodies = rt.block_on(async {
        let handle = hclient_rt_tokio::TokioHandle::current().expect("inside block_on");
        fetch_once(handle, &s.cert_der, s.addr).await
    });
    assert_eq!(bodies, 2);
    assert_eq!(s.requests(), 2);
    assert_eq!(
        s.accepted(),
        1,
        "the second request must reuse the first connection"
    );
}

#[test]
fn http3_over_a_real_socket_under_smol() {
    // `futures_executor::block_on` — not smol's own executor, and not a
    // tokio runtime with a compatibility shim. The only smol machinery in
    // play is `async-io`'s reactor thread and the executor `Smol::spawn`
    // starts for the connection driver, both of which are this backend's
    // own business.
    let s = server::start(Behaviour::Echo);
    let bodies = futures_executor::block_on(fetch_once(hclient_rt_smol::Smol, &s.cert_der, s.addr));
    assert_eq!(bodies, 2);
    assert_eq!(s.requests(), 2);
    assert_eq!(
        s.accepted(),
        1,
        "the second request must reuse the first connection"
    );
}
