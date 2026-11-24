//! Criterion benches mirroring Go's chunker benchmarks (JSON parsing).

use criterion::{Criterion, criterion_group, criterion_main};
use datastreamdb_chunker::{NQuadBuffer, Op};

fn no_facets_payload() -> Vec<u8> {
    use std::fmt::Write as _;
    let mut out = String::from("[{\"uid\":123,\"flguid\":123,");
    for i in 0..120 {
        let _ = write!(out, "\"pred{i}\":\"xxxxxxxxxx\",");
    }
    out.push_str("\"contains\":{\"uid\":123,\"flguid\":123}}]");
    out.into_bytes()
}

fn bench_parse_json(c: &mut Criterion) {
    let json = no_facets_payload();
    c.bench_function("parse_json_no_facets", |b| {
        b.iter(|| {
            let mut buf = NQuadBuffer::new(1000);
            buf.parse_json(&json, Op::Set).expect("parse");
            std::hint::black_box(buf.pending().len());
        });
    });
}

criterion_group!(benches, bench_parse_json);
criterion_main!(benches);
