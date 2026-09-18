"""SIMPIPE2 T3 acceptance — payload entries degrade to render+submit-only.

NUUJFA revision: the payload arm routes through the SAME sim seam the FFI
batch uses — :func:`merge_payload_results` (Rust) — so the mutual-exclusion
pool keys (``derive_path_pools`` over the engine's typed hops) and the net
profitability threshold (the Rust-owned ``MIN_PROFIT_NET``) are evaluated
exactly once, Rust-side, for BOTH entry arms. Python only renders/stitches
the returned record rows.

The contract (unchanged by NUUJFA):

- per-entry presence decides: payload entries NEVER enter the FFI sim batch;
- a payload success yields a ``SubmitCandidate`` built by the Rust
  ``join_sim_result`` FFI join (net/gas/calldata/access-list parity with
  the FFI batch rows — NEW: byte-identical ``path_pools`` too, asserted
  through both entry points below, including the Dispatcher blocking
  semantics the mutual-exclusion guard reads);
- a payload ``failure`` surfaces as a ``[sim-fail]`` record through the
  merged outcome's ``failures`` (revert detail carried through);
- a mixed batch keeps the legacy FFI path for the payload-less entries.

The engine is REAL (``ArbitrageEngine`` over a shared ``Bot``) with
offline-registered V2/V4 pools and paths — no live RPC.
"""

from __future__ import annotations

import asyncio
import types
from fractions import Fraction
from typing import Any

import pytest

from degenbot._ffi import ArbitrageEngine, Bot
from degenbot._ffi.simulation import merge_payload_results_py
from degenbot.arbitrage.engine_registry import EngineRegistry
from degenbot.runner._dispatch import _merge_payload_outcome
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v2_pool_factory import make_v2_pool
from tests.helpers.v4_pool_factory import make_v4_pool

EXECUTOR = "0x690B9A9E9aa1C9dB991C7721a92d351Db4FaC990"
V4_POOL_MANAGER = "0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"
# keccak(abi.encode(pool_key)) for the (USDC, WETH, 500, 10, hooks=0) key —
# the factory helper asserts the round-trip, so it must be the real id.
V4_POOL_ID = "0x4f88f7c99022eace4740c6898f59ce6a2e798a1e64ce54589720b7153eb224a7"
V2_POOL_A = "0x1100000000000000000000000000000000000000"
V2_POOL_B = "0x1200000000000000000000000000000000000000"


def _payload(pid: int, *, net: int = 500_000_000_000, failure: dict | None = None) -> dict:
    return {
        "path_id": pid,
        "gross_profit": 600_000_000_000,
        "net_profit": net,
        "gas_used": 300_000,
        "priority_fee": 2,
        "base_fee_next": 30,
        "execute_calldata": b"\xab\x58\x98\xe8\x01",
        "access_list": None,
        "captured_swaps": [],
        "hop_count": 2,
        "failure": failure,
    }


def _pool_key(hop: dict[str, Any]) -> str:
    """The render-dict's pool identity — the Rust derive rule's OUTPUT shape."""
    return hop["pool_id_hex"] if hop["family"] == "V4" else hop["pool_address"]


