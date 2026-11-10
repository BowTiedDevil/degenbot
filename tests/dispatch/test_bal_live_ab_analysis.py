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

# The additive `fanout` key and the `bal-head` rows

The parser reads an old committed capture and an attributed one alike: the
`fanout=<id>` tail of the two dispatch rows and of the `[sim] cands=` row, and
the `"fanout":<id>` key of a trace row, are OPTIONAL (a capture taken before the
key existed carries neither), and a candidate row WITHOUT the key is reported
unattributed rather than assigned to a fan-out by block number — that is what
keeps the inline hook re-quote arm's rows out of a dispatch fan-out's lane.
`bal-head {...}` is a row family an old capture simply does not have.

The literals below are exactly what the emitters write: the Rust rows are pinned
by `degenbot-arbitrage`'s `fanout_trace`/`sim_metrics` tests and
`degenbot-simulation`'s `bal_heads::tests`, and the Python pins here consume the
same wire shape.
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

# The attributed shapes: the same rows with the additive `fanout` tail, plus the
# per-head row. Every candidate row of ONE fan-out names that fan-out's id (41).
SELECTION_ATTRIBUTED = (
    "DEBUG [sim] fan-out storage selection {current_block=26382800, anchor=26382799, "
    'anchor_fenced=true, storage="delta", reason=None, fanout=41}'
)

PREFETCH_ATTRIBUTED = (
    "DEBUG [sim] storage prefetch {current_block=26382752, anchor=26382751, anchor_fenced=true, "
    "derived=115, learned=96, hook=36, requested=247, seeded=247, skipped=None, fanout=41}"
)

LAB_ATTRIBUTED = (
    "INFO [sim] cands=50 sims=172 sim_ms=1710 builds=123 build_ms=7 cold: b_fast=0 b_slow=164 "
    "b_ms=79 code=0 c_ms=0 s_fast=18379 s_slow=4753 s_ms=1601 bh=0 warm_basic_hits=3691 "
    "slow_ms=1681 fanout=41 {current_block=26382752, fanout_ms=814}"
)

TRACE_ATTRIBUTED = (
    'DEBUG [sim] fanout-trace {"block":26382800,"cand":"47821@1","sim_ns":279587028,'
    '"verdict":"none","gas":0,"reads":['
    '"0x556b9306565093c855aea9ae92a594704c2cd59e|0xd,0x10,0x11"],'
    '"relevance":["0x556b9306565093c855aea9ae92a594704c2cd59e"],"fanout":41}'
)

HEAD_ROW = (
    'DEBUG [sim] bal-head {"anchor":26382799,"hash":"0xaaaa'
    'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",'
    '"parent":"0x9999999999999999999999999999999999999999999999999999999999999999",'
    '"outcome":"cold","chain":"cold","refusal":"not-consecutive","bal":null,'
    '"prepare_us":700,"apply_us":31,"writes":1400,"overwritten":1392,"skipped":8}'
)


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


def _write_capture(
    path: Path,
    *,
    selections: list[str],
    traces: list[str],
    heads: list[str] | None = None,
    labs: list[str] | None = None,
    prefetches: list[str] | None = None,
) -> Path:
    path.write_text(
        "\n".join([*(heads or []), *(prefetches or []), *(labs or []), *selections, *traces, ""])
        + "\n"
    )
    return path


def _selection_row(block: int, storage: str, fanout: int | None, reason: str | None = None) -> str:
    """One `fan-out storage selection` row, in the emitter's wire shape."""
    reason_field = f'Some("{reason}")' if reason else "None"
    tail = f", fanout={fanout}" if fanout is not None else ""
    return (
        f"DEBUG [sim] fan-out storage selection {{current_block={block}, anchor={block - 1}, "
        f'anchor_fenced=true, storage="{storage}", reason={reason_field}{tail}}}'
    )


def _lab_row(block: int, candidates: int, wall_ms: int, fanout: int | None) -> str:
    """One `[sim] cands=` row, in the emitter's wire shape."""
    tail = f" fanout={fanout}" if fanout is not None else ""
    return (
        f"INFO [sim] cands={candidates} sims={candidates} sim_ms=1 builds=1 build_ms=0 "
        "cold: b_fast=0 b_slow=0 b_ms=0 code=0 c_ms=0 s_fast=0 s_slow=0 s_ms=0 bh=0 "
        f"warm_basic_hits=0 slow_ms=0{tail} {{current_block={block}, fanout_ms={wall_ms}}}"
    )


