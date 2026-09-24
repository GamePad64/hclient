//! Recording what the server asked for, without a handshake that can pause.
//!
//! rustls hands the `CertificateRequest`'s contents to
//! [`rustls::client::ResolvesClientCert::resolve`] and to nothing else —
//! there is no getter on `ClientConnection` for them afterwards, so the
//! only way to see them is to be the resolver. That is what [`Recording`]
//! is: it wraps whichever resolver the config already had, writes down
//! the question, and delegates the answer unchanged.
//!
//! **The hard part is that a resolver belongs to a `ClientConfig` and a
//! record belongs to a connection.** The config is shared — cloning one
//! is rustls' most expensive operation and this crate caches it per ALPN
//! precisely to avoid that — so the resolver cannot own the slot it
//! writes into. What it can do is write into whichever slot is installed
//! for the duration of the call, and `resolve` is only ever reached from
//! inside `ClientConnection::process_new_packets`, which is a
//! **synchronous call this crate makes**. So [`Installed`] scopes a slot
//! around that call the way a lock scopes a critical section.
//!
//! That is the same shape `hclient-tls-native-tls` uses to reach a waker
//! from a synchronous `Read`/`Write`, and here it costs no `unsafe`,
//! because rustls is sans-io: the value being scoped is an `Arc`, not a
//! borrowed `Context`, and the crate keeps `#![forbid(unsafe_code)]`.
//!
//! A poll that returns `Pending` mid-handshake resumes on whatever thread
//! polls next, which costs nothing here: the slot is re-installed on
//! every poll, so it is the *current* thread's for exactly as long as
//! rustls can call back into it.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use hclient_core::hooks::{ClientCertAsk, ClientCertRequest};
use rustls::SignatureScheme;
use rustls::client::ResolvesClientCert;
use rustls::sign::CertifiedKey;

/// Where a handshake's record lands. One per connection.
pub(crate) type Slot = Arc<Mutex<ClientCertAsk>>;

thread_local! {
    /// The slot [`Recording`] writes into, for the length of one scoped
    /// call. `None` outside one — a resolver reached by any other route
    /// records nothing rather than guessing whose connection it is on.
    static CURRENT: RefCell<Option<Slot>> = const { RefCell::new(None) };
}

/// Installs a slot for the length of a scope, restoring the previous one
/// on drop — **including on unwind**, which is why it is a guard and not
/// a pair of calls.
pub(crate) struct Installed(Option<Slot>);

impl Installed {
    pub(crate) fn new(slot: &Slot) -> Self {
        let previous = CURRENT.with(|c| c.borrow_mut().replace(Arc::clone(slot)));
        Self(previous)
    }
}

impl Drop for Installed {
    fn drop(&mut self) {
        CURRENT.with(|c| *c.borrow_mut() = self.0.take());
    }
}

/// A [`ResolvesClientCert`] that records the question and delegates the
/// answer.
///
/// **It never changes what is sent.** `resolve` returns the inner
/// resolver's answer verbatim, and `has_certs` and `only_raw_public_keys`
/// forward — so wrapping a config cannot alter a handshake, only observe
/// one. That matters because this wrap is unconditional: every `Rustls`
/// gets it, including one built from a caller's own config.
#[derive(Debug)]
pub(crate) struct Recording {
    inner: Arc<dyn ResolvesClientCert>,
}

impl Recording {
    pub(crate) fn wrap(inner: Arc<dyn ResolvesClientCert>) -> Arc<dyn ResolvesClientCert> {
        Arc::new(Self { inner })
    }
}

impl ResolvesClientCert for Recording {
    fn resolve(
        &self,
        root_hint_subjects: &[&[u8]],
        sigschemes: &[SignatureScheme],
    ) -> Option<Arc<CertifiedKey>> {
        let answer = self.inner.resolve(root_hint_subjects, sigschemes);
        CURRENT.with(|c| {
            let Some(slot) = c.borrow().clone() else {
                return;
            };
            let record = ClientCertRequest::new()
                .authority_names(root_hint_subjects.iter().map(|d| d.to_vec()).collect())
                // `u16` rather than `rustls::SignatureScheme`, because the
                // seam may not name a backend's type — the argument is
                // written on the field.
                .sigschemes(sigschemes.iter().map(|s| u16::from(*s)).collect())
                .answered(answer.is_some());
            // A poisoned mutex is not reachable: nothing panics while it
            // is held, and it is held only here and in the poll that
            // takes the record out.
            if let Ok(mut slot) = slot.lock() {
                *slot = ClientCertAsk::Asked(record);
            }
        });
        answer
    }

    fn has_certs(&self) -> bool {
        self.inner.has_certs()
    }

    fn only_raw_public_keys(&self) -> bool {
        self.inner.only_raw_public_keys()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn current() -> Option<Slot> {
        CURRENT.with(|c| c.borrow().clone())
    }

    /// **The guard restores what it replaced**, so a slot is installed for
    /// exactly one scope. Unobservable through a handshake today — rustls'
    /// client has no post-handshake authentication, so `resolve` is only
    /// ever reached inside a poll that installs its own slot — which is
    /// why it is pinned here rather than trusted: `TlsStream::poll_read`
    /// calls `process_new_packets` with nothing installed, and a guard that
    /// leaked would leave the last handshake's slot in place for it.
    #[test]
    fn a_slot_is_installed_for_one_scope_and_the_previous_one_comes_back() {
        assert!(current().is_none());
        let outer = Slot::default();
        let inner = Slot::default();
        {
            let _o = Installed::new(&outer);
            assert!(Arc::ptr_eq(&current().unwrap(), &outer));
            {
                let _i = Installed::new(&inner);
                assert!(Arc::ptr_eq(&current().unwrap(), &inner));
            }
            assert!(
                Arc::ptr_eq(&current().unwrap(), &outer),
                "leaving the inner scope restores the outer slot"
            );
        }
        assert!(
            current().is_none(),
            "and leaving the outer one restores none"
        );
    }
}