@pytest.fixture
def mixed_engine_and_paths() -> tuple[ArbitrageEngine, int, int, set[str], set[str]]:
    """A real engine + a registered V2-only path AND a V4-V2 path (offline).

    Returns ``(engine, v2_pid, v4v2_pid, v2_pools, v4v2_pools)`` where the
    pool sets are the EXPECTED mutual-exclusion keys (the checksummed V2
    addresses / the V4 pool id) the Rust ``derive_path_pools`` walk must
    produce — byte-identically — on both entry arms.
    """
    py_bot = Bot()
    weth = make_erc20(
        py_bot,
        "0xC02aaA39b223FE8D0A0e5C4f27eAD9083C756Cc2",
        chain_id=1,
        name="Wrapped Ether",
        symbol="WETH",
        decimals=18,
    )
    usdc = make_erc20(
        py_bot,
        "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
        chain_id=1,
        name="USD Coin",
        symbol="USDC",
        decimals=6,
    )
    pool_a = make_v2_pool(
        address=V2_POOL_A,
        token0=weth,
        token1=usdc,
        factory="0x0000000000000000000000000000000000000000",
        fee_token0=Fraction(3, 1000),
        fee_token1=Fraction(3, 1000),
        reserves_token0=800 * 10**18,
        reserves_token1=1_600_000 * 10**6,
        state_block=18_000_000,
        py_bot=py_bot,
    )
    pool_b = make_v2_pool(
        address=V2_POOL_B,
        token0=usdc,
        token1=weth,
        factory="0x0000000000000000000000000000000000000000",
        fee_token0=Fraction(3, 1000),
        fee_token1=Fraction(3, 1000),
        reserves_token0=1_500_000 * 10**6,
        reserves_token1=800 * 10**18,
        state_block=18_000_000,
        py_bot=py_bot,
    )
    registry = EngineRegistry(bot=None, engine=ArbitrageEngine(py_bot=py_bot))
    registry.register_v2_pool(pool_a)
    registry.register_v2_pool(pool_b)
    v2_pid, _created = registry.register_path([(pool_a, True), (pool_b, False)])

    v4 = make_v4_pool(
        pool_id=V4_POOL_ID,
        pool_manager_address=V4_POOL_MANAGER,
        token0=usdc,
        token1=weth,
        fee=500,
        tick_spacing=10,
        hook_address="0x0000000000000000000000000000000000000000",
        sqrt_price_x96=2_198_666_895_605_149_686_863,
        tick=-76020,
        liquidity=9876543210,
        protocol_fee_zero_for_one=0,
        protocol_fee_one_for_zero=0,
        lp_fee=500000,
        state_block=18_000_000,
        py_bot=py_bot,
    )
    asyncio.run(registry.register_v4_pool(v4))
    v4v2_pid, _created2 = registry.register_path([(v4, True), (pool_a, False)])
    return (
        registry.engine,
        v2_pid,
        v4v2_pid,
        {V2_POOL_A, V2_POOL_B},  # expected V2-path pool keys
        {V4_POOL_ID, V2_POOL_A},  # expected V4-V2-path pool keys
    )


# ── the payload arm over the real seam ───────────────────────────────────


class TestPayloadSeamArms:
    """merge_payload_results_py — the Rust-owned categorization verdicts."""

    def test_submit_unprofitable_failure_arms(self, mixed_engine_and_paths) -> None:
        engine, v2_pid, v4v2_pid, _v2_pools, _v4v2_pools = mixed_engine_and_paths
        out = merge_payload_results_py(
            [
                _payload(v2_pid),
                _payload(v4v2_pid),
                _payload(v2_pid, net=0),
                _payload(v2_pid, failure={"fail_index": 1, "revert_data": "", "bucket": None}),
            ],
            engine,
            EXECUTOR,
        )
        # Two submits (both paths), one below-threshold, one failure row.
        assert sorted(c.path_id for c in out.candidates) == sorted([v2_pid, v4v2_pid])
        assert out.unprofitable_count == 1
        assert len(out.failures) == 1
        rec = out.failures[0]
        assert rec["path_id"] == v2_pid
        assert rec["bucket"] == "inline-fail"  # the Rust default bucket
        assert rec["fail_index"] == 1
        assert rec["revert_data"] == ""
        # Verdicts carry the Rust categorization as kind strings.
        kinds = {(v.path_id, v.kind) for v in out.verdicts}
        assert (v2_pid, "submit") in kinds
        assert (v4v2_pid, "submit") in kinds
        assert (v2_pid, "unprofitable") in kinds
        # path_infos render parity (the [profit] render source).
        assert out.path_infos[v2_pid]["path_type"] == "V2-V2"
        assert out.path_infos[v4v2_pid]["path_type"] == "V4-V2"


# ── THE NUUJFA PARITY GATE: one rule, byte-identical on both arms ────────


