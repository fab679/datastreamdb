//! Count-min sketch for frequency estimation. Port of `algo/cm-sketch.go`
//! (itself from `BoomFilters`).
//!
//! The binary format is byte-compatible with Go's `WriteDataTo`:
//! LE `f64` epsilon, LE `f64` delta, LE `u64` count, then the matrix rows.

#![warn(missing_docs)]

use std::io::{Read, Write};

/// 64-bit FNV-1 (multiply-then-xor) matching Go's `fnv.New64`.
#[must_use]
pub fn fnv1_64(data: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0100_0000_01b3;
    let mut hash = OFFSET;
    for &b in data {
        hash = hash.wrapping_mul(PRIME);
        hash ^= u64::from(b);
    }
    hash
}

/// Count-min sketch over `u64` counters.
#[derive(Debug, Clone)]
pub struct CountMinSketch {
    matrix: Vec<u64>,
    width: usize,
    depth: usize,
    count: u64,
    epsilon: f64,
    delta: f64,
    hash: fn(&[u8]) -> u64,
}

/// Sketch merge/config error.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum SketchError {
    /// Merge requires equal depth.
    #[error("matrix depth must match")]
    DepthMismatch,
    /// Merge requires equal width.
    #[error("matrix width must match")]
    WidthMismatch,
    /// Serialized config differs from this sketch's config.
    #[error("expected cms values for epsilon {epsilon} and delta {delta}")]
    ConfigMismatch {
        /// Serialized epsilon.
        epsilon: f64,
        /// Serialized delta.
        delta: f64,
    },
    /// I/O failure while (de)serializing.
    #[error("sketch I/O: {0}")]
    Io(String),
}

