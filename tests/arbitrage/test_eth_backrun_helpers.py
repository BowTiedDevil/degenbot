"""Regression tests for example command-stream encoders.

These tests focus on command-stream *structure* for path types that have
historically produced settlement reverts. They do not simulate on-chain
execution, but they lock in the encoder ordering that the executor contract
expects.
"""

from web3 import Web3

from examples.cmd_stream import (
    BEGIN_EXECUTION,
    CMD_V2_SWAP_CALC,
    CMD_V2_SWAP_COMPACT,
    CMD_V2_SWAP_DIRECT,
    CMD_V3_SWAP_COMPACT,
    CMD_V4_SETTLE,
    CMD_V4_SETTLE_ALL,
    CMD_V4_SWAP_COMPACT,
    CMD_V4_SWAP_DYNAMIC,
    CMD_V4_SYNC,
    CMD_V4_TAKE_COMPACT,
    CMD_V4_UNLOCK,
)
from examples.eth_backrun_helpers import (
    PathInfo,
    V2HopInfo,
    V3HopInfo,
    V4HopInfo,
    _3hop_v2_v2_v2,
    _3hop_v2_v2_v3,
    _3hop_v2_v2_v4,
)

WETH = Web3.to_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
USDC = Web3.to_checksum_address("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48")
COMP = Web3.to_checksum_address("0xc00e94Cb662C3520282E6f5717214004A7f26888")
POOL_MANAGER = Web3.to_checksum_address("0x000000000004444c5dc75cB358380D2e3dE08A90")
EXECUTOR = Web3.to_checksum_address("0x1111111111111111111111111111111111111111")
V2A = Web3.to_checksum_address("0x2222222222222222222222222222222222222222")
V2B = Web3.to_checksum_address("0x3333333333333333333333333333333333333333")


def _extract_v4_unlock_inner(cmd_stream: bytes) -> bytes:
    """Return the payload inside the first V4_UNLOCK command."""
    exec_pos = cmd_stream.index(BEGIN_EXECUTION)
    unlock_pos = exec_pos + 1
    assert cmd_stream[unlock_pos] == CMD_V4_UNLOCK[0]
    inner_len = cmd_stream[unlock_pos + 1]
    inner_start = unlock_pos + 2
    inner = cmd_stream[inner_start : inner_start + inner_len]
    assert len(inner) == inner_len
    return inner


def test_v2_v2_v4_final_v4_swap_is_dynamic_and_settled() -> None:
    """Regression for V2-V2-V4 CurrencyNotSettled reverts.

    The V4 final hop must be dynamic (it consumes whatever V2b actually
    delivers to the PoolManager) and the forward_b currency must be
    synced+settled before that dynamic swap.
    """
    ha = V2HopInfo(
        pool_address=V2A,
        token0_address=COMP,
        token1_address=WETH,
        fee=30,
        zfo=False,
    )
    hb = V2HopInfo(
        pool_address=V2B,
        token0_address=USDC,
        token1_address=COMP,
        fee=30,
        zfo=False,
    )
    hc = V4HopInfo(
        pool_manager_address=POOL_MANAGER,
        pool_id_hex="0x" + "00" * 32,
        currency0_address=USDC,
        currency1_address=WETH,
        fee=500,
        tick_spacing=10,
        hook_address="0x0000000000000000000000000000000000000000",
        zfo=True,
    )
    path_info = PathInfo(hops=[ha, hb, hc])

    cmd = _3hop_v2_v2_v4(
        path_info,
        optimal_input=10**18,
        hop_outputs=(10**18, 2_000_000, 2 * 10**18),
        executor_address=EXECUTOR,
        pool_manager_address=POOL_MANAGER,
        weth_address=WETH,
    )
    assert cmd is not None

    inner = _extract_v4_unlock_inner(cmd)

    # Expected layout inside the V4 unlock:
    # [SYNC forward_b][TAKE WETH->V2a][V2a SWAP_CALC][V2b SWAP_CALC]
    # [SETTLE forward_b][V4 SWAP_DYNAMIC][SETTLE_ALL]
    assert inner[0] == CMD_V4_SYNC[0]
    assert inner[2] == CMD_V4_TAKE_COMPACT[0]
    assert inner[17] == CMD_V2_SWAP_CALC[0]
    assert inner[23] == CMD_V2_SWAP_CALC[0]
    assert inner[29] == CMD_V4_SETTLE[0]
    assert inner[30] == CMD_V4_SWAP_DYNAMIC[0]
    assert inner[39] == CMD_V4_SETTLE_ALL[0]

    # The exact-input V4 compact opcode must not appear — using a static solver
    # amount for the final V4 hop caused CurrencyNotSettled when the actual V2b
    # output differed by rounding or stale reserves.
    assert CMD_V4_SWAP_COMPACT not in inner


