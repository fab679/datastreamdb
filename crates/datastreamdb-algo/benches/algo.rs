//! Criterion benches mirroring Go's algo benchmarks (merge + intersect).

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use datastreamdb_algo::uidlist::{internal_merge_sort, intersect_sorted, merge_sorted};
use datastreamdb_proto::List;

fn sorted_lists(lists: usize, len: usize) -> Vec<List> {
    // Deterministic stand-in for Go's unseeded math/rand data.
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    (0..lists)
        .map(|_| {
            let mut v: Vec<u64> = (0..len)
                .map(|_| {
                    x ^= x >> 12;
                    x ^= x << 25;
                    x ^= x >> 27;
                    x = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
                    x >> 1
                })
                .collect();
            v.sort_unstable();
            List { uids: v }
        })
        .collect()
}

fn benches(c: &mut Criterion) {
    let merge = sorted_lists(200, 100);
    c.bench_function("merge_sorted/200x100", |b| b.iter(|| merge_sorted(&merge)));
    c.bench_function("internal_merge_sort/200x100", |b| {
        b.iter(|| internal_merge_sort(&merge));
    });
    let narrow: Vec<List> = (0..4)
        .map(|i| List {
            uids: (i * 500..i * 500 + 2000).collect(),
        })
        .collect();
    c.bench_with_input(
        BenchmarkId::new("intersect_sorted", "4x2000"),
        &narrow,
        |b, lists| {
            b.iter(|| intersect_sorted(lists));
        },
    );
}

criterion_group!(algo, benches);
criterion_main!(algo);
