//! Versioned posting storage over a [`datastreamdb_store::Backend`].
//!
//! Go keeps one Badger key per posting list and relies on Badger's MVCC:
//! every commit lands as a new *version* of the key tagged with a `UserMeta`
//! bit (`BitDeltaPosting`, `BitCompletePosting`, `BitEmptyPosting`), and
//! reads iterate versions newest-first. Our backends are single-version, so
//! the posting layer owns versioning explicitly: each key holds one
//! [`StoredChain`] value — an optional rolled-up base plus the delta
//! [`PostingList`]s above it, newest first.
//!
//! The read path filters by `read_ts` exactly like a Badger transaction at
//! that timestamp would: deltas with `commit_ts > read_ts` are invisible,
//! and a base with `base_ts > read_ts` rejects the read (`minTs`; Go reports
//! the same once a rollup discards older versions). Commits serialize on the
//! engine commit mutex, so read-modify-write cycles on a chain are safe.

#![warn(missing_docs)]

use datastreamdb_proto::PostingList;
use datastreamdb_store::Backend;
use prost::Message as _;

use crate::errors::Result;

/// Storage partition holding every posting chain.
pub const PARTITION: &str = "posting";

/// Encoding version of [`StoredChain`]. Bumped on layout changes.
pub const CHAIN_VERSION: u32 = 1;

/// Rolled-up base of a version chain.
#[derive(Clone, PartialEq, prost::Message)]
pub struct StoredBase {
    /// Commit timestamp the base is visible from (`minTs`).
    #[prost(uint64, tag = "1")]
    pub commit_ts: u64,
    /// Base is an explicit empty marker (`BitEmptyPosting`).
    #[prost(bool, tag = "2")]
    pub empty: bool,
    /// Rolled-up list (absent when `empty`).
    #[prost(message, optional, tag = "3")]
    pub plist: Option<PostingList>,
}

/// One committed delta above the base.
#[derive(Clone, PartialEq, prost::Message)]
pub struct StoredDelta {
    /// Commit timestamp of the delta.
    #[prost(uint64, tag = "1")]
    pub commit_ts: u64,
    /// Delta list.
    #[prost(message, optional, tag = "2")]
    pub plist: Option<PostingList>,
}

/// Version chain stored under one posting key.
#[derive(Clone, PartialEq, prost::Message)]
pub struct StoredChain {
    /// Encoding version ([`CHAIN_VERSION`]).
    #[prost(uint32, tag = "1")]
    pub version: u32,
    /// Rolled-up base, if any.
    #[prost(message, optional, tag = "2")]
    pub base: Option<StoredBase>,
    /// Deltas above the base, newest first.
    #[prost(message, repeated, tag = "3")]
    pub deltas: Vec<StoredDelta>,
}

impl StoredChain {
    /// Highest timestamp visible in the chain (base or any delta).
    #[must_use]
    pub fn max_ts(&self) -> u64 {
        let mut max = self.base.as_ref().map_or(0, |b| b.commit_ts);
        for d in &self.deltas {
            max = max.max(d.commit_ts);
        }
        max
    }

    /// Number of deltas above the base.
    #[must_use]
    pub fn delta_count(&self) -> usize {
        self.deltas.len()
    }
}

/// Prost encode/decode helpers local to this module.
trait ProstMessage: prost::Message + Default {
    /// Encodes the message to bytes.
    fn encode_vec(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(self.encoded_len());
        self.encode(&mut buf).expect("prost encode");
        buf
    }
}

impl ProstMessage for StoredChain {}

/// Reads the version chain stored under `key` (`None` when absent).
///
/// # Errors
///
/// Returns [`PostingError`](crate::PostingError) on backend failure or when
/// the stored bytes do not decode or carry an unknown version.
pub fn read_chain(backend: &dyn Backend, key: &[u8]) -> Result<Option<StoredChain>> {
    let Some(bytes) = backend.get(PARTITION, key)? else {
        return Ok(None);
    };
    let chain = StoredChain::decode(bytes.as_slice())?;
    if chain.version != CHAIN_VERSION {
        return Err(crate::PostingError::Corrupt(format!(
            "unsupported chain version {}",
            chain.version
        )));
    }
    Ok(Some(chain))
}

/// Writes `chain` under `key`.
///
/// # Errors
///
/// Returns [`PostingError`](crate::PostingError) on backend failure.
pub fn write_chain(backend: &dyn Backend, key: &[u8], chain: &StoredChain) -> Result<()> {
    backend.put(PARTITION, key, &chain.encode_vec())?;
    Ok(())
}

