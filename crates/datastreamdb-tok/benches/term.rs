//! Criterion benches mirroring Go's tokenizer benchmarks.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use datastreamdb_tok::text::TermTokenizer;
use datastreamdb_tok::tok::Tokenizer;
use datastreamdb_types::{TypeId, Val, Value};

fn benches(c: &mut Criterion) {
    let tokenizer = TermTokenizer::default();
    for text in [
        "the quick brown fox jumps over the lazy dog",
        "Lorem ipsum dolor sit amet, consectetur adipiscing elit",
        "a b c d e f g h i j k l m n o p q r s t u v w x y z",
    ] {
        let value = Val {
            tid: TypeId::String,
            value: Value::String(text.to_owned()),
        };
        c.bench_with_input(BenchmarkId::new("term", text.len()), &value, |b, value| {
            b.iter(|| tokenizer.tokens(value));
        });
    }
}

criterion_group!(term, benches);
criterion_main!(term);
