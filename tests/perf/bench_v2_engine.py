#!/usr/bin/env python3
"""DEPRECATED: This benchmark tests the deleted V2ArbEngine prototype.

Use bench_v2_block_engine_e2e.py instead, which tests the V2BlockEngine
(Rust-centric arbitrage engine from Plan 078).
"""

raise RuntimeError(
    "This benchmark is deprecated. Use bench_v2_block_engine_e2e.py instead."
)

import struct
from fractions import Fraction
from time import perf_counter

from degenbot.degenbot_rs import V2ArbEngine
from degenbot.arbitrage.optimizers.solver import ArbSolver

# ---------------------------------------------------------------------------
# Test data
# ---------------------------------------------------------------------------

USDC_DECIMALS = 6
WETH_DECIMALS = 18
USDC_1_5M = 1_500_000 * 10**USDC_DECIMALS
USDC_2M = 2_000_000 * 10**USDC_DECIMALS
WETH_800 = 800 * 10**WETH_DECIMALS
WETH_1000 = 1_000 * 10**WETH_DECIMALS

FEE_0_3_PCT = Fraction(3, 1000)  # gamma_numer=997, fee_denom=1000
GAMMA_03 = 997
FEE_DENOM_03 = 1000

NUM_WARMUP = 200
NUM_ITERATIONS = 10000


def build_engine(n_paths=50):
    """Build a V2ArbEngine with n_paths registered."""
    engine = V2ArbEngine()
    for i in range(n_paths):
        factor = 1.0 + (i % 10) * 0.05
        engine.register_pool(i * 2, int(USDC_1_5M * factor), int(WETH_800 * factor), GAMMA_03, FEE_DENOM_03)
        engine.register_pool(i * 2 + 1, int(WETH_1000 * factor), int(USDC_2M * factor), GAMMA_03, FEE_DENOM_03)
        engine.register_path([i * 2, i * 2 + 1])
    return engine


def build_updates(n_paths=50, delta=0.001):
    """Build update tuples and packed bytes buffer."""
    updates = []
    for i in range(n_paths):
        factor = 1.0 + (i % 10) * 0.05 + delta
        updates.append((i * 2, int(USDC_1_5M * factor), int(WETH_800 * factor), GAMMA_03, FEE_DENOM_03))
        updates.append((i * 2 + 1, int(WETH_1000 * factor), int(USDC_2M * factor), GAMMA_03, FEE_DENOM_03))

    # Packed binary buffer: [num_pools: u16 BE] + per pool: [pool_id: u64, r_in: 32B, r_out: 32B, gamma: u64, fee: u64]
    buf = struct.pack('>H', len(updates))
    for pool_id, r_in, r_out, gamma, fee_d in updates:
        buf += struct.pack('>Q', pool_id)
        buf += r_in.to_bytes(32, 'big')
        buf += r_out.to_bytes(32, 'big')
        buf += struct.pack('>Q', gamma)
        buf += struct.pack('>Q', fee_d)

    return updates, buf


def verify_results():
    """Verify V2ArbEngine produces correct results."""
    print("=" * 72)
    print("Verification: V2ArbEngine vs ArbSolver")
    print("=" * 72)

    fee = Fraction(3, 1000)
    solver = ArbSolver()
    pid0 = solver.register_pool(USDC_1_5M, WETH_800, fee)
    pid1 = solver.register_pool(WETH_1000, USDC_2M, fee)
    path_id = solver.register_path([pid0, pid1])

    engine = V2ArbEngine()
    engine.register_pool(0, USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM_03)
    engine.register_pool(1, WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03)
    e_path_id = engine.register_path([0, 1])

    solver_result = solver.solve_registered_ints([path_id])[0]
    engine_result = engine.solve_paths([e_path_id])

    print(f"ArbSolver: input={solver_result[0]}, profit={solver_result[1]}")
    print(f"V2Engine:  input={engine_result[1]}, profit={engine_result[2]}")
    match = solver_result[0] == engine_result[1] and solver_result[1] == engine_result[2]
    print(f"Values match: {match}\n")


