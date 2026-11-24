//! Group-varint coding for groups of 4 `u32` deltas.
//!
//! Byte-compatible with `dgryski/go-groupvarint` `Encode4`/`Decode4`: one
//! selector byte (2 bits per value = byte length minus one) followed by the
//! little-endian payload bytes concatenated.

#![warn(missing_docs)]

/// Maximum bytes one group occupies (1 selector + 4 × 4 payload).
pub const MAX_GROUP_BYTES: usize = 17;

/// Encodes 4 values into `dst` (must hold [`MAX_GROUP_BYTES`]).
/// Returns the bytes written.
///
/// # Panics
///
/// Panics when `dst` holds fewer than [`MAX_GROUP_BYTES`] bytes.
#[must_use]
pub fn encode4(src: [u32; 4], dst: &mut [u8]) -> usize {
    debug_assert!(dst.len() >= MAX_GROUP_BYTES);
    let mut selector = 0u8;
    let mut off = 1usize;
    for (i, &n) in src.iter().enumerate() {
        // `n | 1` forces 0 to encode as 1 byte, matching the Go version.
        let b = 3 - (n | 1).leading_zeros() / 8;
        // b is in 0..=3 by construction.
        #[allow(clippy::cast_possible_truncation, reason = "selector nibble fits u8")]
        let b = b as u8;
        selector |= b << (2 * i);
        let len = (b + 1) as usize;
        dst[off..off + len].copy_from_slice(&n.to_le_bytes()[..len]);
        off += len;
    }
    dst[0] = selector;
    off
}

/// Bytes one group occupies (selector + payload) for the given selector.
#[must_use]
pub const fn bytes_used(selector: u8) -> usize {
    let mut total = 1usize;
    let mut i = 0;
    while i < 4 {
        total += (((selector >> (2 * i)) & 3) + 1) as usize;
        i += 1;
    }
    total
}

/// Decodes 4 values. `src` must hold at least `bytes_used(src[0])` bytes.
///
/// # Panics
///
/// Panics on empty or truncated input.
pub fn decode4(dst: &mut [u32; 4], src: &[u8]) {
    let mut selector = src[0];
    let mut off = 1usize;
    for d in dst.iter_mut() {
        let len = ((selector & 3) + 1) as usize;
        let mut buf = [0u8; 4];
        buf[..len].copy_from_slice(&src[off..off + len]);
        *d = u32::from_le_bytes(buf);
        off += len;
        selector >>= 2;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_byte_group_is_verbatim() {
        let mut buf = [0u8; MAX_GROUP_BYTES];
        let n = encode4([1, 2, 3, 4], &mut buf);
        assert_eq!(n, 5);
        assert_eq!(&buf[..n], &[0x00, 1, 2, 3, 4]);
        assert_eq!(bytes_used(0x00), 5);
    }

    #[test]
    fn mixed_width_group_matches_spec() {
        // Widths 1,2,3,4 -> selector bits 00|01|10|11 = 0xE4.
        let mut buf = [0u8; MAX_GROUP_BYTES];
        let n = encode4([0x01, 0x0100, 0x01_0000, 0x0100_0000], &mut buf);
        assert_eq!(n, 11);
        assert_eq!(
            &buf[..n],
            &[
                0xE4, 0x01, 0x00, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01
            ]
        );
        assert_eq!(bytes_used(0xE4), 11);
    }

    #[test]
    fn zero_and_max_roundtrip() {
        let mut buf = [0u8; MAX_GROUP_BYTES];
        for values in [
            [0, 0, 0, 0],
            [u32::MAX, 0, 1, u32::MAX],
            [7, 700, 70_000, 7_000_000],
        ] {
            let n = encode4(values, &mut buf);
            assert_eq!(n, bytes_used(buf[0]));
            let mut out = [0u32; 4];
            decode4(&mut out, &buf[..n]);
            assert_eq!(out, values);
        }
    }

    #[test]
    fn roundtrips_powers_and_boundaries() {
        let mut buf = [0u8; MAX_GROUP_BYTES];
        // Sweep byte-width boundaries 0xFF/0x100, 0xFFFF/0x10000, ...
        let interesting = [
            0u32,
            1,
            0xFF,
            0x100,
            0xFFFF,
            0x1_0000,
            0xFF_FFFF,
            0x100_0000,
            u32::MAX,
        ];
        for (i, chunk) in interesting.chunks(4).enumerate() {
            let mut values = [u32::try_from(i).expect("test index fits"), 0, 0, 0];
            for (j, &v) in chunk.iter().enumerate() {
                values[j] = v;
            }
            let n = encode4(values, &mut buf);
            let mut out = [0u32; 4];
            decode4(&mut out, &buf[..n]);
            assert_eq!(out, values);
        }
    }
}
