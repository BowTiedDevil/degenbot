"""Tier-2 behavioral dual-driver parity — Curve `get_dy`.

The behavioral companion to the Rust `parity_curve_swap.rs` test. Proves the
**same** canonical Curve fixture driven through the **Python consumer**
(`degenbot.curve.dy.calculate_dy`, the PyO3 seam) produces the **same** `dy`
as the **Rust consumer** (`degenbot_curve_math::calculate_dy` directly).
Divergence = a lossy FFI seam on the Curve swap-arg extraction (the
`DyCalculationInputs` builder).

The Curve StableSwap dy math has no simple closed form (the `stableswap_get_y`
invariant is a Newton solve), so — like V3/V4 — the oracle is the recorded
constant in the shared fixture; the Python and Rust sides independently
re-derive it from the same inputs.

## The shared contract (HRT356 — single source of truth)

The fixture + expected outputs are loaded from the SHARED file
`tests/standalone_parity/fixtures/curve_swap.json`, which the Rust parity test
ALSO loads. A fixture edit that drifts an expected output fails BOTH sides
mechanically.
"""

from __future__ import annotations

import json
from pathlib import Path

from degenbot.curve.dy import DyCalculationInputs, calculate_dy

_FIXTURE_PATH = Path(__file__).parent / "fixtures" / "curve_swap.json"
_FIXTURE = json.loads(_FIXTURE_PATH.read_text())


def _build_inputs(raw: dict) -> DyCalculationInputs:
    """Build the Rust `DyCalculationInputs` snapshot from the fixture dict."""
    r = DyCalculationInputs()
    r.precision = int(raw["precision"])
    r.fee_denominator = int(raw["fee_denominator"])
    r.fee = int(raw["fee"])
    r.n_coins = int(raw["n_coins"])
    r.balances = [int(v) for v in raw["balances"]]
    r.rate_multipliers = [int(v) for v in raw["rate_multipliers"]]
    r.precision_multipliers = [int(v) for v in raw["precision_multipliers"]]
    r.resolved_rates = [int(v) for v in raw["resolved_rates"]]
    r.xp = [int(v) for v in raw["xp"]]
    r.amp = int(raw["amp"])
    r.a_precision = int(raw["a_precision"])
    r.d_variant = int(raw["d_variant"])
    r.y_variant = int(raw["y_variant"])
    r.swap_style = int(raw["swap_style"])
    r.metapool = bool(raw["metapool"])
    r.metapool_rate_style = int(raw["metapool_rate_style"])
    r.metapool_underlying_style = int(raw["metapool_underlying_style"])
    r.virtual_price = int(raw["virtual_price"]) if raw["virtual_price"] is not None else None
    return r


def test_python_consumer_curve_dy_matches_recorded_constant() -> None:
    """The Python consumer reproduces every recorded dy from the shared fixture."""
    probes = _FIXTURE["probes"]
    assert probes, "fixture must contain probes"

    for probe in probes:
        inputs = _build_inputs(probe["inputs"])
        # Block/timestamp are unused by the pure calc; leave at defaults.
        dy = calculate_dy(
            probe["probe"]["i"], probe["probe"]["j"], int(probe["probe"]["dx"]), inputs
        )
        expected = int(probe["expected"]["dy"])
        assert dy == expected, f"Python consumer dy mismatch for probe `{probe['name']}`"


def test_python_consumer_curve_get_dy_matches_recorded_constant() -> None:
    """The Python consumer drives the Rust-owned `Bot.curve_get_dy`.

    The orchestration-layer complement of the pure-calc parity test above: the
    Python consumer registers the shared `standard_plain` pool into a `Bot`
    and calls `Bot.curve_get_dy` (identity + balances + provider →
    `resolve_dy_inputs` → `calculate_dy`), which must reproduce the same
    recorded dy as the Rust `BotState::curve_get_dy` dual-driver test. Both
    sides read the `standard_plain` expected constant from the shared fixture.
    """
    from degenbot._ffi import Bot

    plain = next(p for p in _FIXTURE["probes"] if p["name"] == "standard_plain")
    inputs = plain["inputs"]

    py_bot = Bot()
    pool_id = py_bot.register_curve_pool(
        address="0x" + "cc" * 20,
        tokens=["0x" + "00" * 20, "0x" + "01" * 20],
        a_coefficient=100,
        a_precision=100,
        fee=500_000,
        admin_fee=0,
        rate_multipliers=[int(v) for v in inputs["rate_multipliers"]],
        balances=[int(v) for v in inputs["balances"]],
        update_block=0,
        swap_style=1,  # STANDARD
        lending_rate_style=1,  # NONE
        d_variant=1,
        y_variant=1,
        yd_variant=1,
        precision_multipliers=[int(v) for v in inputs["precision_multipliers"]],
    )

    dy = py_bot.curve_get_dy(
        pool_id,
        plain["probe"]["i"],
        plain["probe"]["j"],
        int(plain["probe"]["dx"]),
        block_number=0,
    )
    expected = int(plain["expected"]["dy"])
    assert dy == expected, f"Python consumer curve_get_dy mismatch: {dy} != {expected}"
