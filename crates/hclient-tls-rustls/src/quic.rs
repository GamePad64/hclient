//! `QuicTlsConnect` on rustls, behind the `quic` feature.
//!
//! The only implementation of that trait in this workspace, and — unlike
//! [`TlsConnect`](hclient_tls::TlsConnect), which has three — the only one
//! there can be for now: `native-tls` binds no QUIC API at any level, so
//! for HTTP/3 it is a compile error rather than a weaker backend. See
//! `hclient-tls-quic`'s module doc.
//!
//! # Two things this module does that the TCP path does not, and why
//!
//! **A session store of its own.** rustls keeps resumption tickets in the
//! `ClientConfig` (`Resumption { store: Arc<dyn ClientSessionStore>, .. }`),
//! and [`Rustls`] holds exactly one `Arc<ClientConfig>` — so one `Rustls`
//! value is one ticket cache, scoped to one [`TlsConfigId`], which is
//! already the identity v0.2 W2 put in the connection pool's key. That fit
//! is real and nothing about it needed redesigning.
//!
//! What does not carry over is the keying. `ClientSessionStore`'s methods
//! are keyed by `ServerName` **alone**, while a TLS 1.3 ticket issued over
//! QUIC also carries `quic_params`, which rustls sets only on QUIC
//! connections and reads back unconditionally when resuming one. One
//! `Rustls` serving both an h1/h2 TCP path and an h3 QUIC path would
//! therefore have one slot per host for two kinds of ticket. What actually
//! happens when a TCP-issued ticket is offered to a QUIC handshake is
//! **unverified**. This module does not answer the question; it removes
//! it, for
//! the price of one `Arc`.
//!
//! **A second cache dimension.** `enable_early_data` is a field on
//! `rustls::ClientConfig`, i.e. per config, while
//! [`QuicTlsRequest::early_data`] is per connection. So the config cache is
//! keyed by `(alpn, early_data)` rather than by ALPN alone. Cheap, but it
//! is a change rather than a read of something that was already there.

use crate::Rustls;
use hclient_core::{Error, ErrorKind};
use hclient_tls::quic::{QuicTlsConnect, QuicTlsRequest};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// The QUIC-side config cache, hung off a [`Rustls`] value.
///
/// Not fields on `Rustls` itself: they would exist in every build, feature
/// or no feature, and this crate's whole shape is that a build pays for
/// what it uses. `OnceLock` because the first QUIC connection is the first
/// time any of it is needed, and a client that never speaks h3 allocates
/// nothing.
/// The config cache's key: the ALPN set this config proposes, and whether
/// it enables early data. Two dimensions, not one, because
/// `enable_early_data` lives on `rustls::ClientConfig` while
/// [`QuicTlsRequest::early_data`] is per connection.
type QuicConfigKey = (Vec<Vec<u8>>, bool);

#[derive(Debug, Default)]
pub(crate) struct QuicState {
    /// Keyed by `(alpn, early_data)` — see the module doc.
    configs: Mutex<HashMap<QuicConfigKey, Arc<rustls::ClientConfig>>>,
    /// The QUIC path's own ticket store, deliberately not the one the TCP
    /// path uses. See the module doc.
    store: OnceLock<Arc<dyn rustls::client::ClientSessionStore>>,
}

impl QuicState {
    fn store(&self) -> Arc<dyn rustls::client::ClientSessionStore> {
        self.store
            .get_or_init(|| Arc::new(rustls::client::ClientSessionMemoryCache::new(256)))
            .clone()
    }
}

impl QuicTlsConnect for Rustls {
    fn quic_client_config(
        &self,
        req: QuicTlsRequest<'_>,
    ) -> Result<Arc<dyn quinn_proto::crypto::ClientConfig>, Error> {
        if req.ech.is_some() {
            // A typed refusal rather than a silent drop. rustls builds ECH
            // into the `ClientConfig` through a different builder entry
            // point (`builder_with_provider(..).with_ech(..)`), so honouring
            // it here is a third cache dimension and a second construction
            // path — real work, not a line. Until that is written, an ECH
            // config that arrived from a DNS answer must not be quietly
            // dropped: a caller who asked for encrypted SNI and did not get
            // it is worse off than one who was told no.
            return Err(Error::new(
                ErrorKind::Tls,
                std::io::Error::other(
                    "hclient-tls-rustls does not yet apply an ECH config on the QUIC path; \
                     the connection would have sent the server name in the clear",
                ),
            ));
        }
        let cfg = self.quic_config_for(req.alpn, req.early_data, req.identity);
        let quic = quinn_proto::crypto::rustls::QuicClientConfig::try_from(cfg)
            .map_err(|e| Error::new(ErrorKind::Tls, e))?;
        Ok(Arc::new(quic))
    }

    /// `true`: rustls has `enable_early_data`, this module sets it when
    /// asked, and the ticket store above is what makes a second visit able
    /// to use it.
    ///
    /// This says the backend can *offer* early data. It says nothing about
    /// whether any particular request's early data was accepted — in QUIC
    /// that verdict arrives after the response, so it cannot be a property
    /// of a connector. See `hclient_h3`'s early-data module.
    fn offers_early_data(&self) -> bool {
        true
    }
}

impl Rustls {
    fn quic_config_for(
        &self,
        alpn: &[&[u8]],
        early_data: bool,
        identity: Option<&str>,
    ) -> Arc<rustls::ClientConfig> {
        // **The QUIC half of the client-identity seam, and it is where
        // this implementation nearly repeated the mistake its own design
        // document warns about.** Adding `identity` to `QuicTlsRequest`
        // made the *transport* fail to compile; a backend that took the
        // field and ignored it fails nothing at all, and the result is a
        // certificate presented over TCP and silently omitted over QUIC.
        //
        // Uncached, for `config_for`'s reason one file over: the cache
        // exists for a path taken on every connection, and a named
        // identity is asked for.
        if let Some(name) = identity {
            let Some(cfg) = self.config_for_identity(name) else {
                return self.quic_config_for(alpn, early_data, None);
            };
            let state = self.quic.get_or_init(QuicState::default);
            let mut cfg = (*cfg).clone();
            cfg.alpn_protocols = alpn.iter().map(|a| a.to_vec()).collect();
            cfg.resumption = rustls::client::Resumption::store(state.store());
            cfg.enable_early_data = early_data;
            return Arc::new(cfg);
        }
        let key: Vec<Vec<u8>> = alpn.iter().map(|a| a.to_vec()).collect();
        let state = self.quic.get_or_init(QuicState::default);
        let mut cache = state.configs.lock().expect("quic config cache poisoned");
        cache
            .entry((key.clone(), early_data))
            .or_insert_with(|| {
                let mut cfg = (*self.base).clone();
                cfg.alpn_protocols = key;
                // The QUIC path's own store, replacing whatever the base
                // config carried — including the one the TCP path is using.
                cfg.resumption = rustls::client::Resumption::store(state.store());
                // Off unless asked, which is rustls's own default and the
                // right one: early data is replayable, so it is never
                // something a client ends up offering because nobody said
                // otherwise.
                cfg.enable_early_data = early_data;
                Arc::new(cfg)
            })
            .clone()
    }
}
