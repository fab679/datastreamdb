//! Sharded UID→value map. Port of `types/sharded_map.go`.
//!
//! Delta: `Merge` runs sequentially (Go fans out per shard); sharding still
//! keeps merges contention-friendly when callers parallelize externally.

#![warn(missing_docs)]

use std::collections::HashMap;

use super::value::Val;

/// Shard count (`key % NUM_SHARDS` selects the shard).
pub const NUM_SHARDS: usize = 30;

/// Thirty-shard map from UID to [`Val`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShardedMap {
    shards: Vec<HashMap<u64, Val>>,
}

impl ShardedMap {
    /// Empty map with [`NUM_SHARDS`] shards.
    #[must_use]
    pub fn new() -> Self {
        Self {
            shards: vec![HashMap::new(); NUM_SHARDS],
        }
    }

    /// Merges `other` into `self`, combining collisions with `aggregate`.
    pub fn merge(&mut self, other: &Self, mut aggregate: impl FnMut(&Val, &Val) -> Val) {
        for (ours, theirs) in self.shards.iter_mut().zip(other.shards.iter()) {
            for (key, value) in theirs {
                ours.entry(*key)
                    .and_modify(|current| *current = aggregate(current, value))
                    .or_insert_with(|| value.clone());
            }
        }
    }

    /// Whether the map holds no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.shards.iter().all(HashMap::is_empty)
    }

    /// Total entry count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.shards.iter().map(HashMap::len).sum()
    }

    /// Reads one shard by index (Go `GetShardOrNil`).
    #[must_use]
    pub fn shard(&self, index: usize) -> Option<&HashMap<u64, Val>> {
        self.shards.get(index)
    }

    /// Inserts or replaces `key`.
    pub fn set(&mut self, key: u64, value: Val) {
        self.shards[key as usize % NUM_SHARDS].insert(key, value);
    }

    /// Looks up `key`.
    #[must_use]
    pub fn get(&self, key: u64) -> Option<&Val> {
        self.shards[key as usize % NUM_SHARDS].get(&key)
    }

    /// Calls `f` for every entry, stopping on the first error.
    ///
    /// # Errors
    ///
    /// Returns the first error from `f`.
    pub fn iterate<E>(&self, mut f: impl FnMut(u64, &Val) -> Result<(), E>) -> Result<(), E> {
        for shard in &self.shards {
            for (key, value) in shard {
                f(*key, value)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TypeId, Value};

    fn val(v: i64) -> Val {
        Val {
            tid: TypeId::Int,
            value: Value::Int(v),
        }
    }

    #[test]
    fn roundtrips_and_merges() {
        let mut map = ShardedMap::new();
        assert!(map.is_empty());
        map.set(1, val(10));
        map.set(31, val(20)); // same shard as 1 (mod 30)
        assert_eq!(map.len(), 2);
        assert_eq!(map.get(31).map(|v| &v.value), Some(&Value::Int(20)));
        assert_eq!(map.get(2), None);

        let mut other = ShardedMap::new();
        other.set(1, val(5));
        other.set(2, val(7));
        map.merge(&other, |a, b| match (&a.value, &b.value) {
            (Value::Int(x), Value::Int(y)) => val(x + y),
            _ => a.clone(),
        });
        assert_eq!(map.get(1).map(|v| &v.value), Some(&Value::Int(15)));
        assert_eq!(map.len(), 3);

        let mut count = 0;
        map.iterate(|_, _| {
            count += 1;
            Ok::<_, ()>(())
        })
        .expect("iterate");
        assert_eq!(count, 3);
        assert!(map.shard(0).is_some() && map.shard(30).is_none());
    }
}
