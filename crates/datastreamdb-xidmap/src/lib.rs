//! External-ID to UID map (concurrent, persistent).
//!
//! Rust port of the upstream Go `xidmap` package (`xidmap.go`, `trie.go`). The scaffold
//! is an in-memory sharded map; the persistent trie and `datastreamdb-store` backing
//! follow per `TODO.md`.

#![warn(missing_docs)]

use std::{
    collections::HashMap,
    sync::atomic::{AtomicU64, Ordering},
};

use parking_lot::RwLock;

/// First UID handed out to external ids.
pub const FIRST_UID: u64 = 2;

/// Thread-safe external-id to UID map.
#[derive(Debug, Default)]
pub struct XidMap {
    inner: RwLock<HashMap<String, u64>>,
    next: AtomicU64,
}

impl XidMap {
    /// Empty map; UIDs start at [`FIRST_UID`].
    #[must_use]
    pub fn new() -> Self {
        Self::with_start(FIRST_UID)
    }

    /// Empty map; UIDs start at `start` (loaders seed this above the
    /// highest stored UID, the embedded stand-in for a Zero lease).
    #[must_use]
    pub fn with_start(start: u64) -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
            next: AtomicU64::new(start.max(FIRST_UID)),
        }
    }

    /// Looks up `xid`, assigning a fresh UID on first sight.
    pub fn get_or_assign(&self, xid: &str) -> u64 {
        if let Some(uid) = self.get(xid) {
            return uid;
        }
        let mut guard = self.inner.write();
        if let Some(uid) = guard.get(xid) {
            return *uid;
        }
        let uid = self.next.fetch_add(1, Ordering::Relaxed);
        guard.insert(xid.to_owned(), uid);
        uid
    }

    /// Looks up `xid` without assigning.
    #[must_use]
    pub fn get(&self, xid: &str) -> Option<u64> {
        self.inner.read().get(xid).copied()
    }

    /// Raises the allocation floor above `uid` (Go `alloc.BumpTo`):
    /// later assignments never reuse an explicit UID at or below it.
    pub fn bump_to(&self, uid: u64) {
        let _ = self
            .next
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                Some(next.max(uid.saturating_add(1)))
            });
    }

    /// Number of mapped external ids.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.read().len()
    }

    /// Whether the map holds no external ids.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.read().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assigns_stable_uids() {
        let map = XidMap::new();
        assert!(map.is_empty());
        let a = map.get_or_assign("alice");
        let b = map.get_or_assign("bob");
        assert_ne!(a, b);
        assert!(a >= FIRST_UID);
        assert_eq!(map.get_or_assign("alice"), a);
        assert_eq!(map.get("bob"), Some(b));
        assert_eq!(map.get("carol"), None);
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn concurrent_assigns_agree() {
        let map = std::sync::Arc::new(XidMap::new());
        let mut handles = Vec::new();
        for _ in 0..8 {
            let map = map.clone();
            handles.push(std::thread::spawn(move || {
                (0..200)
                    .map(|i| map.get_or_assign(&format!("xid-{i}")))
                    .collect::<Vec<_>>()
            }));
        }
        let results: Vec<Vec<u64>> = handles
            .into_iter()
            .map(|h| h.join().expect("join"))
            .collect();
        for row in &results[1..] {
            assert_eq!(row, &results[0]);
        }
        assert_eq!(map.len(), 200);
    }
}
