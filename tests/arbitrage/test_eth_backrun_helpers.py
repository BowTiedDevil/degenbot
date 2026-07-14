"""Tests for the non-encoder helpers in ``eth_backrun_helpers``.

The command-stream encoders (``_3hop_*`` / ``_encode_cmd_*``) were retired in
the YQORTM §4.3 cutover — their byte-exact parity now lives in the Rust
golden-file tests (``cargo test -p degenbot-executor``). This file keeps the
regression coverage for the **stays-Python** helpers: ``format_sim_diag_line``
(display-only) and ``filter_thin_margin_results`` (display-only thinning over
the now-Rust ``classify_revert`` label).
"""

import json

from examples.eth_backrun_helpers import (
    BPS_DENOM,
    filter_thin_margin_results,
    format_sim_diag_line,
)

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


def test_format_sim_diag_line_emits_engine_processed_block_and_onchain_block() -> None:
    """O5SKZ6: the [sim-diag] line must carry ``engine_processed_block`` (the
    engine's last-applied block at diagnostic snapshot time) and
    ``onchain_block`` (the block the diagnostic's onchain RPC fetch hit) so the
    analyzer can distinguish a post-publish snapshot timing artifact from a
    real publish-time state lag. When ``engine_processed_block > solve_block``
    (the engine has advanced past the published solve_block by the time the
    snapshot is read), the visible drift is the post-publish swap the live
    engine read includes — a SNAPSHOT ARTIFACT (DriftArtifact), NOT real
    publish-time drift."""
    snapshot = {
        "engine_processed_block": 1001,  # engine has advanced past publish
        "onchain_block": 1000,
        "hops": [{"drift": True, "recompute": {"matches_solver": None}}],
    }

    line = format_sim_diag_line(
        snapshot,
        path_id=99,
        path_type="V4-V4-V3",
        solve_block=1000,
        block=1001,
        age=1,
        revert_info="0x IIA",
    )
    payload = json.loads(line[len("[sim-diag] ") :])
    assert payload["solve_block"] == 1000
    assert payload["engine_processed_block"] == 1001
    assert payload["onchain_block"] == 1000
    assert payload["engine_processed_block"] > payload["solve_block"]


def test_format_sim_diag_line_emits_null_block_metadata_when_absent() -> None:
    """When the snapshot doesn't carry block metadata (older engine-side
    snapshots), the line emits nulls — the analyzer treats absent metadata
    conservatively (real drift, not artifact)."""
    line = format_sim_diag_line(
        {"hops": []},
        path_id=1,
        path_type="V2-V3",
        solve_block=42,
        block=42,
        age=0,
        revert_info="0x whatever",
    )
    payload = json.loads(line[len("[sim-diag] ") :])
    assert payload["engine_processed_block"] is None
    assert payload["onchain_block"] is None


def test_format_sim_diag_line_never_raises_on_missing_snapshot_keys() -> None:
    """A taxonomy/emission path must never raise — malformed snapshots emit a
    best-effort line with whatever fields are present (the analyzer tolerates
    missing keys)."""
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
        _result(1, 10_000, 100),  # 1.0% — keep
        _result(2, 10_000, 10),  # 0.1% — drop
        _result(3, 10_000, 50),  # 0.5% — keep (boundary, inclusive)
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
