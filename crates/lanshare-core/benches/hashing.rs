//! BLAKE3 chunk hashing throughput. Run with `cargo bench -p lanshare-core --bench hashing`.

// `criterion_group!` and `criterion_main!` generate public functions without doc comments.
#![allow(missing_docs)]

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use lanshare_core::transfer::{hash_chunk, DEFAULT_CHUNK_SIZE};

fn chunk_hashing(criterion: &mut Criterion) {
    let chunk = vec![0xa5u8; DEFAULT_CHUNK_SIZE as usize];
    let mut group = criterion.benchmark_group("blake3");
    group.throughput(Throughput::Bytes(chunk.len() as u64));
    group.bench_function("8 MiB chunk", |bencher| bencher.iter(|| hash_chunk(black_box(&chunk))));
    group.finish();
}

criterion_group!(benches, chunk_hashing);
criterion_main!(benches);
