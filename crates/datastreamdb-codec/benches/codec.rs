//! Criterion benches mirroring Go `BenchmarkUidPack` (encode/decode × 128/256).

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use datastreamdb_codec::{decode, encode};

fn ascending(size: usize) -> Vec<u64> {
    // Deterministic stand-in for Go's unseeded math/rand walk.
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    let mut last = 0u64;
    (0..size)
        .map(|_| {
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            x = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
            last += x % 33;
            last
        })
        .collect()
}

fn benches(c: &mut Criterion) {
    let uids = ascending(200_000);
    for &block in &[128usize, 256] {
        c.bench_with_input(BenchmarkId::new("encode", block), &block, |b, &block| {
            b.iter(|| encode(&uids, block));
        });
        let pack = encode(&uids, block);
        c.bench_with_input(BenchmarkId::new("decode", block), &block, |b, _| {
            b.iter(|| decode(&pack, 0));
        });
    }
}

criterion_group!(codec, benches);
criterion_main!(codec);
