#!/usr/bin/env python3
"""Benchmark: V2BlockEngine (Rust-centric loop) vs ArbSolver (baseline).

Answers the Plan 078 thesis question:
  Does eliminating Python from the hot path deliver a meaningful speedup?

The V2BlockEngine model: Python calls `latest_results()` once per block.
The ArbSolver model: Python calls `update_all_paths()` + `solve_registered_ints()`
per block, shuttling data across the PyO3 bridge each time.

Both produce identical arbitrage results for the same pool states.
"""

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

FEE_0_3_PCT = Fraction(3, 1000)
GAMMA_03 = 997
FEE_DENOM_03 = 1000

NUM_WARMUP = 200
NUM_ITERATIONS = 10000


def build_v2_block_engine(n_paths: int = 50) -> tuple[V2ArbEngine, list[str]]:
    """Build a V2ArbEngine with n_paths registered.

    Each path uses two pools: forward (USDC→WETH) and forward (WETH→USDC).
    Returns the engine and the list of pool addresses for Sync updates.
    """
    engine = V2ArbEngine()
    addresses = []

    for i in range(n_paths):
        factor = 1.0 + (i % 10) * 0.05
        addr_even = f"0x{(i * 2):040x}"
        addr_odd = f"0x{(i * 2 + 1):040x}"

        fwd0 = engine.register_pool(
            addr_even,
            int(USDC_1_5M * factor),
            int(WETH_800 * factor),
            GAMMA_03,
            FEE_DENOM_03,
        )
        fwd1 = engine.register_pool(
            addr_odd,
            int(WETH_1000 * factor),
            int(USDC_2M * factor),
            GAMMA_03,
            FEE_DENOM_03,
        )
        engine.register_path([fwd0, fwd1])
        addresses.extend([addr_even, addr_odd])

    return engine, addresses


def build_sync_updates(
    n_paths: int = 50, delta: float = 0.001
) -> list[tuple[str, int, int]]:
    """Build Sync update tuples for process_logs()."""
    updates = []
    for i in range(n_paths):
        factor = 1.0 + (i % 10) * 0.05 + delta
        addr_even = f"0x{(i * 2):040x}"
        addr_odd = f"0x{(i * 2 + 1):040x}"
        updates.append((addr_even, int(USDC_1_5M * factor), int(WETH_800 * factor)))
        updates.append((addr_odd, int(WETH_1000 * factor), int(USDC_2M * factor)))
    return updates


def build_arb_solver(n_paths: int = 50) -> tuple[ArbSolver, list[int]]:
    """Build an ArbSolver with n_paths registered.

    Returns the solver and the list of path IDs.
    """
    fee = Fraction(3, 1000)
    solver = ArbSolver()
    path_ids = []

    for i in range(n_paths):
        factor = 1.0 + (i % 10) * 0.05
        pid0 = solver.register_pool(int(USDC_1_5M * factor), int(WETH_800 * factor), fee)
        pid1 = solver.register_pool(int(WETH_1000 * factor), int(USDC_2M * factor), fee)
        rid = solver.register_path([pid0, pid1])
        path_ids.append(rid)

    return solver, path_ids


def update_arb_solver(solver: ArbSolver, n_paths: int = 50, delta: float = 0.001) -> None:
    """Update all pool states in the ArbSolver."""
    fee = Fraction(3, 1000)
    for i in range(n_paths):
        factor = 1.0 + (i % 10) * 0.05 + delta
        solver.update_pool(i * 2, int(USDC_1_5M * factor), int(WETH_800 * factor), fee)
        solver.update_pool(i * 2 + 1, int(WETH_1000 * factor), int(USDC_2M * factor), fee)


# ---------------------------------------------------------------------------
# Correctness verification
# ---------------------------------------------------------------------------


