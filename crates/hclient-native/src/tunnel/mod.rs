//! Tunnels to a proxy spoken to over HTTP — what
//! [`hclient_proxy::egress::Dial::connect_tunnel`] lends a filter.
//!
//! One dedicated connection per tunnel: nothing here pools connections to
//! a proxy, and nothing here touches the transport's own pool, so the
//! settings an ordinary request's connection announces do not change.
//!
//! The HTTP/3 half lives in `http3::tunnel`, beside the rest of the code
//! that names quinn.

#[cfg(feature = "http2")]
pub(crate) mod h2;
