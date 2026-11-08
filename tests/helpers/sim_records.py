"""Record-dict factories for the dispatch/simulator seams.

The PyO3 result types are too heavy to instantiate without a full simulate
round-trip, so tests build the plain-dict shapes the Python side reads:
the inline-sim payload row (the engine's result-channel field set) and the
failure record (the shape ``outcome.failures()`` emits).
"""

from __future__ import annotations

from typing import Any


def inline_sim_payload(
    pid: int,
    *,
    net: int = 500_000_000_000,
    failure: dict[str, Any] | None = None,
    deployment_id: int = 1,
) -> dict[str, Any]:
    """One inline-sim payload row (the engine's result-channel field set).

    ``deployment_id`` defaults to the built-in deployment a production session
    simulates at; a test overrides it to mint a foreign-deployment row.
    """
    return {
        "candidate": {"path_id": pid, "deployment_id": deployment_id},
        "gross_profit": 600_000_000_000,
        "net_profit": net,
        "gas_used": 300_000,
        "priority_fee": 2,
        "base_fee_next": 30,
        "execute_calldata": b"\xab\x58\x98\xe8\x01",
        "access_list": None,
        "captured_swaps": [],
        "hop_count": 2,
        "failure": failure,
    }


def failure_record(**overrides: object) -> dict[str, object]:
    """A minimal failure-record dict (the shape ``outcome.failures()`` emits)."""
    base: dict[str, object] = {
        "candidate": {"path_id": 7, "deployment_id": 1},
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
