"""Structured per-revert four-way classifier for permutation logs (CPCZZV).

Replaces the false ``Stale_Reverts``/``Bug_Reverts`` dichotomy with
**Drift / SolverCalc / Encoding / Unknown**, parsed from the ``[sim-diag]`` JSON
line each reverted candidate emits (added in the LAV44W task).

Re-pointed in ergo epic 63I7WJ (task AM5AJW) at the inspector's captured
swap amounts (the ACTUAL amounts the in-process EVM emitted) vs the solver's
reported ``hop_outputs`` (the EXPECTED amounts), retiring the deleted
``recompute.matches_solver`` (the onchain-recompute basis that retired with
``diagnostic.rs``). V4 re-enabled in task JIXPNZ (capture proven byte-exact
by the ``swap_capture_correctness`` mainnet probe; the reverted-frame
over-capture fix makes captured-swaps trustworthy across V2/V3/V4).

Classification logic (applied per reverted candidate, in order):

- **Drift**: not detectable without the onchain recompute (the in-process
  captured swaps reflect the engine's own state, the same state the solver
  read); the ``Drift``/``DriftArtifact`` columns are kept for stability but
  always tally 0. Restoring a drift signal is its own task (a fresh onchain
  probe, not the retired recompute).
- **SolverCalc**: any hop's captured-swap output != the solver's
  ``hop_outputs[i]`` — the solver's math was wrong (or read stale state).
- **Encoding**: every hop's captured-swap output MATCHES its
  ``hop_outputs[i]``, yet the sim reverted → the amounts were right, so the
  encoded stream must be wrong.
- **Unknown**: bare/empty revert (``0x execution reverted``), no captured
  swaps (orchestration-only revert before any swap ran), or a malformed
  snapshot. Classified conservatively — never as "stale".

When ``[sim-diag]`` lines are absent (older logs predating LAV44W), every revert
falls into a legacy ``Unknown`` column with a header note (see
``analyze_log``).
"""

from __future__ import annotations

import json
import re
from dataclasses import dataclass

PREFIX = "[sim-diag] "
PERMUTATIONS = [
    "V2-V2-V2", "V2-V2-V3", "V2-V2-V4", "V2-V3-V2", "V2-V3-V3", "V2-V3-V4",
    "V2-V4-V2", "V2-V4-V3", "V2-V4-V4", "V3-V2-V2", "V3-V2-V3", "V3-V2-V4",
    "V3-V3-V2", "V3-V3-V3", "V3-V3-V4", "V3-V4-V2", "V3-V4-V3", "V3-V4-V4",
    "V4-V2-V2", "V4-V2-V3", "V4-V2-V4", "V4-V3-V2", "V4-V3-V3", "V4-V3-V4",
    "V4-V4-V2", "V4-V4-V3", "V4-V4-V4",
]

# Verdict labels (kept stable for the TSV columns).
DRIFT = "Drift"
DRIFT_ARTIFACT = "DriftArtifact"
SOLVER_CALC = "SolverCalc"
ENCODING = "Encoding"
UNKNOWN = "Unknown"


PASSING_PCT = 80
PARTIAL_PCT = 20


def classify_candidate(sim_diag: dict) -> str:
    """Classify one reverted candidate from its parsed ``[sim-diag]`` payload.

    Ergo epic 63I7WJ (task AM5AJW): re-pointed at the inspector's captured
    swap amounts (the ACTUAL amounts the in-process EVM emitted) vs the
    solver's reported ``hop_outputs`` (the EXPECTED amounts). Replaces the
    deleted ``recompute.matches_solver`` (the onchain-recompute basis that
    retired with ``diagnostic.rs``).

    Pure + total: never raises (a malformed/empty snapshot classifies as
    ``Unknown`` so the analyzer never blocks on a bad line).
    """
    if not isinstance(sim_diag, dict):
        return UNKNOWN

    # A bare/empty revert cannot be attributed even with clean captured swaps.
    revert_info = sim_diag.get("revert_info", "") or ""
    if not revert_info.strip():
        return UNKNOWN

    hop_outputs = sim_diag.get("hop_outputs")
    captured_swaps = sim_diag.get("captured_swaps")
    if not isinstance(hop_outputs, list) or not isinstance(captured_swaps, list):
        return UNKNOWN
    # No captured swaps = orchestration-only bucket (encode-failed,
    # balance-decode, int128-overflow — no swaps ran before the revert).
    if not captured_swaps:
        return UNKNOWN
    # A count mismatch shouldn't happen in normal operation (each hop emits
    # one swap); defensively Unknown, not a false SolverCalc/Encoding.
    if len(captured_swaps) != len(hop_outputs):
        return UNKNOWN

    all_match = True
    any_mismatch = False
    for i, swap in enumerate(captured_swaps):
        if not isinstance(swap, dict):
            return UNKNOWN
        # V4 capture correctness is PROVEN (ergo task JIXPNZ): the
        # `swap_capture_correctness` mainnet probe asserts captured V4
        # swaps byte-match the onchain receipt (block 25615015 tx[0],
        # byte-exact across amounts + post-swap sqrtPriceX96/liquidity/
        # tick), and the reverted-frame over-capture fix drops reverted
        # sub-call swaps. So V4 hops classify on the same captured-amount-vs-
        # hop_output basis as V2/V3 — no longer gated on the retired 5RI47E
        # transient seeder (false premise: V4 pool state is PERSISTENT at
        # slot 6, not transient storage).
        amount0 = swap.get("amount0", 0)
        amount1 = swap.get("amount1", 0)
        # The output is the positive amount (received); the input is negative
        # (paid in). For an exact-input swap, exactly one is positive.
        actual_output = max(amount0, amount1)
        expected_output = hop_outputs[i]
        if actual_output != expected_output:
            any_mismatch = True
            all_match = False

    if any_mismatch:
        return SOLVER_CALC
    if all_match:
        return ENCODING
    return UNKNOWN