def verify_correctness() -> None:
    """Verify V2BlockEngine produces the same results as ArbSolver."""
    print("=" * 72)
    print("Correctness Verification: V2BlockEngine vs ArbSolver")
    print("=" * 72)

    fee = Fraction(3, 1000)

    # ArbSolver
    solver = ArbSolver()
    pid0 = solver.register_pool(USDC_1_5M, WETH_800, fee)
    pid1 = solver.register_pool(WETH_1000, USDC_2M, fee)
    path_id = solver.register_path([pid0, pid1])
    solver.update_all_paths()

    # V2BlockEngine
    engine = V2ArbEngine()
    fwd0 = engine.register_pool(
        "0x0000000000000000000000000000000000000000",
        USDC_1_5M,
        WETH_800,
        GAMMA_03,
        FEE_DENOM_03,
    )
    fwd1 = engine.register_pool(
        "0x0000000000000000000000000000000000000001",
        WETH_1000,
        USDC_2M,
        GAMMA_03,
        FEE_DENOM_03,
    )
    engine.register_path([fwd0, fwd1])

    # Trigger solve on both
    engine.process_logs(
        [
            ("0x0000000000000000000000000000000000000000", USDC_1_5M, WETH_800),
            ("0x0000000000000000000000000000000000000001", WETH_1000, USDC_2M),
        ],
        block_number=1,
    )
    solver_results = solver.solve_registered_ints([path_id])
    engine_results, _ = engine.latest_results()

    if solver_results:
        solver_input, solver_profit = solver_results[0]
        # Engine results are flat: [path_id, input, profit, ...]
        assert len(engine_results) >= 3, f"Expected at least 3 elements, got {len(engine_results)}"
        engine_input = engine_results[1]
        engine_profit = engine_results[2]

        match = solver_input == engine_input and solver_profit == engine_profit
        print(f"  ArbSolver: input={solver_input}, profit={solver_profit}")
        print(f"  V2BlockEngine: input={engine_input}, profit={engine_profit}")
        print(f"  Values match: {match}")

        if not match:
            print("  FAILED: Results do not match!")
            raise SystemExit(1)
    else:
        print("  No profitable path found (both agree)")
    print()


# ---------------------------------------------------------------------------
# Benchmarks
# ---------------------------------------------------------------------------


def bench_latest_results() -> float:
    """Benchmark: V2BlockEngine latest_results() — the only per-block Python call."""
    print("=" * 72)
    print("V2BlockEngine: latest_results() (per-block read)")
    print("=" * 72)

    num_paths = 50
    engine, addresses = build_v2_block_engine(num_paths)
    sync_updates = build_sync_updates(num_paths)

    # Prime the engine with some results
    engine.process_logs(sync_updates, block_number=1)

    print(f"  Pools: {engine.pool_count()}, Paths: {engine.path_count()}")

    # --- Benchmark: latest_results() only (the per-block cost) ---
    for _ in range(NUM_WARMUP):
        engine.latest_results()

    t0 = perf_counter()
    for _ in range(NUM_ITERATIONS):
        engine.latest_results()
    t1 = perf_counter()
    ns_per_call = (t1 - t0) * 1e9 / NUM_ITERATIONS
    ns_per_path = ns_per_call / num_paths
    print(f"  latest_results() (50 paths):     {ns_per_path:>7,.0f} ns/path  ({ns_per_call:>8,.0f} ns/call)")

    # --- Benchmark: full per-block cycle (process_logs + latest_results) ---
    for _ in range(NUM_WARMUP):
        engine.process_logs(sync_updates, block_number=1)
        engine.latest_results()

    block_num = 0
    t0 = perf_counter()
    for _ in range(NUM_ITERATIONS):
        block_num += 1
        engine.process_logs(sync_updates, block_number=block_num)
        engine.latest_results()
    t1 = perf_counter()
    ns_per_call = (t1 - t0) * 1e9 / NUM_ITERATIONS
    ns_per_path = ns_per_call / num_paths
    cycle_ns = ns_per_path
    print(f"  process_logs + latest_results:   {ns_per_path:>7,.0f} ns/path  ({ns_per_call:>8,.0f} ns/cycle)")

    # --- Benchmark: process_logs only (the update + solve cost) ---
    for _ in range(NUM_WARMUP):
        engine.process_logs(sync_updates, block_number=1)

    block_num = 0
    t0 = perf_counter()
    for _ in range(NUM_ITERATIONS):
        block_num += 1
        engine.process_logs(sync_updates, block_number=block_num)
    t1 = perf_counter()
    ns_per_call = (t1 - t0) * 1e9 / NUM_ITERATIONS
    ns_per_path = ns_per_call / num_paths
    print(f"  process_logs only:               {ns_per_path:>7,.0f} ns/path  ({ns_per_call:>8,.0f} ns/call)")

    return cycle_ns


