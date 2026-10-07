"""Deterministic, RPC-free tests for the GoldenOracle scaffold itself.

These do not exercise any on-chain contract. They prove the record/replay
mechanics, the record-mode failure taxonomy (``tests.golden.record_errors``),
the missing-key and stale-block guard rails, the nodeid→path mapping, and the
shared offline-contract API (dial-block, golden-file resolution, key-set
assert) the parity suites import — so CI can validate the scaffold the moment
it lands, before any real parity test is converted.
"""

from __future__ import annotations

import json
import socket
from typing import TYPE_CHECKING

import pytest

from degenbot.exceptions import ContractLogicError
from tests.golden import oracle as oracle_module
from tests.golden.oracle import (
    GOLDEN_ROOT,
    REPLAY_DIAL_MSG,
    GoldenError,
    GoldenOracle,
    _nodeid_to_path,
    assert_golden_keys_exact,
    parity_golden_file,
    replay_makes_no_network_calls,  # ruff: ignore[unused-import]
)
from tests.golden.record_errors import (
    RECORD_RETRY_ATTEMPTS,
    canonical_revert_reason,
    classify_record_failure,
)

if TYPE_CHECKING:
    from collections.abc import Callable
    from pathlib import Path

_REVERT_MSG = "execution reverted"


def _raise_contract(text: str) -> Callable[[], int]:
    def _call() -> int:
        raise ContractLogicError(text)

    return _call


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
    res = oracle_in_tmp.check("k-revert", contract=_raise_contract(_REVERT_MSG))

    assert res.reverted
    assert res.exception_type == "ContractLogicError"
    assert res.value is None
    entry = json.loads(oracle_in_tmp.path.read_text())["entries"]["k-revert"]
    # a bare revert carries no reason, so the entry omits `message` entirely
    assert entry == {"reverted": True, "exception": "ContractLogicError"}


def test_record_canonicalizes_provider_revert_text(oracle_in_tmp: GoldenOracle) -> None:
    """Reverts persist the parsed reason only: provider wrappers (the FFI
    eth_call frame, node code prefixes, inline data echoes) never reach the
    golden bytes, so the same on-chain outcome records identically from any
    node."""
    reason = "Insufficient liquidity for the swap"
    data_echo = "08c379a0" + f"{32:064x}" + f"{len(reason):064x}" + reason.encode().hex()
    shapes = (
        f"eth_call to 0xabc reverted: execution reverted: {reason}",
        f"eth_call to 0xabc reverted: error code 3: execution reverted: {reason}",
        f"eth_call to 0xabc reverted: execution reverted, data: '0x{data_echo}'",
    )
    for i, text in enumerate(shapes):
        oracle_in_tmp.check(f"k{i}", contract=_raise_contract(text))

    entries = json.loads(oracle_in_tmp.path.read_text())["entries"]
    expected = {"reverted": True, "exception": "ContractLogicError", "message": reason}
    assert entries["k0"] == entries["k1"] == entries["k2"] == expected