def _trace_row(block: int, cand: str, sim_ns: int, fanout: int | None) -> str:
    """One `fanout-trace` row; `fanout=None` is the unkeyed (pre-key) shape."""
    key = f',"fanout":{fanout}' if fanout is not None else ""
    body = (
        f'{{"block":{block},"cand":"{cand}","sim_ns":{sim_ns},"verdict":"none",'
        f'"gas":0,"reads":[],"relevance":[]{key}}}'
    )
    return f"DEBUG [sim] fanout-trace {body}"


def _head_row(
    anchor: int,
    *,
    outcome: str,
    prepare_us: int,
    apply_us: int,
    refusal: str | None = None,
    chain: str = "carried",
) -> str:
    """One `bal-head {...}` row, in the emitter's wire shape."""
    refusal_field = f'"{refusal}"' if refusal else "null"
    return (
        f'DEBUG [sim] bal-head {{"anchor":{anchor},"hash":"0x{"a" * 64}",'
        f'"parent":"0x{"9" * 64}","outcome":"{outcome}","chain":"{chain}",'
        f'"refusal":{refusal_field},"bal":null,"prepare_us":{prepare_us},'
        f'"apply_us":{apply_us},"writes":1,"overwritten":0,"skipped":0}}'
    )


def test_a_trace_row_without_the_fanout_key_still_parses(analysis: ModuleType) -> None:
    legacy = analysis.parse_trace_line(TRACE_LINE)
    attributed = analysis.parse_trace_line(TRACE_ATTRIBUTED)

    assert legacy is not None
    assert legacy.fanout is None, "a pre-`fanout` row carries no attribution"
    assert (legacy.block, legacy.sim_ns, legacy.verdict) == (26382751, 279587028, "none")
    assert attributed is not None
    assert attributed.fanout == 41
    assert attributed.cand == "47821@1"
    assert attributed.block == 26382800


def test_the_dispatch_rows_carry_their_optional_fanout_key(analysis: ModuleType) -> None:
    assert analysis.parse_selection_line(SELECTION_DELTA).fanout is None
    attributed = analysis.parse_selection_line(SELECTION_ATTRIBUTED)
    assert attributed is not None
    assert (attributed.fanout, attributed.storage, attributed.reason) == (41, "delta", None)
    assert analysis.parse_prefetch_line(PREFETCH_LINE).fanout is None
    prefetch = analysis.parse_prefetch_line(PREFETCH_ATTRIBUTED)
    assert prefetch is not None
    assert (prefetch.fanout, prefetch.seeded) == (41, 247)
    assert analysis.parse_lab_line(LAB_LINE).fanout is None
    lab = analysis.parse_lab_line(LAB_ATTRIBUTED)
    assert lab is not None
    assert (lab.fanout, lab.current_block, lab.fanout_ms) == (41, 26382752, 814)


def test_the_fanout_id_joins_selection_prefetch_sim_and_trace_rows_for_one_fan_out(
    analysis: ModuleType, tmp_path: Path
) -> None:
    capture = analysis.scan_capture(
        _write_capture(
            tmp_path / "on.log",
            selections=[SELECTION_ATTRIBUTED],
            traces=[TRACE_ATTRIBUTED, TRACE_ATTRIBUTED],
        )
    )
    capture.prefetches.append(analysis.parse_prefetch_line(PREFETCH_ATTRIBUTED))
    capture.labs.append(analysis.parse_lab_line(LAB_ATTRIBUTED))

    joined = analysis.fanout_attribution(capture)

    assert list(joined) == [41], "one fan-out, one identifier"
    entry = joined[41]
    assert entry.selection is not None
    assert entry.prefetch is not None and entry.prefetch.seeded == 247
    assert entry.lab is not None and entry.lab.candidates == 50
    assert len(entry.traces) == 2, "both candidates of the fan-out join it"
    assert {row.cand for row in entry.traces} == {"47821@1"}
    assert analysis.unattributed_traces(capture) == []


def test_an_unattributed_candidate_is_never_assigned_to_a_fanout(
    analysis: ModuleType, tmp_path: Path
) -> None:
    # The inline hook arm's row is at the SAME block as the dispatch fan-out's.
    inline_hook_row = TRACE_ATTRIBUTED.replace(',"fanout":41', "").replace(
        '"cand":"47821@1"', '"cand":"9@1"'
    )
    capture = analysis.scan_capture(
        _write_capture(
            tmp_path / "on.log",
            selections=[SELECTION_ATTRIBUTED],
            traces=[TRACE_ATTRIBUTED, inline_hook_row],
        )
    )

    joined = analysis.fanout_attribution(capture)
    unattributed = analysis.unattributed_traces(capture)

    assert len(joined[41].traces) == 1, "the fan-out owns only its own candidate"
    assert [row.cand for row in unattributed] == ["9@1"]
    assert '"fanout":' not in inline_hook_row, "the inline hook row carries no key"
    report = analysis.render_markdown(capture, capture)
    assert "unattributed candidates: 1" in report
    assert "attributed candidates: 1" in report


