"""Tests for the structured per-revert four-way classifier.

Ergo epic 63I7WJ (task AM5AJW): re-pointed at the inspector's captured swap
amounts vs the solver's ``hop_outputs``. Replaces the retired
``recompute.matches_solver`` / ``drift`` basis (deleted with the
``diagnostic.rs`` onchain-recompute half).

The ``Drift`` / ``DriftArtifact`` columns are kept in the TSV for column
stability but always tally 0 — the in-process captured swaps reflect the
engine's current state (same state the solver read), so drift (stale engine
vs mainnet) cannot be detected without the onchain recompute.
"""

import json

from logs.permutation_analyzer import classify_candidate

_PREFIX = "[sim-diag] "


def _line(payload: dict) -> dict:
    """Parse a [sim-diag] line back into the dict the classifier consumes."""
    return json.loads((_PREFIX + json.dumps(payload))[_PREFIX.__len__() :])


def _swap(*, family: str = "v2", amount0: int = -1000, amount1: int = 3000) -> dict:
    """A captured swap dict (the inspector's CapturedSwap shape). The OUTPUT is
    the positive amount (received); the input is negative (paid in)."""
    return {
        "family": family,
        "emitter": "0x" + "aa" * 20,
        "amount0": amount0,
        "amount1": amount1,
        "sqrt_price_x96": 0,
        "liquidity": 0,
        "tick": 0,
    }


def test_classify_solvercalc_when_captured_amount_differs_from_hop_output() -> None:
    """The captured swap's output (amount1=+3000) differs from the solver's
    hop_outputs[0] (2900) → SolverCalc (solver math was wrong)."""
    snap = _line({
        "revert_info": "0x CurrencyNotSettled",
        "hop_outputs": [2900],
        "captured_swaps": [_swap(amount0=-1000, amount1=3000)],
    })
    assert classify_candidate(snap) == "SolverCalc"


def test_classify_encoding_when_all_captured_amounts_match_hop_outputs() -> None:
    """Every captured swap's output matches its hop_outputs[i], yet the sim
    reverted → the amounts were right, so the encoded stream must be wrong →
    Encoding."""
    snap = _line({
        "revert_info": "0x CurrencyNotSettled",
        "hop_outputs": [3000],
        "captured_swaps": [_swap(amount0=-1000, amount1=3000)],
    })
    assert classify_candidate(snap) == "Encoding"


def test_classify_unknown_when_bare_empty_revert() -> None:
    """A bare/empty revert (no payload) → Unknown, never 'stale'."""
    snap = _line({"revert_info": "", "hop_outputs": [3000], "captured_swaps": [_swap()]})
    assert classify_candidate(snap) == "Unknown"


def test_classify_unknown_when_no_captured_swaps() -> None:
    """No captured swaps (orchestration-only bucket — no swaps ran before the
    revert, e.g. encode-failed or balance-decode) → Unknown."""
    snap = _line({"revert_info": "0x whatever", "hop_outputs": [3000], "captured_swaps": []})
    assert classify_candidate(snap) == "Unknown"


def test_classify_unknown_when_no_hop_outputs_or_malformed() -> None:
    """Malformed snapshot (no hop_outputs) → Unknown (never raises)."""
    snap = _line({"revert_info": "0x whatever", "captured_swaps": [_swap()]})
    assert classify_candidate(snap) == "Unknown"


def test_classify_unknown_when_v4_captured_swap() -> None:
    """A V4 captured swap's amount correctness is gated on task 5RI47E (the
    transient seeder) — cannot validate, so the hop is Unknown."""
    snap = _line({
        "revert_info": "0x CurrencyNotSettled",
        "hop_outputs": [3000],
        "captured_swaps": [_swap(family="v4", amount0=-1000, amount1=3000)],
    })
    assert classify_candidate(snap) == "Unknown"


# ---------------------------------------------------------------------------
# analyze_log: integration (sim-diag parsing, NoProfit, fallback, verify basis)
# ---------------------------------------------------------------------------

from logs.permutation_analyzer import analyze_log, result_to_tsv_row, tsv_header


def test_analyze_log_classifies_reverts_from_sim_diag_lines() -> None:
    log = (
        "[sim] 5 ok (3 profitable, 2 below threshold), 3 failed, 0 exceptions\n"
        "[sim] by reason: no-profit=1 CurrencyNotSettled=1 unknown:0x..=1\n"
        '[sim-diag] {"path_id":1,"revert_info":"0x CurrencyNotSettled",'
        '"hop_outputs":[3000],"captured_swaps":[{"family":"v2","amount0":-1000,"amount1":3000}]}\n'
        '[sim-diag] {"path_id":2,"revert_info":"0x IIA",'
        '"hop_outputs":[2900],"captured_swaps":[{"family":"v2","amount0":-1000,"amount1":3000}]}\n'
        '[sim-diag] {"path_id":3,"revert_info":"0x execution reverted",'
        '"hop_outputs":[3000],"captured_swaps":[]}\n'
    )
    r = analyze_log(log, permutation="V2-V3-V4")
    assert r.sim_ok == 5
    assert r.no_profit == 1
    assert r.reverts == 3
    assert r.candidates == 9
    assert r.classification == "⚠️ Partial"
    # One Encoding (amounts match), one SolverCalc (mismatch), one Unknown (no swaps).
    assert r.encoding == 1
    assert r.solver_calc == 1
    assert r.unknown == 1
    assert r.structured is True