def bench_v2_engine():
    """Benchmark the V2ArbEngine fully-Rust path."""
    print("=" * 72)
    print("V2ArbEngine: Fully-Rust arbitrage engine")
    print("=" * 72)

    num_paths = 50
    engine = build_engine(num_paths)
    updates, buf = build_updates(num_paths)

    print(f"Pools: {engine.pool_count()}, Paths: {engine.path_count()}")

    # Warmup all paths
    for _ in range(NUM_WARMUP):
        engine.solve_all()
        engine.batch_update(updates)
        engine.solve_all()
        engine.update_and_solve_all(updates)
        engine.update_and_solve_raw(buf)

    # --- Benchmark: solve_all ---
    t0 = perf_counter()
    for _ in range(NUM_ITERATIONS):
        engine.solve_all()
    t1 = perf_counter()
    ns_per_path = (t1 - t0) * 1e9 / (NUM_ITERATIONS * num_paths)
    ns_per_call = (t1 - t0) * 1e9 / NUM_ITERATIONS
    print(f"\nsolve_all (50 paths):              {ns_per_path:>7,.0f} ns/path  ({ns_per_call:>8,.0f} ns/call)")

    # --- Benchmark: 2-call cycle ---
    t0 = perf_counter()
    for _ in range(NUM_ITERATIONS):
        engine.batch_update(updates)
        engine.solve_all()
    t1 = perf_counter()
    ns_per_path = (t1 - t0) * 1e9 / (NUM_ITERATIONS * num_paths)
    ns_per_call = (t1 - t0) * 1e9 / NUM_ITERATIONS
    print(f"batch_update + solve_all:          {ns_per_path:>7,.0f} ns/path  ({ns_per_call:>8,.0f} ns/cycle)")

    # --- Benchmark: update_and_solve_all (tuple input) ---
    t0 = perf_counter()
    for _ in range(NUM_ITERATIONS):
        engine.update_and_solve_all(updates)
    t1 = perf_counter()
    ns_per_path = (t1 - t0) * 1e9 / (NUM_ITERATIONS * num_paths)
    ns_per_call = (t1 - t0) * 1e9 / NUM_ITERATIONS
    tuple_ns = ns_per_path
    print(f"update_and_solve_all (tuples):     {ns_per_path:>7,.0f} ns/path  ({ns_per_call:>8,.0f} ns/cycle)")

    # --- Benchmark: update_and_solve_raw (packed bytes) ---
    t0 = perf_counter()
    for _ in range(NUM_ITERATIONS):
        engine.update_and_solve_raw(buf)
    t1 = perf_counter()
    ns_per_path = (t1 - t0) * 1e9 / (NUM_ITERATIONS * num_paths)
    ns_per_call = (t1 - t0) * 1e9 / NUM_ITERATIONS
    raw_ns = ns_per_path
    print(f"update_and_solve_raw (bytes):      {ns_per_path:>7,.0f} ns/path  ({ns_per_call:>8,.0f} ns/cycle)")

    # --- Buffer packing cost ---
    # Also measure just the Python-side buffer packing
    t0 = perf_counter()
    for _ in range(NUM_ITERATIONS):
        b = struct.pack('>H', len(updates))
        for pool_id, r_in, r_out, gamma, fee_d in updates:
            b += struct.pack('>Q', pool_id)
            b += r_in.to_bytes(32, 'big')
            b += r_out.to_bytes(32, 'big')
            b += struct.pack('>Q', gamma)
            b += struct.pack('>Q', fee_d)
    t1 = perf_counter()
    pack_ns = (t1 - t0) * 1e9 / NUM_ITERATIONS
    print(f"  (Python buffer packing cost: {pack_ns:,.0f} ns — excluded from above)")

    # --- True end-to-end: buffer packing + raw ---
    # This is the realistic cost if buffer must be packed each block
    t0 = perf_counter()
    for _ in range(NUM_ITERATIONS):
        b = struct.pack('>H', len(updates))
        for pool_id, r_in, r_out, gamma, fee_d in updates:
            b += struct.pack('>Q', pool_id)
            b += r_in.to_bytes(32, 'big')
            b += r_out.to_bytes(32, 'big')
            b += struct.pack('>Q', gamma)
            b += struct.pack('>Q', fee_d)
        engine.update_and_solve_raw(b)
    t1 = perf_counter()
    ns_per_path = (t1 - t0) * 1e9 / (NUM_ITERATIONS * num_paths)
    print(f"pack + update_and_solve_raw:      {ns_per_path:>7,.0f} ns/path  (TRUE end-to-end)")

    return raw_ns, tuple_ns


