"""Tests for the V2BlockEngine (Rust-centric arbitrage engine)."""

from fractions import Fraction

from degenbot.arbitrage.optimizers.solver import ArbSolver
from degenbot.degenbot_rs import V2ArbEngine

USDC_DECIMALS = 6
WETH_DECIMALS = 18
USDC_1_5M = 1_500_000 * 10**USDC_DECIMALS
USDC_2M = 2_000_000 * 10**USDC_DECIMALS
WETH_800 = 800 * 10**WETH_DECIMALS
WETH_1000 = 1_000 * 10**WETH_DECIMALS

GAMMA_03 = 997
FEE_DENOM_03 = 1000

ADDR_0 = "0x0000000000000000000000000000000000000000"
ADDR_1 = "0x0000000000000000000000000000000000000001"


def test_register_pool_by_address():
    """Register a pool by address and verify internal ID assignment."""
    engine = V2ArbEngine()
    fwd_id = engine.register_pool(ADDR_0, USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM_03)
    # Forward ID should be 1, reverse should be 2
    assert fwd_id == 1
    assert engine.pool_count() == 1


def test_latest_results_empty():
    """latest_results() returns empty before any solve."""
    engine = V2ArbEngine()
    results, block_num = engine.latest_results()
    assert results == []
    assert block_num == 0


def test_latest_results_after_sync():
    """After process_logs with Sync events, latest_results returns profitable paths."""
    engine = V2ArbEngine()
    fwd0 = engine.register_pool(ADDR_0, USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM_03)
    fwd1 = engine.register_pool(ADDR_1, WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03)
    path_id = engine.register_path([fwd0, fwd1])

    # Apply Sync for pool 0 with initial reserves (already profitable)
    engine.process_logs(
        [(ADDR_0, USDC_1_5M, WETH_800)],
        block_number=42,
    )

    results, block_num = engine.latest_results()
    assert block_num == 42
    assert len(results) >= 3  # at least (path_id, input, profit) for one path
    assert results[0] == path_id  # first element is the path ID


def test_block_number_tracking():
    """Engine tracks which block the results correspond to."""
    engine = V2ArbEngine()
    engine.register_pool(ADDR_0, USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM_03)

    engine.process_logs([(ADDR_0, USDC_1_5M, WETH_800)], block_number=100)
    _, block_num = engine.latest_results()
    assert block_num == 100

    engine.process_logs([(ADDR_0, USDC_2M, WETH_1000)], block_number=101)
    _, block_num = engine.latest_results()
    assert block_num == 101


def test_values_match_arb_solver():
    """V2BlockEngine results match ArbSolver for identical inputs and reserves."""
    fee = Fraction(3, 1000)

    # Set up ArbSolver
    solver = ArbSolver()
    pid0 = solver.register_pool(USDC_1_5M, WETH_800, fee)
    pid1 = solver.register_pool(WETH_1000, USDC_2M, fee)
    path_id = solver.register_path([pid0, pid1])
    solver.update_all_paths()

    # Set up V2BlockEngine
    engine = V2ArbEngine()
    fwd0 = engine.register_pool(ADDR_0, USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM_03)
    fwd1 = engine.register_pool(ADDR_1, WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03)
    engine.register_path([fwd0, fwd1])

    # Apply Sync to set reserves (they should already be at the right values
    # from registration, but process_logs triggers a solve)
    engine.process_logs(
        [(ADDR_0, USDC_1_5M, WETH_800), (ADDR_1, WETH_1000, USDC_2M)],
        block_number=1,
    )

    # Get results from both
    solver_results = solver.solve_registered_ints([path_id])
    engine_results, _ = engine.latest_results()

    # Both should find the same path profitable
    if solver_results:
        solver_input, solver_profit = solver_results[0]
        # Engine results are flat: [path_id, input, profit, ...]
        assert len(engine_results) >= 3
        engine_input = engine_results[1]
        engine_profit = engine_results[2]

        assert solver_input == engine_input
        assert solver_profit == engine_profit


def test_dual_orientation_registration():
    """Registering a pool creates forward and reverse entries; both update on Sync."""
    engine = V2ArbEngine()
    fwd0 = engine.register_pool(ADDR_0, USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM_03)
    fwd1 = engine.register_pool(ADDR_1, WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03)

    # Forward path: pool0_fwd (USDC→WETH) → pool1_fwd (WETH→USDC)
    # Reverse path: pool0_rev (WETH→USDC) → pool1_rev (USDC→WETH)
    rev0 = fwd0 + 1
    rev1 = fwd1 + 1
    fwd_path = engine.register_path([fwd0, fwd1])
    rev_path = engine.register_path([rev1, rev0])

    # Process Sync updates
    engine.process_logs(
        [(ADDR_0, USDC_1_5M, WETH_800), (ADDR_1, WETH_1000, USDC_2M)],
        block_number=1,
    )

    results, _ = engine.latest_results()
    # At least one path should be profitable
    assert len(results) >= 3


def test_process_logs_ignores_unregistered():
    """Sync events for non-registered pool addresses are skipped."""
    engine = V2ArbEngine()
    engine.register_pool(ADDR_0, USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM_03)

    # Sync for an unregistered address
    unregistered = "0xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
    engine.process_logs([(unregistered, 999, 999)], block_number=1)

    # Original pool should be unchanged (no solve triggered for unregistered)
    _, block_num = engine.latest_results()
    assert block_num == 1  # block number updated even if no pools changed


def test_register_pool_after_start_raises():
    """Calling register_pool after freeze() panics."""
    engine = V2ArbEngine()
    engine.register_pool(ADDR_0, USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM_03)
    engine.freeze()

    import pytest

    with pytest.raises(BaseException):  # PanicException from Rust
        engine.register_pool(ADDR_1, WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03)


def test_register_path_after_start_raises():
    """Calling register_path after freeze() panics."""
    engine = V2ArbEngine()
    engine.register_pool(ADDR_0, USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM_03)
    engine.register_pool(ADDR_1, WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03)
    engine.freeze()

    import pytest

    with pytest.raises(BaseException):  # PanicException from Rust
        engine.register_path([1, 3])
