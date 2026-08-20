//! A client with no TLS stack and no resolver — for a target that has
//! `std` but no room for either.
//!
//! This example exists to be COMPILED, which is the whole claim: swapping
//! in `NoTls` and `IpLiteralOnly` drops rustls, native-tls, OpenSSL and
//! the system resolver out of the dependency graph, and the result is
//! still an ordinary `Transport`. Verify with
//! `cargo tree -p hclient-native -e normal`.
//!
//! What such a client can do: plain HTTP to an address the caller already
//! knows — `http://192.0.2.1:8080/`, `http://[2001:db8::1]/`. What it
//! cannot do it says so about, rather than discovering at runtime:
//! `Capabilities::tls_config` reads `TlsSupport::None`, and a hostname
//! comes back as `ErrorKind::Resolve` naming what is missing.

use hclient_core::{TlsSupport, unversioned::Transport};
use hclient_dns::IpLiteralOnly;
use hclient_native::Native;
use hclient_tls::NoTls;

fn main() {
    let transport = Native::new(hclient_rt_smol::Smol, NoTls, IpLiteralOnly);

    // The capability reflects the parts actually plugged in. Before
    // `TlsConnect::tls_support` existed, this line printed `Full` for a
    // client that refuses every TLS connection.
    assert_eq!(transport.capabilities().tls_config, TlsSupport::None);
    println!(
        "minimal client: tls_config = {:?}",
        transport.capabilities().tls_config
    );
}