impl CountMinSketch {
    /// Sketch accurate within `epsilon` with probability `delta`.
    #[must_use]
    pub fn new(epsilon: f64, delta: f64) -> Self {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "positive by construction; Go int()"
        )]
        let width = (std::f64::consts::E / epsilon).ceil() as usize;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "positive by construction; Go int()"
        )]
        let depth = (1.0 / delta).ln().ceil() as usize;
        Self {
            matrix: vec![0; width * depth],
            width,
            depth,
            count: 0,
            epsilon,
            delta,
            hash: fnv1_64,
        }
    }

    /// Relative-accuracy factor.
    #[must_use]
    pub fn epsilon(&self) -> f64 {
        self.epsilon
    }

    /// Relative-accuracy probability.
    #[must_use]
    pub fn delta(&self) -> f64 {
        self.delta
    }

    /// Items added (counting multiplicity).
    #[must_use]
    pub fn total_count(&self) -> u64 {
        self.count
    }

    /// Matrix dimensions `(depth, width)`.
    #[must_use]
    pub fn shape(&self) -> (usize, usize) {
        (self.depth, self.width)
    }

    fn positions(&self, data: &[u8]) -> impl Iterator<Item = usize> + '_ {
        let sum = (self.hash)(data);
        // NOTE: Go's hashKernel names are swapped (upper holds the LOW word);
        // this matches the effective index math, not the names.
        let lo = (sum & 0xffff_ffff) as usize;
        let hi = ((sum >> 32) & 0xffff_ffff) as usize;
        (0..self.depth).map(move |i| i * self.width + (lo + hi * i) % self.width)
    }

    /// Adds `data` once. Returns `self` for chaining.
    pub fn add(&mut self, data: &[u8]) -> &mut Self {
        let positions: Vec<usize> = self.positions(data).collect();
        for pos in positions {
            self.matrix[pos] += 1;
        }
        self.count += 1;
        self
    }

    /// Raises `data`'s counters to at least `n`. Returns `self` for chaining.
    pub fn add_int(&mut self, data: &[u8], n: u64) -> &mut Self {
        let positions: Vec<usize> = self.positions(data).collect();
        let existing = positions.iter().map(|&p| self.matrix[p]).min().unwrap_or(0);
        for p in positions {
            self.matrix[p] = self.matrix[p].max(n);
        }
        if n > existing {
            self.count += n - existing;
        }
        self
    }

    /// Estimated count for `data`.
    #[must_use]
    pub fn count(&self, data: &[u8]) -> u64 {
        self.positions(data)
            .map(|p| self.matrix[p])
            .min()
            .unwrap_or(0)
    }

    /// Adds `other`'s counters into this sketch.
    ///
    /// # Errors
    ///
    /// Returns [`SketchError`] when matrix shapes differ.
    pub fn merge(&mut self, other: &Self) -> Result<(), SketchError> {
        if self.depth != other.depth {
            return Err(SketchError::DepthMismatch);
        }
        if self.width != other.width {
            return Err(SketchError::WidthMismatch);
        }
        for (a, b) in self.matrix.iter_mut().zip(other.matrix.iter()) {
            *a += *b;
        }
        self.count += other.count;
        Ok(())
    }

    /// Zeroes all counters. Returns `self` for chaining.
    pub fn reset(&mut self) -> &mut Self {
        self.matrix.fill(0);
        self.count = 0;
        self
    }

    /// Replaces the hash kernel (default [`fnv1_64`]).
    pub fn set_hash(&mut self, hash: fn(&[u8]) -> u64) {
        self.hash = hash;
    }

    /// Serializes in Go `WriteDataTo` format. Returns bytes written.
    ///
    /// # Errors
    ///
    /// Returns [`SketchError::Io`] on write failure.
    pub fn write_to(&self, stream: &mut impl Write) -> Result<usize, SketchError> {
        let mut buf = Vec::with_capacity(24 + self.matrix.len() * 8);
        buf.extend_from_slice(&self.epsilon.to_le_bytes());
        buf.extend_from_slice(&self.delta.to_le_bytes());
        buf.extend_from_slice(&self.count.to_le_bytes());
        for cell in &self.matrix {
            buf.extend_from_slice(&cell.to_le_bytes());
        }
        stream
            .write_all(&buf)
            .map_err(|e| SketchError::Io(e.to_string()))?;
        Ok(buf.len())
    }

    /// Reads Go `WriteDataTo` format into this sketch (config must match).
    /// Returns bytes read.
    ///
    /// # Errors
    ///
    /// Returns [`SketchError`] on config mismatch or truncated input.
    pub fn read_from(&mut self, stream: &mut impl Read) -> Result<usize, SketchError> {
        fn read_f64(stream: &mut impl Read) -> Result<f64, SketchError> {
            let mut buf = [0u8; 8];
            stream
                .read_exact(&mut buf)
                .map_err(|e| SketchError::Io(e.to_string()))?;
            Ok(f64::from_le_bytes(buf))
        }
        fn read_u64(stream: &mut impl Read) -> Result<u64, SketchError> {
            let mut buf = [0u8; 8];
            stream
                .read_exact(&mut buf)
                .map_err(|e| SketchError::Io(e.to_string()))?;
            Ok(u64::from_le_bytes(buf))
        }
        let epsilon = read_f64(stream)?;
        let delta = read_f64(stream)?;
        #[allow(clippy::float_cmp, reason = "wire config must match exactly")]
        if self.epsilon != epsilon || self.delta != delta {
            return Err(SketchError::ConfigMismatch { epsilon, delta });
        }
        let count = read_u64(stream)?;
        for cell in &mut self.matrix {
            *cell = read_u64(stream)?;
        }
        self.count = count;
        Ok(self.depth * self.width * 8 + 8 + 2 * 8)
    }

    /// Removes `n` occurrences when at least `n` are present.
    pub fn test_and_remove(&mut self, data: &[u8], n: u64) -> bool {
        let positions: Vec<usize> = self.positions(data).collect();
        let min = positions.iter().map(|&p| self.matrix[p]).min().unwrap_or(0);
        if n > min {
            return false;
        }
        for p in positions {
            self.matrix[p] -= n;
        }
        true
    }

    /// Removes all occurrences; `false` when the count was already 0.
    pub fn test_and_remove_all(&mut self, data: &[u8]) -> bool {
        let positions: Vec<usize> = self.positions(data).collect();
        let min = positions.iter().map(|&p| self.matrix[p]).min().unwrap_or(0);
        if min == 0 {
            return false;
        }
        for p in positions {
            self.matrix[p] -= min;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1_matches_structure() {
        // Empty input is the offset basis; single byte pins multiply-then-xor.
        assert_eq!(fnv1_64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(
            fnv1_64(b"a"),
            0xcbf2_9ce4_8422_2325u64.wrapping_mul(0x0100_0000_01b3) ^ 0x61
        );
        assert_ne!(fnv1_64(b"a"), fnv1_64(b"b"));
    }

    #[test]
    fn counts_and_totals() {
        let mut sketch = CountMinSketch::new(0.01, 0.99);
        assert_eq!(sketch.total_count(), 0);
        sketch.add(b"a").add(b"a").add(b"b");
        assert_eq!(sketch.count(b"a"), 2);
        assert!(sketch.count(b"b") >= 1);
        assert_eq!(sketch.total_count(), 3);
        sketch.add_int(b"c", 10);
        assert_eq!(sketch.count(b"c"), 10);
        assert_eq!(sketch.total_count(), 13);
    }

    #[test]
    fn merge_reset_and_remove() {
        let mut a = CountMinSketch::new(0.01, 0.99);
        let mut b = CountMinSketch::new(0.01, 0.99);
        a.add(b"x");
        b.add(b"x").add(b"x");
        a.merge(&b).expect("merge");
        assert_eq!(a.count(b"x"), 3);
        assert!(a.test_and_remove(b"x", 2));
        assert_eq!(a.count(b"x"), 1);
        assert!(!a.test_and_remove(b"x", 5));
        assert!(a.test_and_remove_all(b"x"));
        assert_eq!(a.count(b"x"), 0);
        assert!(!a.test_and_remove_all(b"x"));
        a.reset();
        assert_eq!(a.total_count(), 0);

        let mut other = CountMinSketch::new(0.001, 0.99);
        assert_eq!(a.merge(&other), Err(SketchError::WidthMismatch));
        // Same width (same epsilon), deeper matrix (delta 0.36 -> depth 2).
        other = CountMinSketch::new(0.01, 0.36);
        assert_eq!(other.shape().1, a.shape().1);
        assert_eq!(a.merge(&other), Err(SketchError::DepthMismatch));
    }

    #[test]
    fn binary_format_roundtrips() {
        let mut sketch = CountMinSketch::new(0.01, 0.99);
        sketch.add(b"hello").add(b"world");
        let mut buf = Vec::new();
        let written = sketch.write_to(&mut buf).expect("write");
        let (depth, width) = sketch.shape();
        assert_eq!(written, depth * width * 8 + 24);
        // Header pins the Go layout: epsilon, delta, count.
        assert_eq!(&buf[..8], &0.01f64.to_le_bytes());
        assert_eq!(&buf[8..16], &0.99f64.to_le_bytes());
        assert_eq!(&buf[16..24], &2u64.to_le_bytes());

        let mut back = CountMinSketch::new(0.01, 0.99);
        let read = back.read_from(&mut buf.as_slice()).expect("read");
        assert_eq!(read, written);
        assert_eq!(back.count(b"hello"), 1);
        assert_eq!(back.total_count(), 2);

        let mut wrong = CountMinSketch::new(0.001, 0.99);
        assert!(wrong.read_from(&mut buf.as_slice()).is_err());
    }
}
