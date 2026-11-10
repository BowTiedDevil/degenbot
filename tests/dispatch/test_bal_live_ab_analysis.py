"""The live OFF/ON capture analysis (`scripts/bal_live_ab_analysis.py`).

The analysis exists to report TWO different evidence classes apart, so the pins
here are shaped the same way:

* the `fan-out storage selection` / `storage prefetch` / `[sim] cands=` rows are
  per-dispatch telemetry — one row per fenced fan-out, carrying its own anchor
  and storage lane;
* the `fanout-trace` stream is a per-CANDIDATE stream emitted at the simulation
  seam by every production caller, so a row names the caller's head block and
  nothing about the storage lane it read through. Pinning that separation is the
  point: a report that attributed trace rows to `delta`/`legacy` by block alone
  would manufacture a comparison the capture cannot support.

Every parser pin states the wire shape it consumes; each literal line below is
copied from an emitted row, so the expected parse is the format's truth and not
a restatement of the implementation.
"""

from __future__ import annotations

from importlib.util import module_from_spec, spec_from_file_location
from pathlib import Path
from typing import TYPE_CHECKING

import pytest

if TYPE_CHECKING:
    from types import ModuleType


SCRIPT_PATH = Path(__file__).resolve().parents[2] / "scripts" / "bal_live_ab_analysis.py"


@pytest.fixture(scope="module")
def analysis() -> ModuleType:
    """The analysis script, loaded by path (it is not an importable package)."""
    spec = spec_from_file_location("bal_live_ab_analysis", SCRIPT_PATH)
    if spec is None or spec.loader is None:
        message = f"cannot load analysis script: {SCRIPT_PATH}"
        raise RuntimeError(message)
    module = module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


TRACE_LINE = (
    'DEBUG [sim] fanout-trace {"block":26382751,"cand":"47821@1","sim_ns":279587028,'
    '"verdict":"none","gas":0,"reads":['
    '"0x556b9306565093c855aea9ae92a594704c2cd59e|0xd,0x10,0x11",'
    '"0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2|0x1a2d"],'
    '"relevance":["0x556b9306565093c855aea9ae92a594704c2cd59e",'
    '"0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"]}'
)

SELECTION_DISABLED = (
    "DEBUG [sim] fan-out storage selection {current_block=26382752, anchor=26382751, "
    'anchor_fenced=true, storage="legacy", reason=None}'
)

SELECTION_REFUSED = (
    "DEBUG [sim] fan-out storage selection {current_block=26382797, anchor=26382795, "
    'anchor_fenced=true, storage="legacy", reason=Some("lineage-refused")}'
)

SELECTION_DELTA = (
    "DEBUG [sim] fan-out storage selection {current_block=26382800, anchor=26382799, "
    'anchor_fenced=true, storage="delta", reason=None}'
)

PREFETCH_LINE = (
    "DEBUG [sim] storage prefetch {current_block=26382752, anchor=26382751, anchor_fenced=true, "
    "derived=115, learned=96, hook=36, requested=247, seeded=247, skipped=None}"
)

LAB_LINE = (
    "INFO [sim] cands=50 sims=172 sim_ms=1710 builds=123 build_ms=7 cold: b_fast=0 b_slow=164 "
    "b_ms=79 code=0 c_ms=0 s_fast=18379 s_slow=4753 s_ms=1601 bh=0 warm_basic_hits=3691 "
    "slow_ms=1681 {current_block=26382752, fanout_ms=814}"
)

OUTCOME_LINE = (
    "INFO [logging] [sim] ready_ms=1794292608.984 50 candidates: 0 ok (0 profitable, 0 below "
    "threshold), 50 failed, 0 exceptions — by reason: CurrencyNotSettled=26 ERC20: transfer "
    "amount exceeds balance=20 PoolNotInitialized=4"
)

MEMO_LINE = "INFO [sim] storage memo stats (block retired) {block_number=26382751, memo.hits=2888, memo.misses=391}"


def test_a_trace_row_parses_its_block_candidate_verdict_and_read_walk(analysis: ModuleType) -> None:
    row = analysis.parse_trace_line(TRACE_LINE)

    assert row is not None
    assert row.block == 26382751
    assert row.cand == "47821@1"
    assert row.sim_ns == 279587028
    assert row.verdict == "none"
    assert row.gas == 0
    assert row.addresses == (
        "0x556b9306565093c855aea9ae92a594704c2cd59e",
        "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2",
    )
    assert row.slots == 4
    assert row.relevance == row.addresses


