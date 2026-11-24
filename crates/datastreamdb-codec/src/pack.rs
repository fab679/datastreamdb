//! Blocked UID packing: [`Encoder`] writes delta-encoded [`UidPack`]s,
//! [`Decoder`] seeks and unpacks them. Direct port of `codec/codec.go`.
//!
//! Deltas from Go: no manual allocator (no `FreePack`; `alloc_ref` stays 0),
//! and corrupt/truncated delta bytes truncate the block instead of decoding
//! garbage.

#![warn(missing_docs)]

use datastreamdb_proto::{UidBlock, UidPack};

use super::groupvarint::{self, MAX_GROUP_BYTES};

/// Mask selecting the high 32 bits: a block never spans two MSB groups.
const MSB_MASK: u64 = 0xffff_ffff_0000_0000;

/// Seek anchor, mirroring `codec.SeekStart` / `codec.SeekCurrent`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeekPos {
    /// Include `uid` itself in the results.
    Start,
    /// Use `uid` as an offset; exclude it from the results.
    Current,
}

/// Packs ascending UIDs into a [`UidPack`].
#[derive(Debug, Default)]
pub struct Encoder {
    block_size: usize,
    pack: UidPack,
    uids: Vec<u64>,
}

impl Encoder {
    /// Encoder emitting blocks of at most `block_size` UIDs.
    #[must_use]
    pub fn new(block_size: usize) -> Self {
        // Go truncates here too; real sizes fit u32.
        #[allow(clippy::cast_possible_truncation, reason = "Go uint32() wrap")]
        let wire_size = block_size as u32;
        Self {
            block_size,
            pack: UidPack {
                block_size: wire_size,
                blocks: Vec::new(),
                alloc_ref: 0,
            },
            uids: Vec::new(),
        }
    }

    /// Adds one UID, flushing full blocks (or MSB changes) as needed.
    pub fn add(&mut self, uid: u64) {
        if let Some(&last) = self.uids.last() {
            if !match_32msb(last, uid) {
                self.pack_block();
            }
        }
        self.uids.push(uid);
        if self.uids.len() >= self.block_size {
            self.pack_block();
        }
    }

    /// Flushes the trailing block and returns the pack.
    #[must_use]
    pub fn finish(mut self) -> UidPack {
        self.pack_block();
        self.pack
    }

    fn pack_block(&mut self) {
        if self.uids.is_empty() {
            return;
        }
        let base = self.uids[0];
        let num_uids = self.uids.len();
        let mut last = base;
        let mut rest = &self.uids[1..];
        let mut deltas = Vec::new();
        let mut buf = [0u8; MAX_GROUP_BYTES];
        loop {
            let mut tmp = [0u32; 4];
            for (i, slot) in tmp.iter_mut().enumerate() {
                if let Some(&uid) = rest.get(i) {
                    // Go truncates deltas to uint32; sorted UIDs fit in practice.
                    #[allow(clippy::cast_possible_truncation, reason = "Go uint32() wrap")]
                    let delta = uid.wrapping_sub(last) as u32;
                    *slot = delta;
                    last = uid;
                }
            }
            let n = groupvarint::encode4(tmp, &mut buf);
            deltas.extend_from_slice(&buf[..n]);
            if rest.len() <= 4 {
                break;
            }
            rest = &rest[4..];
        }
        self.uids.clear();
        // Go truncates here too; real blocks fit u32.
        #[allow(clippy::cast_possible_truncation, reason = "Go uint32() wrap")]
        let num_uids = num_uids as u32;
        self.pack.blocks.push(UidBlock {
            base,
            deltas,
            num_uids,
        });
    }
}

/// Unpacks a [`UidPack`] block by block, with seeking.
#[derive(Debug)]
pub struct Decoder<'p> {
    pack: &'p UidPack,
    block_idx: usize,
    uids: Vec<u64>,
    start: usize,
}

impl<'p> Decoder<'p> {
    /// Decoder positioned at the first block, like `codec.NewDecoder`.
    #[must_use]
    pub fn new(pack: &'p UidPack) -> Self {
        let mut decoder = Self {
            pack,
            block_idx: 0,
            uids: Vec::new(),
            start: 0,
        };
        decoder.seek(0, SeekPos::Start);
        decoder
    }

    /// UIDs of the current block (after any seek truncation).
    #[must_use]
    pub fn uids(&self) -> &[u64] {
        &self.uids[self.start..]
    }

