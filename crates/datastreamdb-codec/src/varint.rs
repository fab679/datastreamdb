//! Unsigned LEB128 varints (Go `encoding/binary` `Uvarint` semantics).

#![warn(missing_docs)]

/// Maximum bytes one `u64` varint occupies.
pub const MAX_VARINT_BYTES: usize = 10;

/// Appends the varint encoding of `v` to `out`.
pub fn put_uvarint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        // Low 8 bits then the continuation flag (the 8th bit is OR'd anyway).
        #[allow(clippy::cast_possible_truncation, reason = "LEB128 low byte")]
        let byte = v as u8;
        out.push(byte | 0x80);
        v >>= 7;
    }
    // v < 0x80 here, so the cast is exact.
    #[allow(clippy::cast_possible_truncation, reason = "v < 0x80")]
    let byte = v as u8;
    out.push(byte);
}

/// Decodes one varint: `(value, bytes consumed)`, or `None` on empty,
/// truncated, or overflowing input.
#[must_use]
pub fn read_uvarint(buf: &[u8]) -> Option<(u64, usize)> {
    let mut value = 0u64;
    let mut shift = 0u32;
    for (i, &b) in buf.iter().enumerate().take(MAX_VARINT_BYTES) {
        if b < 0x80 {
            if i == 9 && b > 1 {
                return None; // overflow: exceeds 64 bits
            }
            return Some((value | (u64::from(b) << shift), i + 1));
        }
        value |= u64::from(b & 0x7F) << shift;
        shift += 7;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vectors_match_go_encoding() {
        for (value, expected) in [
            (0u64, vec![0x00]),
            (1, vec![0x01]),
            (127, vec![0x7F]),
            (128, vec![0x80, 0x01]),
            (300, vec![0xAC, 0x02]),
            (
                u64::MAX,
                vec![0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01],
            ),
        ] {
            let mut out = Vec::new();
            put_uvarint(&mut out, value);
            assert_eq!(out, expected, "value {value}");
            assert_eq!(read_uvarint(&out), Some((value, expected.len())));
        }
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(read_uvarint(&[]), None);
        assert_eq!(read_uvarint(&[0x80]), None); // truncated
        assert_eq!(read_uvarint(&[0xFF; 10]), None); // overflow
    }
}
