//! Benchmarks for ABI encoding operations.

#![allow(clippy::unwrap_used)]

use alloy::primitives::U256;
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use degenbot_rs::abi_encoder::{encode_rust, encode_single_rust};
use degenbot_rs::abi_types::AbiValue;

fn bench_abi_encode(c: &mut Criterion) {
    let mut group = c.benchmark_group("abi_encode");

    // Single uint256 encode
    let value = AbiValue::Uint(U256::from(12345u64), 256);
    group.bench_function("encode_single/uint256", |b| {
        b.iter(|| encode_single_rust(black_box("uint256"), black_box(&value)).unwrap());
    });

    // Multi-type encode: (address, uint256)
    let values_addr_uint: Vec<AbiValue> = vec![
        AbiValue::Address([0u8; 20]),
        AbiValue::Uint(U256::from(1_000_000u64), 256),
    ];
    group.bench_function("encode_multi/address_uint256", |b| {
        b.iter(|| {
            encode_rust(
                black_box(&["address", "uint256"]),
                black_box(&values_addr_uint),
            )
            .unwrap()
        });
    });

    // Transfer event: (address, address, uint256)
    let values_transfer: Vec<AbiValue> = vec![
        AbiValue::Address([0u8; 20]),
        AbiValue::Address([0u8; 20]),
        AbiValue::Uint(U256::from(1_000_000u64), 256),
    ];
    group.bench_function("encode_multi/transfer_event", |b| {
        b.iter(|| {
            encode_rust(
                black_box(&["address", "address", "uint256"]),
                black_box(&values_transfer),
            )
            .unwrap()
        });
    });

    group.finish();
}

criterion_group!(benches, bench_abi_encode);
criterion_main!(benches);