    /// The pack being decoded.
    #[must_use]
    pub fn pack(&self) -> &'p UidPack {
        self.pack
    }

    /// Decodes the current block in full.
    pub fn unpack_block(&mut self) -> &[u64] {
        self.uids.clear();
        self.start = 0;
        let Some(block) = self.pack.blocks.get(self.block_idx) else {
            return &[];
        };
        let mut last = block.base;
        self.uids.push(last);
        let mut tmp = [0u32; 4];
        let mut data = block.deltas.as_slice();
        while self.uids.len() < block.num_uids as usize {
            let Some(&selector) = data.first() else { break };
            let used = groupvarint::bytes_used(selector);
            if data.len() < used {
                break; // corrupt input: truncate (Go would decode garbage)
            }
            groupvarint::decode4(&mut tmp, data);
            data = &data[used..];
            for &delta in &tmp {
                last = last.wrapping_add(u64::from(delta));
                self.uids.push(last);
            }
        }
        self.uids.truncate(block.num_uids as usize);
        &self.uids
    }

    /// Approximate UIDs remaining: `block_size × blocks left`.
    #[must_use]
    pub fn approx_len(&self) -> usize {
        self.pack.block_size as usize * self.pack.blocks.len().saturating_sub(self.block_idx)
    }

    /// Seeks the block containing `uid` and returns the whole block.
    /// Faster than [`Decoder::seek`] when the caller intersects afterwards.
    pub fn seek_to_block(&mut self, uid: u64, whence: SeekPos) -> &[u64] {
        let prev = self.block_idx;
        self.block_idx = 0;
        if uid == 0 {
            return self.unpack_block();
        }
        let mut from = prev;
        // Past-end `prev` (or an older uid) restarts the search from 0; Go
        // would index out of bounds here.
        if from >= self.pack.blocks.len() || (from > 0 && uid < self.pack.blocks[from].base) {
            from = 0;
        }
        let idx = match whence {
            SeekPos::Start => self.pack.blocks[from..].partition_point(|b| b.base < uid) + from,
            SeekPos::Current => self.pack.blocks[from..].partition_point(|b| b.base <= uid) + from,
        };
        if idx == 0 {
            return self.unpack_block();
        }
        if idx < self.pack.blocks.len() && self.pack.blocks[idx].base == uid {
            self.block_idx = idx;
            return self.unpack_block();
        }
        self.block_idx = idx - 1;
        if self.block_idx != prev {
            self.unpack_block();
        }
        if let Some(&last) = self.uids.last() {
            if uid <= last {
                return &self.uids[self.start..];
            }
        }
        self.next()
    }

    /// Seeks `uid` and returns the block contents from that position.
    pub fn seek(&mut self, uid: u64, whence: SeekPos) -> &[u64] {
        self.block_idx = 0;
        if uid == 0 {
            return self.unpack_block();
        }
        let idx = match whence {
            SeekPos::Start => self.pack.blocks.partition_point(|b| b.base < uid),
            SeekPos::Current => self.pack.blocks.partition_point(|b| b.base <= uid),
        };
        if idx == 0 {
            return self.unpack_block();
        }
        if idx < self.pack.blocks.len() && self.pack.blocks[idx].base == uid {
            self.block_idx = idx;
            return self.unpack_block();
        }
        self.block_idx = idx - 1;
        self.unpack_block();
        let uidx = match whence {
            SeekPos::Start => self.uids.partition_point(|&u| u < uid),
            SeekPos::Current => self.uids.partition_point(|&u| u <= uid),
        };
        if uidx < self.uids.len() {
            self.start = uidx;
            return &self.uids[self.start..];
        }
        self.next()
    }

    /// Advances to the block whose base is below `seek` and unpacks it.
    /// Only moves forward; used for closest-position scans.
    pub fn linear_seek(&mut self, seek: u64) -> &[u64] {
        // Bounded: Go spins forever on `seek == u64::MAX`; we stop past the end.
        while self.block_idx < self.pack.blocks.len() && seek >= self.peek_next_base() {
            self.block_idx += 1;
        }
        self.unpack_block()
    }

    /// Base of the next block, or `u64::MAX` past the end.
    #[must_use]
    pub fn peek_next_base(&self) -> u64 {
        self.pack
            .blocks
            .get(self.block_idx + 1)
            .map_or(u64::MAX, |b| b.base)
    }

    /// Whether the decoder sits on a live block.
    #[must_use]
    pub fn valid(&self) -> bool {
        self.block_idx < self.pack.blocks.len()
    }

    /// Advances one block and unpacks it.
    #[allow(clippy::should_implement_trait, reason = "cursor API, not an iterator")]
    pub fn next(&mut self) -> &[u64] {
        self.block_idx += 1;
        self.unpack_block()
    }

    /// Index of the block currently being decoded.
    #[must_use]
    pub fn block_idx(&self) -> usize {
        self.block_idx
    }
}