def _parse_sim_diag_lines(log_text: str) -> list[dict]:
    """Extract every ``[sim-diag]`` JSON payload from a log (in order)."""
    out: list[dict] = []
    for raw in log_text.splitlines():
        idx = raw.find(PREFIX)
        if idx < 0:
            continue
        try:
            out.append(json.loads(raw[idx + len(PREFIX) :]))
        except json.JSONDecodeError:
            continue
    return out


@dataclass
class AnalysisResult:
    """Per-permutation analysis row."""

    permutation: str
    candidates: int
    sim_ok: int
    no_profit: int
    reverts: int
    sim_rate: str
    classification: str
    drift: int = 0
    drift_artifact: int = 0
    solver_calc: int = 0
    encoding: int = 0
    unknown: int = 0
    hung: int = 0
    # True when [sim-diag] lines were present (structured classification);
    # False when the fallback (every revert → Unknown) was used.
    structured: bool = True
    # The verification basis the run used (for the drift attribution header):
    # "verified" if a startup `[verify] … OK` line is present,
    # "skipped" if verification was skipped, "" if undetectable.
    verify_basis: str = ""


_OK_RE = re.compile(r"(\d+) ok \(")
_NO_PROFIT_REASON_RE = re.compile(r"by reason:.*?no-profit=(\d+)")
_NO_PROFIT_DETAIL_RE = re.compile(r"no-profit=(\d+)")
_DISPATCH_SIM_RE = re.compile(r"\[dispatch\] simulating (\d+)/")
_SIM_CANDIDATES_RE = re.compile(r"\[sim\] (\d+) candidates:")
_VERIFY_OK_RE = re.compile(r"\[verify(?:-seed|-drain)?\].*OK")
_VERIFY_SKIPPED_RE = re.compile(r"verify.*SKIPPED|verification.*skipped", re.IGNORECASE)
# GTOD23-YBEYKY (T4): the recurring in-loop verifier's Python-side
# ``[verify] (recurring)`` lines were silenced (S2/PB24RX). Its Rust-side
# mismatch emit survives under ``[dbg-verify] MISMATCH`` — recognize it as
# "recurring verify ran + detected drift" so the analyzer reports the
# drift detection instead of reporting no recurring activity.
_VERIFY_RECUR_RE = re.compile(r"\[verify\].*\(recurring\)|\[dbg-verify\]\s*MISMATCH")


