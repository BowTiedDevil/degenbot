"""Tests for the per-pool solver-divergence aggregator (ergo epic GAXXNJ task BEGMB5).

Pins `aggregate_pool_divergence` — the per-block per-pool `SolverCalc`
aggregate that surfaces "this pool's state is what the solver read wrong"
across every path routing through it in the same block, instead of letting
each path fail independently before per-path suppression.

Aggregation is pure-Python over the `outcome.failures` dict shape the PyO3
seam surfaces (`rust/crates/degenbot-python/src/simulation/outcome.rs::failures()`).
The verdict is derived by calling `classify_candidate` on the SAME payload
`format_sim_diag_line` builds (the classifier + aggregator agree because they
read the same dict — the emit→classify seam pinned in commit 34d80b90).
"""

from eth_backrun_helpers import aggregate_pool_divergence

_POOL_A = "0x" + "aa" * 20
_POOL_B = "0x" + "bb" * 20
_POOL_C = "0x" + "cc" * 20


def _swap(
    *,
    family: str = "v2",
    emitter: str = _POOL_A,
    amount0: int = -1000,
    amount1: int = 3000,
) -> dict[str, object]:
    """A captured swap dict (the inspector's CapturedSwap shape)."""
    return {
        "family": family,
        "emitter": emitter,
        "amount0": amount0,
        "amount1": amount1,
        "sqrt_price_x96": 0,
        "liquidity": 0,
        "tick": 0,
    }


def _failure(
    *,
    path_id: int = 1,
    bucket: str = "0x CurrencyNotSettled",
    captured_swaps: list[dict[str, object]] | None = None,
    hop_outputs: list[int] | None = None,
) -> dict[str, object]:
    """A minimal failure-record dict (the shape outcome.failures() emits)."""
    if captured_swaps is None:
        captured_swaps = [_swap(emitter=_POOL_A, amount0=-1000, amount1=3000)]
    if hop_outputs is None:
        # Default: matches the captured swap output (3000) → Encoding, NOT
        # SolverCalc. Tests that want SolverCalc must override hop_outputs.
        hop_outputs = [3000]
    return {
        "path_id": path_id,
        "bucket": bucket,
        "fail_index": 3,
        "revert_data": "0xcafebabe",
        "reverting_frame": None,
        "captured_swaps": captured_swaps,
        "optimal_input": 1000,
        "hop_outputs": hop_outputs,
    }


def test_solvercalc_failure_aggregated_per_pool() -> None:
    """Two SolverCalc failures on pool A, one on pool B → two lines, A first
    (count desc), each carrying the path_ids that hit it."""
    failures = [
        _failure(path_id=1, hop_outputs=[2900]),  # A, SolverCalc (3000≠2900)
        _failure(path_id=7, hop_outputs=[2950]),  # A, SolverCalc (3000≠2950)
        _failure(
            path_id=3,
            captured_swaps=[_swap(emitter=_POOL_B, amount0=-1000, amount1=3000)],
            hop_outputs=[2900],
        ),  # B, SolverCalc
    ]
    lines = aggregate_pool_divergence(failures, total_sims=10)
    assert len(lines) == 2
    # A (count=2) before B (count=1) — sort by count desc.
    assert f"pool={_POOL_A} solvercalc=2 paths=[1, 7] total_sims=10" in lines[0]
    assert f"pool={_POOL_B} solvercalc=1 paths=[3] total_sims=10" in lines[1]


def test_no_lines_when_all_encoding_matching_amounts() -> None:
    """Encoding failures (captured == hop_output) are NOT divergence — the
    pool state was fine, the calldata was wrong. No [pool-divergence] lines."""
    failures = [
        _failure(path_id=1, hop_outputs=[3000]),  # matches → Encoding, not SolverCalc
        _failure(path_id=2, hop_outputs=[3000]),
    ]
    assert aggregate_pool_divergence(failures, total_sims=5) == []


def test_no_lines_when_all_unknown_empty_revert() -> None:
    """Unknown failures (bare revert, no payload) are NOT divergence."""
    failures = [
        _failure(path_id=1, bucket=""),  # empty bucket → Unknown
        _failure(path_id=2, bucket=""),
    ]
    assert aggregate_pool_divergence(failures, total_sims=5) == []