def test_a_head_row_names_the_refusal_and_walls_behind_the_collapsed_label(
    analysis: ModuleType, tmp_path: Path
) -> None:
    row = analysis.parse_head_line(HEAD_ROW)
    assert row is not None
    assert (row.anchor, row.outcome, row.chain) == (26382799, "cold", "cold")
    assert row.refusal == "not-consecutive", "the cause the fan-out label collapses"
    assert row.bal is None
    assert (row.prepare_us, row.apply_us) == (700, 31)
    assert (row.writes, row.overwritten, row.skipped) == (1400, 1392, 8)
    assert analysis.parse_head_line(TRACE_LINE) is None

    old = analysis.scan_capture(
        _write_capture(tmp_path / "old.log", selections=[SELECTION_REFUSED], traces=[TRACE_LINE])
    )
    assert old.heads == [], "a pre-`bal-head` capture has no head rows"
    assert "No `bal-head` rows" in analysis.render_markdown(old, old)


def test_the_report_reads_an_old_capture_pair_and_an_attributed_pair_alike(
    analysis: ModuleType, tmp_path: Path
) -> None:
    old_off = analysis.scan_capture(
        _write_capture(
            tmp_path / "old_off.log", selections=[SELECTION_DISABLED], traces=[TRACE_LINE]
        )
    )
    old_on = analysis.scan_capture(
        _write_capture(tmp_path / "old_on.log", selections=[SELECTION_REFUSED], traces=[TRACE_LINE])
    )
    old_report = analysis.render_markdown(old_off, old_on)
    assert "No row in this capture carries a `fanout` key" in old_report
    assert "No `bal-head` rows" in old_report
    assert "No candidate count is shared by the two captures" in old_report
    assert "attributed candidates: 0 of 1" in old_report

    new_off = analysis.scan_capture(
        _write_capture(
            tmp_path / "new_off.log",
            selections=[SELECTION_ATTRIBUTED],
            traces=[TRACE_ATTRIBUTED],
            heads=[HEAD_ROW],
        )
    )
    new_on = analysis.scan_capture(
        _write_capture(
            tmp_path / "new_on.log",
            selections=[SELECTION_ATTRIBUTED],
            traces=[TRACE_ATTRIBUTED],
            heads=[HEAD_ROW],
        )
    )
    new_report = analysis.render_markdown(new_off, new_on)
    assert "not-consecutive" in new_report, "the detailed refusal reached the report"
    assert "| fanout | current_block |" in new_report
    assert "| 41 |" in new_report
    assert "No `bal-head` rows" not in new_report


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


# ── the mixed-lane report (OFF legacy versus ON delta) ──────────────────────


def test_a_matched_candidate_count_table_names_each_arms_own_lane(
    analysis: ModuleType, tmp_path: Path
) -> None:
    off = analysis.scan_capture(
        _write_capture(
            tmp_path / "off.log",
            selections=[_selection_row(100, "legacy", 1), _selection_row(101, "legacy", 2)],
            labs=[_lab_row(100, 4, 7, 1), _lab_row(101, 6, 12, 2)],
            traces=[
                *(_trace_row(100, f"{cand}@1", 1_000_000, 1) for cand in range(1, 5)),
                *(_trace_row(101, f"{cand}@1", 2_000_000, 2) for cand in range(5, 11)),
            ],
        )
    )
    on = analysis.scan_capture(
        _write_capture(
            tmp_path / "on.log",
            selections=[_selection_row(200, "delta", 41)],
            labs=[_lab_row(200, 4, 1, 41)],
            traces=[*(_trace_row(200, f"{cand}@1", 500_000, 41) for cand in range(20, 24))],
        )
    )

    report = analysis.render_markdown(off, on)

    assert "| 4 | off | legacy | 100 | 7 | 4 of 4 | 4.00 |" in report
    assert "| 4 | on | delta | 200 | 1 | 4 of 4 | 2.00 |" in report
    matched = report.split("### Matched candidate counts", 1)[1].split("## Whole-window", 1)[0]
    assert "| 6 |" not in matched, "a count only one arm carries is not count-matched"
    assert "Every row here is a legacy-lane fan-out" not in report
    assert "never a paired head comparison" in report
    assert "controlled speedup" in report


