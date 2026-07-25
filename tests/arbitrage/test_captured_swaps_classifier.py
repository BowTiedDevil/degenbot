"""Tests for the captured-swaps-based four-way classifier (ergo epic 63I7WJ AM5AJW).

Replaces the old `recompute.matches_solver` basis (deleted with the
`diagnostic.rs` onchain-recompute half) with a direct comparison of the
inspector's captured swap amounts (the ACTUAL amounts the in-process EVM
emitted) vs the solver's reported `hop_outputs` (the EXPECTED amounts).

Classification logic (applied per reverted candidate, in order):

- **SolverCalc**: any captured swap's output amount ≠ the corresponding
  `hop_outputs[i]` (the solver's math disagreed with the EVM's actual output).
- **Encoding**: every captured swap matches its `hop_outputs[i]`, yet the sim
  reverted (the amounts were right, so the encoded command stream must be
  wrong — or the engine state drifted from mainnet, which the in-process
  captured swaps cannot distinguish; noted in the basis).
- **Unknown**: bare/empty revert OR no captured swaps (orchestration-only
  bucket — no swaps ran before the revert) OR V4 hops (amount correctness
  gated on task 5RI47E, the transient seeder).
- **Drift**: 0 — the in-process captured swaps reflect the engine's current
  state (same state the solver read), so drift (stale engine vs mainnet)
  cannot be detected without the onchain recompute. The column is kept for TSV
  stability; `analyze_log` always tallies 0.

The `[sim-diag]` payload shape:
    {
        "path_id": int, "revert_info": "0x...", "optimal_input": int,
        "hop_outputs": [int, ...],
        "captured_swaps": [{family, emitter, amount0, amount1, ...}, ...]
    }
"""

from __future__ import annotations

import json

from logs.permutation_analyzer import classify_candidate

_PREFIX = "[sim-diag] "


def _line(payload: dict) -> dict:
    """Parse a [sim-diag] line back into the dict the classifier consumes."""
    return json.loads((_PREFIX + json.dumps(payload))[_PREFIX.__len__() :])


def _swap(*, family: str = "v2", amount0: int = -1000, amount1: int = 3000) -> dict:
    """A captured swap dict (the inspector's CapturedSwap shape). amount0/amount1
    are signed deltas: negative = paid in, positive = received. For an
    exact-input swap, one is negative (the input) and one is positive (the
    output). The OUTPUT amount is the positive one."""
    return {
        "family": family,
        "emitter": "0x" + "aa" * 20,
        "amount0": amount0,
        "amount1": amount1,
        "sqrt_price_x96": 0,
        "liquidity": 0,
        "tick": 0,
    }


# ---------------------------------------------------------------------------
# SolverCalc: captured swap amount ≠ solver's hop_output
# ---------------------------------------------------------------------------


def test_classify_solvercalc_when_captured_amount_differs_from_hop_output() -> None:
    """The captured swap's output (amount1=+3000) differs from the solver's
    hop_outputs[0] (2900) → SolverCalc (solver math was wrong)."""
    snap = _line({
        "revert_info": "0x CurrencyNotSettled",
        "hop_outputs": [2900],
        "captured_swaps": [_swap(amount0=-1000, amount1=3000)],
    })
    assert classify_candidate(snap) == "SolverCalc"


def test_classify_solvercalc_on_second_hop_mismatch() -> None:
    """A multi-hop path where the SECOND hop's captured amount differs from
    hop_outputs[1] → SolverCalc (the mismatch doesn't have to be the first)."""
    snap = _line({
        "revert_info": "0x IIA",
        "hop_outputs": [3000, 2900],
        "captured_swaps": [
            _swap(amount0=-1000, amount1=3000),  # matches hop_outputs[0]
            _swap(amount0=-3000, amount1=3100),  # != hop_outputs[1]=2900
        ],
    })
    assert classify_candidate(snap) == "SolverCalc"


# ---------------------------------------------------------------------------
# Encoding: all captured amounts match, but sim reverted
# ---------------------------------------------------------------------------


