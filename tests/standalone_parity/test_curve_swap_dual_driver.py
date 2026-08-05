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
