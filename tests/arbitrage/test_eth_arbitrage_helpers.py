"""Tests for ``format_sim_diag_line``, the ``[sim-diag]`` line renderer.

The renderer's home is :mod:`degenbot.runner._render`: the line is a
**stays-Python** display concern (the amounts it reports are the Rust sim's own
captured swaps), so its regression coverage lives here.
"""

import json

from degenbot.dispatch import CandidateId
from degenbot.runner._render import SimDiagWindow, format_sim_diag_line
from tests.helpers.sim_records import failure_record


def test_format_sim_diag_line_emits_parseable_json_with_required_fields() -> None:
    """The [sim-diag] line is one JSON object parseable with json.loads.

    Carries the captured-swaps basis: candidate (path + deployment),
    path_type, solve_block, block, age, revert_info (the bucket), optimal_input,
    hop_outputs (expected), and captured_swaps (actual). The classifier compares hop_outputs[i] vs the
    i-th captured swap's output amount.
    """
    line = format_sim_diag_line(
        failure_record(),
        candidate=CandidateId(path_id=7, deployment_id=1),
        path_type="V2-V3-V4",
        window=SimDiagWindow(solve_block=100, block=103, age=3),
    )
    assert line.startswith("[sim-diag] "), "line is prefixed [sim-diag] "
    payload = json.loads(line[len("[sim-diag] ") :])
    assert payload["candidate"] == {"path_id": 7, "deployment_id": 1}
    assert payload["path_type"] == "V2-V3-V4"
    assert payload["solve_block"] == 100
    assert payload["block"] == 103
    assert payload["age"] == 3
    assert payload["revert_info"] == "unknown:0xcafebabe"
    assert payload["optimal_input"] == 1000
    assert payload["hop_outputs"] == [3000]
    swap = payload["captured_swaps"][0]
    assert swap["family"] == "v2"
    assert swap["amount0"] == -1000
    assert swap["amount1"] == 3000


def test_format_sim_diag_line_never_raises_on_missing_keys() -> None:
    """A taxonomy/emission path must never raise — malformed failure records
    emit a best-effort line with whatever fields are present."""
    line = format_sim_diag_line(
        {},
        candidate=CandidateId(path_id=1, deployment_id=1),
        path_type="V2",
        window=SimDiagWindow(solve_block=1, block=1, age=0),
    )
    payload = json.loads(line[len("[sim-diag] ") :])
    assert payload["candidate"]["path_id"] == 1
    assert payload["revert_info"] == ""
    assert payload["optimal_input"] is None
    assert payload["hop_outputs"] == []
    assert payload["captured_swaps"] == []