def test_analyze_log_fallback_when_sim_diag_absent_every_revert_unknown() -> None:
    """Older logs without [sim-diag]: reverts counted from [sim-fail] lines and
    every revert falls into the Unknown column via the fallback."""
    log = (
        "[sim] 2 ok (1 profitable, 1 below threshold), 2 failed, 0 exceptions\n"
        "[sim] by reason: no-profit=0 IIA=1 CurrencyNotSettled=1\n"
        "[sim-fail] path=1 V2-V3-V4: call[0] failed revert=0x.. IIA\n"
        "[sim-fail] path=2 V2-V3-V4: call[0] failed revert=0x.. CurrencyNotSettled\n"
    )
    r = analyze_log(log, permutation="V2-V3-V4")
    assert r.reverts == 2
    assert r.unknown == 2
    assert r.drift == 0 and r.solver_calc == 0 and r.encoding == 0
    assert r.structured is False


def test_analyze_log_detects_verify_basis_from_startup_log() -> None:
    log_with_ok = "[verify] V3 snapshot OK: pool 0x… at block 100\n[sim] 1 ok (1), 0 failed\n"
    assert analyze_log(log_with_ok).verify_basis == "verified"

    log_skipped = "verify SKIPPED (no rpc_url)\n[sim] 1 ok (1), 0 failed\n"
    assert analyze_log(log_skipped).verify_basis == "skipped"

    log_none = "[sim] 1 ok (1), 0 failed\n"
    assert analyze_log(log_none).verify_basis == ""


def test_analyze_log_verify_basis_from_per_pool_gates() -> None:
    """The per-pool two-step gates (step-1 seed + step-2 post-drain) ARE the
    verification basis."""
    log = (
        "[verify-seed] V3 snapshot seed OK for 0x88e6…5640 at block 25396501\n"
        "[verify-drain] V3 post-drain snapshot OK for 0x88e6…5640 at block 25397043\n"
        "[sim] 1 ok (1), 0 failed\n"
    )
    assert analyze_log(log).verify_basis == "verified"


def test_analyze_log_verify_basis_recurring_drift_from_rust_mismatch() -> None:
    log = "[dbg-verify] MISMATCH 0x8ad5… tick=203880 block=25398650\n[sim] 1 ok (1), 0 failed\n"
    assert analyze_log(log).verify_basis == "recurring-drift"


def test_analyze_log_verify_basis_recurring_drift_takes_priority_when_ok_also_present() -> None:
    log = (
        "[verify-seed] V3 snapshot seed OK for 0x88e6…5640 at block 25396501\n"
        "[dbg-verify] MISMATCH 0x8ad5… tick=203880 block=25398650\n"
        "[sim] 1 ok (1), 0 failed\n"
    )
    assert analyze_log(log).verify_basis == "recurring-drift"


def test_analyze_log_verify_basis_recurring_python_line_now_visible() -> None:
    log = "[verify] (recurring) checking at block 25398650\n[sim] 1 ok (1), 0 failed\n"
    assert analyze_log(log).verify_basis == "recurring-drift"


def test_tsv_header_has_four_way_columns_no_stale() -> None:
    h = tsv_header()
    assert "Drift" in h and "SolverCalc" in h and "Encoding" in h and "Unknown" in h
    assert "Stale" not in h and "Bug" not in h and "IIA_Reverts" not in h


# ---------------------------------------------------------------------------
# basis_note (verify-basis qualifier)
# ---------------------------------------------------------------------------

from logs.permutation_analyzer import basis_note


def test_basis_note_structured_but_unverified() -> None:
    n = basis_note(structured=True, skipped=False, verified=False)
    assert n == (
        "# basis: structured-four-way; drift basis unconfirmed "
        "(no [verify] line in structured runs)"
    )


def test_basis_note_structured_and_verified() -> None:
    n = basis_note(structured=True, skipped=False, verified=True)
    assert "drift attributable to pump desync (verify OK present)" in n


def test_basis_note_skipped_trumps_verified() -> None:
    n = basis_note(structured=True, skipped=True, verified=True)
    assert "verify SKIPPED" in n and "pump desync" not in n


def test_basis_note_fallback_when_unstructured() -> None:
    n = basis_note(structured=False, skipped=False, verified=False)
    assert n.startswith("# basis: fallback-unknown")