/// Whether both UIDs share the high 32 bits (block-split rule).
#[must_use]
const fn match_32msb(a: u64, b: u64) -> bool {
    a & MSB_MASK == b & MSB_MASK
}

/// Packs `uids` into blocks of `block_size`.
#[must_use]
pub fn encode(uids: &[u64], block_size: usize) -> UidPack {
    let mut encoder = Encoder::new(block_size);
    for &uid in uids {
        encoder.add(uid);
    }
    encoder.finish()
}

/// Packs delta-uvarints from `buf` (inverse of [`decode_to_uvarints`]).
#[must_use]
pub fn encode_from_buffer(buf: &[u8], block_size: usize) -> UidPack {
    let mut encoder = Encoder::new(block_size);
    let mut prev = 0u64;
    let mut rest = buf;
    while !rest.is_empty() {
        let Some((delta, n)) = super::varint::read_uvarint(rest) else {
            break;
        };
        rest = &rest[n..];
        let next = prev.wrapping_add(delta);
        encoder.add(next);
        prev = next;
    }
    encoder.finish()
}

/// Approximate UID count: `block_size × block count`.
#[must_use]
pub fn approx_len(pack: &UidPack) -> usize {
    pack.blocks.len() * pack.block_size as usize
}

/// Exact UID count by summing block headers.
#[must_use]
pub fn exact_len(pack: &UidPack) -> usize {
    pack.blocks.iter().map(|b| b.num_uids as usize).sum()
}

/// Decodes the pack from `seek` (inclusive) into a UID vector.
#[must_use]
pub fn decode(pack: &UidPack, seek: u64) -> Vec<u64> {
    let mut out = Vec::with_capacity(approx_len(pack));
    let mut decoder = Decoder::new(pack);
    decoder.seek(seek, SeekPos::Start);
    loop {
        let uids = decoder.uids().to_vec();
        if uids.is_empty() {
            break;
        }
        out.extend_from_slice(&uids);
        decoder.next();
    }
    out
}

/// Decodes the pack into delta-uvarint bytes (see [`encode_from_buffer`]).
#[must_use]
pub fn decode_to_uvarints(pack: &UidPack) -> Vec<u8> {
    let mut out = Vec::new();
    let mut last = 0u64;
    let mut decoder = Decoder::new(pack);
    decoder.seek(0, SeekPos::Start);
    loop {
        let uids = decoder.uids().to_vec();
        if uids.is_empty() {
            break;
        }
        for uid in uids {
            super::varint::put_uvarint(&mut out, uid.wrapping_sub(last));
            last = uid;
        }
        decoder.next();
    }
    out
}

