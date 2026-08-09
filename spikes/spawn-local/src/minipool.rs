//! A miniature of `http_ng_native::pool`, with the same storage shape, so
//! the reaper question is asked about the real thing and not a strawman.
//!
//! `http-ng-native`'s pool is `Pool<I> { inner: Arc<Inner<I>> }` with
//! `Inner { idle: Mutex<HashMap<PoolKey, Vec<Idle<I>>>> }` (`pool.rs`
//! lines 310-372). That is reproduced here verbatim in shape; the key is
//! a `String` and the entry is the connection itself, because nothing in
//! the reaper question depends on `PoolKey`'s five components.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

pub struct Idle<I> {
    pub conn: I,
    /// `http-ng-native` stamps `R::Instant`; `std::time::Instant` is
    /// enough here and keeps the spike free of a `Timer` bound it does
    /// not need.
    pub deadline: Instant,
}

pub struct Inner<I> {
    pub idle: Mutex<HashMap<String, Vec<Idle<I>>>>,
}

/// Cheap to clone: an `Arc` bump. Same as the real one.
pub struct Pool<I> {
    inner: Arc<Inner<I>>,
}

impl<I> Clone for Pool<I> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<I> Default for Pool<I> {
    fn default() -> Self {
        Self::new()
    }
}

impl<I> Pool<I> {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                idle: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub fn put(&self, key: &str, conn: I, idle_timeout: Duration) {
        self.inner
            .idle
            .lock()
            .unwrap()
            .entry(key.to_owned())
            .or_default()
            .push(Idle {
                conn,
                deadline: Instant::now() + idle_timeout,
            });
    }

    pub fn len(&self) -> usize {
        self.inner.idle.lock().unwrap().values().map(Vec::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// **`Weak`, not `Arc`.** A reaper holding a strong reference would
    /// keep the pool — and therefore every socket in it — alive for as
    /// long as the reaper runs, which is forever. With a `Weak`, the
    /// moment the last `Native` is dropped the upgrade fails and the
    /// reaper returns.
    pub fn weak(&self) -> Weak<Inner<I>> {
        Arc::downgrade(&self.inner)
    }
}

/// One sweep. Returns how many entries it dropped, or `None` if the pool
/// is gone.
pub fn sweep<I>(pool: &Weak<Inner<I>>) -> Option<usize> {
    let inner = pool.upgrade()?;
    let now = Instant::now();
    let mut guard = inner.idle.lock().unwrap();
    let mut dropped = 0;
    guard.retain(|_, v| {
        let before = v.len();
        // Dropping the `Idle` drops the connection, which closes the
        // socket. That is the entire point of the exercise.
        v.retain(|e| e.deadline > now);
        dropped += before - v.len();
        !v.is_empty()
    });
    Some(dropped)
}