def test_the_attribution_summary_counts_ids_selections_and_missing_selection_rows(
    analysis: ModuleType, tmp_path: Path
) -> None:
    capture = analysis.scan_capture(
        _write_capture(
            tmp_path / "on.log",
            selections=[_selection_row(100, "delta", 1)],
            labs=[_lab_row(100, 2, 3, 1), _lab_row(101, 2, 9, 2)],
            traces=[
                _trace_row(100, "1@1", 1_000_000, 1),
                _trace_row(101, "2@1", 1_000_000, 2),
            ],
        )
    )

    report = analysis.render_markdown(capture, capture)

    assert "fanout ids observed: 2" in report
    assert "selection rows: 1" in report
    assert "no surviving selection row: 1" in report, (
        "the dropped selection row is named, not guessed"
    )
    assert "| 2 | on | delta | 100 | 3 | 1 of 2 | 1.00 |" in report
    assert "| 2 | on | unknown | 101 | 9 | 1 of 2 | 1.00 |" in report
    assert "fan-outs carrying a `fanout` key:" not in report


def test_the_head_section_summarizes_prepare_and_apply_walls_apart_from_the_fanout_wall(
    analysis: ModuleType, tmp_path: Path
) -> None:
    capture = analysis.scan_capture(
        _write_capture(
            tmp_path / "on.log",
            selections=[_selection_row(100, "delta", 1)],
            labs=[_lab_row(100, 4, 7, 1)],
            traces=[_trace_row(100, "1@1", 1_000_000, 1)],
            heads=[
                _head_row(100, outcome="settled", prepare_us=300, apply_us=9),
                _head_row(
                    101,
                    outcome="cold",
                    prepare_us=700,
                    apply_us=31,
                    refusal="no-parent",
                    chain="cold",
                ),
            ],
        )
    )

    report = analysis.render_markdown(capture, capture)

    assert "| prepare | 2 | 1.00 | 0.50 | 0.66 | 0.70 |" in report
    assert "| apply | 2 | 0.04 | 0.02 | 0.03 | 0.03 |" in report
    assert "settled=1" in report
    assert "cold=1" in report
    assert "no-parent=1" in report
    assert "not part of a fan-out's" in report


def test_the_verdict_separates_readiness_from_the_non_paired_wall_observation(
    analysis: ModuleType, tmp_path: Path
) -> None:
    off = analysis.scan_capture(
        _write_capture(
            tmp_path / "off.log",
            selections=[_selection_row(100, "legacy", 1), _selection_row(101, "legacy", 2)],
            labs=[_lab_row(100, 2, 7, 1), _lab_row(101, 4, 7, 2)],
            traces=[
                *(_trace_row(100, f"{cand}@1", 1_000_000, 1) for cand in range(1, 3)),
                *(_trace_row(101, f"{cand}@1", 1_000_000, 2) for cand in range(3, 7)),
            ],
        )
    )
    on = analysis.scan_capture(
        _write_capture(
            tmp_path / "on.log",
            selections=[
                _selection_row(200, "delta", 41),
                _selection_row(201, "delta", 42),
                _selection_row(202, "legacy", 43, reason="undelivered"),
            ],
            labs=[
                _lab_row(200, 2, 1, 41),
                _lab_row(201, 4, 2, 42),
                _lab_row(202, 50, 113, 43),
            ],
            traces=[
                *(_trace_row(200, f"{cand}@1", 500_000, 41) for cand in range(20, 22)),
                *(_trace_row(201, f"{cand}@1", 500_000, 42) for cand in range(30, 34)),
                *(_trace_row(202, f"{cand}@1", 2_000_000, 43) for cand in range(40, 90)),
            ],
        )
    )

    report = analysis.render_markdown(off, on)
    verdict = report.split("## Verdict", 1)[1]

    assert "Directional small-fan-out" in verdict
    assert "non-paired" in verdict
    assert "not a paired head comparison" in report
    assert "Still missing: controlled large-fan-out evidence" in verdict
    assert "50" in verdict, "the burst-scale fan-out has no counterpart in the OFF arm"
    assert "No GO from whole-window totals" in verdict
    assert "most ON candidates read through the same legacy lane" not in report
    assert "most of the ON window's fenced fan-outs ran on the same legacy lane" not in report