/// Appends committed deltas to the chain under `key` (read-modify-write).
///
/// Deltas are `(commit_ts, list)` pairs. A delta whose timestamp already
/// exists in the chain replaces the stored one (Badger-version overwrite
/// semantics, which keeps Raft/WAL replay idempotent). The chain stays
/// newest-first. Callers must hold the engine commit mutex.
///
/// # Errors
///
/// Returns [`PostingError`](crate::PostingError) on backend failure.
pub fn append_deltas(
    backend: &dyn Backend,
    key: &[u8],
    deltas: &[(u64, PostingList)],
) -> Result<()> {
    if deltas.is_empty() {
        return Ok(());
    }
    let mut chain = read_chain(backend, key)?.unwrap_or(StoredChain {
        version: CHAIN_VERSION,
        base: None,
        deltas: Vec::new(),
    });
    for (commit_ts, plist) in deltas {
        let mut plist = plist.clone();
        plist.commit_ts = *commit_ts;
        match chain.deltas.iter_mut().find(|d| d.commit_ts == *commit_ts) {
            Some(existing) => existing.plist = Some(plist),
            None => chain.deltas.push(StoredDelta {
                commit_ts: *commit_ts,
                plist: Some(plist),
            }),
        }
    }
    chain
        .deltas
        .sort_by_key(|delta| std::cmp::Reverse(delta.commit_ts));
    write_chain(backend, key, &chain)
}

/// Replaces the chain under `key` with a rolled-up base.
///
/// `plist` is `None` for an explicit empty base (`BitEmptyPosting`). Deltas
/// newer than `base_ts` survive above the base, mirroring the Badger versions
/// a rollup leaves behind. Callers must hold the engine commit mutex.
///
/// # Errors
///
/// Returns [`PostingError`](crate::PostingError) on backend failure.
pub fn write_base(
    backend: &dyn Backend,
    key: &[u8],
    base_ts: u64,
    plist: Option<PostingList>,
) -> Result<()> {
    let mut chain = read_chain(backend, key)?.unwrap_or(StoredChain {
        version: CHAIN_VERSION,
        base: None,
        deltas: Vec::new(),
    });
    chain.deltas.retain(|d| d.commit_ts > base_ts);
    chain.base = Some(StoredBase {
        commit_ts: base_ts,
        empty: plist.is_none(),
        plist,
    });
    write_chain(backend, key, &chain)
}

/// Deletes the chain under `key` (drop flows). Missing keys are a no-op.
///
/// # Errors
///
/// Returns [`PostingError`](crate::PostingError) on backend failure.
pub fn delete_chain(backend: &dyn Backend, key: &[u8]) -> Result<()> {
    backend.delete(PARTITION, key)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use datastreamdb_proto::Posting;
    use datastreamdb_store::MemoryBackend;

    fn delta_plist(uid: u64) -> PostingList {
        PostingList {
            pack: None,
            postings: vec![Posting {
                uid,
                ..Posting::default()
            }],
            commit_ts: 0,
            splits: vec![],
        }
    }

    #[test]
    fn missing_key_reads_none() {
        let backend = MemoryBackend::new();
        assert!(read_chain(&backend, b"k").expect("read").is_none());
    }

    #[test]
    fn append_orders_newest_first_and_replaces() {
        let backend = MemoryBackend::new();
        append_deltas(&backend, b"k", &[(5, delta_plist(1)), (9, delta_plist(2))]).expect("append");
        append_deltas(&backend, b"k", &[(7, delta_plist(3))]).expect("append");
        // Same-ts replay replaces instead of duplicating.
        append_deltas(&backend, b"k", &[(7, delta_plist(4))]).expect("append");
        let chain = read_chain(&backend, b"k").expect("read").expect("some");
        let ts: Vec<u64> = chain.deltas.iter().map(|d| d.commit_ts).collect();
        assert_eq!(ts, [9, 7, 5]);
        assert_eq!(
            chain.deltas[1].plist.as_ref().expect("pl").postings[0].uid,
            4
        );
        assert_eq!(chain.max_ts(), 9);
        assert_eq!(chain.delta_count(), 3);
    }

    #[test]
    fn base_keeps_newer_deltas() {
        let backend = MemoryBackend::new();
        append_deltas(&backend, b"k", &[(5, delta_plist(1)), (9, delta_plist(2))]).expect("append");
        write_base(&backend, b"k", 7, Some(delta_plist(0))).expect("base");
        let chain = read_chain(&backend, b"k").expect("read").expect("some");
        assert_eq!(chain.base.as_ref().expect("base").commit_ts, 7);
        assert!(!chain.base.as_ref().expect("base").empty);
        let ts: Vec<u64> = chain.deltas.iter().map(|d| d.commit_ts).collect();
        assert_eq!(ts, [9]);
    }

    #[test]
    fn empty_base_roundtrips() {
        let backend = MemoryBackend::new();
        write_base(&backend, b"k", 3, None).expect("base");
        let chain = read_chain(&backend, b"k").expect("read").expect("some");
        let base = chain.base.expect("base");
        assert!(base.empty && base.plist.is_none());
    }

    #[test]
    fn corrupt_bytes_error() {
        let backend = MemoryBackend::new();
        backend.put(PARTITION, b"k", b"\xff\xff\xff").expect("put");
        assert!(read_chain(&backend, b"k").is_err());
        delete_chain(&backend, b"k").expect("delete");
        assert!(read_chain(&backend, b"k").expect("read").is_none());
    }
}
