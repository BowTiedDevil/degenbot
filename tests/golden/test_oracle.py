"""Deterministic, RPC-free tests for the GoldenOracle scaffold itself.

These do not exercise any on-chain contract. They prove the record/replay
mechanics, the revert-preservation contract, the missing-key and stale-block
guard rails, and the nodeid→path mapping — so CI can validate the scaffold the
moment it lands, before any real parity test is converted.

This is the tracer bullet for the golden-on-chain-parity design
(``docs/architecture/golden-onchain-parity.md``).
"""

from __future__ import annotations

import json
from typing import TYPE_CHECKING

import pytest

from tests.golden.oracle import (
    GOLDEN_ROOT,
    GoldenError,
    GoldenOracle,
    _nodeid_to_path,
)

if TYPE_CHECKING:
    from pathlib import Path

_REVERT_MSG = "execution reverted"


@pytest.fixture
def oracle_in_tmp(tmp_path: Path) -> GoldenOracle:
    """A GoldenOracle wired to a throwaway directory, in record mode."""
    return GoldenOracle(
        path=tmp_path / "data" / "mytest.json",
        chain_id=1,
        block_number=17_600_000,
        mode="record",
    )


def _replay(path: Path) -> GoldenOracle:
    return GoldenOracle(path=path, chain_id=1, block_number=17_600_000, mode="replay")


def test_record_captures_value_and_writes_file(oracle_in_tmp: GoldenOracle) -> None:
    res = oracle_in_tmp.check("k1", contract=lambda: 15808930695950518795)

    assert res.ok
    assert res.value == 15808930695950518795
    # exact-precision big int round-trips through JSON
    assert (
        json.loads(oracle_in_tmp.path.read_text())["entries"]["k1"]["value"] == 15808930695950518795
    )


def test_record_captures_revert_as_skippable_entry(oracle_in_tmp: GoldenOracle) -> None:
    def reverts() -> int:
        raise RuntimeError(_REVERT_MSG)

    res = oracle_in_tmp.check("k-revert", contract=reverts)

    assert res.reverted
    assert res.exception_type == "RuntimeError"
    assert res.value is None
    entry = json.loads(oracle_in_tmp.path.read_text())["entries"]["k-revert"]
    assert entry["reverted"] is True
    assert entry["exception"] == "RuntimeError"


def test_replay_never_invokes_contract(oracle_in_tmp: GoldenOracle, tmp_path: Path) -> None:
    path = oracle_in_tmp.path
    oracle_in_tmp.check("k1", contract=lambda: 42)

    calls: list[int] = []

    def live() -> int:
        calls.append(1)
        return -999  # would corrupt the assertion if it were ever called

    replay = _replay(path)
    res = replay.check("k1", contract=live)

    assert calls == []
    assert res.value == 42


def test_replay_reproduces_recorded_revert_skip(oracle_in_tmp: GoldenOracle) -> None:
    path = oracle_in_tmp.path
    oracle_in_tmp.check("k-revert", contract=lambda: (_ for _ in ()).throw(ValueError("nope")))

    replay = _replay(path)
    res = replay.check("k-revert", contract=lambda: 7)

    assert res.reverted  # the test would `continue` here, mirroring record time
    assert res.value is None


def test_replay_missing_key_raises_golden_error(tmp_path: Path) -> None:
    replay = _replay(tmp_path / "absent.json")
    with pytest.raises(GoldenError, match="no golden entry"):
        replay.check("never-recorded", contract=lambda: 1)


def test_replay_rejects_block_mismatch(oracle_in_tmp: GoldenOracle) -> None:
    path = oracle_in_tmp.path
    oracle_in_tmp.check("k1", contract=lambda: 1)

    # Replay binds a DIFFERENT block than the file pins → stale-goldal guard
    # fires at construction (load) time, before any key lookup.
    with pytest.raises(GoldenError, match="recorded at block 17600000"):
        GoldenOracle(path=path, chain_id=1, block_number=17_650_000, mode="replay")


def test_nodeid_to_path_maps_directory_structure() -> None:
    nodeid = "tests/uniswap/v3/test_uniswap_v3_liquidity_pool.py::test_cached_calculations"
    assert _nodeid_to_path(nodeid) == (
        GOLDEN_ROOT
        / "tests/uniswap/v3/test_uniswap_v3_liquidity_pool/test_cached_calculations.json"
    )


def test_nodeid_to_path_strips_param_bracket() -> None:
    nodeid = "tests/curve/test_curve_stableswap_pool.py::test_single_pool[0xdead]"
    path = _nodeid_to_path(nodeid)
    # one file per test function; the param must live in the caller's `key`
    assert path.name == "test_single_pool.json"
    assert "[0xdead]" not in str(path)


def test_entries_are_sorted_for_reviewable_diffs(oracle_in_tmp: GoldenOracle) -> None:
    # record out of order
    oracle_in_tmp.check("zebra", contract=lambda: 2)
    oracle_in_tmp.check("alpha", contract=lambda: 1)

    keys = list(json.loads(oracle_in_tmp.path.read_text())["entries"].keys())
    assert keys == ["alpha", "zebra"]