def test_solvercalc_with_empty_captured_swaps_not_counted() -> None:
    """A SolverCalc failure whose captured_swaps is empty (orchestration-only)
    is NOT counted — there's no pool to attribute it to."""
    failures = [
        _failure(path_id=1, captured_swaps=[], hop_outputs=[]),
    ]
    assert aggregate_pool_divergence(failures, total_sims=5) == []


def test_v4_pool_aggregates_to_pool_manager_address() -> None:
    """V4 captured swaps' emitter is the PoolManager address (the V4 Swap
    event is emitted by the PoolManager, not a per-pool contract). All V4
    SolverCalc reverts aggregate to that one address for v1 — collapsing
    per-pool-id is acceptable (the operator question is "is our V4 solver
    wrong", not "which V4 pool"; per-pool-id split is a later refinement).
    """
    v4_manager = "0x" + "44" * 20  # the PoolManager address
    failures = [
        _failure(
            path_id=1,
            captured_swaps=[_swap(family="v4", emitter=v4_manager, amount0=-1000, amount1=3000)],
            hop_outputs=[2900],
        ),
        _failure(
            path_id=2,
            captured_swaps=[_swap(family="v4", emitter=v4_manager, amount0=-2000, amount1=6000)],
            hop_outputs=[5900],
        ),
    ]
    lines = aggregate_pool_divergence(failures, total_sims=5)
    assert len(lines) == 1
    assert f"pool={v4_manager} solvercalc=2 paths=[1, 2] total_sims=5" in lines[0]


def test_one_path_through_two_divergent_pools_counts_both() -> None:
    """A 2-hop failure whose two captured swaps are on two different pools
    attributes the SolverCalc to BOTH pools (the solver read both wrong)."""
    failures = [
        _failure(
            path_id=1,
            captured_swaps=[
                _swap(emitter=_POOL_A, amount0=-1000, amount1=3000),
                _swap(emitter=_POOL_B, amount0=-500, amount1=1500),
            ],
            hop_outputs=[2900, 1450],
        ),
    ]
    lines = aggregate_pool_divergence(failures, total_sims=3)
    assert len(lines) == 2
    # both pools count=1, so sort by pool address asc for stable output
    pools_in_lines = [line.split()[1].split("=")[1] for line in lines]
    assert pools_in_lines == sorted([_POOL_A, _POOL_B])


def test_mixed_solvercalc_and_encoding_only_counts_solvercalc() -> None:
    """A batch with 2 Encoding + 1 SolverCalc on the same pool → only the
    SolverCalc counts (Encoding means the amount was right)."""
    failures = [
        _failure(path_id=1, hop_outputs=[3000]),  # Encoding (match)
        _failure(path_id=2, hop_outputs=[3000]),  # Encoding (match)
        _failure(path_id=3, hop_outputs=[2900]),  # SolverCalc (mismatch)
    ]
    lines = aggregate_pool_divergence(failures, total_sims=5)
    assert len(lines) == 1
    assert "solvercalc=1" in lines[0]
    assert "paths=[3]" in lines[0]


def test_no_failures_emits_nothing() -> None:
    """An empty failures list → no lines (no noise)."""
    assert aggregate_pool_divergence([], total_sims=0) == []


def test_sort_stable_count_desc_then_address_asc() -> None:
    """When two pools have the same count, sort by pool address asc for
    deterministic output (so log diffs are stable across runs)."""
    failures = [
        _failure(
            path_id=1,
            captured_swaps=[_swap(emitter=_POOL_C, amount0=-1000, amount1=3000)],
            hop_outputs=[2900],
        ),
        _failure(
            path_id=2,
            captured_swaps=[_swap(emitter=_POOL_A, amount0=-1000, amount1=3000)],
            hop_outputs=[2900],
        ),
    ]
    lines = aggregate_pool_divergence(failures, total_sims=5)
    assert len(lines) == 2
    # Same count (1) → address asc: A (0xaa..) before C (0xcc..)
    assert f"pool={_POOL_A}" in lines[0]
    assert f"pool={_POOL_C}" in lines[1]