def test_a_trace_row_with_an_empty_read_walk_still_parses(analysis: ModuleType) -> None:
    line = (
        'DEBUG [sim] fanout-trace {"block":7,"cand":"42-1","sim_ns":9,"verdict":"result",'
        '"gas":3,"reads":[],"relevance":["0x1111111111111111111111111111111111111111"]}'
    )

    row = analysis.parse_trace_line(line)

    assert row is not None
    assert (row.block, row.sim_ns, row.verdict, row.gas) == (7, 9, "result", 3)
    assert row.addresses == ()
    assert row.slots == 0


def test_a_non_trace_line_is_not_a_trace_row(analysis: ModuleType) -> None:
    assert analysis.parse_trace_line(SELECTION_DELTA) is None


def test_a_selection_row_carries_its_anchor_representation_and_cold_reason(
    analysis: ModuleType,
) -> None:
    disabled = analysis.parse_selection_line(SELECTION_DISABLED)
    refused = analysis.parse_selection_line(SELECTION_REFUSED)
    delta = analysis.parse_selection_line(SELECTION_DELTA)

    assert (disabled.current_block, disabled.anchor, disabled.storage) == (
        26382752,
        26382751,
        "legacy",
    )
    assert disabled.reason is None
    assert refused.reason == "lineage-refused"
    assert delta.storage == "delta"
    assert delta.anchor == 26382799


def test_a_prefetch_row_parses_its_plan_attribution(analysis: ModuleType) -> None:
    row = analysis.parse_prefetch_line(PREFETCH_LINE)

    assert row is not None
    assert (row.current_block, row.anchor) == (26382752, 26382751)
    assert (row.derived, row.learned, row.hook) == (115, 96, 36)
    assert (row.requested, row.seeded) == (247, 247)
    assert row.skipped is None


def test_a_cands_row_parses_the_fanout_wall_and_cold_counters(analysis: ModuleType) -> None:
    row = analysis.parse_lab_line(LAB_LINE)

    assert row is not None
    assert (row.current_block, row.candidates, row.sims) == (26382752, 50, 172)
    assert (row.sim_ms, row.fanout_ms) == (1710, 814)
    assert (row.b_slow, row.s_slow, row.slow_ms) == (164, 4753, 1681)


def test_a_dispatch_outcome_row_parses_candidates_and_the_failure_buckets(
    analysis: ModuleType,
) -> None:
    row = analysis.parse_outcome_line(OUTCOME_LINE)

    assert row is not None
    assert row.candidates == 50
    assert (row.ok, row.failed, row.exceptions) == (0, 50, 0)
    assert row.buckets == {
        "CurrencyNotSettled": 26,
        "ERC20: transfer amount exceeds balance": 20,
        "PoolNotInitialized": 4,
    }


def test_a_memo_row_parses_the_retired_anchors_hit_miss_counts(analysis: ModuleType) -> None:
    row = analysis.parse_memo_line(MEMO_LINE)

    assert row is not None
    assert (row.block, row.hits, row.misses) == (26382751, 2888, 391)


def test_percentiles_interpolate_over_the_sorted_sample(analysis: ModuleType) -> None:
    summary = analysis.summarize([4, 1, 3, 2])

    assert summary.count == 4
    assert summary.total == 10
    assert summary.mean == pytest.approx(2.5)
    assert summary.p50 == pytest.approx(2.5)
    assert summary.p95 == pytest.approx(3.85)
    assert summary.maximum == 4


def test_an_empty_sample_summarizes_to_zero_rather_than_dividing(analysis: ModuleType) -> None:
    summary = analysis.summarize([])

    assert (summary.count, summary.total, summary.mean, summary.p50, summary.p95) == (
        0,
        0,
        0.0,
        0.0,
        0.0,
    )


@pytest.mark.parametrize(
    ("candidate_count", "regime"),
    [(1, "sparse"), (2, "sparse"), (3, "steady"), (19, "steady"), (20, "burst"), (146, "burst")],
)
def test_the_candidate_density_regime_boundaries(
    analysis: ModuleType, candidate_count: int, regime: str
) -> None:
    assert analysis.regime_of(candidate_count) == regime