def test_record_transport_failure_retries_then_fails_without_entry(
    oracle_in_tmp: GoldenOracle,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A transport failure is endpoint weather: bounded retry, then the record
    run fails loudly with the exact error — and no golden entry is written."""
    monkeypatch.setattr("tests.golden.record_errors.RECORD_RETRY_BACKOFF_SECONDS", 0)
    attempts: list[int] = []

    def times_out() -> int:
        attempts.append(1)
        msg = "Provider error: Request timeout: node hung up"
        raise TimeoutError(msg)

    with pytest.raises(TimeoutError, match="Request timeout: node hung up"):
        oracle_in_tmp.check("k-transport", contract=times_out)

    assert len(attempts) == RECORD_RETRY_ATTEMPTS
    assert not oracle_in_tmp.path.exists()


def test_record_programmer_error_fails_without_entry(oracle_in_tmp: GoldenOracle) -> None:
    """A helper bug (``TypeError``/``KeyError``/...) fails the record run
    immediately — a defect must never be laundered into a golden entry."""

    def broken() -> int:
        msg = "helper signature changed"
        raise TypeError(msg)

    with pytest.raises(TypeError, match="helper signature changed"):
        oracle_in_tmp.check("k-bug", contract=broken)

    assert not oracle_in_tmp.path.exists()


def test_record_failure_taxonomy() -> None:
    assert classify_record_failure(ContractLogicError(_REVERT_MSG)) == "contract-revert"
    assert (
        classify_record_failure(RuntimeError("Provider error: execution reverted: over budget"))
        == "contract-revert"
    )
    assert (
        classify_record_failure(TimeoutError("Provider error: Request timeout: 30 seconds"))
        == "transport"
    )
    assert (
        classify_record_failure(ConnectionError("Provider error: Connection failed: refused"))
        == "transport"
    )
    assert (
        classify_record_failure(
            RuntimeError("Provider error: RPC error: -32001 - requested block not available")
        )
        == "transport"
    )
    assert classify_record_failure(TypeError("bad call")) == "programmer"
    assert classify_record_failure(KeyError("helper key")) == "programmer"


def test_canonical_revert_reason_shapes() -> None:
    assert canonical_revert_reason("execution reverted") is None
    assert canonical_revert_reason(_REVERT_MSG) is None
    assert (
        canonical_revert_reason(f"eth_call to 0xa reverted: execution reverted: {_REVERT_MSG} done")
        == f"{_REVERT_MSG} done"
    )
    # the retired web3 recorder str()'d custom errors as a payload tuple
    assert canonical_revert_reason("('0xdeadbeef', '0xdeadbeef')") == "0xdeadbeef"
    panic = f"eth_call to 0xa reverted: execution reverted, data: '0x4e487b71{17:064x}'"
    assert canonical_revert_reason(panic) == "Panic(17)"


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
    oracle_in_tmp.check("k-revert", contract=_raise_contract(_REVERT_MSG))

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


# -- shared offline-contract API ------------------------------------------------
#
# Importing `replay_makes_no_network_calls` above arms the shared dial-block
# for this whole module (replay mode by default) — the bite probe relies on it.


def test_replay_dial_block_bites() -> None:
    """A replay-mode socket dial fails with the shared message."""
    with pytest.raises(AssertionError, match=REPLAY_DIAL_MSG):
        socket.create_connection(("localhost", 1))


def test_parity_golden_file_resolution(request: pytest.FixtureRequest) -> None:
    """The shared resolver maps (this module, test name) exactly like
    golden_factory maps the nodeid, under the default --golden-root."""
    resolved = parity_golden_file(request, "test_nodeid_to_path_maps_directory_structure")
    assert resolved == (
        GOLDEN_ROOT / "tests/golden/test_oracle/test_nodeid_to_path_maps_directory_structure.json"
    )


def test_assert_golden_keys_exact_set_equality(
    request: pytest.FixtureRequest,
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The shared key-set assert passes on equality and reports the diff with
    the stale=/missing= shape."""
    golden = tmp_path / "g.json"
    golden.write_text(json.dumps({"entries": {"alpha": {"value": 1}, "zeta": {"value": 2}}}))
    monkeypatch.setattr(oracle_module, "parity_golden_file", lambda req, name: golden)

    assert_golden_keys_exact(request, "some_test", {"alpha", "zeta"})

    with pytest.raises(AssertionError, match=r"stale=\['zeta'\] missing=\[\]"):
        assert_golden_keys_exact(request, "some_test", {"alpha"})

    with pytest.raises(AssertionError, match=r"stale=\['zeta'\] missing=\['omega'\]"):
        assert_golden_keys_exact(request, "some_test", {"alpha", "omega"})


def test_assert_golden_keys_exact_skips_in_record_mode(
    request: pytest.FixtureRequest,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """In record mode the shared assert skips: the run rewrites the very file
    being diffed."""
    monkeypatch.setattr(request.config.option, "golden_mode", "record")
    with pytest.raises(pytest.skip.Exception, match="the record run rewrites"):
        assert_golden_keys_exact(request, "whatever", set())
