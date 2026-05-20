//! Benchmarks for ABI decoding operations.

#![allow(clippy::unwrap_used)]

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use degenbot_rs::abi_decoder::{decode_rust, decode_single_rust};

fn bench_abi_decode(c: &mut Criterion) {
    let mut group = c.benchmark_group("abi_decode");

    // Single uint256 decode (32 bytes)
    let uint256_data = vec![0u8; 32];
    group.bench_function("decode_single/uint256", |b| {
        b.iter(|| decode_single_rust(black_box("uint256"), black_box(&uint256_data)).unwrap());
    });

    // Single address decode (32 bytes, last 20 are the address)
    let mut address_data = vec![0u8; 32];
    address_data[31] = 1;
    group.bench_function("decode_single/address", |b| {
        b.iter(|| decode_single_rust(black_box("address"), black_box(&address_data)).unwrap());
    });

    // Multi-type decode: Transfer event (address, address, uint256) = 96 bytes
    let mut transfer_data = Vec::with_capacity(96);
    transfer_data.extend_from_slice(&[0u8; 32]); // from
    transfer_data.extend_from_slice(&[0u8; 32]); // to
    transfer_data.extend_from_slice(&[0u8; 32]); // amount
    group.bench_function("decode_multi/transfer_event", |b| {
        b.iter(|| {
            decode_rust(
                black_box(&["address", "address", "uint256"]),
                black_box(&transfer_data),
            )
            .unwrap()
        });
    });

    // Cache hit: same types again (measures Arc::clone path)
    group.bench_function("decode_multi/transfer_event_cached", |b| {
        b.iter(|| {
            decode_rust(
                black_box(&["address", "address", "uint256"]),
                black_box(&transfer_data),
            )
            .unwrap()
        });
    });

    group.finish();
}

criterion_group!(benches, bench_abi_decode);
criterion_main!(benches);
