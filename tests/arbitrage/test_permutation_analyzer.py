"""Tests for the structured per-revert four-way classifier (CPCZZV).

The classifier replaces the false `Stale_Reverts`/`Bug_Reverts` dichotomy with
Drift / SolverCalc / Encoding / Unknown, parsed from the `[sim-diag]` JSON line
each reverted candidate emits.
"""

import json

from logs.permutation_analyzer import classify_candidate

_PREFIX = "[sim-diag] "


def _line(payload: dict) -> dict:
    """Parse a [sim-diag] line back into the dict the classifier consumes."""
    return json.loads((_PREFIX + json.dumps(payload))[_PREFIX.__len__() :])


def _hop(*, drift: bool = False, matches_solver: object = None) -> dict:
    return {
        "drift": drift,
        "recompute": {"matches_solver": matches_solver},
    }


def test_classify_drift_when_any_hop_drifts() -> None:
    """Drift takes precedence: any hop with drift=true → Drift, regardless of
    the recompute result (a drifted state makes the recompute basis invalid)."""
    snap = _line(
        {
            "hops": [
                _hop(drift=False, matches_solver=True),
                _hop(drift=True, matches_solver=False),
                _hop(drift=False, matches_solver=True),
            ],
            "revert_info": "0x IIA",
        }
    )
    assert classify_candidate(snap) == "Drift"


def test_classify_solvercalc_when_no_drift_and_recompute_mismatches() -> None:
    """No drift + any hop recompute.matches_solver == False → SolverCalc
    (correct on-chain state, wrong solver output)."""
    snap = _line(
        {
            "hops": [
                _hop(drift=False, matches_solver=True),
                _hop(drift=False, matches_solver=False),
            ],
            "revert_info": "0x SomeError",
        }
    )
    assert classify_candidate(snap) == "SolverCalc"


def test_classify_encoding_when_no_drift_and_all_recompute_matches() -> None:
    """No drift + all hops matches_solver == True, yet sim reverted → the
    amounts were right, so the stream must be wrong → Encoding."""
    snap = _line(
        {
            "hops": [
                _hop(drift=False, matches_solver=True),
                _hop(drift=False, matches_solver=True),
            ],
            "revert_info": "0x CurrencyNotSettled",
        }
    )
    assert classify_candidate(snap) == "Encoding"


def test_classify_unknown_when_recompute_unavailable() -> None:
    """No drift + recompute unavailable (matches_solver None on every hop — V3/V4
    where onchain recompute is deferred) → Unknown (cannot attribute)."""
    snap = _line(
        {
            "hops": [
                _hop(drift=False, matches_solver=None),
                _hop(drift=False, matches_solver=None),
            ],
            "revert_info": "0x execution reverted",
        }
    )
    assert classify_candidate(snap) == "Unknown"


def test_classify_unknown_when_bare_empty_revert() -> None:
    """A bare/empty revert (no payload) → Unknown, never 'stale'."""
    snap = _line({"hops": [_hop(drift=False, matches_solver=None)], "revert_info": ""})
    assert classify_candidate(snap) == "Unknown"


def test_classify_unknown_when_no_hops_or_malformed() -> None:
    """Malformed snapshot (no hops) → Unknown (never raises)."""
    snap = _line({"hops": [], "revert_info": "0x whatever"})
    assert classify_candidate(snap) == "Unknown"

# ---------------------------------------------------------------------------
# analyze_log: integration (sim-diag parsing, NoProfit, fallback, verify basis)
# ---------------------------------------------------------------------------

from logs.permutation_analyzer import analyze_log, tsv_header


def test_analyze_log_classifies_reverts_from_sim_diag_lines() -> None:
    log = (
        "[sim] 5 ok (3 profitable, 2 below threshold), 3 failed, 0 exceptions\n"
        "[sim] by reason: no-profit=1 CurrencyNotSettled=1 unknown:0x..=1\n"
        "[sim-diag] {\"path_id\":1,\"revert_info\":\"0x CurrencyNotSettled\","
        "\"hops\":[{\"drift\":false,\"recompute\":{\"matches_solver\":true}}]}\n"
        "[sim-diag] {\"path_id\":2,\"revert_info\":\"0x IIA\","
        "\"hops\":[{\"drift\":true,\"recompute\":{\"matches_solver\":null}}]}\n"
        "[sim-diag] {\"path_id\":3,\"revert_info\":\"0x execution reverted\","
        "\"hops\":[{\"drift\":false,\"recompute\":{\"matches_solver\":null}}]}\n"
    )
    r = analyze_log(log, permutation="V2-V3-V4")
    assert r.sim_ok == 5
    assert r.no_profit == 1
    assert r.reverts == 3
    assert r.candidates == 9
    assert r.classification == "⚠️ Partial"
    # One Encoding (no drift, all matches true), one Drift, one Unknown.
    assert r.drift == 1
    assert r.solver_calc == 0
    assert r.encoding == 1
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


def test_tsv_header_has_four_way_columns_no_stale() -> None:
    h = tsv_header()
    assert "Drift" in h and "SolverCalc" in h and "Encoding" in h and "Unknown" in h
    assert "Stale" not in h and "Bug" not in h and "IIA_Reverts" not in h


def test_classify_v3_no_drift_partial_recompute_is_unknown_not_solvercalc() -> None:
    """A V3/V4 no-drift revert where recompute is unavailable (matches_solver
    None on every hop — the 4BKMKX deferred case) must classify as Unknown,
    NEVER as SolverCalc. Guards against the V2 refresh clobbering a V3/V4
    hop's deferred-None on-chain fields with a spurious matches_solver=False
    (which would wrongly trigger SolverCalc)."""
    snap = _line(
        {
            "hops": [
                # V3 hop: no drift, recompute fully deferred (all None).
                _hop(drift=False, matches_solver=None),
            ],
            "revert_info": "0x CurrencyNotSettled",
        }
    )
    assert classify_candidate(snap) == "Unknown"


# ---------------------------------------------------------------------------
# basis_note (verify-basis qualifier on the Drift verdict)
# ---------------------------------------------------------------------------

from logs.permutation_analyzer import basis_note


def test_basis_note_structured_but_unverified() -> None:
    """[sim-diag] lines exist but no run emitted a [verify] line → drift basis
    unconfirmed (can't attribute to pump desync vs bad snapshot)."""
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
