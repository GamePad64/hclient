//! Tunnels to a proxy spoken to over HTTP — what
//! [`hclient_proxy::Dial::connect_tunnel`] lends a filter.
//!
//! One dedicated connection per tunnel: nothing here pools connections to
//! a proxy, and nothing here touches the transport's own pool, so the
//! settings an ordinary request's connection announces do not change.

#[cfg(feature = "http2")]
pub(crate) mod h2;
#[cfg(feature = "http3")]
pub(crate) mod h3;
