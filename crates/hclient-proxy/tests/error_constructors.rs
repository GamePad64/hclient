//! A filter or handshake written outside this crate reports a proxy's
//! refusal with the same types the built-in protocols use, so each one
//! whose fields are public for reading has a constructor for building.
//!
//! The types are `#[non_exhaustive]`, so a struct literal is refused from
//! here; these calls are the only way in, and this file is outside the
//! crate as far as visibility goes.

use hclient_proxy::{
    error::ProxyRefused, error::ProxySpokeFirst, error::Socks4Refused, error::Socks5HandshakeError,
    error::Socks5Refused,
};

#[test]
fn a_proxy_refusal_is_built_from_the_status_it_carries() {
    let e = ProxyRefused::new(http::StatusCode::PROXY_AUTHENTICATION_REQUIRED);
    assert_eq!(e.status, http::StatusCode::PROXY_AUTHENTICATION_REQUIRED);
    assert_eq!(
        e.to_string(),
        "the proxy refused CONNECT with 407 Proxy Authentication Required"
    );
}

#[test]
fn a_socks4_refusal_is_built_from_its_cd() {
    let e = Socks4Refused::new(92);
    assert_eq!(e.cd, 92);
    assert!(e.to_string().contains("identd unreachable"), "{e}");
}

#[test]
fn a_socks5_refusal_is_built_from_its_rep() {
    let e = Socks5Refused::new(0x05);
    assert_eq!(e.rep, 0x05);
    assert!(e.to_string().contains("connection refused"), "{e}");
}

#[test]
fn bytes_past_a_handshake_are_built_from_their_count() {
    let e = ProxySpokeFirst::new(7);
    assert_eq!(e.bytes, 7);
    assert!(e.to_string().starts_with("the proxy sent 7 bytes"), "{e}");
}

#[test]
fn a_socks5_handshake_error_compares_like_the_socks4_one() {
    assert_eq!(
        Socks5HandshakeError::BadVersion(4),
        Socks5HandshakeError::BadVersion(4)
    );
    assert_ne!(
        Socks5HandshakeError::BadVersion(4),
        Socks5HandshakeError::UnofferedMethod(4)
    );
}