def _extract_forward_data(cmd_stream: bytes, top_opcode: bytes) -> bytes:
    """Return the forward_data payload of a V2/V3 compact top-level command."""
    exec_pos = cmd_stream.index(BEGIN_EXECUTION)
    top_pos = exec_pos + 1
    assert cmd_stream[top_pos] == top_opcode[0]
    if top_opcode == CMD_V2_SWAP_COMPACT:
        # [0x20][pool:1][zfo:1][amount:12][recipient:1][fee:2][fwd_len:1][fwd_data:N]
        fwd_len_pos = top_pos + 1 + 1 + 12 + 1 + 2
    elif top_opcode == CMD_V3_SWAP_COMPACT:
        # [0x30][pool:1][zfo:1][amount:12][recipient:1][fwd_len:1][fwd_data:N]
        fwd_len_pos = top_pos + 1 + 1 + 12 + 1
    else:
        msg = f"unsupported top opcode: {top_opcode}"
        raise ValueError(msg)
    fwd_len = cmd_stream[fwd_len_pos]
    fwd_start = fwd_len_pos + 1
    return cmd_stream[fwd_start : fwd_start + fwd_len]


def test_v2_v2_v2_uses_calc_not_direct_for_intermediate_hops() -> None:
    """Regression for V2-V2-V2 UniswapV2: K reverts.

    Static V2_SWAP_DIRECT amounts on intermediate hops become stale relative
    to on-chain reserves. The encoder should use V2_SWAP_CALC so the contract
    derives the correct output from actual reserves.
    """
    ha = V2HopInfo(
        pool_address=V2A,
        token0_address=COMP,
        token1_address=WETH,
        fee=30,
        zfo=False,
    )
    hb = V2HopInfo(
        pool_address=V2B,
        token0_address=USDC,
        token1_address=COMP,
        fee=30,
        zfo=False,
    )
    hc = V2HopInfo(
        pool_address="0x4444444444444444444444444444444444444444",
        token0_address=USDC,
        token1_address=WETH,
        fee=30,
        zfo=True,
    )
    path_info = PathInfo(hops=[ha, hb, hc])

    cmd = _3hop_v2_v2_v2(
        path_info,
        optimal_input=10**18,
        hop_outputs=(2 * 10**18, 2_000_000, 11 * 10**17),
        executor_address=EXECUTOR,
        pool_manager_address=POOL_MANAGER,
        weth_address=WETH,
    )
    assert cmd is not None

    fwd = _extract_forward_data(cmd, CMD_V2_SWAP_COMPACT)
    assert CMD_V2_SWAP_CALC in fwd
    assert CMD_V2_SWAP_DIRECT not in fwd
    assert fwd.count(CMD_V2_SWAP_CALC[0]) >= 2


def test_v2_v2_v3_uses_calc_not_direct_for_v2_hops() -> None:
    """Regression for V2-V2-V3 UniswapV2: K reverts.

    The two V2 hops inside the V3 callback must use V2_SWAP_CALC rather than
    static V2_SWAP_DIRECT amounts.
    """
    ha = V2HopInfo(
        pool_address=V2A,
        token0_address=COMP,
        token1_address=WETH,
        fee=30,
        zfo=False,
    )
    hb = V2HopInfo(
        pool_address=V2B,
        token0_address=USDC,
        token1_address=COMP,
        fee=30,
        zfo=False,
    )
    hc = V3HopInfo(
        pool_address="0x4444444444444444444444444444444444444444",
        token0_address=USDC,
        token1_address=WETH,
        fee=500,
        zfo=True,
    )
    path_info = PathInfo(hops=[ha, hb, hc])

    cmd = _3hop_v2_v2_v3(
        path_info,
        optimal_input=10**18,
        hop_outputs=(2 * 10**18, 2_000_000, 11 * 10**17),
        executor_address=EXECUTOR,
        pool_manager_address=POOL_MANAGER,
        weth_address=WETH,
    )
    assert cmd is not None

    fwd = _extract_forward_data(cmd, CMD_V3_SWAP_COMPACT)
    assert CMD_V2_SWAP_CALC in fwd
    assert CMD_V2_SWAP_DIRECT not in fwd
    assert fwd.count(CMD_V2_SWAP_CALC[0]) >= 2


# ---------------------------------------------------------------------------
# [sim-diag] structured per-revert emission (LAV44W)
# ---------------------------------------------------------------------------