def bench_baseline():
    """Benchmark the current best: solve_registered_ints."""
    print("\n" + "=" * 72)
    print("Baseline: ArbSolver solve_registered_ints")
    print("=" * 72)

    num_paths = 50
    fee = Fraction(3, 1000)
    solver = ArbSolver()
    path_ids = []

    for i in range(num_paths):
        factor = 1.0 + (i % 10) * 0.05
        pid0 = solver.register_pool(int(USDC_1_5M * factor), int(WETH_800 * factor), fee)
        pid1 = solver.register_pool(int(WETH_1000 * factor), int(USDC_2M * factor), fee)
        rid = solver.register_path([pid0, pid1])
        path_ids.append(rid)

    for _ in range(NUM_WARMUP):
        solver.solve_registered_ints(path_ids)
        solver.update_all_paths()

    # Solve only
    t0 = perf_counter()
    for _ in range(NUM_ITERATIONS):
        solver.solve_registered_ints(path_ids)
    t1 = perf_counter()
    ns_per_path = (t1 - t0) * 1e9 / (NUM_ITERATIONS * num_paths)
    print(f"solve_registered_ints (50 paths):  {ns_per_path:>7,.0f} ns/path")

    # Full cycle
    t0 = perf_counter()
    for _ in range(NUM_ITERATIONS):
        solver.update_all_paths()
        solver.solve_registered_ints(path_ids)
    t1 = perf_counter()
    ns_per_path = (t1 - t0) * 1e9 / (NUM_ITERATIONS * num_paths)
    baseline_ns = ns_per_path
    print(f"update_all + solve_registered:     {ns_per_path:>7,.0f} ns/path")

    return baseline_ns


def bench_scaling():
    """V2ArbEngine: scaling with batch size."""
    print("\n" + "=" * 72)
    print("V2ArbEngine: Scaling (batch_size → ns/path)")
    print("=" * 72)

    for batch_size in [1, 2, 5, 10, 20, 50, 100, 200]:
        engine = build_engine(batch_size)
        _, buf = build_updates(batch_size)

        for _ in range(NUM_WARMUP):
            engine.solve_all()
            engine.update_and_solve_raw(buf)

        t0 = perf_counter()
        for _ in range(NUM_ITERATIONS):
            engine.solve_all()
        t1 = perf_counter()
        ns_solve = (t1 - t0) * 1e9 / (NUM_ITERATIONS * batch_size)

        t0 = perf_counter()
        for _ in range(NUM_ITERATIONS):
            engine.update_and_solve_raw(buf)
        t1 = perf_counter()
        ns_cycle = (t1 - t0) * 1e9 / (NUM_ITERATIONS * batch_size)

        print(f"  batch_size={batch_size:>3}:  solve={ns_solve:>7,.0f} ns/path  raw_cycle={ns_cycle:>7,.0f} ns/path")


if __name__ == "__main__":
    verify_results()
    raw_ns, tuple_ns = bench_v2_engine()
    baseline_ns = bench_baseline()
    bench_scaling()

    print("\n" + "=" * 72)
    print("Summary")
    print("=" * 72)
    print(f"  V2ArbEngine update_and_solve_raw:  {raw_ns:>7,.0f} ns/path")
    print(f"  V2ArbEngine update_and_solve_all:  {tuple_ns:>7,.0f} ns/path")
    print(f"  ArbSolver update + solve:          {baseline_ns:>7,.0f} ns/path")
    if raw_ns > 0:
        print(f"  Speedup (raw vs baseline): {baseline_ns / raw_ns:.2f}x")