class TestPathPoolsParityAcrossEntryArms:
    """The mutual-exclusion keying rule (V4 → pool_id_hex; V2/V3 → EIP-55
    address) is derived ONCE in Rust (``derive_path_pools``); the payload
    arm's submit rows must carry the byte-identical set — asserted per
    hop-family here, plus the blocking semantics through the Dispatcher.
    """

    def test_payload_rows_carry_typed_hop_pool_keys(self, mixed_engine_and_paths) -> None:
        engine, v2_pid, v4v2_pid, v2_pools, v4v2_pools = mixed_engine_and_paths
        out = merge_payload_results_py([_payload(v2_pid), _payload(v4v2_pid)], engine, EXECUTOR)
        by_pid = {c.path_id: c for c in out.candidates}
        # The V2 path: checksummed-address keys (checksummed V2 addresses).
        assert by_pid[v2_pid].path_pools == v2_pools
        # The V4-V2 path: the V4 pool_id_hex + the checksummed V2 address.
        assert by_pid[v4v2_pid].path_pools == v4v2_pools
        # And the [profit] render rows (the engine's typed hops) agree
        # hop-for-hop with the SETS the rows stamped.
        assert {_pool_key(h) for h in out.path_infos[v4v2_pid]["hops"]} == v4v2_pools
        assert {_pool_key(h) for h in out.path_infos[v2_pid]["hops"]} == v2_pools

    def test_ffi_join_map_and_payload_derive_the_same_sets(self, mixed_engine_and_paths) -> None:
        """The FFI batch arm's PathInfo source (the outcome.path_infos join
        map, consumed by join_sim_result → derive_path_pools) and the
        payload arm's submit-row path_pools must be byte-identical for the
        same path id — one typed-hop source, one derive walk."""
        engine, _v2_pid, v4v2_pid, _v2_pools, v4v2_pools = mixed_engine_and_paths
        out = merge_payload_results_py([_payload(v4v2_pid)], engine, EXECUTOR)
        join_map_hops = engine.payload_path_info(v4v2_pid)
        assert join_map_hops is not None
        row = next(c for c in out.candidates if c.path_id == v4v2_pid)
        assert row.path_pools == {_pool_key(h) for h in join_map_hops["hops"]}
        assert row.path_pools == v4v2_pools
        assert V4_POOL_ID in row.path_pools  # the V4 key is the salted id
        assert V2_POOL_A in row.path_pools  # the V2 key is the checksummed addr

    def test_mutual_exclusion_blocking_semantics(self, mixed_engine_and_paths) -> None:
        """The payload arm's path_pools block through the Dispatcher exactly
        like the FFI batch's (is_path_blocked / reserve_pools membership)."""
        from degenbot._ffi.submission import Dispatcher

        engine, v2_pid, v4v2_pid, _v2_pools, _v4v2_pools = mixed_engine_and_paths
        out = merge_payload_results_py([_payload(v2_pid), _payload(v4v2_pid)], engine, EXECUTOR)
        dispatcher = Dispatcher.for_block(100)
        rows_by_pid = {c.path_id: c for c in out.candidates}
        v2_pools = rows_by_pid[v2_pid].path_pools
        v4v2_pools = rows_by_pid[v4v2_pid].path_pools

        # Nothing reserved: both clean.
        assert not dispatcher.is_path_blocked(v2_pools, set())
        assert not dispatcher.is_path_blocked(v4v2_pools, set())
        # Reserve the V2 arm; the V4-V2 arm shares V2_POOL_A → blocked.
        dispatcher.reserve_pools(set(v2_pools))
        assert dispatcher.is_path_blocked(v4v2_pools, set())
        assert dispatcher.is_path_blocked(v2_pools, set())
        # Pending-reservation membership (no committed set) also blocks.
        fresh = Dispatcher.for_block(100)
        fresh.reserve_pools(set(v4v2_pools))
        assert fresh.is_path_blocked(v4v2_pools, set())
        # A disjoint path would not block — sanity on set membership.
        assert not fresh.is_path_blocked({"0x9900000000000000000000000000000000000000"}, set())


# ── the merged-outcome stitching over the seam ───────────────────────────


def _session(engine: Any) -> Any:
    """The session double: the real engine behind the registry boundary."""
    return types.SimpleNamespace(
        dispatcher=type("D", (), {"current_block": 42})(),
        sim_ctx=types.SimpleNamespace(executor_address=EXECUTOR),
        engine_registry=type("R", (), {"engine": engine})(),
    )


