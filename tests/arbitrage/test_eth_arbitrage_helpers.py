"""Tests for the non-encoder helpers in ``eth_backrun_helpers``.

The command-stream encoders (``_3hop_*`` / ``_encode_cmd_*``) were retired in
the YQORTM §4.3 cutover — their byte-exact parity now lives in the Rust
golden-file tests (``cargo test -p degenbot-executor``). This file keeps the
regression coverage for the **stays-Python** display helper
``format_sim_diag_line`` (now in :mod:`degenbot.runner._render`).

The ``filter_thin_margin_results`` tests were deleted with the function
(epic Y7PA5A, task 34XJ6C — the legacy ``main()`` that used it no longer
exists). The ``classify_candidate`` round-trip tests were removed along with
the ``logs.permutation_analyzer`` module (Rust tracing replaced Python-side
classifier infra).
"""

import json

from degenbot.runner._render import format_sim_diag_line


def _failure(**overrides: object) -> dict[str, object]:
    """A minimal failure-record dict (the shape outcome.failures() emits)."""
    base: dict[str, object] = {
        "path_id": 7,
        "bucket": "unknown:0xcafebabe",
        "fail_index": 3,
        "revert_data": "0xcafebabe",
        "reverting_frame": None,
        "captured_swaps": [
            {
                "family": "v2",
                "emitter": "0x" + "aa" * 20,
                "amount0": -1000,
                "amount1": 3000,
                "sqrt_price_x96": 0,
                "liquidity": 0,
                "tick": 0,
            }
        ],
        "optimal_input": 1000,
        "hop_outputs": [3000],
    }
    base.update(overrides)
    return base


def test_format_sim_diag_line_emits_parseable_json_with_required_fields() -> None:
    """The [sim-diag] line is one JSON object parseable with json.loads.

    Carries the captured-swaps basis: path_id, path_type, solve_block, block,
    age, revert_info (the bucket), optimal_input, hop_outputs (expected), and
    captured_swaps (actual). The classifier compares hop_outputs[i] vs the
    i-th captured swap's output amount.
    """
    line = format_sim_diag_line(
        _failure(),
        path_id=7,
        path_type="V2-V3-V4",
        solve_block=100,
        block=103,
        age=3,
    )
    assert line.startswith("[sim-diag] "), "line is prefixed [sim-diag] "
    payload = json.loads(line[len("[sim-diag] ") :])
    assert payload["path_id"] == 7
    assert payload["path_type"] == "V2-V3-V4"
    assert payload["solve_block"] == 100
    assert payload["block"] == 103
    assert payload["age"] == 3
    assert payload["revert_info"] == "unknown:0xcafebabe"
    assert payload["optimal_input"] == 1000
    assert payload["hop_outputs"] == [3000]
    swap = payload["captured_swaps"][0]
    assert swap["family"] == "v2"
    assert swap["amount0"] == -1000
    assert swap["amount1"] == 3000


def test_format_sim_diag_line_never_raises_on_missing_keys() -> None:
    """A taxonomy/emission path must never raise — malformed failure records
    emit a best-effort line with whatever fields are present."""
    line = format_sim_diag_line(
        {},
        path_id=1,
        path_type="V2",
        solve_block=1,
        block=1,
        age=0,
    )
    payload = json.loads(line[len("[sim-diag] ") :])
    assert payload["path_id"] == 1
    assert payload["revert_info"] == ""
    assert payload["optimal_input"] is None
    assert payload["hop_outputs"] == []
    assert payload["captured_swaps"] == []


def test_format_sim_diag_line_omits_retired_recompute_fields() -> None:
    """The new payload does NOT carry the retired engine_state/onchain_state/
    recompute/drift/engine_processed_block/onchain_block fields (deleted with
    the diagnostic.rs onchain-recompute half). Only captured_swaps +
    hop_outputs + optimal_input + revert_info remain."""
    line = format_sim_diag_line(
        _failure(),
        path_id=1,
        path_type="V2-V3",
        solve_block=1,
        block=1,
        age=0,
    )
    payload = json.loads(line[len("[sim-diag] ") :])
    assert "hops" not in payload, "retired per-hop snapshot shape is gone"
    assert "recompute" not in payload
    assert "engine_processed_block" not in payload
    assert "onchain_block" not in payload