def test_relevance_coverage_counts_the_pre_simulation_addresses_the_reads_carried(
    analysis: ModuleType,
) -> None:
    full = analysis.parse_trace_line(
        'DEBUG [sim] fanout-trace {"block":1,"cand":"1@1","sim_ns":1,"verdict":"none","gas":0,'
        '"reads":["0xaa|","0xbb|","0xcc|"],"relevance":["0xaa","0xbb","0xcc","0xdd"]}'
    )
    assert full is not None
    assert analysis.relevance_coverage(full) == pytest.approx(0.75)

    no_relevance = analysis.parse_trace_line(
        'DEBUG [sim] fanout-trace {"block":1,"cand":"1@1","sim_ns":1,"verdict":"none","gas":0,'
        '"reads":["0xaa|"],"relevance":[]}'
    )
    assert no_relevance is not None
    assert analysis.relevance_coverage(no_relevance) == 1.0


def _write_capture(path: Path, *, selections: list[str], traces: list[str]) -> Path:
    path.write_text("\n".join([*selections, *traces, ""]) + "\n")
    return path


def test_a_capture_whose_off_window_selects_delta_fails_validation(
    analysis: ModuleType, tmp_path: Path
) -> None:
    off = analysis.scan_capture(
        _write_capture(
            tmp_path / "off.log",
            selections=[SELECTION_DELTA],
            traces=[TRACE_LINE],
        )
    )
    on = analysis.scan_capture(
        _write_capture(
            tmp_path / "on.log",
            selections=[SELECTION_REFUSED],
            traces=[TRACE_LINE],
        )
    )

    problems = analysis.validation_problems(off, on)

    assert any("delta" in problem and "off" in problem.lower() for problem in problems)


def test_a_capture_with_no_trace_rows_fails_validation(
    analysis: ModuleType, tmp_path: Path
) -> None:
    empty = analysis.scan_capture(_write_capture(tmp_path / "empty.log", selections=[], traces=[]))

    problems = analysis.validation_problems(empty, empty)

    assert any("no fanout-trace rows" in problem for problem in problems)


def test_a_trace_extract_matching_the_capture_cross_checks_clean(
    analysis: ModuleType, tmp_path: Path
) -> None:
    big = _write_capture(tmp_path / "off.log", selections=[SELECTION_DISABLED], traces=[TRACE_LINE])
    extract = _write_capture(tmp_path / "off_trace.log", selections=[], traces=[TRACE_LINE])

    capture = analysis.scan_capture(big, trace_extract=extract)

    assert capture.extract_rows == 1
    assert capture.extract_mismatch == 0
    assert not [
        problem
        for problem in analysis.validation_problems(capture, capture)
        if "extract" in problem
    ]


def test_a_trace_extract_with_a_row_the_capture_does_not_hold_fails_validation(
    analysis: ModuleType, tmp_path: Path
) -> None:
    big = _write_capture(tmp_path / "off.log", selections=[SELECTION_DISABLED], traces=[TRACE_LINE])
    extra = TRACE_LINE.replace('"block":26382751', '"block":26382752')
    extract = _write_capture(tmp_path / "off_trace.log", selections=[], traces=[TRACE_LINE, extra])

    capture = analysis.scan_capture(big, trace_extract=extract)

    problems = analysis.validation_problems(capture, capture)

    assert any("extract" in problem for problem in problems)


def test_the_report_keeps_lane_selection_at_dispatch_level_and_timing_window_wide(
    analysis: ModuleType, tmp_path: Path
) -> None:
    off = analysis.scan_capture(
        _write_capture(
            tmp_path / "off.log",
            selections=[SELECTION_DISABLED],
            traces=[TRACE_LINE],
        )
    )
    on = analysis.scan_capture(
        _write_capture(
            tmp_path / "on.log",
            selections=[SELECTION_REFUSED, SELECTION_DELTA],
            traces=[TRACE_LINE],
        )
    )

    report = analysis.render_markdown(off, on)

    assert "## Dispatch-level storage selection" in report
    assert "## Whole-window candidate timing" in report
    assert "lineage-refused" in report
    assert "delta" in report
    assert "unattributed" in report.lower()