def analyze_log(log_text: str, permutation: str = "") -> AnalysisResult:
    """Analyze one permutation log into a four-way classification row.

    ``NoProfit`` is derived from the bot's authoritative ``by reason:
    no-profit=N`` breakdown summary (the detailed ``[sim-fail] no-profit`` line
    count undercounts). Falls back to the detail-line count when the summary is
    absent.
    """
    ok_total = sum(int(m.group(1)) for m in _OK_RE.finditer(log_text))
    no_profit = 0
    for m in _NO_PROFIT_REASON_RE.finditer(log_text):
        no_profit += int(m.group(1))
    if no_profit == 0:
        # Fallback to the detail-line tally if the authoritative summary is
        # absent (older logs).
        no_profit = sum(int(m.group(1)) for m in _NO_PROFIT_DETAIL_RE.finditer(log_text))

    sim_diag_payloads = _parse_sim_diag_lines(log_text)
    structured = bool(sim_diag_payloads)
    reverts = len(sim_diag_payloads)
    # When [sim-diag] is absent, count reverts from [sim-fail] revert lines
    # (the canonical revert marker; [sim-revert-data] duplicates with calldata).
    if not structured:
        reverts = len(re.findall(r"\[sim-fail\].*revert=0x", log_text))

    counts = {DRIFT: 0, DRIFT_ARTIFACT: 0, SOLVER_CALC: 0, ENCODING: 0, UNKNOWN: 0}
    for snap in sim_diag_payloads:
        counts[classify_candidate(snap)] += 1
    if not structured:
        # Legacy fallback: every revert is Unknown (no structured basis).
        counts[UNKNOWN] = reverts

    total = ok_total + no_profit + reverts
    if total == 0:
        cls, pct = "⬜ Inconclusive", "—"
    else:
        sim_ok = ok_total + no_profit
        pv = sim_ok * 100 // total
        if pv >= PASSING_PCT:
            cls = "✅ Passing"
        elif pv >= PARTIAL_PCT:
            cls = "⚠️ Partial"
        else:
            cls = "❌ Broken"
        pct = f"{pv}%"

    disp = sum(int(m.group(1)) for m in _DISPATCH_SIM_RE.finditer(log_text))
    summ = sum(int(m.group(1)) for m in _SIM_CANDIDATES_RE.finditer(log_text))
    hung = max(0, disp - summ)

    verify_basis = ""
    if _VERIFY_OK_RE.search(log_text):
        verify_basis = "verified"
        # S2/T4: surface recurring-verify drift detection alongside the
        # per-pool two-step. ``recurring-drift`` means the in-loop verifier
        # ran AND mismatched (caught drift the registration-time two-step
        # didn't, because it only verifies at registration).
        if _VERIFY_RECUR_RE.search(log_text):
            verify_basis = "recurring-drift"
    elif _VERIFY_RECUR_RE.search(log_text):
        verify_basis = "recurring-drift"
    elif _VERIFY_SKIPPED_RE.search(log_text):
        verify_basis = "skipped"

    return AnalysisResult(
        permutation=permutation,
        candidates=total,
        sim_ok=ok_total,
        no_profit=no_profit,
        reverts=reverts,
        sim_rate=pct,
        classification=cls,
        drift=counts[DRIFT],
        drift_artifact=counts[DRIFT_ARTIFACT],
        solver_calc=counts[SOLVER_CALC],
        encoding=counts[ENCODING],
        unknown=counts[UNKNOWN],
        hung=hung,
        structured=structured,
        verify_basis=verify_basis,
    )


def tsv_header() -> str:
    """The six-column TSV header (CPCZZV schema)."""
    return "\t".join(
        [
            "#",
            "Permutation",
            "Candidates",
            "SimOK",
            "NoProfit",
            "Reverts",
            "SimRate",
            "Classification",
            "Drift",
            "DriftArtifact",
            "SolverCalc",
            "Encoding",
            "Unknown",
            "Hung",
        ]
    )


def result_to_tsv_row(index: int, r: AnalysisResult) -> str:
    return "\t".join(
        str(x)
        for x in [
            index,
            r.permutation,
            r.candidates,
            r.sim_ok,
            r.no_profit,
            r.reverts,
            r.sim_rate,
            r.classification,
            r.drift,
            r.drift_artifact,
            r.solver_calc,
            r.encoding,
            r.unknown,
            r.hung,
        ]
    )


def analyze_logfile(logpath: str, permutation: str = "") -> AnalysisResult:
    """Read + analyze a single permutation log file."""
    with open(logpath, encoding="utf-8", errors="replace") as f:
        return analyze_log(f.read(), permutation=permutation)

def basis_note(
    *,
    structured: bool,
    skipped: bool,
    verified: bool,
) -> str:
    """Render the ``# basis:`` TSV footer note (CPCZZV/ZS2EYW).

    Qualifies what a ``Drift`` verdict means given the verification basis
    across the analyzed run(s).
    """
    note = "# basis: "
    if structured:
        note += "structured-four-way"
    else:
        note += "fallback-unknown (no [sim-diag] lines — older logs)"
    if skipped:
        note += "; drift may also indicate a bad snapshot (verify SKIPPED in some runs)"
    elif verified:
        note += "; drift attributable to pump desync (verify OK present)"
    elif structured:
        note += "; drift basis unconfirmed (no [verify] line in structured runs)"
    return note