def bench_baseline() -> float:
    """Benchmark the current baseline: ArbSolver update + solve cycle."""
    print("\n" + "=" * 72)
    print("Baseline: ArbSolver update_all_paths + solve_registered_ints")
    print("=" * 72)

    num_paths = 50
    solver, path_ids = build_arb_solver(num_paths)

    print(f"  Paths: {len(path_ids)}")

    # --- Benchmark: solve only ---
    solver.update_all_paths()
    for _ in range(NUM_WARMUP):
        solver.solve_registered_ints(path_ids)

    t0 = perf_counter()
    for _ in range(NUM_ITERATIONS):
        solver.solve_registered_ints(path_ids)
    t1 = perf_counter()
    ns_per_call = (t1 - t0) * 1e9 / NUM_ITERATIONS
    ns_per_path = ns_per_call / num_paths
    print(f"  solve_registered_ints (50 paths): {ns_per_path:>7,.0f} ns/path  ({ns_per_call:>8,.0f} ns/call)")

    # --- Benchmark: full cycle (update + solve) ---
    for _ in range(NUM_WARMUP):
        update_arb_solver(solver, num_paths)
        solver.update_all_paths()
        solver.solve_registered_ints(path_ids)

    t0 = perf_counter()
    for _ in range(NUM_ITERATIONS):
        update_arb_solver(solver, num_paths)
        solver.update_all_paths()
        solver.solve_registered_ints(path_ids)
    t1 = perf_counter()
    ns_per_call = (t1 - t0) * 1e9 / NUM_ITERATIONS
    ns_per_path = ns_per_call / num_paths
    baseline_ns = ns_per_path
    print(f"  update + solve cycle:             {ns_per_path:>7,.0f} ns/path  ({ns_per_call:>8,.0f} ns/cycle)")

    return baseline_ns


def bench_scaling() -> None:
    """V2BlockEngine: scaling with number of paths."""
    print("\n" + "=" * 72)
    print("V2BlockEngine: Scaling (paths → ns/path)")
    print("=" * 72)

    for n_paths in [1, 2, 5, 10, 20, 50, 100, 200]:
        engine, _ = build_v2_block_engine(n_paths)
        sync_updates = build_sync_updates(n_paths)

        # Warmup
        for _ in range(NUM_WARMUP):
            engine.process_logs(sync_updates, block_number=1)
            engine.latest_results()

        # Measure full cycle
        block_num = 0
        t0 = perf_counter()
        for _ in range(NUM_ITERATIONS):
            block_num += 1
            engine.process_logs(sync_updates, block_number=block_num)
            engine.latest_results()
        t1 = perf_counter()
        ns_per_path = (t1 - t0) * 1e9 / (NUM_ITERATIONS * n_paths)

        # Measure latest_results only
        for _ in range(NUM_WARMUP):
            engine.latest_results()

        t0 = perf_counter()
        for _ in range(NUM_ITERATIONS):
            engine.latest_results()
        t1 = perf_counter()
        ns_read = (t1 - t0) * 1e9 / (NUM_ITERATIONS * n_paths)

        print(f"  paths={n_paths:>3}:  full_cycle={ns_per_path:>7,.0f} ns/path  read_only={ns_read:>7,.0f} ns/path")


if __name__ == "__main__":
    verify_correctness()

    engine_ns = bench_latest_results()
    baseline_ns = bench_baseline()
    bench_scaling()

    print("\n" + "=" * 72)
    print("Summary")
    print("=" * 72)
    print(f"  V2BlockEngine process_logs + read:  {engine_ns:>7,.0f} ns/path")
    print(f"  ArbSolver update + solve:            {baseline_ns:>7,.0f} ns/path")
    if baseline_ns > 0:
        print(f"  Speedup: {baseline_ns / engine_ns:.2f}x")

    print("\n  Note: In the Rust-centric model, process_logs() is only used")
    print("  for testing. The real pump drives process_block() entirely in Rust.")
    print("  The truly interesting number is latest_results() read cost, which")
    print("  is the only PyO3 crossing per block when the pump is running.")