def test_format_sim_diag_line_emits_parseable_json_with_required_fields() -> None:
    """The [sim-diag] line is one JSON object parseable with json.loads.

    Contains the per-candidate attribution fields the analyzer needs:
    path_id, path_type, solve_block, block, age, revert_info, optimal_input,
    hop_outputs, and per-hop {engine_state, onchain_state, drift, field_drift,
    recompute}. Engine-only snapshot (no onchain fetch) yields onchain_state
    None / drift False / field_drift [].
    """
    import json

    from examples.eth_backrun_helpers import format_sim_diag_line

    snapshot = {
        "path_id": 7,
        "path_type": "V2-V3-V4",
        "solve_block": 100,
        "hops": [
            {
                "position": 0,
                "hop_type": "V2",
                "engine_state": {"pool_family": "V2", "reserve_in": "0x1"},
                "onchain_state": None,
                "drift": False,
                "field_drift": [],
                "recompute": {
                    "amount_in": "0xa",
                    "solver_out": "0x64",
                    "expected_out_engine": "0x64",
                    "expected_out_onchain": None,
                    "matches_solver": None,
                },
            }
        ],
        "optimal_input": "0xa",
        "hop_outputs": ["0x64"],
    }

    line = format_sim_diag_line(
        snapshot,
        path_id=7,
        path_type="V2-V3-V4",
        solve_block=100,
        block=103,
        age=3,
        revert_info="0x CurrencyNotSettled",
    )

    assert line.startswith("[sim-diag] "), "line is prefixed [sim-diag] "
    payload = json.loads(line[len("[sim-diag] ") :])
    assert payload["path_id"] == 7
    assert payload["path_type"] == "V2-V3-V4"
    assert payload["solve_block"] == 100
    assert payload["block"] == 103
    assert payload["age"] == 3
    assert payload["revert_info"] == "0x CurrencyNotSettled"
    assert payload["optimal_input"] == "0xa"
    assert payload["hop_outputs"] == ["0x64"]
    hop = payload["hops"][0]
    assert hop["engine_state"]["reserve_in"] == "0x1"
    assert hop["onchain_state"] is None
    assert hop["drift"] is False
    assert hop["field_drift"] == []
    assert hop["recompute"]["expected_out_engine"] == "0x64"


def test_format_sim_diag_line_never_raises_on_missing_snapshot_keys() -> None:
    """A taxonomy/emission path must never raise — malformed snapshots emit a
    best-effort line with whatever fields are present (the analyzer tolerates
    missing keys)."""
    import json

    from examples.eth_backrun_helpers import format_sim_diag_line

    line = format_sim_diag_line(
        {},
        path_id=1,
        path_type="V2",
        solve_block=1,
        block=1,
        age=0,
        revert_info="",
    )
    payload = json.loads(line[len("[sim-diag] ") :])
    assert payload["path_id"] == 1
    assert payload["hops"] == []
    assert payload["optimal_input"] is None


# ── T3: thin-margin profit filter (GTOD23-IKJRGO) ───────────────────────────

from examples.eth_backrun_helpers import (
    BPS_DENOM,
    filter_thin_margin_results,
)


def _result(path_id: int, opt_input: int, profit: int) -> tuple:
    """Build a minimal engine-result row for the filter tests."""
    return (path_id, opt_input, profit, (), (), 0)


def test_filter_disabled_when_bps_zero_keeps_all() -> None:
    """min_profit_margin_bps=0 disables the filter (backwards-compatible default)."""
    results = [_result(1, 1_000, 1), _result(2, 1_000, 999)]
    kept, dropped = filter_thin_margin_results(results, 0)
    assert kept == results
    assert dropped == 0


def test_filter_drops_sub_threshold_margin_drops_keeps_above() -> None:
    """A 50 bps threshold drops profit < 0.5% of input, keeps ≥ 0.5%."""
    # profit = 100 bps of input (1%) → kept.
    # profit = 10 bps of input (0.1%) → dropped (below 50 bps).
    # profit = 50 bps exactly (0.5%) → kept (≥ threshold, inclusive).
    results = [
        _result(1, 10_000, 100),    # 1.0% — keep
        _result(2, 10_000, 10),     # 0.1% — drop
        _result(3, 10_000, 50),     # 0.5% — keep (boundary, inclusive)
    ]
    kept, dropped = filter_thin_margin_results(results, 50)
    kept_ids = [r[0] for r in kept]
    assert kept_ids == [1, 3]
    assert dropped == 1


def test_filter_uses_integer_math_no_float_rounding() -> None:
    """The threshold check uses profit*BPS_DENOM >= opt*bps (integer only).

    A result that floats would round wrong at the boundary stays exact.
    """
    # opt_input = 3, profit = 1 → 33.33% — way above 50 bps, keep.
    # opt_input = 3, profit = 0 → 0% — drop.
    results = [_result(1, 3, 1), _result(2, 3, 0)]
    kept, dropped = filter_thin_margin_results(results, 50)
    assert [r[0] for r in kept] == [1]
    assert dropped == 1


def test_filter_handles_zero_opt_input_keeps() -> None:
    """A result with opt_input=0 (no ratio basis) is kept, not dropped."""
    results = [_result(1, 0, 5)]
    kept, dropped = filter_thin_margin_results(results, 50)
    assert kept == results
    assert dropped == 0


def test_filter_returns_copy_not_alias() -> None:
    """The kept list is a new list — mutating it doesn't touch the input."""
    results = [_result(1, 1_000, 100)]
    kept, _ = filter_thin_margin_results(results, 50)
    kept.append(_result(2, 1_000, 100))
    assert len(results) == 1, "original list must not be mutated"