def _base_outcome() -> Any:
    """The FFI batch outcome double (the mixed-batch stitch fixture)."""
    return types.SimpleNamespace(
        gas_profitable=["legacy-cand"],
        gas_unprofitable_count=1,
        exception_count=0,
        fail_count=2,
        candidate_count=3,
        suppressed_count=0,
        thin_dropped=0,
        divergent_dropped=0,
        fot_dropped=0,
        fail_buckets={"rpc-failed": 2},
        failures=[{"path_id": 1, "bucket": "rpc-failed"}],
        path_infos={1: {"path_type": "V2", "hops": []}},
    )


class TestMergedOutcomeStitch:
    """_merge_payload_outcome — Python renders + stitches only (NUUJFA)."""

    def test_merge_payload_outcome_builds_submit_candidate(self, mixed_engine_and_paths) -> None:
        engine, v2_pid, _v4v2_pid, _v2_pools, _v4v2_pools = mixed_engine_and_paths
        merged = _merge_payload_outcome(_session(engine), None, {v2_pid: _payload(v2_pid)})
        assert merged is not None
        assert bool(merged)
        assert merged.candidate_count == 1
        assert merged.fail_count == 0
        assert merged.gas_unprofitable_count == 0
        cand = merged.gas_profitable[0]
        assert cand.path_id == v2_pid
        assert int(cand.net_profit) == 500_000_000_000
        # path_infos attribute parity (the [profit] render source).
        assert merged.path_infos[v2_pid]["path_type"] == "V2-V2"

    def test_merge_payload_failure_surfaces_sim_fail_record(self, mixed_engine_and_paths) -> None:
        engine, v2_pid, _v4v2_pid, _v2_pools, _v4v2_pools = mixed_engine_and_paths
        payloads = {
            v2_pid: _payload(
                v2_pid,
                failure={
                    "fail_index": 3,
                    "revert_data": bytes([0xDE, 0xAD]),
                    "bucket": "inline-revert",
                },
            )
        }
        merged = _merge_payload_outcome(_session(engine), None, payloads)
        assert merged is not None
        assert merged.gas_profitable == []
        assert merged.fail_count == 1
        assert merged.fail_buckets == {"inline-revert": 1}
        rec = merged.failures[0]
        assert rec["path_id"] == v2_pid
        assert rec["bucket"] == "inline-revert"
        assert rec["fail_index"] == 3
        assert rec["revert_data"].startswith("dead")

    def test_merge_mixed_batch_stitches_base_outcome(self, mixed_engine_and_paths) -> None:
        engine, v2_pid, v4v2_pid, _v2_pools, _v4v2_pools = mixed_engine_and_paths
        # net=0 is the below-threshold arm (Rust-categorized).
        payloads = {v2_pid: _payload(v2_pid), v4v2_pid: _payload(v4v2_pid, net=0)}
        merged = _merge_payload_outcome(_session(engine), _base_outcome(), payloads)
        assert merged is not None
        assert len(merged.gas_profitable) == 2
        assert merged.gas_profitable[0] == "legacy-cand"
        assert merged.gas_unprofitable_count == 2  # 1 base + 1 below threshold
        assert merged.candidate_count == 5  # 3 base + 1 payload cand + 1 unprof
        assert merged.fail_buckets["rpc-failed"] == 2
        assert set(merged.path_infos) == {1, v2_pid, v4v2_pid}

    def test_below_threshold_payload_counts_unprofitable(self, mixed_engine_and_paths) -> None:
        engine, v2_pid, _v4v2_pid, _v2_pools, _v4v2_pools = mixed_engine_and_paths
        merged = _merge_payload_outcome(_session(engine), None, {v2_pid: _payload(v2_pid, net=0)})
        assert merged is not None
        assert merged.gas_profitable == []
        assert merged.gas_unprofitable_count == 1

    def test_unregistered_payload_path_fails_loud(self, mixed_engine_and_paths) -> None:
        """The old arm silently submitted with an EMPTY path_pools set (no
        mutual exclusion) for an unresolvable path. The seam fails closed
        instead — the payload is rejected, never dispatched without an
        isolation key set."""
        engine, _v2_pid, _v4v2_pid, _v2_pools, _v4v2_pools = mixed_engine_and_paths
        with pytest.raises(ValueError, match="not registered in this engine"):
            _merge_payload_outcome(_session(engine), None, {999: _payload(999)})