/// Deep-copies a pack.
#[must_use]
pub fn copy_uid_pack(pack: &UidPack) -> UidPack {
    pack.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic PRNG (xorshift64*) so vectors are stable.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    fn ascending(size: usize, rng: &mut Rng) -> Vec<u64> {
        let mut last = rng.below(100);
        let mut out = Vec::with_capacity(size);
        if size > 0 {
            out.push(last);
        }
        for _ in 1..size {
            last += rng.below(33);
            out.push(last);
        }
        out
    }

    #[test]
    fn empty_pack_edges() {
        let pack = encode(&[], 128);
        assert_eq!(approx_len(&pack), 0);
        assert!(decode(&pack, 0).is_empty());
        assert!(decode_to_uvarints(&pack).is_empty());
    }

    #[test]
    fn roundtrips_ascending_lists() {
        let mut rng = Rng(0x1234_5678);
        for &size in &[1usize, 2, 3, 5, 13, 100, 999, 10_000] {
            let expected = ascending(size, &mut rng);
            let pack = encode(&expected, 256);
            assert_eq!(expected.len(), exact_len(&pack));
            assert_eq!(expected, decode(&pack, 0));
        }
    }

    #[test]
    fn roundtrips_msb_spanning_lists() {
        // Port of TestEncoding: high-bit UIDs force MSB block splits.
        let big = [
            0xf000_0000_0000_0000u64,
            0xf00f_0000_0000_0000,
            0x00f0_0f00_0000_0000,
            0x000f_0f00_0000_0000,
            0x0f0f_0f0f_0000_0000,
        ];
        let mut rng = Rng(0xC0DE);
        for &len in &[0usize, 1, 2, 3, 5, 13, 18, 100, 99, 98] {
            let mut ints: Vec<u64> = (0..len)
                .map(|i| {
                    let r = rng.below(u64::from(u32::MAX));
                    if i < 50 { r } else { r + big[i % big.len()] }
                })
                .collect();
            ints.sort_unstable();
            let pack = encode(&ints, 256);
            assert_eq!(ints, decode(&pack, 0));
        }
    }

    #[test]
    fn buffer_roundtrip_matches_decoded_uids() {
        let mut rng = Rng(99);
        let expected = ascending(2000, &mut rng);
        let pack = encode(&expected, 256);
        let buf = decode_to_uvarints(&pack);
        let repacked = encode_from_buffer(&buf, 256);
        assert_eq!(exact_len(&pack), exact_len(&repacked));
        // Manual uvarint walk, mirroring TestBufferUidPack.
        let mut prev = 0u64;
        let mut rest = buf.as_slice();
        let mut uids = Vec::new();
        while !rest.is_empty() {
            let (delta, n) = super::super::varint::read_uvarint(rest).expect("varint");
            rest = &rest[n..];
            prev += delta;
            uids.push(prev);
        }
        assert_eq!(expected, uids);
    }

    fn seek_fixture() -> UidPack {
        // 0, 10, 20, ..., 10000 in blocks of 10 (port of TestSeek setup).
        encode(&(0..10001u64).step_by(10).collect::<Vec<_>>(), 10)
    }

    #[test]
    fn seek_table() {
        // Port of TestSeek.
        let pack = seek_fixture();
        let mut decoder = Decoder::new(&pack);
        let cases: &[(u64, Option<u64>, SeekPos)] = &[
            (0, Some(0), SeekPos::Start),
            (0, Some(0), SeekPos::Current),
            (100, Some(100), SeekPos::Start),
            (100, Some(110), SeekPos::Current),
            (1000, Some(1000), SeekPos::Start),
            (1000, Some(1010), SeekPos::Current),
            (1999, Some(2000), SeekPos::Start),
            (1999, Some(2000), SeekPos::Current),
            (1101, Some(1110), SeekPos::Start),
            (1101, Some(1110), SeekPos::Current),
            (10000, Some(10000), SeekPos::Start),
            (9999, Some(10000), SeekPos::Current),
            (10001, None, SeekPos::Start),
            (10001, None, SeekPos::Current),
            (u64::MAX, None, SeekPos::Start),
            (u64::MAX, None, SeekPos::Current),
        ];
        for &(input, expected, whence) in cases {
            let uids = decoder.seek(input, whence).to_vec();
            match expected {
                None => assert!(uids.is_empty(), "input {input}"),
                Some(first) => assert_eq!(uids[0], first, "input {input}"),
            }
        }
        decoder.seek(0, SeekPos::Start); // reset, like Go's blockIdx = 0
        for i in (100..10000).step_by(100) {
            assert!(decoder.linear_seek(i).contains(&i));
        }
    }

    #[test]
    fn linear_seek_is_forward_only() {
        // Port of TestLinearSeek.
        let pack = seek_fixture();
        let mut decoder = Decoder::new(&pack);
        for i in (0..20002).step_by(10) {
            let uids = decoder.linear_seek(i).to_vec();
            if i < 10001 {
                assert!(uids.contains(&i), "missing {i}");
            } else {
                assert!(!uids.contains(&i), "unexpected {i}");
            }
        }
        // Decoder now sits at the last block: earlier UIDs are gone.
        for i in (0..9990).step_by(10) {
            assert!(!decoder.linear_seek(i).contains(&i));
        }
    }

    #[test]
    fn decoder_sweep() {
        // Port of TestDecoder.
        let expected: Vec<u64> = (3..10001).step_by(3).collect();
        let pack = encode(&expected, 10);
        let mut decoder = Decoder::new(&pack);
        for &i in &expected {
            assert_eq!(decoder.seek(i, SeekPos::Start)[0], i);
            assert_eq!(decoder.seek(i - 1, SeekPos::Start)[0], i);
            assert_eq!(decoder.seek(i - 2, SeekPos::Start)[0], i);
            let start = (i / 3) as usize - 1;
            assert_eq!(expected[start..], decode(&pack, i));
        }
    }

    #[test]
    fn copy_uid_pack_roundtrips() {
        let pack = encode(&[1, 2, 3, 4, 5], 10);
        assert_eq!(decode(&pack, 0), decode(&copy_uid_pack(&pack), 0));
    }
}
