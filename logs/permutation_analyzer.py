"""Structured per-revert four-way classifier for permutation logs (CPCZZV).

Replaces the false ``Stale_Reverts``/``Bug_Reverts`` dichotomy with
**Drift / SolverCalc / Encoding / Unknown**, parsed from the ``[sim-diag]`` JSON
line each reverted candidate emits (added in the LAV44W task).

Classification logic (applied per reverted candidate, in order):

- **Drift**: any hop has ``drift == true`` (engine_state != onchain_state). The
  map basis is verified at startup (snapshot + backfill phases; see the WFDTUR
  task), so a drift flag does NOT indict the snapshot — it means the per-block
  event pump desynced post-backfill, or the sim block tag differs from the solve
  block.
- **SolverCalc**: no drift AND any hop ``recompute.matches_solver == false``
  (an independent recompute of the on-chain output disagrees with the solver's
  reported ``hop_outputs``). Meaningful for V2 hops (the only family with a
  genuine recompute); V3/V4 hops carry ``matches_solver == None`` (deferred —
  see ``HopRecompute`` docs) and so never trigger this.
- **Encoding**: no drift AND every hop ``matches_solver == true``, yet the sim
  reverted (the amounts were right, so the stream must be wrong).
- **Unknown**: bare/empty revert (``0x execution reverted``) OR recompute
  unavailable for the reverting hop family (``matches_solver == None`` on every
  hop). Classified conservatively — never as "stale".

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
SOLVER_CALC = "SolverCalc"
ENCODING = "Encoding"
UNKNOWN = "Unknown"


PASSING_PCT = 80
PARTIAL_PCT = 20


def classify_candidate(sim_diag: dict) -> str:
    """Classify one reverted candidate from its parsed ``[sim-diag]`` payload.

    Pure + total: never raises (a malformed/empty snapshot classifies as
    ``Unknown`` so the analyzer never blocks on a bad line).
    """
    hops = sim_diag.get("hops") if isinstance(sim_diag, dict) else None
    if not hops or not isinstance(hops, list):
        return UNKNOWN

    # A bare/empty revert cannot be attributed even with a clean recompute.
    revert_info = sim_diag.get("revert_info", "") or ""
    if not revert_info.strip():
        return UNKNOWN

    any_drift = False
    any_solvercalc = False
    all_matches_true = True
    any_recompute_available = False
    for hop in hops:
        if not isinstance(hop, dict):
            continue
        if hop.get("drift"):
            any_drift = True
        recompute = hop.get("recompute")
        if isinstance(recompute, dict):
            matches = recompute.get("matches_solver")
            if matches is not None:
                any_recompute_available = True
            if matches is False:
                any_solvercalc = True
            if matches is not True:
                all_matches_true = False

    if any_drift:
        return DRIFT
    if any_solvercalc:
        return SOLVER_CALC
    if all_matches_true and any_recompute_available:
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
_VERIFY_OK_RE = re.compile(r"\[verify\].*OK")
_VERIFY_SKIPPED_RE = re.compile(r"verify.*SKIPPED|verification.*skipped", re.IGNORECASE)


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

    counts = {DRIFT: 0, SOLVER_CALC: 0, ENCODING: 0, UNKNOWN: 0}
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