def test_classify_encoding_when_all_captured_amounts_match_hop_outputs() -> None:
    """Every captured swap's output matches its hop_outputs[i], yet the sim
    reverted → the amounts were right, so the encoded stream must be wrong →
    Encoding."""
    snap = _line({
        "revert_info": "0x CurrencyNotSettled",
        "hop_outputs": [3000, 2990],
        "captured_swaps": [
            _swap(amount0=-1000, amount1=3000),  # matches hop_outputs[0]
            _swap(amount0=-3000, amount1=2990),  # matches hop_outputs[1]
        ],
    })
    assert classify_candidate(snap) == "Encoding"


def test_classify_encoding_single_hop_amount_matches() -> None:
    """Single-hop path, captured matches expected, sim reverted → Encoding."""
    snap = _line({
        "revert_info": "0x execution reverted",
        "hop_outputs": [3000],
        "captured_swaps": [_swap(amount0=-1000, amount1=3000)],
    })
    assert classify_candidate(snap) == "Encoding"


# ---------------------------------------------------------------------------
# Unknown: bare revert, no captured swaps, V4 deferred
# ---------------------------------------------------------------------------


def test_classify_unknown_when_bare_empty_revert() -> None:
    """A bare/empty revert (no payload) → Unknown."""
    snap = _line({"revert_info": "", "hop_outputs": [3000], "captured_swaps": [_swap()]})
    assert classify_candidate(snap) == "Unknown"


def test_classify_unknown_when_no_captured_swaps() -> None:
    """No captured swaps (orchestration-only bucket — the sim reverted before
    any swap emitted, e.g. encode-failed or balance-decode) → Unknown."""
    snap = _line({
        "revert_info": "0x whatever",
        "hop_outputs": [3000],
        "captured_swaps": [],
    })
    assert classify_candidate(snap) == "Unknown"


def test_classify_unknown_when_v4_captured_swap_present() -> None:
    """A V4 captured swap's amount correctness is gated on task 5RI47E (the
    transient seeder) — cannot validate the amount, so the hop is Unknown."""
    snap = _line({
        "revert_info": "0x CurrencyNotSettled",
        "hop_outputs": [3000],
        "captured_swaps": [_swap(family="v4", amount0=-1000, amount1=3000)],
    })
    assert classify_candidate(snap) == "Unknown"


def test_classify_unknown_when_malformed_no_hop_outputs() -> None:
    """Malformed snapshot (no hop_outputs) → Unknown (never raises)."""
    snap = _line({"revert_info": "0x whatever", "captured_swaps": [_swap()]})
    assert classify_candidate(snap) == "Unknown"


def test_classify_unknown_when_captured_swaps_count_mismatches_hop_outputs() -> None:
    """The captured-swaps count ≠ the hop_outputs count (shouldn't happen in
    normal operation — each hop emits one swap — but defensively → Unknown,
    not a false SolverCalc/Encoding)."""
    snap = _line({
        "revert_info": "0x whatever",
        "hop_outputs": [3000, 2990],
        "captured_swaps": [_swap()],  # only 1 swap, 2 hop_outputs
    })
    assert classify_candidate(snap) == "Unknown"


# ---------------------------------------------------------------------------
# Direction-agnostic: the output is the positive amount regardless of which
# token (amount0 vs amount1) is the output (zfo flag).
# ---------------------------------------------------------------------------


def test_classify_solvercalc_reverse_direction_amount0_is_output() -> None:
    """When the swap direction is reversed (token1 in, token0 out), the
    output is amount0 (positive), not amount1. The classifier must compare the
    positive amount (the output) to hop_outputs[i], regardless of direction."""
    snap = _line({
        "revert_info": "0x IIA",
        "hop_outputs": [3000],
        "captured_swaps": [_swap(amount0=3100, amount1=-1000)],  # token0 is output
    })
    assert classify_candidate(snap) == "SolverCalc"  # 3100 ≠ 3000


def test_classify_encoding_reverse_direction_amount0_matches() -> None:
    """Reverse direction, the output (amount0=+3000) matches hop_outputs[0]
    → Encoding (amounts right, sim reverted)."""
    snap = _line({
        "revert_info": "0x IIA",
        "hop_outputs": [3000],
        "captured_swaps": [_swap(amount0=3000, amount1=-1000)],
    })
    assert classify_candidate(snap) == "Encoding"
