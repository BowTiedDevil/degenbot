"""RSP-8 running dual-driver decision gate — pytest half (ergo 23DLCY).

The CI-safe recorded comparator + its seeded-divergence proofs. The live
anvil mode is wired but skipped by default (marked online_rpc and gated
behind DEGENBOT_DUAL_DRIVER_GATE=1 + DEGENBOT_FORK_RPC).

See tests/standalone_parity/dual_driver_gate.py for the harness and
tests/standalone_parity/test_settlement_bot_boot_gate.py for the fixture
boot gate both drivers share.
"""

from __future__ import annotations

import copy
import os

import pytest

from tests.standalone_parity import dual_driver_gate


def test_recorded_dual_driver_decisions_agree_modulo_permitted_divergence() -> None:
    """The recorded Python + Rust decision streams agree except where permitted."""
    fixture = dual_driver_gate.load_decisions_fixture()
    divergences = dual_driver_gate.diff_decisions(
        fixture["python"], fixture["rust"], fixture["permitted_divergence"]
    )
    assert divergences == [], f"unpermitted dual-driver divergence: {divergences}"


def test_permitted_divergence_hides_a_real_documented_split() -> None:
    """The allowlist must cover a genuinely divergent key (not a tautology)."""
    fixture = dual_driver_gate.load_decisions_fixture()
    permitted_keys = {entry["key"] for entry in fixture["permitted_divergence"]}
    assert "graph.candidate_tokens" in permitted_keys
    assert (
        dual_driver_gate.diff_decisions(fixture["python"], fixture["rust"], []) != []
    ), "the permitted key must actually differ, or the allowlist is dead weight"


def test_recorded_comparator_has_teeth() -> None:
    """A seeded divergence in the Rust stream must fail the comparator."""
    fixture = dual_driver_gate.load_decisions_fixture()
    mutated = copy.deepcopy(fixture["rust"])
    for entry in mutated:
        if entry["key"] == "snapshot_seed_block":
            entry["value"] = 99
    divergences = dual_driver_gate.diff_decisions(
        fixture["python"], mutated, fixture["permitted_divergence"]
    )
    assert divergences, "a mutated Rust decision must fail the gate"
    assert any("snapshot_seed_block" in divergence for divergence in divergences)


def test_missing_decision_is_a_divergence() -> None:
    """A decision present on one driver but absent on the other is a divergence."""
    fixture = dual_driver_gate.load_decisions_fixture()
    pruned_rust = [entry for entry in fixture["rust"] if entry["key"] != "discovery_count"]
    divergences = dual_driver_gate.diff_decisions(
        fixture["python"], pruned_rust, fixture["permitted_divergence"]
    )
    assert any("discovery_count" in divergence for divergence in divergences)


def test_recorded_python_decisions_match_the_live_probe() -> None:
    """The recorded Python stream is the real Python consumer probe output."""
    fixture = dual_driver_gate.load_decisions_fixture()
    assert dual_driver_gate.python_offline_decisions() == fixture["python"]


@pytest.mark.online_rpc
def test_live_dual_driver_gate_is_wired() -> None:
    """Live anvil mode — skipped unless the gate + fork env are provided."""
    if not dual_driver_gate.gate_enabled():
        pytest.skip("set DEGENBOT_DUAL_DRIVER_GATE=1 to run the live dual-driver gate")
    fork_rpc = os.environ.get(dual_driver_gate.FORK_ENV, "")
    if not fork_rpc:
        pytest.skip("set DEGENBOT_FORK_RPC to run the live dual-driver gate")
    fork_block = int(os.environ.get(dual_driver_gate.BLOCK_ENV, "0"))
    assert dual_driver_gate.run_live(fork_rpc, fork_block) == 0
